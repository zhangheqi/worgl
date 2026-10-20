//! A holder's notes, the keys it has handed out, and the transfers it has
//! signed but not seen land.

use std::cmp::Reverse;
use std::collections::{HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use worgl::{Denominations, HolderKey, Note, Output, PolicyError, PublicKey, TransferRequest};

use crate::mint::{MintView, NoteState};
use crate::payment::{Payment, PaymentRequest, ScheduledTransfer, Spacing};
use crate::random;

/// Most notes one payment request may ask for.
pub const MAX_REQUESTED_NOTES: usize = 256;

/// How many of the keys it paid last a holder remembers, so as to refuse
/// paying one of them again: the notes of four requests of the most notes a
/// request may ask for, or of hundreds of usual ones, for about 67 KiB of
/// the saved store. A key paid by a transfer still outgoing, or by a payment
/// the holder still keeps, is refused however long ago it was paid.
pub const PAID_KEYS_KEPT: usize = 4 * MAX_REQUESTED_NOTES;

/// Version of the format [`Holder::to_json`] writes for a store that holds
/// unconfirmed change or notes owed to payees. A reader of format 1 skips
/// fields it does not know, so it would drop those unseen and lose their
/// value. A store without them is still written as format 1, which such a
/// reader opens, missing only the payments kept and the keys paid last.
const FORMAT: u32 = 2;

/// Seconds before a scheduled rotation by which the transfers that
/// [`Holder::refresh`] and [`Holder::due_resends`] plan are due, or half the
/// time left when that is shorter. A plan made at or past the rotation's
/// time, which the mint has not made yet as far as its info says, has them
/// all due at once. A payer should settle a payment this long before the
/// rotation that drops a note it spends.
pub const REFRESH_MARGIN_SECS: u64 = 300;

/// Seconds over which refreshes are spread when the mint has no schedule.
const UNSCHEDULED_REFRESH_SECS: u64 = 60;

/// The notes one holder owns, with the one-time key of each, plus the keys
/// it has handed out, the transfers it has signed but not yet seen land, and
/// the payments it made until it is done with them.
///
/// It keeps the privacy rules without being told: a fresh key for every
/// note it expects, never two notes in one transfer, transfers of one
/// payment and refreshes spread out in time, and outputs in random order.
/// It does no networking: submit the transfers it makes to the mint, and
/// give it the mint's answers.
///
/// Notes are bearer: whoever holds this store holds the money. Save it with
/// [`to_json`](Self::to_json), encrypted at rest, after every change.
#[derive(Debug, Clone, Default)]
pub struct Holder {
    notes: Vec<Held>,
    expected: Vec<Expected>,
    outgoing: Vec<Outgoing>,
    unconfirmed: Vec<Unconfirmed>,
    payments: Vec<MadePayment>,
    /// The keys this holder paid last, oldest first.
    paid: VecDeque<PublicKey>,
}

/// A note and the key that spends it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Held {
    note: Note,
    #[serde(with = "secret")]
    key: HolderKey,
}

/// A key handed out for a note of `amount` from `mint`, not yet received.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Expected {
    mint: PublicKey,
    amount: u64,
    #[serde(with = "secret")]
    key: HolderKey,
}

/// A transfer this holder signed and has not yet seen answered, with the
/// note it spends.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Outgoing {
    request: TransferRequest,
    spends: Held,
}

/// The key for a note of change from a transfer this holder dropped or
/// cancelled. The note comes only if the counterparty hands it over, and it
/// would sit in `generation`, so it is worthless once that is dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Unconfirmed {
    change: Expected,
    generation: u64,
}

/// A payment this holder made, kept while one of its transfers is
/// [outgoing](Holder::outgoing) or the holder keeps notes owed to the payee
/// for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MadePayment {
    /// The request it answers.
    pub request: PaymentRequest,
    /// The payment as [`Holder::pay`] returned it, to send again to a payee
    /// who lost it. A transfer [cancelled](Holder::cancel) or
    /// [forgotten](Holder::forget) leaves it, so it is never handed out
    /// again; the payment then no longer pays the whole request, which
    /// [`PaymentRequest::check`] tells, and the payee would refuse it.
    pub payment: Payment,
    /// The payee's notes from the mint's answers this holder got itself:
    /// each time it [settles](Holder::settle) one of the payment's
    /// transfers, whether a resend or its own submission of the payment got
    /// the answer. It cannot tell whether the payee submitted the payment
    /// and has them already, so it keeps them for the payee until they are
    /// handed over ([`forget_owed`](Holder::forget_owed)) or their layer is
    /// dropped ([`prune`](Holder::prune)).
    pub owed: Vec<Note>,
}

/// Value a holder has at one mint, by what it can do now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    /// Can be spent now.
    pub spendable: u128,
    /// In the last layer: spendable after the next rotation.
    pub frozen: u128,
    /// Part of the above that dies at the next rotation unless refreshed.
    pub expiring: u128,
    /// In notes the view cannot place: read the mint's info again.
    pub unknown: u128,
    /// In notes spent by transfers not yet answered.
    pub outgoing: u128,
    /// Expected from keys handed out and not yet paid.
    pub expected: u128,
    /// Change of transfers dropped or cancelled, which comes only if the
    /// counterparty hands it over: see [`Holder::forget`].
    #[serde(default)]
    pub unconfirmed: u128,
}

