//! A holder that talks to its mints and saves itself.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use tokio::time::{Instant, sleep, sleep_until};
use worgl::{KeySet, Note, PublicKey, TransferRequest};
use worgl_sdk::{Balance, Holder, Payment, PaymentRequest, ScheduledTransfer, Spacing};

use crate::client::Client;
use crate::error::Error;
use crate::store::{FileStore, Locked, Store};

/// Longest wait honored before submitting a transfer, in seconds: a year.
/// Later times are submitted at this delay.
pub const MAX_DELAY_SECS: u64 = 365 * 24 * 60 * 60;

/// Longest a mint's `Retry-After` makes a purse wait before a retry.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// A [`Holder`] connected to a server and a [`Store`]: it reads mint info
/// itself, submits transfers at their scheduled times, retries what failed
/// on the way, and saves after every change.
///
/// Nothing that depends on a change leaves the purse before the change is
/// saved: a payment request's keys are saved before the request is
/// returned, and the spent notes and change keys of a payment before the
/// payment is. If a save fails, the change is undone and the call fails.
///
/// Methods take `&self`, so one purse can be shared between tasks; a
/// refresh waiting for its schedule does not block payments. The holder is
/// locked only while it changes, never across a request.
///
/// Several processes can share one store too. Before each change the purse
/// takes the store's [lock](Store::lock) and reads the holder saved there,
/// makes the change to that, and saves before letting go, so a note
/// another process took in meanwhile is kept, not saved over. Reads such as
/// [`holder`](Self::holder) show the holder as this purse last read or
/// saved it.
#[derive(Debug)]
pub struct Purse<S: Store = FileStore> {
    client: Client,
    holder: Mutex<Holder>,
    store: S,
    retry: Retry,
}

/// How a purse resends a transfer that failed on the way: a lost
/// connection, a timeout, or a mint that was paused, busy or could not
/// store it. Resending is safe, since the mint answers an identical
/// transfer with the identical notes, before or after a rotation, for as
/// long as it remembers the transfer (see [`Purse::settle`]).
///
/// When the mint answers `busy`, the next wait is as long as its
/// `Retry-After` asks, up to 30 seconds, if that is longer; the waits after
/// it double from where they were. A `Retry-After` on any other answer,
/// such as a proxy's, changes no wait. The mint answers `busy` only to a
/// request it did not apply, so with the default and [`Client::new`]'s
/// 30-second timeout the last try still ends within 127 seconds of any the
/// mint may have applied, inside the 128 seconds a mint keeps each answer
/// by default, and is answered as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    /// Tries in all, the first included.
    pub attempts: u32,
    /// Wait before the second try; it doubles before each further one.
    pub first_delay: Duration,
}

impl Default for Retry {
    /// Four tries, one, two and four seconds apart.
    fn default() -> Self {
        Self {
            attempts: 4,
            first_delay: Duration::from_secs(1),
        }
    }
}

/// What submitting a set of transfers came to.
#[derive(Debug, Default)]
pub struct Submitted {
    /// Value the purse took in: payments to it and its own change.
    pub value: u128,
    /// Transfers the mint answered.
    pub answered: usize,
    /// Every note in the answers, this purse's own included.
    pub notes: Vec<Note>,
    /// The notes in the answers for neither a note this purse holds nor a
    /// key it expects: the counterparty's (see [`Holder::theirs`]). Hand
    /// them over for its [`Purse::accept`]: a payee hands the payer its
    /// change, and a payer that submitted a payment hands the payee its
    /// notes. The payer's purse keeps those for the payee too, until
    /// [`forget_owed`](Purse::forget_owed) says they are handed over.
    pub theirs: Vec<Note>,
    /// Transfers the mint refused, or that kept failing on the way.
    pub refused: Vec<Refused>,
}

