//! The issuer: its key, its ledger, its policy and the rules that tie them.
//!
//! Every state change is an [`Event`]. The issuer *plans* an event from a
//! request without changing anything, the caller makes the event durable if
//! it wants to (see [`store`](crate::store)), and then the issuer *applies*
//! it. Applying is deterministic and checks the event against the state, so
//! replaying a journal of events onto the snapshot it started from rebuilds
//! the exact same issuer. [`Issuer::mint`], [`Issuer::transfer`] and
//! [`Issuer::rotate`] do all three steps for in-memory use.
//!
//! Besides the ledger's bits, the issuer keeps a few counters that cost
//! nothing per note: the live value in each layer, the value ever minted and
//! the value destroyed by rotations. They satisfy
//! `outstanding = minted − destroyed` at all times.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::codec::{DecodeError, Reader, Writer};
use crate::error::{ApplyError, Error, SetupError};
use crate::keys::{IssuerKey, PublicKey};
use crate::ledger::{Ledger, LedgerConfig, Serial, Status};
use crate::note::{Note, NoteError, read_key};
use crate::permutation::Permutation;
use crate::policy::Policy;
use crate::transfer::{TransferId, TransferRequest, VerifiedTransfer, Verifier};

const SNAPSHOT_MAGIC: &[u8; 8] = b"WORGLSNP";
/// Version 2 records whether serial indexes are permuted; version 1
/// snapshots still restore, with the bits as indexes as they always had.
const SNAPSHOT_VERSION: u16 = 2;

/// A note created by a mint or transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Issued {
    /// The new note's serial.
    pub serial: Serial,
    /// Its amount.
    pub amount: u64,
    /// The key it is bound to.
    pub holder: PublicKey,
}

/// A note consumed by a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Spent {
    /// The spent note's serial.
    pub serial: Serial,
    /// Its amount.
    pub amount: u64,
}

/// What a rotation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationReport {
    /// Generation of the dropped first layer.
    pub dropped: u64,
    /// Generation of the new last layer.
    pub created: u64,
    /// Live notes the dropped layer held, now expired.
    pub destroyed_notes: u32,
    /// Their total value.
    pub destroyed_value: u128,
}

/// One state change of the issuer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// New money was minted into the mint layer.
    Mint(Issued),
    /// Notes were spent and new notes issued one layer past the furthest input.
    Transfer {
        /// Identity of the transfer request.
        id: TransferId,
        /// The consumed notes.
        spent: Vec<Spent>,
        /// The created notes.
        issued: Vec<Issued>,
    },
    /// The first layer was dropped and a new last layer created.
    Rotate(RotationReport),
}

impl Event {
    /// The event's binary encoding, as used by the journal in [`store`](crate::store).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::default();
        match self {
            Event::Mint(issued) => {
                w.u8(1);
                write_issued(&mut w, issued);
            }
            Event::Transfer { id, spent, issued } => {
                w.u8(2).bytes(&id.to_bytes()).u32(spent.len() as u32);
                for s in spent {
                    w.u64(s.serial.generation).u32(s.serial.index).u64(s.amount);
                }
                w.u32(issued.len() as u32);
                for i in issued {
                    write_issued(&mut w, i);
                }
            }
            Event::Rotate(r) => {
                w.u8(3)
                    .u64(r.dropped)
                    .u64(r.created)
                    .u32(r.destroyed_notes)
                    .u128(r.destroyed_value);
            }
        }
        w.finish()
    }

    /// Decodes an event written by [`to_bytes`](Self::to_bytes).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        let event = match r.u8()? {
            1 => Event::Mint(read_issued(&mut r)?),
            2 => {
                let id = TransferId::from_bytes(r.array()?);
                let spent = read_list(&mut r, 20, |r| {
                    Ok(Spent {
                        serial: Serial::new(r.u64()?, r.u32()?),
                        amount: r.u64()?,
                    })
                })?;
                let issued = read_list(&mut r, 52, read_issued)?;
                Event::Transfer { id, spent, issued }
            }
            3 => Event::Rotate(RotationReport {
                dropped: r.u64()?,
                created: r.u64()?,
                destroyed_notes: r.u32()?,
                destroyed_value: r.u128()?,
            }),
            _ => return Err(DecodeError::Invalid("event type")),
        };
        r.finish()?;
        Ok(event)
    }
}

fn write_issued(w: &mut Writer, issued: &Issued) {
    w.u64(issued.serial.generation)
        .u32(issued.serial.index)
        .u64(issued.amount)
        .bytes(issued.holder.as_bytes());
}