/// A payment request cannot be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    /// The amount is zero.
    #[error("the amount must be positive")]
    Zero,
    /// The amount cannot be split into the mint's denominations.
    #[error("{0} cannot be split into the mint's denominations")]
    Denominations(u64),
    /// It would take more than [`MAX_REQUESTED_NOTES`] notes.
    #[error("the request would need more than {MAX_REQUESTED_NOTES} notes")]
    TooManyNotes,
}

/// A payment cannot be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayError {
    /// The request is for another mint than the view.
    #[error("the request is for another mint")]
    WrongMint,
    /// The request asks for nothing.
    #[error("the request asks for nothing")]
    Empty,
    /// The request asks for a note of an amount that is not one of the
    /// mint's denominations, such as 0. The mint would refuse the transfer.
    #[error("the request asks for a note of {0}, which is not one of the mint's denominations")]
    Denomination(u64),
    /// The request asks for a note to a key this holder has paid already: by
    /// a transfer still outgoing, by a payment it keeps, among the last
    /// [`PAID_KEYS_KEPT`] keys it paid, or twice in the request. The payee
    /// would keep only one of two notes to one key, and the mint would see
    /// the key twice. Paying again takes a new request.
    #[error("the request asks for a note to {0}, which this holder has paid already")]
    Paid(PublicKey),
    /// Not enough value is spendable now. Frozen notes become spendable at
    /// the next rotation.
    #[error("{spendable} is spendable now and {needed} is needed")]
    Unaffordable {
        /// Value spendable now.
        spendable: u128,
        /// Value asked for.
        needed: u128,
    },
    /// Enough value is spendable, but in notes too small for the notes the
    /// request asks for. Ask the payee for a request with smaller notes; see
    /// [`Holder::request_at_most`].
    #[error(
        "the requested notes do not fit the spendable notes; the largest spendable note is {largest}"
    )]
    Fragmented {
        /// The largest spendable note.
        largest: u64,
    },
    /// The change would take more notes than one transfer may create.
    #[error("the change does not fit in {max_outputs} notes per transfer")]
    TooManyOutputs {
        /// The mint's limit.
        max_outputs: u8,
    },
    /// A planned transfer breaks the mint's policy. Plans follow the policy,
    /// so this means a bug.
    #[error("a planned transfer breaks the mint's policy: {0}")]
    Policy(PolicyError),
    /// Signing failed. The library refuses only malformed inputs, so this
    /// means the store is corrupt.
    #[error(transparent)]
    Sign(#[from] worgl::Error),
}

/// A saved store cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// It is not a saved store.
    #[error("not a saved note store: {0}")]
    Json(#[from] serde_json::Error),
    /// It was written by a newer SDK.
    #[error("saved in format {0}, which this SDK does not read")]
    Format(u32),
}

#[derive(Serialize, Deserialize)]
struct Saved {
    format: u32,
    notes: Vec<Held>,
    expected: Vec<Expected>,
    outgoing: Vec<Outgoing>,
    // Missing from stores of format 1 written before format 2 existed.
    #[serde(default)]
    unconfirmed: Vec<Unconfirmed>,
    #[serde(default)]
    payments: Vec<MadePayment>,
    #[serde(default)]
    paid: VecDeque<PublicKey>,
}

impl Holder {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks for `amount` from `mint`, as the fewest notes its denominations
    /// allow, each to a fresh key.
    pub fn request(
        &mut self,
        mint: &MintView,
        amount: u64,
    ) -> Result<PaymentRequest, RequestError> {
        self.request_notes(mint, amount, None)
    }

    /// Asks for `amount` from `mint` in notes of at most `largest` each. A
    /// payer whose notes are small answers [`PayError::Fragmented`] to a
    /// plain [`request`](Self::request); this request it can pay.
    pub fn request_at_most(
        &mut self,
        mint: &MintView,
        amount: u64,
        largest: u64,
    ) -> Result<PaymentRequest, RequestError> {
        self.request_notes(mint, amount, Some(largest))
    }

    fn request_notes(
        &mut self,
        mint: &MintView,
        amount: u64,
        largest: Option<u64>,
    ) -> Result<PaymentRequest, RequestError> {
        if amount == 0 {
            return Err(RequestError::Zero);
        }
        let amounts = match split(
            &mint.policy.denominations,
            amount,
            largest,
            MAX_REQUESTED_NOTES,
        ) {
            Ok(amounts) => amounts,
            Err(Split::Inexact) => return Err(RequestError::Denominations(amount)),
            Err(Split::TooMany) => return Err(RequestError::TooManyNotes),
        };
        let mut outputs = Vec::with_capacity(amounts.len());
        for amount in amounts {
            let key = HolderKey::generate();
            outputs.push(Output {
                amount,
                holder: key.public_key(),
            });
            self.expected.push(Expected {
                mint: mint.issuer,
                amount,
                key,
            });
        }
        random::shuffle(&mut outputs);
        Ok(PaymentRequest {
            mint: mint.issuer,
            outputs,
        })
    }

    /// Stops expecting the notes a request asked for, for a request that
    /// will not be paid.
    pub fn withdraw(&mut self, request: &PaymentRequest) {
        self.expected.retain(|e| {
            !request
                .outputs
                .iter()
                .any(|o| o.holder == e.key.public_key())
        });
    }