/// A transfer that did not go through, and why.
#[derive(Debug)]
pub struct Refused {
    /// The transfer.
    pub request: TransferRequest,
    /// The last error. [`Error::is_transient`] tells whether submitting it
    /// again later may work; a refusal such as `spent`, `expired` or
    /// `layer_full` will not.
    pub error: Error,
    /// For a transfer refused as `spent` or `expired` that this purse made
    /// or is paid by, what it may lose ([`Holder::at_stake`]), as it stood
    /// before the transfer was dropped: the transfer may have been applied,
    /// with an answer the mint no longer keeps. Only a counterparty that
    /// submitted it can then hand the notes over, for
    /// [`accept`](Purse::accept). `None` otherwise.
    pub at_stake: Option<u128>,
    /// What the purse did with the transfer.
    pub outcome: Outcome,
}

/// What a purse did with a transfer the mint refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing: a transfer this purse made stays outgoing, for the next
    /// [`settle`](Purse::settle). So it is after a refusal that may pass
    /// later, one [`Error::is_transient`] accepts or `frozen`, and after one
    /// that shows nothing about the transfer: `unknown_mint` or `unissued`
    /// from a server that is not the mint's or lost its state, `spent` or
    /// `expired` with another status than the mint's 409, `layer_full` with
    /// another than its 503, or `unauthorized` or `duplicate_input`, which
    /// the mint never answers to a transfer a holder signed.
    Kept,
    /// Refused as `spent` or `expired`, with the mint's status 409: the note
    /// it spends is gone, so the purse [forgot](Holder::forget) the
    /// transfer. Change the counterparty hands over later is still taken in
    /// by [`accept`](Purse::accept).
    Dropped,
    /// Refused as `layer_full`, with the mint's status 503, or as `policy`,
    /// when the policy the mint publishes now refuses it too: the purse
    /// [cancelled](Holder::cancel) the transfer and took its note back. The
    /// mint checks the note before the layer the new notes would land in, so
    /// after `layer_full` the note is live and the transfer was never
    /// applied, and it never will be: that layer gets no bits back before
    /// the note's own is dropped.
    Cancelled,
}

impl Purse<FileStore> {
    /// A purse saved in the file at `path`, created on the first save if it
    /// does not exist yet.
    pub fn open(client: Client, path: impl Into<PathBuf>) -> Result<Self, Error> {
        let store = FileStore::new(path);
        let holder = store.load()?;
        Ok(Self::new(client, holder, store))
    }
}

impl<S: Store> Purse<S> {
    /// A purse holding `holder`, talking to `client` and saving to `store`.
    pub fn new(client: Client, holder: Holder, store: S) -> Self {
        Self {
            client,
            holder: Mutex::new(holder),
            store,
            retry: Retry::default(),
        }
    }