fn read_issued(r: &mut Reader<'_>) -> Result<Issued, DecodeError> {
    Ok(Issued {
        serial: Serial::new(r.u64()?, r.u32()?),
        amount: r.u64()?,
        holder: read_key(r)?,
    })
}

fn read_list<T>(
    r: &mut Reader<'_>,
    item_len: usize,
    mut item: impl FnMut(&mut Reader<'_>) -> Result<T, DecodeError>,
) -> Result<Vec<T>, DecodeError> {
    let count = r.u32()? as usize;
    // Refuse counts the remaining input cannot hold before allocating for them.
    if count > r.remaining() / item_len {
        return Err(DecodeError::Truncated);
    }
    (0..count).map(|_| item(r)).collect()
}

/// The outcome of [`Issuer::plan_transfer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// A fresh transfer: make the event durable if needed, then
    /// [`apply`](Issuer::apply) it and hand out [`notes`](Issuer::notes).
    Apply(Event),
    /// The identical request was already applied; these are the notes it
    /// produced, signed again. Nothing needs to be stored or applied.
    Replay(Vec<Note>),
}

/// The issuer's view of a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NoteStatus {
    /// The note can be spent or held.
    Live {
        /// Position of its layer, 0 for the first.
        position: u32,
        /// Transfers its lineage can still make before the next rotation.
        /// 0 means it sits in the last layer and is frozen until then.
        hops_left: u32,
        /// Untouched, the note expires in this many rotations from now. It
        /// survives one fewer: a note in the first layer reports 1 and dies
        /// in the next rotation.
        expires_after_rotations: u32,
    },
    /// The note was spent.
    Spent,
    /// The note's layer was dropped.
    Expired,
    /// The issuer signed the note but its ledger has no record of it.
    Unissued,
}

/// Value accounting across the whole ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Supply {
    /// Value ever minted.
    pub minted: u128,
    /// Value destroyed by rotations.
    pub destroyed: u128,
    /// Value held in live notes: `minted − destroyed`.
    pub outstanding: u128,
}

/// Usage and value of one layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerSummary {
    /// Position, 0 for the first layer.
    pub position: u32,
    /// The layer's generation.
    pub generation: u64,
    /// Bits handed out so far in this generation.
    pub issued: u32,
    /// Notes still live.
    pub live: u32,
    /// Bits the layer has in total.
    pub capacity: u32,
    /// Total value of the live notes.
    pub value: u128,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Receipts {
    order: VecDeque<TransferId>,
    issued: HashMap<TransferId, Vec<Issued>>,
}

impl Receipts {
    fn insert(&mut self, capacity: usize, id: TransferId, issued: Vec<Issued>) {
        if capacity == 0 {
            return;
        }
        if self.issued.insert(id, issued).is_none() {
            self.order.push_back(id);
        }
        self.shrink_to(capacity);
    }

    fn shrink_to(&mut self, capacity: usize) {
        while self.order.len() > capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.issued.remove(&oldest);
            }
        }
    }

    /// Forgets transfers whose notes have expired.
    fn prune(&mut self, oldest_generation: u64) {
        let issued = &mut self.issued;
        self.order.retain(|id| {
            let alive = issued[id]
                .first()
                .is_some_and(|i| i.serial.generation >= oldest_generation);
            if !alive {
                issued.remove(id);
            }
            alive
        });
    }
}

/// The note issuer. See the [module documentation](self).
#[derive(Debug, Clone)]
pub struct Issuer {
    key: IssuerKey,
    public: PublicKey,
    policy: Policy,
    ledger: Ledger,
    sequence: u64,
    /// Live value per layer, indexed by position.
    value: Vec<u128>,
    minted: u128,
    destroyed: u128,
    receipts: Receipts,
}

impl Issuer {
    /// Creates an issuer with an empty ledger. Serial indexes are a keyed
    /// permutation of the ledger's bits, derived from `key`, so they do not
    /// reveal the order notes were issued in.
    pub fn new(key: IssuerKey, ledger: LedgerConfig, policy: Policy) -> Result<Self, SetupError> {
        let ledger =
            Ledger::new(ledger)?.with_order(Permutation::for_issuer(&key, ledger.layer_size));
        policy.validate(ledger.config())?;
        Ok(Self {
            public: key.public_key(),
            key,
            policy,
            value: vec![0; ledger.config().layers as usize],
            ledger,
            sequence: 0,
            minted: 0,
            destroyed: 0,
            receipts: Receipts::default(),
        })
    }

    /// The issuer's public key, which every note names.
    pub fn public_key(&self) -> &PublicKey {
        &self.public
    }