    /// Answers a payment request with signed transfers.
    ///
    /// It prefers one note that covers everything, paid in one transfer
    /// with the change back to fresh keys. Otherwise it spends several
    /// notes, one per transfer, largest first, and spaces the transfers by
    /// `spacing`. Only notes [spendable](NoteState::Spendable) under `mint`
    /// are used. The notes spent leave the store and wait among the
    /// [outgoing](Self::outgoing) transfers until [`settle`](Self::settle)
    /// or [`take_answers`](Self::take_answers) sees them answered, and the
    /// payment is kept among the [payments](Self::payments).
    ///
    /// Before signing anything, it refuses a request for a note the mint
    /// would not make, of an amount outside its denominations
    /// ([`PayError::Denomination`]), and a request for a note to a key it
    /// has paid already ([`PayError::Paid`]), such as the same request
    /// twice. Each planned transfer is checked against the mint's policy as
    /// well.
    pub fn pay(
        &mut self,
        mint: &MintView,
        request: &PaymentRequest,
        spacing: Spacing,
    ) -> Result<Payment, PayError> {
        if request.mint != mint.issuer {
            return Err(PayError::WrongMint);
        }
        if request.outputs.is_empty() {
            return Err(PayError::Empty);
        }
        let mut asked = HashSet::new();
        for output in &request.outputs {
            if !mint.policy.denominations.contains(output.amount) {
                return Err(PayError::Denomination(output.amount));
            }
            if !asked.insert(output.holder) || self.has_paid(&output.holder) {
                return Err(PayError::Paid(output.holder));
            }
        }
        let plan = self.plan(mint, request)?;
        for part in &plan {
            let outputs: Vec<u64> = part
                .paid
                .iter()
                .map(|o| o.amount)
                .chain(part.change.iter().copied())
                .collect();
            mint.policy
                .check_transfer(&[self.notes[part.note].note.amount()], &outputs)
                .map_err(PayError::Policy)?;
        }
        let mut transfers = Vec::with_capacity(plan.len());
        let mut expected = Vec::new();
        let mut after_secs = 0u64;
        for (n, part) in plan.iter().enumerate() {
            let held = &self.notes[part.note];
            let mut outputs = part.paid.clone();
            for &amount in &part.change {
                let key = HolderKey::generate();
                outputs.push(Output {
                    amount,
                    holder: key.public_key(),
                });
                expected.push(Expected {
                    mint: mint.issuer,
                    amount,
                    key,
                });
            }
            random::shuffle(&mut outputs);
            let request = TransferRequest::sign(&[(held.note, &held.key)], outputs)?;
            if n > 0 {
                after_secs = after_secs.saturating_add(spacing.gap());
            }
            transfers.push(ScheduledTransfer {
                after_secs,
                request,
            });
        }
        // Nothing failed: move the spent notes out.
        for (part, scheduled) in plan.iter().zip(&transfers) {
            self.outgoing.push(Outgoing {
                request: scheduled.request.clone(),
                spends: self.notes[part.note].clone(),
            });
        }
        let mut used: Vec<usize> = plan.iter().map(|part| part.note).collect();
        used.sort_unstable_by_key(|&i| Reverse(i));
        for i in used {
            self.notes.remove(i);
        }
        self.expected.extend(expected);
        let payment = Payment {
            mint: mint.issuer,
            transfers,
        };
        self.payments.push(MadePayment {
            request: request.clone(),
            payment: payment.clone(),
            owed: Vec::new(),
        });
        self.remember_paid(request.outputs.iter().map(|o| o.holder));
        Ok(payment)
    }

    /// Whether this holder has paid a note to `key`: by an outgoing
    /// transfer, by a payment it keeps, or among the last keys it paid.
    fn has_paid(&self, key: &PublicKey) -> bool {
        self.paid.contains(key)
            || self
                .outgoing
                .iter()
                .any(|o| o.request.outputs.iter().any(|out| out.holder == *key))
            || self
                .payments
                .iter()
                .any(|p| p.request.outputs.iter().any(|out| out.holder == *key))
    }

    /// Adds `keys` to the keys paid last, forgetting the oldest beyond
    /// [`PAID_KEYS_KEPT`].
    fn remember_paid(&mut self, keys: impl IntoIterator<Item = PublicKey>) {
        self.paid.extend(keys);
        let excess = self.paid.len().saturating_sub(PAID_KEYS_KEPT);
        self.paid.drain(..excess);
    }

