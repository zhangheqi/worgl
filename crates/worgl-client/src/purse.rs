//! A holder that talks to its mints and saves itself.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use tokio::time::{Instant, sleep, sleep_until};
use worgl::{KeySet, Note, PublicKey, TransferRequest};
use worgl_sdk::{Balance, Holder, Payment, PaymentRequest, ScheduledTransfer, Spacing};

use crate::client::{Client, unix_now};
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
    /// Every note in the answers, the counterparty's included, to hand on
    /// when the counterparty did not submit them itself.
    pub notes: Vec<Note>,
    /// Transfers the mint refused, or that kept failing on the way.
    pub refused: Vec<Refused>,
}

/// A transfer that did not go through, and why.
#[derive(Debug)]
pub struct Refused {
    /// The transfer.
    pub request: TransferRequest,
    /// The last error. [`Error::is_transient`] tells whether submitting it
    /// again later may work; a refusal such as `spent` or `expired` will
    /// not.
    pub error: Error,
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
    /// spent and the keys for the change are saved before the payment is
    /// returned. Send it to the payee, who submits it, then
    /// [`settle`](Self::settle) later to take in the change.
    pub async fn pay(&self, request: &PaymentRequest, spacing: Spacing) -> Result<Payment, Error> {
        let view = self.client.view(&request.mint).await?;
        self.change(
            |holder| Ok(holder.pay(&view, request, spacing)?),
            |holder, payment| {
                for transfer in &payment.transfers {
                    holder.cancel(&transfer.request);
                }
            },
        )
    }

    /// Takes a payment for `request`: checks that it pays what was asked,
    /// then submits each transfer at its time and takes in the notes.
    /// Returns once the last transfer is answered or refused. The payer
    /// chose the times; wrap the call in a timeout to bound the wait.
    pub async fn receive(
        &self,
        request: &PaymentRequest,
        payment: &Payment,
    ) -> Result<Submitted, Error> {
        request.check(payment)?;
        self.submit(&payment.transfers).await
    }

    /// Takes in notes the counterparty handed over from the mint's
    /// answers, after checking each one's signature against its mint's keys.
    /// Notes for keys this purse never handed out are ignored. Returns the
    /// value taken in.
    pub async fn accept(&self, notes: &[Note]) -> Result<u128, Error> {
        let mut keys: HashMap<PublicKey, KeySet> = HashMap::new();
        for note in notes {
            if !keys.contains_key(note.issuer()) {
                let set = self.client.keys(note.issuer()).await?;
                keys.insert(*note.issuer(), set);
            }
            note.verify(&keys[note.issuer()])?;
        }
        self.change(|holder| Ok(holder.absorb(notes)), |_, _| {})
    }

    /// Resends the transfers this purse made and has not seen answered, at
    /// random gaps of `spacing`, and takes in their change. Call it once a
    /// payee has had time to submit a payment, or after a restart. Planned
    /// refreshes still waiting for their time are resent too.
    ///
    /// The mint keeps the answers to its most recent transfers only, and
    /// each only until the notes it issued expire: settle before many more
    /// have passed and while the change is live, or a transfer already
    /// applied is refused as `spent` or `expired` and its change is lost.
    pub async fn settle(&self, spacing: Spacing) -> Result<Submitted, Error> {
        let transfers = self.lock().resend(spacing);
        self.submit(&transfers).await
    }

    /// Plans the refresh of `mint`'s notes that die at the next rotation,
    /// after reading its info, and saves the plan. Submit the transfers at
    /// their times with [`submit`](Self::submit).
    pub async fn plan_refresh(&self, mint: &PublicKey) -> Result<Vec<ScheduledTransfer>, Error> {
        let view = self.client.view(mint).await?;
        self.change(
            |holder| Ok(holder.refresh(&view, unix_now())),
            |holder, transfers| {
                for transfer in transfers {
                    holder.cancel(&transfer.request);
                }
            },
        )
    }

    /// Refreshes `mint`'s notes that die at the next rotation: plans the
    /// transfers and submits each at its random time before the rotation.
    /// Returns once the last one is done, which may be most of a rotation
    /// period away; run it in a task of its own.
    pub async fn refresh(&self, mint: &PublicKey) -> Result<Submitted, Error> {
        let transfers = self.plan_refresh(mint).await?;
        self.submit(&transfers).await
    }

    /// Drops `mint`'s notes whose layer was dropped, after reading its info,
    /// and returns the value lost.
    pub async fn prune(&self, mint: &PublicKey) -> Result<u128, Error> {
        let view = self.client.view(mint).await?;
        self.change(|holder| Ok(holder.prune(&view)), |_, _| {})
    }

    /// Submits each transfer at its time, counted from now, retrying
    /// failures on the way, and records each answer: the transfer stops
    /// being outgoing and the notes for this purse are taken in.
    ///
    /// A refused transfer does not stop the others; it is listed in
    /// [`Submitted::refused`]. The call fails only if a save does.
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
                    submitted.value += self.change(
                        |holder| Ok(holder.settle(&transfer.request, &notes)),
                        |_, _| {},
                    )?;
                    submitted.answered += 1;
                    submitted.notes.extend(notes);
                }
                Err(error) => submitted.refused.push(Refused {
                    request: transfer.request.clone(),
                    error,
                }),
            }
        }
        Ok(submitted)
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