    /// The policy in force.
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// The ledger, for read-only inspection.
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Number of events applied since the issuer was created.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Position of the layer new money lands in.
    pub fn mint_position(&self) -> u32 {
        self.policy.mint_position(self.ledger.config())
    }

    /// Value accounting.
    pub fn supply(&self) -> Supply {
        Supply {
            minted: self.minted,
            destroyed: self.destroyed,
            outstanding: self.value.iter().sum(),
        }
    }

    /// Usage and value of every layer, first to last.
    pub fn layers(&self) -> Vec<LayerSummary> {
        self.ledger
            .layers()
            .map(|l| LayerSummary {
                position: l.position,
                generation: l.generation,
                issued: l.issued,
                live: l.live,
                capacity: l.capacity,
                value: self.value[l.position as usize],
            })
            .collect()
    }

    /// A verifier for transfer requests, bound to this issuer's key and policy.
    pub fn verifier(&self) -> Verifier {
        Verifier::new(self.public, self.policy.clone())
    }

    /// Checks a note's signature and looks up its state. O(1) after the signature check.
    pub fn status(&self, note: &Note) -> Result<NoteStatus, NoteError> {
        note.verify(&self.public)?;
        Ok(self.serial_status(note.serial()))
    }

    /// Looks up the state of a serial without checking any signature. O(1).
    pub fn serial_status(&self, serial: Serial) -> NoteStatus {
        let last = self.ledger.last_position();
        match self.ledger.status(serial) {
            Status::Live { position } => NoteStatus::Live {
                position,
                hops_left: last - position,
                expires_after_rotations: position + 1,
            },
            Status::Spent => NoteStatus::Spent,
            Status::Expired => NoteStatus::Expired,
            Status::Unissued => NoteStatus::Unissued,
        }
    }

    /// Plans minting a note of `amount` bound to `holder`.
    pub fn plan_mint(&self, amount: u64, holder: PublicKey) -> Result<Event, Error> {
        self.policy.check_mint(amount)?;
        let position = self.mint_position();
        let serial = self
            .next_serial(position, 0)
            .ok_or(Error::LayerFull { position })?;
        Ok(Event::Mint(Issued {
            serial,
            amount,
            holder,
        }))
    }