    /// Chooses which note pays which requested outputs.
    fn plan(&self, mint: &MintView, request: &PaymentRequest) -> Result<Vec<Part>, PayError> {
        let max_outputs = usize::from(mint.policy.max_outputs.max(1));
        let denominations = &mint.policy.denominations;
        let change_of = |room: u64| split(denominations, room, None, max_outputs).ok();
        let needed = request.amount();
        let mut spendable: Vec<usize> = (0..self.notes.len())
            .filter(|&i| matches!(mint.state(&self.notes[i].note), NoteState::Spendable { .. }))
            .collect();
        spendable.sort_by_key(|&i| self.notes[i].note.amount());
        let amount = |i: usize| self.notes[i].note.amount();
        let total: u128 = spendable.iter().map(|&i| u128::from(amount(i))).sum();
        if total < needed {
            return Err(PayError::Unaffordable {
                spendable: total,
                needed,
            });
        }

        // The smallest single note that covers everything, change included.
        for &i in &spendable {
            if u128::from(amount(i)) < needed {
                continue;
            }
            let room = (u128::from(amount(i)) - needed) as u64;
            if let Some(change) = change_of(room)
                && request.outputs.len() + change.len() <= max_outputs
            {
                return Ok(vec![Part {
                    note: i,
                    paid: request.outputs.clone(),
                    change,
                }]);
            }
        }

        // Otherwise the largest notes first, each paying the largest
        // requested outputs it can hold.
        let mut wanted = request.outputs.clone();
        wanted.sort_by_key(|o| Reverse(o.amount));
        let mut notes = spendable.clone();
        let mut plan = Vec::new();
        let mut change_limited = false;
        while !wanted.is_empty() {
            let Some(i) = notes.pop() else { break };
            let mut room = amount(i);
            let mut paid = Vec::new();
            let mut k = 0;
            while k < wanted.len() && paid.len() < max_outputs {
                if wanted[k].amount <= room {
                    room -= wanted[k].amount;
                    paid.push(wanted.remove(k));
                } else {
                    k += 1;
                }
            }
            // Give back the smallest outputs until the change fits.
            let change = loop {
                match change_of(room) {
                    Some(change) if paid.len() + change.len() <= max_outputs => break Some(change),
                    _ if paid.is_empty() => break None,
                    _ => {
                        change_limited = true;
                        let out: Output = paid.pop().expect("paid is not empty");
                        room += out.amount;
                        wanted.push(out);
                    }
                }
            };
            wanted.sort_by_key(|o| Reverse(o.amount));
            if let Some(change) = change
                && !paid.is_empty()
            {
                plan.push(Part {
                    note: i,
                    paid,
                    change,
                });
            }
        }
        if !wanted.is_empty() {
            let max_outputs = mint.policy.max_outputs;
            let largest = spendable.last().map_or(0, |&i| amount(i));
            return Err(if change_limited {
                PayError::TooManyOutputs { max_outputs }
            } else {
                PayError::Fragmented { largest }
            });
        }
        Ok(plan)
    }

    /// Takes in the notes of a mint's answer that were made for this
    /// holder's keys: from a payment it received, its own change, unconfirmed
    /// change included, or a refresh. Notes for other keys are ignored, and a
    /// note already held is not taken twice. Returns the value taken in.
    ///
    /// Give it notes from the mint's own answer, or check them first with
    /// [`Note::verify`]: the store does not check signatures.
    pub fn absorb(&mut self, notes: &[Note]) -> u128 {
        let mut taken = 0;
        for note in notes {
            if self.notes.iter().any(|h| h.note == *note) {
                continue;
            }
            let pays = |e: &Expected| {
                e.mint == *note.issuer()
                    && e.amount == note.amount()
                    && e.key.public_key() == *note.holder()
            };
            let key = if let Some(i) = self.expected.iter().position(pays) {
                self.expected.swap_remove(i).key
            } else if let Some(i) = self.unconfirmed.iter().position(|u| pays(&u.change)) {
                self.unconfirmed.swap_remove(i).change.key
            } else {
                continue;
            };
            self.notes.push(Held { note: *note, key });
            taken += u128::from(note.amount());
        }
        taken
    }

    /// The notes of a mint's answers made for neither a note this holder
    /// holds nor a key it expects, unconfirmed change included: the
    /// counterparty's. A payee hands them back to the payer, whose change
    /// they are, and a payer that submitted a payment hands them to the
    /// payee. They tell the counterparty nothing about this holder's notes:
    /// they are bound to the counterparty's own keys.
    pub fn theirs(&self, notes: &[Note]) -> Vec<Note> {
        notes
            .iter()
            .filter(|note| {
                let holder = note.holder();
                !self.notes.iter().any(|h| h.note.holder() == holder)
                    && !self.expected.iter().any(|e| e.key.public_key() == *holder)
                    && !self
                        .unconfirmed
                        .iter()
                        .any(|u| u.change.key.public_key() == *holder)
            })
            .copied()
            .collect()
    }

    /// Records the mint's answer to one of this holder's transfers: the
    /// transfer stops being outgoing and its notes for this holder are taken
    /// in. Returns the value taken in.
    ///
    /// When the transfer was outgoing and is part of a payment this holder
    /// made, the answer's notes for the payee are kept for it, among the
    /// payment's [owed](MadePayment::owed) notes: a resend may be what
    /// carried out the payment.
    pub fn settle(&mut self, request: &TransferRequest, answer: &[Note]) -> u128 {
        let before = self.outgoing.len();
        self.outgoing.retain(|o| o.request != *request);
        if self.outgoing.len() < before
            && let Some(made) = self
                .payments
                .iter_mut()
                .find(|p| p.payment.transfers.iter().any(|t| t.request == *request))
        {
            for note in answer {
                let payee = note.issuer() == &made.payment.mint
                    && made
                        .request
                        .outputs
                        .iter()
                        .any(|o| o.holder == *note.holder() && o.amount == note.amount());
                if payee && !made.owed.contains(note) {
                    made.owed.push(*note);
                }
            }
        }
        let taken = self.absorb(answer);
        self.tidy();
        taken
    }

