//! A holder's notes, the keys it has handed out, and the transfers it has
//! signed but not seen land.

use std::cmp::Reverse;

use serde::{Deserialize, Serialize};
use worgl::{Denominations, HolderKey, Note, Output, PublicKey, TransferRequest};

use crate::mint::{MintView, NoteState};
use crate::payment::{Payment, PaymentRequest, ScheduledTransfer, Spacing};
use crate::random;

/// Most notes one payment request may ask for.
pub const MAX_REQUESTED_NOTES: usize = 256;

/// Version of the format [`Holder::to_json`] writes.
const FORMAT: u32 = 1;

/// Seconds before a scheduled rotation by which refreshes should have been
/// submitted, at most.
const REFRESH_MARGIN_SECS: u64 = 300;

/// Seconds over which refreshes are spread when the mint has no schedule.
const UNSCHEDULED_REFRESH_SECS: u64 = 60;

/// The notes one holder owns, with the one-time key of each, plus the keys
/// it has handed out and the transfers it has signed but not yet seen land.
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
    /// sees them answered.
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
        let plan = self.plan(mint, request)?;
        let mut transfers = Vec::with_capacity(plan.len());
        let mut expected = Vec::new();
        let mut after_secs = 0;
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
                after_secs += random::between(spacing.min_secs, spacing.max_secs);
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
        Ok(Payment {
            mint: mint.issuer,
            transfers,
        })
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
    /// holder's keys: from a payment it received, its own change, or a
    /// refresh. Notes for other keys are ignored, and a note already held is
    /// not taken twice. Returns the value taken in.
    ///
    /// Give it notes from the mint's own answer, or check them first with
    /// [`Note::verify`]: the store does not check signatures.
    pub fn absorb(&mut self, notes: &[Note]) -> u128 {
        let mut taken = 0;
        for note in notes {
            if self.notes.iter().any(|h| h.note == *note) {
                continue;
            }
            let found = self.expected.iter().position(|e| {
                e.mint == *note.issuer()
                    && e.amount == note.amount()
                    && e.key.public_key() == *note.holder()
            });
            if let Some(i) = found {
                let expected = self.expected.swap_remove(i);
                self.notes.push(Held {
                    note: *note,
                    key: expected.key,
                });
                taken += u128::from(note.amount());
            }
        }
        taken
    }

    /// Records the mint's answer to one of this holder's transfers: the
    /// transfer stops being outgoing and its notes for this holder are taken
    /// in. Returns the value taken in.
    pub fn settle(&mut self, request: &TransferRequest, answer: &[Note]) -> u128 {
        self.outgoing.retain(|o| o.request != *request);
        self.absorb(answer)
    }

    /// Drops an outgoing transfer the mint refused because its note was
    /// already spent or had expired. The note is gone either way.
    pub fn forget(&mut self, request: &TransferRequest) {
        self.outgoing.retain(|o| o.request != *request);
    }

    /// Takes back the note of an outgoing transfer that will never be
    /// submitted, and stops expecting its change.
    ///
    /// Only for a transfer nobody else holds: once a payee has it, they can
    /// submit it, and spending the note elsewhere makes the payment fail.
    pub fn cancel(&mut self, request: &TransferRequest) {
        let Some(i) = self.outgoing.iter().position(|o| o.request == *request) else {
            return;
        };
        let outgoing = self.outgoing.remove(i);
        self.expected.retain(|e| {
            !request
                .outputs
                .iter()
                .any(|o| o.holder == e.key.public_key())
        });
        self.notes.push(outgoing.spends);
    }

    /// Transfers not yet answered. Resend them after a crash or a timeout:
    /// the mint answers an identical request with the identical notes.
    pub fn outgoing(&self) -> impl Iterator<Item = &TransferRequest> {
        self.outgoing.iter().map(|o| &o.request)
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
    pub fn refresh(&mut self, mint: &MintView, now: u64) -> Vec<ScheduledTransfer> {
        let window = match mint.until_rotation(now) {
            Some(left) => left.saturating_sub(REFRESH_MARGIN_SECS.min(left / 2)),
            None => UNSCHEDULED_REFRESH_SECS,
        };
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
    /// lost.
    pub fn prune(&mut self, mint: &MintView) -> u128 {
        let mut lost = 0;
        self.notes.retain(|h| {
            let expired = mint.state(&h.note) == NoteState::Expired;
            if expired {
                lost += u128::from(h.note.amount());
            }
            !expired
        });
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
        balance
    }

    /// The notes held, of every mint.
    pub fn notes(&self) -> impl Iterator<Item = &Note> {
        self.notes.iter().map(|h| &h.note)
    }

    /// The whole store as JSON, secret keys included.
    pub fn to_json(&self) -> String {
        serde_json::to_string(&Saved {
            format: FORMAT,
            notes: self.notes.clone(),
            expected: self.expected.clone(),
            outgoing: self.outgoing.clone(),
        })
        .expect("a store always serializes")
    }

    /// Reads a store written by [`to_json`](Self::to_json).
    pub fn from_json(json: &str) -> Result<Self, LoadError> {
        let saved: Saved = serde_json::from_str(json)?;
        if saved.format != FORMAT {
            return Err(LoadError::Format(saved.format));
        }
        Ok(Self {
            notes: saved.notes,
            expected: saved.expected,
            outgoing: saved.outgoing,
        })
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
}