    /// Plans a verified transfer against the ledger: every input must be
    /// live and outside the last layer, and the layer one past the furthest
    /// input must have room for every output.
    ///
    /// The transfer's shape is checked against this issuer's policy again,
    /// which is cheap, in case it was verified under a different one.
    pub fn plan_transfer(&self, transfer: &VerifiedTransfer) -> Result<Plan, Error> {
        if transfer.issuer != self.public {
            return Err(Error::Note {
                input: 0,
                error: NoteError::WrongIssuer,
            });
        }
        let inputs: Vec<u64> = transfer.inputs.iter().map(Note::amount).collect();
        let outputs: Vec<u64> = transfer.outputs.iter().map(|o| o.amount).collect();
        self.policy.check_transfer(&inputs, &outputs)?;
        if let Some(issued) = self.receipts.issued.get(&transfer.id) {
            return Ok(Plan::Replay(self.sign_all(issued)));
        }
        let last = self.ledger.last_position();
        let mut furthest = 0;
        for note in &transfer.inputs {
            let serial = note.serial();
            match self.ledger.status(serial) {
                Status::Live { position } if position == last => return Err(Error::Frozen(serial)),
                Status::Live { position } => furthest = furthest.max(position),
                Status::Spent => return Err(Error::Spent(serial)),
                Status::Expired => return Err(Error::Expired(serial)),
                Status::Unissued => return Err(Error::Unissued(serial)),
            }
        }
        let position = furthest + 1;
        let issued = (0..transfer.outputs.len() as u32)
            .zip(&transfer.outputs)
            .map(|(k, output)| {
                Some(Issued {
                    serial: self.next_serial(position, k)?,
                    amount: output.amount,
                    holder: output.holder,
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(Error::LayerFull { position })?;
        let spent = transfer
            .inputs
            .iter()
            .map(|note| Spent {
                serial: note.serial(),
                amount: note.amount(),
            })
            .collect();
        Ok(Plan::Apply(Event::Transfer {
            id: transfer.id,
            spent,
            issued,
        }))
    }

    /// Plans a rotation.
    pub fn plan_rotate(&self) -> Event {
        let first = self
            .ledger
            .layer(0)
            .expect("a ledger has at least two layers");
        Event::Rotate(RotationReport {
            dropped: first.generation,
            created: self.ledger.newest_generation() + 1,
            destroyed_notes: first.live,
            destroyed_value: self.value[0],
        })
    }

    /// Applies an event, or changes nothing and explains why it does not fit.
    pub fn apply(&mut self, event: &Event) -> Result<(), ApplyError> {
        match event {
            Event::Mint(issued) => self.apply_mint(issued)?,
            Event::Transfer { id, spent, issued } => self.apply_transfer(*id, spent, issued)?,
            Event::Rotate(report) => self.apply_rotate(report)?,
        }
        self.sequence += 1;
        Ok(())
    }

    /// The notes an event created, signed. Empty for a rotation.
    ///
    /// Ed25519 signatures are deterministic, so signing the same event again
    /// yields byte-identical notes.
    pub fn notes(&self, event: &Event) -> Vec<Note> {
        match event {
            Event::Mint(issued) => self.sign_all(std::slice::from_ref(issued)),
            Event::Transfer { issued, .. } => self.sign_all(issued),
            Event::Rotate(_) => Vec::new(),
        }
    }

    /// Mints a note in memory: plan, apply and sign in one step.
    pub fn mint(&mut self, amount: u64, holder: PublicKey) -> Result<Note, Error> {
        let event = self.plan_mint(amount, holder)?;
        self.apply(&event).expect("a freshly planned event applies");
        Ok(self.notes(&event).remove(0))
    }

    /// Verifies and carries out a transfer in memory.
    pub fn transfer(&mut self, request: &TransferRequest) -> Result<Vec<Note>, Error> {
        let verified = self.verifier().verify(request)?;
        match self.plan_transfer(&verified)? {
            Plan::Replay(notes) => Ok(notes),
            Plan::Apply(event) => {
                self.apply(&event).expect("a freshly planned event applies");
                Ok(self.notes(&event))
            }
        }
    }

    /// Rotates in memory.
    pub fn rotate(&mut self) -> RotationReport {
        let event = self.plan_rotate();
        self.apply(&event).expect("a freshly planned event applies");
        match event {
            Event::Rotate(report) => report,
            _ => unreachable!("plan_rotate plans a rotation"),
        }
    }

    /// Encodes the issuer's whole state except its secret key and policy.
    ///
    /// Layout, big-endian: the magic `WORGLSNP`, a `u16` format version, the
    /// issuer public key, the event sequence number, the ledger (see
    /// [`Ledger::to_bytes`]), a byte that is 1 if serial indexes are permuted
    /// and 0 if they are the bits, the live value of each layer as `u128`, value
    /// minted and destroyed as `u128`, the remembered transfer receipts, and
    /// finally the SHA-256 of everything before it.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.bytes(SNAPSHOT_MAGIC)
            .u16(SNAPSHOT_VERSION)
            .bytes(self.public.as_bytes())
            .u64(self.sequence);
        self.ledger.encode(&mut w);
        w.u8(u8::from(self.ledger.is_permuted()));
        for value in &self.value {
            w.u128(*value);
        }
        w.u128(self.minted).u128(self.destroyed);
        w.u32(self.receipts.order.len() as u32);
        for id in &self.receipts.order {
            let issued = &self.receipts.issued[id];
            w.bytes(&id.to_bytes()).u32(issued.len() as u32);
            for i in issued {
                write_issued(&mut w, i);
            }
        }
        let mut bytes = w.finish();
        let checksum: [u8; 32] = Sha256::digest(&bytes).into();
        bytes.extend_from_slice(&checksum);
        bytes
    }

    /// Rebuilds an issuer from a [`snapshot`](Self::snapshot), its key and the
    /// policy to enforce from now on.
    pub fn restore(key: IssuerKey, snapshot: &[u8], policy: Policy) -> Result<Self, SetupError> {
        let split = snapshot
            .len()
            .checked_sub(32)
            .ok_or(DecodeError::Truncated)?;
        let (body, checksum) = snapshot.split_at(split);
        let expected: [u8; 32] = Sha256::digest(body).into();
        if checksum != expected {
            return Err(SetupError::Checksum);
        }
        let mut r = Reader::new(body);
        if &r.array::<8>()? != SNAPSHOT_MAGIC {
            return Err(DecodeError::Invalid("snapshot magic").into());
        }
        let version = r.u16()?;
        if !(1..=SNAPSHOT_VERSION).contains(&version) {
            return Err(DecodeError::Invalid("snapshot version").into());
        }
        if read_key(&mut r)? != key.public_key() {
            return Err(SetupError::KeyMismatch);
        }
        let sequence = r.u64()?;
        let mut ledger = Ledger::decode(&mut r)?;
        let permuted = match version {
            1 => false,
            _ => match r.u8()? {
                0 => false,
                1 => true,
                _ => return Err(DecodeError::Invalid("serial order").into()),
            },
        };
        if permuted {
            let order = Permutation::for_issuer(&key, ledger.config().layer_size);
            ledger = ledger.with_order(order);
        }
        policy.validate(ledger.config())?;
        let value = (0..ledger.config().layers)
            .map(|_| r.u128())
            .collect::<Result<Vec<_>, _>>()?;
        let minted = r.u128()?;
        let destroyed = r.u128()?;
        let mut receipts = Receipts::default();
        for _ in 0..r.u32()? {
            let id = TransferId::from_bytes(r.array()?);
            let issued = read_list(&mut r, 52, read_issued)?;
            if receipts.issued.insert(id, issued).is_some() {
                return Err(DecodeError::Invalid("duplicate receipt").into());
            }
            receipts.order.push_back(id);
        }
        r.finish()?;
        receipts.shrink_to(policy.receipt_capacity as usize);
        Ok(Self {
            public: key.public_key(),
            key,
            policy,
            ledger,
            sequence,
            value,
            minted,
            destroyed,
            receipts,
        })
    }

    /// The serial the `k`-th next allocation in the layer at `position` would get.
    fn next_serial(&self, position: u32, k: u32) -> Option<Serial> {
        self.ledger.next_serial(position, k)
    }

    fn sign_all(&self, issued: &[Issued]) -> Vec<Note> {
        issued
            .iter()
            .map(|i| Note::issue(&self.key, i.serial, i.amount, i.holder))
            .collect()
    }

    fn apply_mint(&mut self, issued: &Issued) -> Result<(), ApplyError> {
        let position = self.check_issued(std::slice::from_ref(issued))?;
        self.allocate(position, issued);
        self.minted += u128::from(issued.amount);
        Ok(())
    }

    fn apply_transfer(
        &mut self,
        id: TransferId,
        spent: &[Spent],
        issued: &[Issued],
    ) -> Result<(), ApplyError> {
        // Check everything first so a misfit event leaves the state untouched.
        if spent.is_empty() || issued.is_empty() {
            return Err(ApplyError(
                "a transfer spends and issues at least one note".into(),
            ));
        }
        let mut drained: Vec<(u32, u128)> = Vec::new();
        let mut serials = Vec::with_capacity(spent.len());
        for s in spent {
            let Status::Live { position } = self.ledger.status(s.serial) else {
                return Err(ApplyError(format!("note {} is not live", s.serial)));
            };
            match drained.iter_mut().find(|(p, _)| *p == position) {
                Some((_, total)) => *total += u128::from(s.amount),
                None => drained.push((position, u128::from(s.amount))),
            }
            serials.push(s.serial);
        }
        serials.sort_unstable();
        if serials.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ApplyError("a note is spent twice".into()));
        }
        if let Some((position, _)) = drained
            .iter()
            .find(|(position, total)| self.value[*position as usize] < *total)
        {
            return Err(ApplyError(format!(
                "layer {position} holds less value than is spent"
            )));
        }
        let total_in: u128 = spent.iter().map(|s| u128::from(s.amount)).sum();
        let total_out: u128 = issued.iter().map(|i| u128::from(i.amount)).sum();
        if total_in != total_out {
            return Err(ApplyError(format!(
                "spends {total_in} but issues {total_out}"
            )));
        }
        let position = self.check_issued(issued)?;

        for s in spent {
            let from = self
                .ledger
                .invalidate(s.serial)
                .expect("checked live above");
            self.value[from as usize] -= u128::from(s.amount);
        }
        for i in issued {
            self.allocate(position, i);
        }
        let capacity = self.policy.receipt_capacity as usize;
        self.receipts.insert(capacity, id, issued.to_vec());
        Ok(())
    }

    fn apply_rotate(&mut self, report: &RotationReport) -> Result<(), ApplyError> {
        if self.plan_rotate() != Event::Rotate(*report) {
            return Err(ApplyError(format!(
                "rotation {report:?} does not match the ledger"
            )));
        }
        self.ledger.rotate();
        self.value.remove(0);
        self.value.push(0);
        self.destroyed += report.destroyed_value;
        self.receipts.prune(self.ledger.oldest_generation());
        Ok(())
    }

    /// Checks that `issued` are exactly the next allocations of one layer and
    /// returns that layer's position.
    fn check_issued(&self, issued: &[Issued]) -> Result<u32, ApplyError> {
        let generation = issued[0].serial.generation;
        let position = self
            .ledger
            .position(generation)
            .ok_or_else(|| ApplyError(format!("generation {generation} is not in the ledger")))?;
        for (k, i) in (0u32..).zip(issued) {
            if i.amount == 0 {
                return Err(ApplyError(format!("note {} has no value", i.serial)));
            }
            if Some(i.serial) != self.next_serial(position, k) {
                return Err(ApplyError(format!(
                    "note {} is not the next free bit of its layer",
                    i.serial
                )));
            }
        }
        Ok(position)
    }

    fn allocate(&mut self, position: u32, issued: &Issued) {
        let serial = self.ledger.allocate(position).expect("checked free above");
        debug_assert_eq!(serial, issued.serial);
        self.value[position as usize] += u128::from(issued.amount);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::HolderKey;
    use crate::transfer::Output;

    fn issuer(layers: u32, policy: Policy) -> Issuer {
        let config = LedgerConfig {
            layers,
            layer_size: 64,
        };
        Issuer::new(IssuerKey::generate(), config, policy).unwrap()
    }

    fn pay(issuer: &mut Issuer, note: Note, key: &HolderKey) -> Result<(Note, HolderKey), Error> {
        let next = HolderKey::generate();
        let output = Output {
            amount: note.amount(),
            holder: next.public_key(),
        };
        let request = TransferRequest::single(note, key, output)?;
        let mut notes = issuer.transfer(&request)?;
        Ok((notes.remove(0), next))
    }

    #[test]
    fn mints_into_the_second_to_last_layer() {
        let mut i = issuer(4, Policy::cash_like());
        let key = HolderKey::generate();
        let note = i.mint(8, key.public_key()).unwrap();
        assert_eq!(
            i.status(&note),
            Ok(NoteStatus::Live {
                position: 2,
                hops_left: 1,
                expires_after_rotations: 3
            })
        );
        assert_eq!(i.supply().outstanding, 8);
    }

    #[test]
    fn transfer_reissues_one_layer_on_and_refuses_double_spends() {
        let mut i = issuer(4, Policy::cash_like());
        let alice = HolderKey::generate();
        let note = i.mint(8, alice.public_key()).unwrap();
        let (bobs, bob) = pay(&mut i, note, &alice).unwrap();
        assert_eq!(i.status(&note), Ok(NoteStatus::Spent));
        assert!(matches!(
            i.status(&bobs),
            Ok(NoteStatus::Live { position: 3, .. })
        ));

        // Alice resends a different request spending the same note.
        let again = Output {
            amount: 8,
            holder: HolderKey::generate().public_key(),
        };
        let request = TransferRequest::single(note, &alice, again).unwrap();
        assert_eq!(i.transfer(&request), Err(Error::Spent(note.serial())));

        // Bob's note is in the last layer and frozen until a rotation.
        assert_eq!(
            pay(&mut i, bobs, &bob).unwrap_err(),
            Error::Frozen(bobs.serial())
        );
        i.rotate();
        assert!(pay(&mut i, bobs, &bob).is_ok());
    }

    #[test]
    fn resending_the_same_request_returns_the_same_notes() {
        let mut i = issuer(4, Policy::cash_like());
        let alice = HolderKey::generate();
        let note = i.mint(8, alice.public_key()).unwrap();
        let output = Output {
            amount: 8,
            holder: HolderKey::generate().public_key(),
        };
        let request = TransferRequest::single(note, &alice, output).unwrap();
        let first = i.transfer(&request).unwrap();
        let sequence = i.sequence();
        assert_eq!(i.transfer(&request).unwrap(), first);
        assert_eq!(i.sequence(), sequence, "a replay changes nothing");
    }

    #[test]
    fn untouched_notes_die_and_refreshed_notes_live() {
        let mut i = issuer(3, Policy::cash_like());
        let idle = HolderKey::generate();
        let busy = HolderKey::generate();
        let idle_note = i.mint(4, idle.public_key()).unwrap();
        let mut busy_note = i.mint(2, busy.public_key()).unwrap();
        let mut busy_key = busy;
        // Minted at position 1 of 3: dies in the second rotation untouched.
        for _ in 0..4 {
            // Refresh: pay the note to a fresh key of one's own.
            (busy_note, busy_key) = pay(&mut i, busy_note, &busy_key).unwrap();
            i.rotate();
        }
        assert_eq!(i.status(&idle_note), Ok(NoteStatus::Expired));
        assert!(matches!(i.status(&busy_note), Ok(NoteStatus::Live { .. })));
        assert_eq!(
            i.supply(),
            Supply {
                minted: 6,
                destroyed: 4,
                outstanding: 2
            }
        );
    }

    #[test]
    fn a_replayed_old_note_cannot_match_a_reused_bit() {
        let mut i = issuer(2, Policy::cash_like());
        let key = HolderKey::generate();
        // Minted into layer 0, generation 0, bit 0.
        let old = i.mint(1, key.public_key()).unwrap();
        i.rotate();
        i.rotate();
        // Slot 0 now holds generation 2; its first mint sets bit 0 again.
        let fresh = i.mint(1, key.public_key()).unwrap();
        assert_eq!(fresh.serial().generation, 2);
        assert_eq!(i.ledger().layer(0).unwrap().issued, 1);
        assert_eq!(i.status(&old), Ok(NoteStatus::Expired));
        assert_eq!(
            pay(&mut i, old, &key).unwrap_err(),
            Error::Expired(old.serial())
        );
    }

    #[test]
    fn full_layers_refuse_mints_and_transfers() {
        let mut i = issuer(3, Policy::cash_like());
        let key = HolderKey::generate();
        let ones: Vec<_> = (0..63)
            .map(|_| i.mint(1, key.public_key()).unwrap())
            .collect();
        let two = i.mint(2, key.public_key()).unwrap();
        assert_eq!(
            i.mint(1, key.public_key()),
            Err(Error::LayerFull { position: 1 })
        );

        let paid: Vec<_> = ones
            .iter()
            .map(|n| pay(&mut i, *n, &key).unwrap())
            .collect();
        // One bit is left in layer 2, but splitting the 2 into 1 + 1 needs two.
        let half = Output {
            amount: 1,
            holder: key.public_key(),
        };
        let split = TransferRequest::sign(&[(two, &key)], vec![half, half]).unwrap();
        assert_eq!(i.transfer(&split), Err(Error::LayerFull { position: 2 }));

        // After a rotation the filled layer moves forward and its notes can
        // move on into the fresh last layer.
        i.rotate();
        let (note, owner) = &paid[0];
        assert!(pay(&mut i, *note, owner).is_ok());
    }

    #[test]
    fn merging_outputs_land_past_the_furthest_input() {
        let mut i = issuer(4, Policy::flexible());
        let key = HolderKey::generate();
        let young = i.mint(3, key.public_key()).unwrap();
        let old = i.mint(4, key.public_key()).unwrap();
        let (old, old_key) = pay(&mut i, old, &key).unwrap();
        i.rotate();
        // young is at position 1, old at position 2.
        let out = HolderKey::generate();
        let request = TransferRequest::sign(
            &[(young, &key), (old, &old_key)],
            vec![Output {
                amount: 7,
                holder: out.public_key(),
            }],
        )
        .unwrap();
        let merged = i.transfer(&request).unwrap();
        assert!(matches!(
            i.status(&merged[0]),
            Ok(NoteStatus::Live { position: 3, .. })
        ));
    }

    #[test]
    fn a_verifier_from_a_looser_policy_does_not_bypass_this_one() {
        let key = IssuerKey::generate();
        let config = LedgerConfig {
            layers: 4,
            layer_size: 64,
        };
        let mut loose = Issuer::new(key.clone(), config, Policy::flexible()).unwrap();
        let holder = HolderKey::generate();
        let a = loose.mint(4, holder.public_key()).unwrap();
        let b = loose.mint(4, holder.public_key()).unwrap();
        let strict = Issuer::restore(key, &loose.snapshot(), Policy::cash_like()).unwrap();
        let merge = TransferRequest::sign(
            &[(a, &holder), (b, &holder)],
            vec![Output {
                amount: 8,
                holder: holder.public_key(),
            }],
        )
        .unwrap();
        let verified = loose.verifier().verify(&merge).unwrap();
        assert!(matches!(
            strict.plan_transfer(&verified),
            Err(Error::Policy(_))
        ));
    }

    #[test]
    fn misfit_events_change_nothing() {
        let mut i = issuer(4, Policy::cash_like());
        let key = HolderKey::generate();
        let event = i.plan_mint(8, key.public_key()).unwrap();
        i.apply(&event).unwrap();
        let before = i.snapshot();
        assert!(i.apply(&event).is_err(), "the same mint cannot apply twice");
        let Event::Mint(issued) = event else {
            unreachable!()
        };
        let forged = Event::Transfer {
            id: TransferId::from_bytes([0; 32]),
            spent: vec![Spent {
                serial: issued.serial,
                amount: 8,
            }],
            issued: vec![Issued {
                amount: 9,
                ..issued
            }],
        };
        assert!(i.apply(&forged).is_err(), "value must balance");
        assert_eq!(i.snapshot(), before);
    }

    #[test]
    fn snapshot_round_trips_and_detects_corruption() {
        let mut i = issuer(4, Policy::cash_like());
        let key = HolderKey::generate();
        let note = i.mint(8, key.public_key()).unwrap();
        pay(&mut i, note, &key).unwrap();
        i.rotate();
        let snapshot = i.snapshot();
        let restored = Issuer::restore(i.key.clone(), &snapshot, Policy::cash_like()).unwrap();
        assert_eq!(restored.snapshot(), snapshot);
        assert_eq!(restored.supply(), i.supply());

        let mut corrupt = snapshot.clone();
        corrupt[20] ^= 1;
        assert_eq!(
            Issuer::restore(i.key.clone(), &corrupt, Policy::cash_like()).unwrap_err(),
            SetupError::Checksum
        );
        assert_eq!(
            Issuer::restore(IssuerKey::generate(), &snapshot, Policy::cash_like()).unwrap_err(),
            SetupError::KeyMismatch
        );
    }

    #[test]
    fn split_outputs_get_unrelated_indexes() {
        let mut i = issuer(3, Policy::cash_like());
        let key = HolderKey::generate();
        let note = i.mint(16, key.public_key()).unwrap();
        let outputs = (0..16)
            .map(|_| Output {
                amount: 1,
                holder: key.public_key(),
            })
            .collect();
        let request = TransferRequest::sign(&[(note, &key)], outputs).unwrap();
        let notes = i.transfer(&request).unwrap();
        let indexes: Vec<u32> = notes.iter().map(|n| n.serial().index).collect();
        // The outputs took bits 0 to 15 of their layer, but not as indexes.
        assert_ne!(indexes, (0..16).collect::<Vec<_>>());
        let mut sorted = indexes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 16);

        // The permutation comes from the key, so a restored issuer agrees.
        let restored = Issuer::restore(i.key.clone(), &i.snapshot(), Policy::cash_like()).unwrap();
        for note in &notes {
            assert!(matches!(restored.status(note), Ok(NoteStatus::Live { .. })));
        }
        assert_eq!(restored.next_serial(2, 0), i.next_serial(2, 0));
    }

    #[test]
    fn version_1_snapshots_restore_with_bits_as_indexes() {
        // An issuer from before serials were permuted.
        let mut i = issuer(3, Policy::cash_like());
        i.ledger = Ledger::new(i.ledger.config()).unwrap();
        let key = HolderKey::generate();
        let first = i.mint(4, key.public_key()).unwrap();
        assert_eq!(first.serial(), Serial::new(1, 0));

        // Its snapshot as version 1 wrote it: no serial order byte.
        let flag = 8 + 2 + 32 + 8 + i.ledger.to_bytes().len();
        let mut body = i.snapshot();
        body.truncate(body.len() - 32);
        assert_eq!(body[flag], 0);
        body.remove(flag);
        body[8..10].copy_from_slice(&1u16.to_be_bytes());
        let checksum: [u8; 32] = Sha256::digest(&body).into();
        body.extend_from_slice(&checksum);

        let mut restored = Issuer::restore(i.key.clone(), &body, Policy::cash_like()).unwrap();
        assert!(!restored.ledger().is_permuted());
        assert!(matches!(
            restored.status(&first),
            Ok(NoteStatus::Live { .. })
        ));
        let second = restored.mint(4, key.public_key()).unwrap();
        assert_eq!(second.serial(), Serial::new(1, 1));
        // Written again, it stays unpermuted.
        let again = Issuer::restore(i.key.clone(), &restored.snapshot(), Policy::cash_like());
        assert!(!again.unwrap().ledger().is_permuted());

        // Any other order byte is refused.
        let mut bad = i.snapshot();
        bad.truncate(bad.len() - 32);
        bad[flag] = 2;
        let checksum: [u8; 32] = Sha256::digest(&bad).into();
        bad.extend_from_slice(&checksum);
        assert!(Issuer::restore(i.key.clone(), &bad, Policy::cash_like()).is_err());
    }

    #[test]
    fn events_round_trip_in_binary_and_json() {
        let mut i = issuer(4, Policy::flexible());
        let key = HolderKey::generate();
        let mint = i.plan_mint(5, key.public_key()).unwrap();
        i.apply(&mint).unwrap();
        let note = i.notes(&mint).remove(0);
        let request = TransferRequest::sign(
            &[(note, &key)],
            vec![
                Output {
                    amount: 2,
                    holder: key.public_key(),
                },
                Output {
                    amount: 3,
                    holder: key.public_key(),
                },
            ],
        )
        .unwrap();
        let Plan::Apply(transfer) = i
            .plan_transfer(&i.verifier().verify(&request).unwrap())
            .unwrap()
        else {
            panic!("fresh transfer")
        };
        for event in [mint, transfer, i.plan_rotate()] {
            assert_eq!(Event::from_bytes(&event.to_bytes()), Ok(event.clone()));
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), event);
        }
        assert!(Event::from_bytes(&[9]).is_err());
    }
}