    /// Takes in notes the counterparty handed over from the mint's answers,
    /// as [`absorb`](Self::absorb) does, then settles each outgoing transfer
    /// that pays this holder notes, once it holds every one of them with the
    /// transfer's mint, amount and key. Returns the value taken in.
    ///
    /// A transfer that pays this holder nothing, such as a payment that
    /// needs no change, stays outgoing: only [resending](Self::resend) it
    /// shows it was applied.
    ///
    /// Give it notes from the mint's own answer, or check them first with
    /// [`Note::verify`]: the store does not check signatures, and a forged
    /// note would settle a transfer as surely as a real one.
    ///
    /// A signature shows that the mint issued a note, not which transfer it
    /// answers. A counterparty holding the transfer can pay the same keys
    /// from notes of its own and leave the transfer unsubmitted: it then
    /// stops being outgoing though the mint never applied it, and nothing
    /// resends it before its note dies. A hand-back settles a transfer only
    /// as surely as the counterparty submitted it; to be sure, ask the mint
    /// (`POST /v1/verify`) whether the note the transfer spends is spent
    /// before taking the notes in.
    pub fn take_answers(&mut self, notes: &[Note]) -> u128 {
        let taken = self.absorb(notes);
        let (held, expected) = (&self.notes, &self.expected);
        self.outgoing.retain(|o| {
            let mint = o.spends.note.issuer();
            let mut own = o
                .request
                .outputs
                .iter()
                .filter(|out| {
                    held.iter().any(|h| h.note.holder() == &out.holder)
                        || expected.iter().any(|e| e.key.public_key() == out.holder)
                })
                .peekable();
            let answered = own.peek().is_some()
                && own.all(|out| {
                    held.iter().any(|h| {
                        h.note.issuer() == mint
                            && h.note.amount() == out.amount
                            && h.note.holder() == &out.holder
                    })
                });
            !answered
        });
        self.tidy();
        taken
    }

    /// Drops an outgoing transfer whose note is gone, such as one the mint
    /// refused because its note was already spent or had expired: had the
    /// mint applied this transfer and still kept its answer, it would have
    /// answered with the notes (see [`resend`](Self::resend)). Returns
    /// whether the transfer was outgoing.
    ///
    /// The mint may have applied it all the same, so the keys of its change
    /// stay, as unconfirmed change: change the counterparty hands over later
    /// is still taken in by [`take_answers`](Self::take_answers) or
    /// [`absorb`](Self::absorb), until the layer it would sit in is dropped
    /// ([`prune`](Self::prune)). The transfer leaves its
    /// [payment](MadePayment::payment).
    pub fn forget(&mut self, request: &TransferRequest) -> bool {
        self.end(request).is_some()
    }

    /// Takes back the note of an outgoing transfer, for one the mint never
    /// applied, such as one nobody else holds that will never be submitted.
    /// The keys of its change stay, as unconfirmed change, and the transfer
    /// leaves its payment, as for [`forget`](Self::forget). Returns whether
    /// the transfer was outgoing.
    ///
    /// A refusal does not always show that the mint never applied it: the
    /// mint checks its policy before its record of answers, so a transfer it
    /// applied under a looser policy is refused as `policy` too. Once a payee
    /// holds the transfer, they can still submit it. If the mint applies it,
    /// or already had, the payee's notes and the note taken back claim the
    /// same value, and the mint refuses whichever is spent second.
    pub fn cancel(&mut self, request: &TransferRequest) -> bool {
        let Some(spends) = self.end(request) else {
            return false;
        };
        self.notes.push(spends);
        true
    }

    /// Ends an outgoing transfer and returns the note it spends. The keys of
    /// its change move from the keys expected to the unconfirmed change, and
    /// it leaves the [payment](MadePayment::payment) that holds it, so that
    /// payment is never handed out again with it.
    fn end(&mut self, request: &TransferRequest) -> Option<Held> {
        let i = self.outgoing.iter().position(|o| o.request == *request)?;
        let outgoing = self.outgoing.remove(i);
        // A transfer's notes sit one layer past the note it spends.
        let generation = outgoing.spends.note.serial().generation.saturating_add(1);
        let mut i = 0;
        while i < self.expected.len() {
            let key = self.expected[i].key.public_key();
            if request.outputs.iter().any(|o| o.holder == key) {
                let change = self.expected.swap_remove(i);
                self.unconfirmed.push(Unconfirmed { change, generation });
            } else {
                i += 1;
            }
        }
        for made in &mut self.payments {
            made.payment.transfers.retain(|t| t.request != *request);
        }
        self.tidy();
        Some(outgoing.spends)
    }

    /// The payments this holder made and keeps: each while one of its
    /// transfers is [outgoing](Self::outgoing), or while it keeps notes
    /// [owed](MadePayment::owed) to the payee.
    pub fn payments(&self) -> impl Iterator<Item = &MadePayment> {
        self.payments.iter()
    }

    /// Stops keeping `notes` for payees, once they are handed over: they
    /// leave the [owed](MadePayment::owed) notes of the payments that keep
    /// them, and a payment with nothing else kept leaves the
    /// [payments](Self::payments). Returns the value it stopped keeping.
    ///
    /// The payee checks their signatures and takes them in, as notes from
    /// the mint's answers: with [`take_answers`](Self::take_answers), or
    /// [`absorb`](Self::absorb).
    pub fn forget_owed(&mut self, notes: &[Note]) -> u128 {
        let mut value = 0;
        for made in &mut self.payments {
            made.owed.retain(|note| {
                let handed = notes.contains(note);
                if handed {
                    value += u128::from(note.amount());
                }
                !handed
            });
        }
        self.tidy();
        value
    }