    /// Sets how failed transfers are retried.
    pub fn with_retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }

    /// The client requests go through.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The store saves go to.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// A copy of the holder as it is now.
    pub fn holder(&self) -> Holder {
        self.lock().clone()
    }

    /// Value held at `mint`, by what it can do now, from the mint's info.
    pub async fn balance(&self, mint: &PublicKey) -> Result<Balance, Error> {
        let view = self.client.view(mint).await?;
        Ok(self.lock().balance(&view))
    }

    /// Asks for `amount` from `mint`: a payment request with a fresh key
    /// per note, saved before it is returned. Send it to the payer.
    pub async fn request(&self, mint: &PublicKey, amount: u64) -> Result<PaymentRequest, Error> {
        let view = self.client.view(mint).await?;
        self.change(
            |holder| Ok(holder.request(&view, amount)?),
            |holder, request| holder.withdraw(request),
        )
    }

    /// Like [`request`](Self::request), in notes of at most `largest`, for a
    /// payer whose notes are small.
    pub async fn request_at_most(
        &self,
        mint: &PublicKey,
        amount: u64,
        largest: u64,
    ) -> Result<PaymentRequest, Error> {
        let view = self.client.view(mint).await?;
        self.change(
            |holder| Ok(holder.request_at_most(&view, amount, largest)?),
            |holder, request| holder.withdraw(request),
        )
    }

    /// Asks a playground faucet for a note and keeps it. The faucet's mint
    /// must be on this purse's server; the faucet may share its URL. The
    /// note goes to a fresh key, saved before the faucet is asked, as for
    /// any payment request. Returns the faucet's mint and the value kept.
    pub async fn take_from_faucet(&self, faucet: &Client) -> Result<(PublicKey, u128), Error> {
        let about = faucet.faucet().await?;
        let request = self
            .request_at_most(&about.mint, about.amount, about.amount)
            .await?;
        let [output] = request.outputs.as_slice() else {
            self.withdraw(&request)?;
            return Err(Error::FaucetAmount(about.amount));
        };
        let taken = match faucet.ask_faucet(&output.holder).await {
            Ok(note) => self.accept(&[note]).await,
            Err(error) => Err(error),
        };
        if taken.is_err() {
            self.withdraw(&request)?;
        }
        Ok((about.mint, taken?))
    }

    /// Stops expecting the notes of a request that will not be paid.
    pub fn withdraw(&self, request: &PaymentRequest) -> Result<(), Error> {
        self.change(
            |holder| {
                holder.withdraw(request);
                Ok(())
            },
            |_, ()| {},
        )
    }

    /// Answers a payment request, after reading the mint's info. The notes
    /// spent, the keys for the change and the payment itself (see
    /// [`Holder::payments`]) are saved before the payment is returned. Send
    /// it to the payee, who submits it and can hand back the change for
    /// [`accept`](Self::accept); or [`settle`](Self::settle) later to take it
    /// in. A request for a note the mint would not make, or to a key this
    /// purse has paid already, is refused before anything is signed (see
    /// [`Holder::pay`]).
    pub async fn pay(&self, request: &PaymentRequest, spacing: Spacing) -> Result<Payment, Error> {
        let view = self.client.view(&request.mint).await?;
        self.change_or_restore(|holder| Ok(holder.pay(&view, request, spacing)?))
    }

    /// Takes back the note of an outgoing transfer the mint never applied,
    /// keeping the keys of its change in case it did, and saves (see
    /// [`Holder::cancel`]). Returns whether the transfer was outgoing.
    ///
    /// A payee that holds the transfer can still submit it: if the mint
    /// applies it, or already had, the payee's notes and the note taken back
    /// claim the same value, and the mint refuses whichever is spent second.
    pub fn cancel(&self, request: &TransferRequest) -> Result<bool, Error> {
        self.change_or_restore(|holder| Ok(holder.cancel(request)))
    }

    /// Drops an outgoing transfer whose note is gone, keeping the keys of its
    /// change so that change handed over later is still taken in, and saves
    /// (see [`Holder::forget`]). Returns whether the transfer was outgoing.
    pub fn forget(&self, request: &TransferRequest) -> Result<bool, Error> {
        self.change_or_restore(|holder| Ok(holder.forget(request)))
    }

    /// Stops keeping notes owed to payees, once they are handed over, and
    /// saves (see [`Holder::forget_owed`]). The notes of a payment are in
    /// [`Holder::payments`]: write them where the payee gets them, then
    /// forget them. Returns the value it stopped keeping.
    pub fn forget_owed(&self, notes: &[Note]) -> Result<u128, Error> {
        self.change_or_restore(|holder| Ok(holder.forget_owed(notes)))
    }

    /// Takes a payment for `request`: checks that it pays what was asked,
    /// then submits each transfer at its time and takes in the notes.
    /// Returns once the last transfer is answered or refused. The payer
    /// chose the times; wrap the call in a timeout to bound the wait. Hand
    /// [`Submitted::theirs`], the payer's change, back to the payer.
    pub async fn receive(
        &self,
        request: &PaymentRequest,
        payment: &Payment,
    ) -> Result<Submitted, Error> {
        request.check(payment)?;
        self.submit(&payment.transfers).await
    }

    /// Takes in notes the counterparty handed over from the mint's answers,
    /// after checking each one's signature against its mint's keys, and
    /// settles the outgoing transfers they answer (see
    /// [`Holder::take_answers`]). Notes for keys this purse never handed out
    /// are ignored. A note whose layer the mint has dropped no longer
    /// verifies and fails the call. Returns the value taken in.
    ///
    /// The signatures show that the mint issued the notes, not that it
    /// applied the transfers they settle: a counterparty could pay the same
    /// keys from notes of its own (see [`Holder::take_answers`]).
    pub async fn accept(&self, notes: &[Note]) -> Result<u128, Error> {
        let mut keys: HashMap<PublicKey, KeySet> = HashMap::new();
        for note in notes {
            if !keys.contains_key(note.issuer()) {
                let set = self.client.keys(note.issuer()).await?;
                keys.insert(*note.issuer(), set);
            }
            note.verify(&keys[note.issuer()])?;
        }
        self.change(|holder| Ok(holder.take_answers(notes)), |_, _| {})
    }

    /// Resends the transfers this purse made and has not seen answered, at
    /// random gaps of `spacing`, and takes in their change. Call it once a
    /// payee has had time to submit a payment, or after a restart. Planned
    /// refreshes still waiting for their time are resent too. The answer to
    /// a payment's transfer keeps the payee's notes for it, in case the payee
    /// has not submitted the payment and the resend carried it out (see
    /// [`MadePayment::owed`](worgl_sdk::MadePayment::owed)), and a transfer
    /// refused for good is dropped or cancelled (see [`Refused::outcome`]).
    ///
    /// The mint answers a resent transfer with the same notes only while it
    /// keeps its answer, and at the latest until the notes it issued expire
    /// (see [Error codes]). After that, a transfer already applied is
    /// refused as `spent` or `expired`, with [`Refused::at_stake`] set, and
    /// its change can only come from the counterparty, for
    /// [`accept`](Self::accept). [`refresh`](Self::refresh) resends the
    /// transfers whose chance the next rotation ends.
    ///
    /// [Error codes]: https://github.com/zhangheqi/worgl/blob/main/site/src/content/docs/docs/error-codes.md#sending-a-request-again
    pub async fn settle(&self, spacing: Spacing) -> Result<Submitted, Error> {
        let transfers = self.lock().resend(spacing);
        self.submit(&transfers).await
    }

    /// Plans the refresh of `mint`'s notes that die at the next rotation,
    /// after reading its info, and saves the plan. The outgoing transfers
    /// to resend before that rotation join it (see
    /// [`Holder::due_resends`]); if the save fails, only the refreshes are
    /// undone. Submit the transfers at their times with
    /// [`submit`](Self::submit).
    ///
    /// The times are planned by the server's clock, from the `Date` of its
    /// info answer ([`Client::view_and_time`]), so this machine's clock,
    /// slow or fast, moves none of them past the rotation. When the answer
    /// has no `Date`, they are planned by this machine's clock instead. Once
    /// the rotation's time has come and the mint has not made it yet, they
    /// are all due at once.
    pub async fn plan_refresh(&self, mint: &PublicKey) -> Result<Vec<ScheduledTransfer>, Error> {
        let (view, now) = self.client.view_and_time(mint).await?;
        let (mut transfers, resends) = self.change_or_restore(|holder| {
            // Before the refreshes join the outgoing transfers.
            let resends = holder.due_resends(&view, now);
            Ok((holder.refresh(&view, now), resends))
        })?;
        transfers.extend(resends);
        transfers.sort_by_key(|t| t.after_secs);
        Ok(transfers)
    }

    /// Refreshes `mint`'s notes that die at the next rotation and resends
    /// the outgoing transfers that must land before it: plans the transfers
    /// with [`plan_refresh`](Self::plan_refresh) and submits each at its
    /// random time before the rotation. Returns once the last one is done,
    /// which may be most of a rotation period away; run it in a task of its
    /// own.
    pub async fn refresh(&self, mint: &PublicKey) -> Result<Submitted, Error> {
        let transfers = self.plan_refresh(mint).await?;
        self.submit(&transfers).await
    }

    /// Drops `mint`'s notes whose layer was dropped, after reading its info,
    /// and returns the value lost. It also stops waiting for unconfirmed
    /// change that would sit in a dropped layer, and stops keeping notes
    /// owed to payees once their layer is dropped (see [`Holder::prune`]).
    /// The info alone decides, not this machine's clock: a note goes, with
    /// its key, only once the mint has dropped its layer.
    pub async fn prune(&self, mint: &PublicKey) -> Result<u128, Error> {
        let view = self.client.view(mint).await?;
        self.change(|holder| Ok(holder.prune(&view)), |_, _| {})
    }

    /// Submits each transfer at its time, counted from now, retrying
    /// failures on the way, and records each answer: the transfer stops
    /// being outgoing and the notes for this purse are taken in, and the
    /// payee's notes are kept for it when the transfer is part of a payment
    /// this purse made (see [`Holder::settle`]). A payer that submits its own
    /// payment hands them to the payee from [`Submitted::theirs`], then
    /// calls [`forget_owed`](Self::forget_owed) with them, as `worgl submit`
    /// does.
    ///
    /// A refused transfer does not stop the others; it is listed in
    /// [`Submitted::refused`]. One this purse made that the mint refused for
    /// good is dropped or cancelled ([`Refused::outcome`]); the rest stay
    /// outgoing. To confirm a `policy` refusal, the purse reads the mint's
    /// info again. The call fails only if a save does.
    pub async fn submit(&self, transfers: &[ScheduledTransfer]) -> Result<Submitted, Error> {
        let mut order: Vec<&ScheduledTransfer> = transfers.iter().collect();
        order.sort_by_key(|transfer| transfer.after_secs);
        let start = Instant::now();
        let mut submitted = Submitted::default();
        for transfer in order {
            let delay = Duration::from_secs(transfer.after_secs.min(MAX_DELAY_SECS));
            sleep_until(start + delay).await;
            match self.send(&transfer.request).await {
                Ok(notes) => {
                    let (value, theirs) = self.change(
                        |holder| {
                            let value = holder.settle(&transfer.request, &notes);
                            Ok((value, holder.theirs(&notes)))
                        },
                        |_, _| {},
                    )?;
                    submitted.value += value;
                    submitted.answered += 1;
                    submitted.notes.extend(notes);
                    submitted.theirs.extend(theirs);
                }
                Err(error) => {
                    let (at_stake, outcome) = self.refused(&transfer.request, &error).await?;
                    submitted.refused.push(Refused {
                        request: transfer.request.clone(),
                        error,
                        at_stake,
                        outcome,
                    });
                }
            }
        }
        Ok(submitted)
    }

    /// Drops or cancels a transfer the mint refused, when the refusal shows
    /// it can never be applied, and returns what is at stake on it and what
    /// was done. A transfer that is not outgoing is left as it is.
    async fn refused(
        &self,
        request: &TransferRequest,
        error: &Error,
    ) -> Result<(Option<u128>, Outcome), Error> {
        let cancel = match error.code() {
            Some("spent" | "expired") => {
                // The mint answers both with 409 Conflict. Any other answer,
                // such as a gateway's, shows nothing about the note.
                if !matches!(error, Error::Refused(refusal) if refusal.status == 409) {
                    return Ok((self.lock().at_stake(request), Outcome::Kept));
                }
                return self.change_or_restore(|holder| {
                    // Before the transfer is dropped.
                    let at_stake = holder.at_stake(request);
                    let outcome = if holder.forget(request) {
                        Outcome::Dropped
                    } else {
                        Outcome::Kept
                    };
                    Ok((at_stake, outcome))
                });
            }
            // The mint answers so with 503, after finding every note spent
            // live. Any other answer shows nothing about the notes.
            Some("layer_full") => {
                matches!(error, Error::Refused(refusal) if refusal.status == 503)
            }
            // The mint checks its policy before its receipts, so a transfer
            // applied under a looser policy is refused too: its change keys
            // stay. Cancel only if the policy published now agrees.
            Some("policy") => match request.inputs.first() {
                Some(spend) => match self.client.view(spend.note.issuer()).await {
                    Ok(view) => {
                        let inputs: Vec<u64> =
                            request.inputs.iter().map(|s| s.note.amount()).collect();
                        let outputs: Vec<u64> = request.outputs.iter().map(|o| o.amount).collect();
                        view.policy.check_transfer(&inputs, &outputs).is_err()
                    }
                    Err(_) => false,
                },
                None => false,
            },
            // Anything else may pass later or shows nothing about the
            // transfer: `unknown_mint` or `unissued` from a server that is
            // not the mint's or lost its state, or `unauthorized` or
            // `duplicate_input`, which the mint never answers to a transfer
            // a holder signed, so they come from another server or version.
            _ => false,
        };
        if !cancel {
            return Ok((None, Outcome::Kept));
        }
        let cancelled = self.change_or_restore(|holder| Ok(holder.cancel(request)))?;
        let outcome = if cancelled {
            Outcome::Cancelled
        } else {
            Outcome::Kept
        };
        Ok((None, outcome))
    }

    /// Sends one transfer, retrying while the failure is transient.
    async fn send(&self, request: &TransferRequest) -> Result<Vec<Note>, Error> {
        let mut delay = self.retry.first_delay;
        let mut attempt = 1;
        loop {
            match self.client.transfer(request).await {
                Err(error) if error.is_transient() && attempt < self.retry.attempts => {
                    sleep(next_wait(delay, &error)).await;
                    delay = delay.saturating_mul(2);
                    attempt += 1;
                }
                result => return result,
            }
        }
    }

    /// Like [`change`](Self::change), but a failed save puts the holder back
    /// as it was before `apply`.
    fn change_or_restore<T>(
        &self,
        apply: impl FnOnce(&mut Holder) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let (value, _) = self.change(
            |holder| {
                let before = holder.clone();
                Ok((apply(holder)?, before))
            },
            |holder, (_, before)| *holder = before.clone(),
        )?;
        Ok(value)
    }

    /// Changes the holder as saved now and saves it, holding the store's
    /// lock throughout. If the save fails, `undo` takes the change back and
    /// the save's error is returned.
    fn change<T>(
        &self,
        apply: impl FnOnce(&mut Holder) -> Result<T, Error>,
        undo: impl FnOnce(&mut Holder, &T),
    ) -> Result<T, Error> {
        let mut holder = self.lock();
        // Held until the save is done.
        let locked = self.store.lock()?;
        if let Some(saved) = locked.as_ref().and_then(Locked::saved) {
            *holder = Holder::from_json(saved)?;
        }
        let value = apply(&mut holder)?;
        if let Err(error) = self.store.save(&holder.to_json()) {
            undo(&mut holder, &value);
            return Err(error.into());
        }
        Ok(value)
    }

    fn lock(&self) -> MutexGuard<'_, Holder> {
        // A panic in another task holding the lock should not take the
        // purse down with it.
        self.holder.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The wait before the next try after `error`: `delay`, or after `busy`
/// what the mint's `Retry-After` asks if that is longer, up to
/// [`MAX_RETRY_AFTER`]. It stretches this wait only, never the ones after.
///
/// Only `busy` stretches it: the mint answers so only to a request it did
/// not apply. Another answer, such as a proxy's, may come after the mint
/// applied the try, and longer waits could take the last try past the
/// time the mint keeps the receipt for.
fn next_wait(delay: Duration, error: &Error) -> Duration {
    match error.retry_after() {
        Some(asked) if error.code() == Some("busy") => delay.max(asked.min(MAX_RETRY_AFTER)),
        _ => delay,
    }
}