    /// Drops the payments with no transfer outgoing and no notes owed.
    fn tidy(&mut self) {
        let outgoing = &self.outgoing;
        self.payments.retain(|p| {
            !p.owed.is_empty()
                || p.payment
                    .transfers
                    .iter()
                    .any(|t| outgoing.iter().any(|o| o.request == t.request))
        });
    }

    /// Transfers not yet answered. Resend them after a crash or a timeout,
    /// with [`resend`](Self::resend): the mint answers an identical request
    /// with the identical notes, before or after a rotation, for as long as
    /// it remembers the transfer.
    pub fn outgoing(&self) -> impl Iterator<Item = &TransferRequest> {
        self.outgoing.iter().map(|o| &o.request)
    }

    /// What this holder may lose on `request` if the mint applied it and its
    /// answer is lost: the value it pays to keys this holder still expects,
    /// unconfirmed change included. `Some` when the transfer is outgoing or
    /// pays such a key, even if the value is 0; `None` when nothing of this
    /// holder's rides on it.
    ///
    /// A transfer refused as spent or expired may have been applied after
    /// all, by the counterparty or by an earlier try whose answer was lost.
    /// A counterparty that submitted it holds the answer and can hand the
    /// notes over, for [`take_answers`](Self::take_answers).
    pub fn at_stake(&self, request: &TransferRequest) -> Option<u128> {
        let mint = request.inputs.first().map(|spend| spend.note.issuer());
        let expected: u128 = request
            .outputs
            .iter()
            .filter(|o| {
                let pays = |e: &Expected| {
                    Some(&e.mint) == mint && e.amount == o.amount && e.key.public_key() == o.holder
                };
                self.expected.iter().any(pays) || self.unconfirmed.iter().any(|u| pays(&u.change))
            })
            .map(|o| u128::from(o.amount))
            .sum();
        let outgoing = self.outgoing.iter().any(|o| o.request == *request);
        (outgoing || expected > 0).then_some(expected)
    }

    /// The [outgoing](Self::outgoing) transfers in random order, each with a
    /// time to resend it, spaced by `spacing`. Resent together, they would
    /// tell the mint their notes had one owner.
    ///
    /// The mint answers a resent transfer with the same notes only while it
    /// keeps its answer, and at the latest until the notes it issued expire
    /// (see [Error codes]). After that, a transfer already applied is
    /// refused as spent or expired, and its change can only come from the
    /// counterparty, for [`take_answers`](Self::take_answers).
    /// [`due_resends`](Self::due_resends) gives the transfers to resend
    /// before a rotation ends their chance.
    ///
    /// [Error codes]: https://github.com/zhangheqi/worgl/blob/main/site/src/content/docs/docs/error-codes.md#sending-a-request-again
    pub fn resend(&self, spacing: Spacing) -> Vec<ScheduledTransfer> {
        let mut requests: Vec<TransferRequest> = self.outgoing().cloned().collect();
        random::shuffle(&mut requests);
        let mut after_secs = 0u64;
        requests
            .into_iter()
            .enumerate()
            .map(|(n, request)| {
                if n > 0 {
                    after_secs = after_secs.saturating_add(spacing.gap());
                }
                ScheduledTransfer {
                    after_secs,
                    request,
                }
            })
            .collect()
    }

    /// The [outgoing](Self::outgoing) transfers at `mint` to resend before
    /// its next rotation, each at its own random moment before it, as
    /// [`refresh`](Self::refresh) spreads its transfers from the same `now`.
    /// Nothing in the store changes.
    ///
    /// A transfer is due when the note it spends dies at that rotation:
    /// after it, a transfer the mint never applied can no longer be
    /// applied. It is still due when that note was dropped at the last
    /// rotation, since the mint may keep an applied transfer's answer until
    /// the notes it issued die, at the next. Call it before
    /// [`refresh`](Self::refresh), whose transfers it would list too, submit
    /// them with the refreshes and [`settle`](Self::settle) the answers.
    /// Resending a payment the payee has not submitted yet carries it out;
    /// the payee still gets the same notes by submitting it while the mint
    /// keeps its answer, and settling the answer keeps them for the payee
    /// too (see [`MadePayment::owed`]).
    pub fn due_resends(&self, mint: &MintView, now: u64) -> Vec<ScheduledTransfer> {
        let window = refresh_window(mint, now);
        let mut transfers: Vec<ScheduledTransfer> = self
            .outgoing
            .iter()
            .filter(|o| {
                let note = &o.spends.note;
                let dropped_last = note.issuer() == &mint.issuer
                    && note.serial().generation.checked_add(1) == Some(mint.oldest);
                dropped_last
                    || matches!(
                        mint.state(note),
                        NoteState::Spendable {
                            expires_after_rotations: 1
                        } | NoteState::Frozen {
                            expires_after_rotations: 1
                        }
                    )
            })
            .map(|o| ScheduledTransfer {
                after_secs: random::between(0, window),
                request: o.request.clone(),
            })
            .collect();
        transfers.sort_by_key(|t| t.after_secs);
        transfers
    }

    /// Pays each of `mint`'s notes that dies at the next rotation to a fresh
    /// key of this holder, each at its own random moment before the
    /// rotation, so the mint cannot group one holder's notes by when they
    /// moved. The refreshed note lands one layer later and lives as long as
    /// a received note.
    ///
    /// Call it once per rotation period, after reading the mint's info.
    /// Submit each transfer at its time and [`settle`](Self::settle) the
    /// answer.
    ///
    /// `mint` alone says which notes are due: those in its first layer.
    /// `now`, the Unix time by the mint's clock, says only when: the
    /// transfers are spread over the time left before the next rotation,
    /// less [`REFRESH_MARGIN_SECS`], and are all due at once when `now` is at
    /// or past it, since the mint has not made it yet as far as `mint` knows.
    /// A clock behind the mint's by more than that margin can spread them
    /// past the rotation, where they fail, so take `now` from the mint, such
    /// as from the `Date` of its answer.
    pub fn refresh(&mut self, mint: &MintView, now: u64) -> Vec<ScheduledTransfer> {
        let window = refresh_window(mint, now);
        let mut transfers = Vec::new();
        let mut i = 0;
        while i < self.notes.len() {
            let due = matches!(
                mint.state(&self.notes[i].note),
                NoteState::Spendable {
                    expires_after_rotations: 1
                }
            );
            if !due {
                i += 1;
                continue;
            }
            let held = self.notes.remove(i);
            let key = HolderKey::generate();
            let output = Output {
                amount: held.note.amount(),
                holder: key.public_key(),
            };
            let request = TransferRequest::single(held.note, &held.key, output)
                .expect("a held note is signed with its own key");
            self.expected.push(Expected {
                mint: mint.issuer,
                amount: output.amount,
                key,
            });
            self.outgoing.push(Outgoing {
                request: request.clone(),
                spends: held,
            });
            transfers.push(ScheduledTransfer {
                after_secs: random::between(0, window),
                request,
            });
        }
        transfers.sort_by_key(|t| t.after_secs);
        transfers
    }

    /// Removes `mint`'s notes whose layer was dropped and returns the value
    /// lost. It also stops waiting for unconfirmed change of `mint` that
    /// would sit in a dropped layer, and stops keeping notes owed to payees
    /// once their layer is dropped; neither counts as lost.
    ///
    /// It goes by `mint` alone, with no clock: with a view of the mint's
    /// info, it removes a note, and its key for good, only once the mint has
    /// dropped the note's layer.
    pub fn prune(&mut self, mint: &MintView) -> u128 {
        let mut lost = 0;
        self.notes.retain(|h| {
            let expired = mint.state(&h.note) == NoteState::Expired;
            if expired {
                lost += u128::from(h.note.amount());
            }
            !expired
        });
        self.unconfirmed
            .retain(|u| u.change.mint != mint.issuer || u.generation >= mint.oldest);
        for made in &mut self.payments {
            made.owed
                .retain(|note| mint.state(note) != NoteState::Expired);
        }
        self.tidy();
        lost
    }

    /// Value held at `mint`, by what it can do now.
    pub fn balance(&self, mint: &MintView) -> Balance {
        let mut balance = Balance::default();
        for held in &self.notes {
            let value = u128::from(held.note.amount());
            match mint.state(&held.note) {
                NoteState::Spendable {
                    expires_after_rotations,
                } => {
                    balance.spendable += value;
                    if expires_after_rotations == 1 {
                        balance.expiring += value;
                    }
                }
                NoteState::Frozen {
                    expires_after_rotations,
                } => {
                    balance.frozen += value;
                    if expires_after_rotations == 1 {
                        balance.expiring += value;
                    }
                }
                NoteState::Unknown => balance.unknown += value,
                NoteState::Expired | NoteState::OtherMint => {}
            }
        }
        for outgoing in &self.outgoing {
            if outgoing.spends.note.issuer() == &mint.issuer {
                balance.outgoing += u128::from(outgoing.spends.note.amount());
            }
        }
        for expected in &self.expected {
            if expected.mint == mint.issuer {
                balance.expected += u128::from(expected.amount);
            }
        }
        for unconfirmed in &self.unconfirmed {
            if unconfirmed.change.mint == mint.issuer {
                balance.unconfirmed += u128::from(unconfirmed.change.amount);
            }
        }
        balance
    }

    /// The notes held, of every mint.
    pub fn notes(&self) -> impl Iterator<Item = &Note> {
        self.notes.iter().map(|h| &h.note)
    }

    /// The whole store as JSON, secret keys included.
    ///
    /// A store with unconfirmed change or notes owed to payees is written in
    /// format 2, which an SDK that reads only format 1 refuses rather than
    /// open without them; any other store is written in format 1.
    pub fn to_json(&self) -> String {
        let plain = self.unconfirmed.is_empty() && self.payments.iter().all(|p| p.owed.is_empty());
        serde_json::to_string(&Saved {
            format: if plain { 1 } else { FORMAT },
            notes: self.notes.clone(),
            expected: self.expected.clone(),
            outgoing: self.outgoing.clone(),
            unconfirmed: self.unconfirmed.clone(),
            payments: self.payments.clone(),
            paid: self.paid.clone(),
        })
        .expect("a store always serializes")
    }

    /// Reads a store written by [`to_json`](Self::to_json), by this SDK or
    /// an earlier one.
    pub fn from_json(json: &str) -> Result<Self, LoadError> {
        let saved: Saved = serde_json::from_str(json)?;
        if !(1..=FORMAT).contains(&saved.format) {
            return Err(LoadError::Format(saved.format));
        }
        Ok(Self {
            notes: saved.notes,
            expected: saved.expected,
            outgoing: saved.outgoing,
            unconfirmed: saved.unconfirmed,
            payments: saved.payments,
            paid: saved.paid,
        })
    }
}

/// Seconds from `now` over which to spread transfers that must land before
/// `mint`'s next rotation: none once that rotation is due.
fn refresh_window(mint: &MintView, now: u64) -> u64 {
    match mint.until_rotation(now) {
        Some(left) => left.saturating_sub(REFRESH_MARGIN_SECS.min(left / 2)),
        None => UNSCHEDULED_REFRESH_SECS,
    }
}

/// One transfer of a payment: the note it spends, the requested outputs it
/// pays and the change it returns.
struct Part {
    note: usize,
    paid: Vec<Output>,
    change: Vec<u64>,
}

enum Split {
    Inexact,
    TooMany,
}

/// Splits `amount` into allowed denominations no larger than `largest`,
/// largest first, as at most `limit` notes.
fn split(
    denominations: &Denominations,
    amount: u64,
    largest: Option<u64>,
    limit: usize,
) -> Result<Vec<u64>, Split> {
    if amount == 0 {
        return Ok(Vec::new());
    }
    let pieces = match (denominations, largest) {
        (_, Some(0)) => return Err(Split::Inexact),
        (denominations, None) => denominations.decompose(amount),
        (Denominations::Any, Some(cap)) => {
            let mut pieces = vec![(cap, amount / cap)];
            if !amount.is_multiple_of(cap) {
                pieces.push((amount % cap, 1));
            }
            Some(pieces)
        }
        (Denominations::PowersOfTwo, Some(cap)) => {
            let top = 1u64 << (63 - cap.leading_zeros());
            let mut pieces = vec![(top, amount / top)];
            pieces.extend(
                Denominations::PowersOfTwo
                    .decompose(amount % top)
                    .unwrap_or_default(),
            );
            Some(pieces)
        }
        (Denominations::List(values), Some(cap)) => {
            let allowed = values.iter().copied().filter(|&v| v <= cap);
            Denominations::list(allowed)
                .ok()
                .and_then(|d| d.decompose(amount))
        }
    };
    let pieces = pieces.ok_or(Split::Inexact)?;
    let count: u128 = pieces.iter().map(|&(_, n)| u128::from(n)).sum();
    if count > limit as u128 {
        return Err(Split::TooMany);
    }
    Ok(pieces
        .into_iter()
        .filter(|&(_, n)| n > 0)
        .flat_map(|(value, n)| std::iter::repeat_n(value, n as usize))
        .collect())
}

/// Holder keys saved as their 32-byte secrets in hex.
mod secret {
    use serde::{Deserialize, Deserializer, Serializer, de};
    use worgl::HolderKey;

    pub(super) fn serialize<S: Serializer>(key: &HolderKey, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(key.to_secret_bytes()))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<HolderKey, D::Error> {
        let text = String::deserialize(d)?;
        let mut secret = [0u8; 32];
        hex::decode_to_slice(text, &mut secret).map_err(de::Error::custom)?;
        Ok(HolderKey::from_secret_bytes(&secret))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_follow_the_denominations_and_the_cap() {
        let p2 = Denominations::PowersOfTwo;
        assert_eq!(split(&p2, 13, None, 16).ok(), Some(vec![8, 4, 1]));
        assert_eq!(split(&p2, 13, Some(4), 16).ok(), Some(vec![4, 4, 4, 1]));
        assert_eq!(split(&p2, 13, Some(5), 16).ok(), Some(vec![4, 4, 4, 1]));
        assert!(matches!(
            split(&p2, 1 << 20, Some(1), 256),
            Err(Split::TooMany)
        ));
        assert_eq!(split(&p2, 0, None, 0).ok(), Some(vec![]));

        let any = Denominations::Any;
        assert_eq!(split(&any, 7, None, 16).ok(), Some(vec![7]));
        assert_eq!(split(&any, 7, Some(3), 16).ok(), Some(vec![3, 3, 1]));

        let list = Denominations::list([1, 2, 5]).unwrap();
        assert_eq!(split(&list, 9, None, 16).ok(), Some(vec![5, 2, 2]));
        assert_eq!(split(&list, 9, Some(2), 16).ok(), Some(vec![2, 2, 2, 2, 1]));
        let fives = Denominations::list([5]).unwrap();
        assert!(matches!(split(&fives, 7, None, 16), Err(Split::Inexact)));
        assert!(matches!(split(&fives, 5, Some(4), 16), Err(Split::Inexact)));
    }

    #[test]
    fn the_keys_paid_last_are_kept_up_to_the_bound_and_saved() {
        let mut holder = Holder::new();
        let keys: Vec<PublicKey> = (0..=PAID_KEYS_KEPT)
            .map(|_| HolderKey::generate().public_key())
            .collect();
        holder.remember_paid(keys.iter().copied());
        assert_eq!(holder.paid.len(), PAID_KEYS_KEPT);
        let holder = Holder::from_json(&holder.to_json()).unwrap();
        assert!(!holder.has_paid(&keys[0]), "the oldest is forgotten");
        assert!(keys[1..].iter().all(|key| holder.has_paid(key)));
    }
}
