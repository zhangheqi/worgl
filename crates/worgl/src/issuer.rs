//! The issuer: its key, its ledger, its policy and the rules that tie them.
//!
//! Every state change is an [`Event`]. The issuer *plans* an event from a
//! request without changing anything, the caller makes the event durable if
//! it wants to (see [`store`](crate::store)), and then the issuer *applies*
//! it. Applying is deterministic and checks the event against the state, so
//! replaying a log of events onto the snapshot it started from rebuilds
//! the exact same issuer. [`Issuer::check`] makes the same checks without
//! applying, so a store can refuse an event before it logs one that would
//! not replay. [`Issuer::mint`], [`Issuer::transfer`],
//! [`Issuer::burn`] and [`Issuer::rotate`] do all three steps for in-memory
//! use.
//!
//! An issuer can be paused, as an emergency stop: until it resumes it plans
//! and applies no mint, transfer, burn or rotation, so no note moves and none
//! expires. The pause is an event like any other, so a replica of the log
//! agrees on it. So is the time a scheduled rotation was due, which keeps a
//! rotation schedule from repeating a period after a restart or a failover.
//!
//! A rotation is the one event that needs a secret the log must not hold:
//! the key that signs the new generation's notes. [`Issuer::plan_rotate`]
//! takes it from the caller and puts it in the issuer's
//! [`KeyRing`], which a store saves before logging
//! the rotation; the event itself carries only the key's certificate.
//!
//! Besides the ledger's bits, the issuer keeps a few counters that cost
//! nothing per note: the live value in each layer, the value ever minted, the
//! value destroyed by rotations and the value burned on purpose. They satisfy
//! `outstanding = minted − destroyed − burned` at all times.
//!
//! It also keeps a receipt of each recent transfer and burn, so the identical
//! request sent again is answered as before instead of refused as spent: with
//! the same notes, signed again, or for a burn with nothing more. It keeps the
//! last [`receipt_capacity`](Policy::receipt_capacity) of them, a transfer's
//! only until the notes it issued expire. Given a [`Clock`] (see
//! [`Issuer::set_clock`]), it also keeps each for at least
//! [`receipt_min_age_secs`](Policy::receipt_min_age_secs), and refuses as
//! [`Error::Busy`] a new transfer or burn whose receipt would push out a
//! younger one. Only planning decides anything by the clock: applying an
//! event stamps its receipt with the time, but the stamps stay out of
//! snapshots and out of whether an event applies, so replicas keep the same
//! receipts, by count.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::codec::{DecodeError, Reader, Writer};
use crate::error::{ApplyError, Error, SetupError};
use crate::kek::{Kek, KekError, SealedKey};
use crate::keys::{GenerationKey, IssuerKey, PublicKey, Signature};
use crate::keyset::{KeyCertificate, KeyRing, KeySet, signing_key};
use crate::ledger::{Ledger, LedgerConfig, Serial, Status};
use crate::mint::{MintAuthorization, MintRequest, VerifiedMint};
use crate::note::{Note, NoteError, read_key};
use crate::permutation::Permutation;
use crate::policy::Policy;
use crate::transfer::{
    BurnRequest, Output, TransferId, TransferRequest, VerifiedBurn, VerifiedTransfer, Verifier,
};

const SNAPSHOT_MAGIC: &[u8; 8] = b"WORGLSNP";
/// Version 2 added the burned value, version 3 records whether serial
/// indexes are permuted, version 4 keeps each mint authority's last mint,
/// version 5 the certificates of the generation keys, version 6 whether the
/// issuer is paused and when its last scheduled rotation was due, version 7
/// the keys of the last generations dropped, and version 8 keeps in a
/// transfer's receipt where its notes went instead of the notes. Older
/// snapshots still restore, with nothing burned, with bits as indexes, with
/// no mint authorities, with every generation signed by the issuer key,
/// running, with no rotation scheduled and with no dropped generation's key,
/// as they always had, and each receipt is read from the notes it kept.
const SNAPSHOT_VERSION: u16 = 8;

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
    /// The certificate of the key that signs the new layer's notes. `None`
    /// only in logs written before generation keys existed, whose new
    /// layers the issuer key signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<KeyCertificate>,
    /// For a rotation a schedule made, the time it was due, in seconds since
    /// the Unix epoch. `None` for a rotation made on request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<u64>,
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
    /// Notes were destroyed on their holders' request, issuing nothing.
    Burn {
        /// Identity of the burn request.
        id: TransferId,
        /// The destroyed notes.
        spent: Vec<Spent>,
    },
    /// New money was minted into the mint layer at a mint authority's
    /// signed request.
    AuthorizedMint {
        /// Who asked, with their signature.
        authorization: MintAuthorization,
        /// The note created.
        issued: Issued,
    },
    /// The issuer stopped making changes until it resumes.
    Pause,
    /// The issuer makes changes again.
    Resume,
}

impl Event {
    /// The event's binary encoding, as used by the [`log`](crate::log).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::default();
        match self {
            Event::Mint(issued) => {
                w.u8(1);
                write_issued(&mut w, issued);
            }
            Event::Transfer { id, spent, issued } => {
                w.u8(2).bytes(&id.to_bytes());
                write_spent(&mut w, spent);
                w.u32(issued.len() as u32);
                for i in issued {
                    write_issued(&mut w, i);
                }
            }
            Event::Rotate(r) => {
                // Tag 3 is the rotation as written before generation keys,
                // tag 6 one with a key, and tag 9 a scheduled one.
                w.u8(match (&r.key, r.due) {
                    (_, Some(_)) => 9,
                    (Some(_), None) => 6,
                    (None, None) => 3,
                })
                .u64(r.dropped)
                .u64(r.created)
                .u32(r.destroyed_notes)
                .u128(r.destroyed_value);
                if let Some(due) = r.due {
                    w.u8(u8::from(r.key.is_some())).u64(due);
                }
                if let Some(key) = &r.key {
                    key.write(&mut w);
                }
            }
            Event::Burn { id, spent } => {
                w.u8(4).bytes(&id.to_bytes());
                write_spent(&mut w, spent);
            }
            Event::AuthorizedMint {
                authorization,
                issued,
            } => {
                w.u8(5)
                    .bytes(authorization.authority.as_bytes())
                    .u64(authorization.sequence)
                    .bytes(&authorization.signature.to_bytes());
                write_issued(&mut w, issued);
            }
            Event::Pause => {
                w.u8(7);
            }
            Event::Resume => {
                w.u8(8);
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
                let spent = read_spent(&mut r)?;
                let issued = read_list(&mut r, 52, read_issued)?;
                Event::Transfer { id, spent, issued }
            }
            tag @ (3 | 6 | 9) => {
                let (dropped, created) = (r.u64()?, r.u64()?);
                let (destroyed_notes, destroyed_value) = (r.u32()?, r.u128()?);
                let (keyed, due) = match tag {
                    9 => {
                        let keyed = match r.u8()? {
                            0 => false,
                            1 => true,
                            _ => return Err(DecodeError::Invalid("rotation key flag")),
                        };
                        (keyed, Some(r.u64()?))
                    }
                    _ => (tag == 6, None),
                };
                Event::Rotate(RotationReport {
                    dropped,
                    created,
                    destroyed_notes,
                    destroyed_value,
                    key: if keyed {
                        Some(KeyCertificate::read(&mut r)?)
                    } else {
                        None
                    },
                    due,
                })
            }
            4 => Event::Burn {
                id: TransferId::from_bytes(r.array()?),
                spent: read_spent(&mut r)?,
            },
            5 => Event::AuthorizedMint {
                authorization: MintAuthorization {
                    authority: read_key(&mut r)?,
                    sequence: r.u64()?,
                    signature: Signature::from_bytes(&r.array()?),
                },
                issued: read_issued(&mut r)?,
            },
            7 => Event::Pause,
            8 => Event::Resume,
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

fn spent(notes: &[Note]) -> Vec<Spent> {
    notes
        .iter()
        .map(|note| Spent {
            serial: note.serial(),
            amount: note.amount(),
        })
        .collect()
}

fn write_spent(w: &mut Writer, spent: &[Spent]) {
    w.u32(spent.len() as u32);
    for s in spent {
        w.u64(s.serial.generation).u32(s.serial.index).u64(s.amount);
    }
}

fn read_spent(r: &mut Reader<'_>) -> Result<Vec<Spent>, DecodeError> {
    read_list(r, 20, |r| {
        Ok(Spent {
            serial: Serial::new(r.u64()?, r.u32()?),
            amount: r.u64()?,
        })
    })
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

/// The outcome of [`Issuer::plan_transfer`], [`Issuer::plan_burn`] and
/// [`Issuer::plan_authorized_mint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// A fresh request: make the event durable if needed, then
    /// [`apply`](Issuer::apply) it and hand out [`notes`](Issuer::notes).
    Apply(Event),
    /// The identical request was already applied; these are the notes it
    /// produced, signed again, which is none for a burn. Nothing needs to be
    /// stored or applied.
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
    /// Value burned on its holders' request.
    #[serde(default)]
    pub burned: u128,
    /// Value held in live notes: `minted − destroyed − burned`.
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

/// Seconds on a clock that never goes back, by which an [`Issuer`] keeps
/// each receipt for at least its policy's
/// [`receipt_min_age_secs`](Policy::receipt_min_age_secs). See
/// [`Issuer::set_clock`]. Cloning it is cheap, and clones read the same
/// clock.
#[derive(Clone)]
pub struct Clock(Arc<dyn Fn() -> u64 + Send + Sync>);

impl Clock {
    /// A clock that reads `now`, which must never go back.
    pub fn new(now: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        Self(Arc::new(now))
    }

    /// The system's monotonic clock, in whole seconds. It reads the Unix time
    /// when it is made and counts on from there however the wall clock is
    /// set later, so [`Issuer::set_clock`] can count receipts as made before
    /// it. Not on `wasm32-unknown-unknown`, which has no clock to read.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub fn monotonic() -> Self {
        let start = std::time::Instant::now();
        let base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        Self::new(move || base.saturating_add(start.elapsed().as_secs()))
    }

    /// The time now, in seconds.
    pub fn now(&self) -> u64 {
        (self.0)()
    }
}

impl fmt::Debug for Clock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Clock").finish_non_exhaustive()
    }
}

/// What the issuer keeps of a transfer or burn it applied, to answer the
/// identical request again. A transfer's outputs took consecutive bits of one
/// layer, and the request's id commits to their amounts and holders, so where
/// they went is all it takes to sign the same notes again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Receipt {
    /// The generation the outputs went to; 0 for a burn.
    generation: u64,
    /// The bit the first output took, the others taking the bits after it;
    /// 0 for a burn.
    first: u32,
    /// How many outputs there were; 0 for a burn.
    outputs: u8,
    /// When the issuer made the receipt, by its clock, or 0 without one.
    /// Each issuer keeps its own: snapshots leave it out.
    at: u64,
}

impl Receipt {
    /// A burn's receipt.
    const BURN: Self = Self {
        generation: 0,
        first: 0,
        outputs: 0,
        at: 0,
    };

    /// The receipt of the transfer that issued `issued`, or of a burn if
    /// that is empty, as snapshots before version 8 kept it: `None` unless
    /// the notes took consecutive bits the ledger handed out in a live
    /// layer.
    fn of_notes(ledger: &Ledger, issued: &[Issued]) -> Option<Self> {
        let Some(head) = issued.first() else {
            return Some(Self::BURN);
        };
        let receipt = Self {
            generation: head.serial.generation,
            first: ledger.bit_of(head.serial)?,
            outputs: u8::try_from(issued.len()).ok()?,
            at: 0,
        };
        let consecutive = |receipt: &Self| {
            (receipt.first..)
                .zip(issued)
                .all(|(bit, i)| ledger.serial_of(receipt.generation, bit) == i.serial)
        };
        (receipt.fits(ledger) && consecutive(&receipt)).then_some(receipt)
    }

    /// Whether the ledger handed out the bits the receipt names: none for a
    /// burn, whose generation and first bit are 0, and bits of a live layer
    /// for a transfer.
    fn fits(&self, ledger: &Ledger) -> bool {
        if self.outputs == 0 {
            return self.generation == 0 && self.first == 0;
        }
        let Some(layer) = ledger
            .position(self.generation)
            .and_then(|position| ledger.layer(position))
        else {
            return false;
        };
        self.first
            .checked_add(u32::from(self.outputs))
            .is_some_and(|end| end <= layer.issued)
    }
}

/// The receipts an issuer keeps, by request id, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Receipts {
    order: VecDeque<TransferId>,
    held: HashMap<TransferId, Receipt>,
}

impl Receipts {
    fn insert(&mut self, capacity: usize, id: TransferId, receipt: Receipt) {
        if capacity == 0 {
            return;
        }
        if self.held.insert(id, receipt).is_none() {
            // Push the oldest out first, so the queue never outgrows
            // `capacity`.
            self.shrink_to(capacity - 1);
            self.order.push_back(id);
        }
    }

    fn shrink_to(&mut self, capacity: usize) {
        while self.order.len() > capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.held.remove(&oldest);
            }
        }
    }

    /// Seconds until one more receipt would push out none younger than
    /// `min_age` at `now`: 0 while there is room for it or the oldest is
    /// old enough.
    fn wait(&self, capacity: usize, min_age: u32, now: u64) -> u32 {
        match self.order.front() {
            Some(oldest) if self.order.len() >= capacity => {
                let age = now.saturating_sub(self.held[oldest].at);
                min_age.saturating_sub(u32::try_from(age).unwrap_or(u32::MAX))
            }
            _ => 0,
        }
    }

    /// Forgets transfers whose notes have expired. Burns issued nothing, so
    /// they stay until newer requests push them out.
    fn prune(&mut self, oldest_generation: u64) {
        let held = &mut self.held;
        self.order.retain(|id| {
            let receipt = held[id];
            let alive = receipt.outputs == 0 || receipt.generation >= oldest_generation;
            if !alive {
                held.remove(id);
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
    /// Public keys of the live generations.
    keys: KeySet,
    /// Public keys of the live generations and of the last ones dropped, up
    /// to one fewer than there are layers: the most rotations by which a
    /// receipt outlives its inputs' generation. Requests are checked against
    /// these.
    recent: KeySet,
    /// The live generations' secrets, and any key a planned rotation is
    /// about to certify.
    ring: KeyRing,
    policy: Policy,
    ledger: Ledger,
    sequence: u64,
    /// Live value per layer, indexed by position.
    value: Vec<u128>,
    minted: u128,
    destroyed: u128,
    burned: u128,
    receipts: Receipts,
    /// Each mint authority's last accepted request and the note it created.
    authorities: BTreeMap<PublicKey, (MintAuthorization, Issued)>,
    paused: bool,
    /// When the last scheduled rotation was due.
    last_due: Option<u64>,
    /// What receipts are stamped by, if anything.
    clock: Option<Clock>,
}

impl Issuer {
    /// Creates an issuer with an empty ledger and a fresh generation key for
    /// each of its layers. Serial indexes are a keyed permutation of the
    /// ledger's bits, derived from `key`, so they do not reveal the order
    /// notes were issued in.
    pub fn new(key: IssuerKey, ledger: LedgerConfig, policy: Policy) -> Result<Self, SetupError> {
        let ledger =
            Ledger::new(ledger)?.with_order(Permutation::for_issuer(&key, ledger.layer_size));
        policy.validate(ledger.config())?;
        let public = key.public_key();
        let mut ring = KeyRing::new(public);
        let mut certificates = Vec::new();
        for generation in ledger.oldest_generation()..=ledger.newest_generation() {
            let next = GenerationKey::try_generate()?;
            certificates.push(KeyCertificate::sign(&key, generation, next.public_key()));
            ring.insert(next);
        }
        let keys = KeySet::new(public, ledger.oldest_generation(), certificates)
            .expect("freshly certified keys form a set");
        Ok(Self {
            public,
            key,
            recent: keys.clone(),
            keys,
            ring,
            policy,
            value: vec![0; ledger.config().layers as usize],
            ledger,
            sequence: 0,
            minted: 0,
            destroyed: 0,
            burned: 0,
            receipts: Receipts::default(),
            authorities: BTreeMap::new(),
            paused: false,
            last_due: None,
            clock: None,
        })
    }

    /// The issuer's public key, which every note names.
    pub fn public_key(&self) -> &PublicKey {
        &self.public
    }

    /// The issuer key, for restoring the issuer again from its store.
    pub(crate) fn issuer_key(&self) -> &IssuerKey {
        &self.key
    }

    /// The clock given by [`set_clock`](Self::set_clock), if any, for giving
    /// it to the issuer restored again from its store.
    pub(crate) fn clock(&self) -> Option<&Clock> {
        self.clock.as_ref()
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

    /// Whether the issuer is paused. See the [module documentation](self).
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// When the last scheduled rotation was due, in seconds since the Unix
    /// epoch, if one was ever logged.
    pub fn last_scheduled_rotation(&self) -> Option<u64> {
        self.last_due
    }

    /// Gives the issuer a clock, by which it keeps each receipt for at least
    /// the policy's [`receipt_min_age_secs`](Policy::receipt_min_age_secs):
    /// from now on [`plan_transfer`](Self::plan_transfer) and
    /// [`plan_burn`](Self::plan_burn) refuse a request as [`Error::Busy`]
    /// when its receipt would push out a younger one. Without a clock it
    /// keeps receipts by count alone.
    ///
    /// The receipts it holds now count as made `age` seconds ago. Each was
    /// made no later than now, so 0 is always safe; a larger age is safe
    /// only if every one of them is at least that old, since a receipt
    /// counted older than it is may be pushed out while its client still
    /// resends. Counted younger than they are, `receipt_capacity` receipts
    /// hold every new transfer and burn back until they reach the minimum
    /// age. Call it once, when the issuer is made or restored; copies of it
    /// share the clock.
    pub fn set_clock(&mut self, clock: Clock, age: u64) {
        self.set_clock_split(clock, age, 0, 0);
    }

    /// Gives the issuer a clock as [`set_clock`](Self::set_clock) does, but
    /// counts only the receipts before the newest `newer` as made `age`
    /// seconds ago, and those `newer` as made `newer_age` seconds ago.
    pub(crate) fn set_clock_split(&mut self, clock: Clock, age: u64, newer: usize, newer_age: u64) {
        let now = clock.now();
        let Receipts { order, held } = &mut self.receipts;
        let older = order.len().saturating_sub(newer);
        for (index, id) in order.iter().enumerate() {
            let age = if index < older { age } else { newer_age };
            held.get_mut(id).expect("every receipt queued is held").at = now.saturating_sub(age);
        }
        self.clock = Some(clock);
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
            burned: self.burned,
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

    /// A verifier for requests, bound to this issuer's keys and policy as
    /// they are now. A rotation adds a key, so get a new one after each.
    pub fn verifier(&self) -> Verifier {
        Verifier::new(self.keys.clone(), self.recent.clone(), self.policy.clone())
    }

    /// The public keys that sign the live generations' notes.
    pub fn keys(&self) -> &KeySet {
        &self.keys
    }

    /// The secrets of the generation keys, to be kept apart from snapshots
    /// and logs and passed back to [`restore`](Self::restore).
    pub fn key_ring(&self) -> &KeyRing {
        &self.ring
    }

    /// Adds a generation key to the [`key_ring`](Self::key_ring), as a
    /// replica does with the key a rotation's log entry carries sealed (see
    /// [`kek`](crate::kek)): applying a rotation needs its key in the ring.
    pub fn add_key(&mut self, key: GenerationKey) {
        self.ring.insert(key);
    }

    /// Seals with `kek` the key of every live generation that has one, as a
    /// replica sends them along with its [`snapshot`](Self::snapshot) to a
    /// replica that lacks them. [`SealedKey::unseal`] opens each into a ring
    /// to [`restore`](Self::restore) the snapshot with.
    pub fn seal_keys(&self, kek: &dyn Kek) -> Result<Vec<SealedKey>, KekError> {
        self.keys
            .certificates()
            .iter()
            .filter_map(|certificate| {
                let key = self.ring.get(&certificate.key)?;
                Some(SealedKey::seal(
                    kek,
                    &self.public,
                    certificate.generation,
                    key,
                ))
            })
            .collect()
    }

    /// Checks a note's signature and looks up its state. O(1) after the
    /// signature check. A note of a dropped generation is expired, whoever
    /// signed it: its signature is not checked.
    pub fn status(&self, note: &Note) -> Result<NoteStatus, NoteError> {
        if note.issuer() == &self.public && note.serial().generation < self.keys.oldest() {
            return Ok(NoteStatus::Expired);
        }
        note.verify(&self.keys)?;
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
        self.check_running()?;
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

    /// The last sequence number accepted from a mint authority, if any.
    pub fn mint_sequence(&self, authority: &PublicKey) -> Option<u64> {
        self.authorities
            .get(authority)
            .map(|(authorization, _)| authorization.sequence)
    }

    /// Plans a verified mint request. Its sequence number must be above the
    /// authority's last, except that the authority's last request replays the
    /// note it created.
    ///
    /// The amount and the authority are checked against this issuer's policy
    /// again, which is cheap, in case the request was verified under a
    /// different one.
    pub fn plan_authorized_mint(&self, mint: &VerifiedMint) -> Result<Plan, Error> {
        self.check_running()?;
        if mint.issuer != self.public {
            return Err(Error::MintUnauthorized);
        }
        self.policy.check_mint(mint.amount)?;
        let MintAuthorization {
            authority,
            sequence,
            ..
        } = mint.authorization;
        if !self.policy.mint_authorities.contains(&authority) {
            return Err(Error::UnknownAuthority(authority));
        }
        if let Some((last, issued)) = self.authorities.get(&authority) {
            if sequence == last.sequence
                && mint.amount == issued.amount
                && mint.holder == issued.holder
            {
                if issued.serial.generation < self.ledger.oldest_generation() {
                    return Err(Error::Expired(issued.serial));
                }
                return Ok(Plan::Replay(self.sign_all(std::slice::from_ref(issued))));
            }
            if sequence <= last.sequence {
                return Err(Error::StaleMint {
                    sequence,
                    last: last.sequence,
                });
            }
        }
        let position = self.mint_position();
        let serial = self
            .next_serial(position, 0)
            .ok_or(Error::LayerFull { position })?;
        Ok(Plan::Apply(Event::AuthorizedMint {
            authorization: mint.authorization,
            issued: Issued {
                serial,
                amount: mint.amount,
                holder: mint.holder,
            },
        }))
    }

    /// Plans a verified transfer against the ledger: every input must be
    /// live and outside the last layer, no layer may hold less live value
    /// than the inputs spend from it, and the layer one past the furthest
    /// input must have room for every output. With a clock (see
    /// [`set_clock`](Self::set_clock)), a transfer whose receipt would push
    /// out one younger than the policy's
    /// [`receipt_min_age_secs`](Policy::receipt_min_age_secs) is then
    /// refused as [`Error::Busy`]; a resend of one applied before is not.
    ///
    /// The transfer's shape is checked against this issuer's policy again,
    /// which is cheap, in case it was verified under a different one.
    pub fn plan_transfer(&self, transfer: &VerifiedTransfer) -> Result<Plan, Error> {
        self.check_running()?;
        if transfer.issuer != self.public {
            return Err(Error::Note {
                input: 0,
                error: NoteError::WrongIssuer,
            });
        }
        let inputs: Vec<u64> = transfer.inputs.iter().map(Note::amount).collect();
        let outputs: Vec<u64> = transfer.outputs.iter().map(|o| o.amount).collect();
        self.policy.check_transfer(&inputs, &outputs)?;
        if let Some(receipt) = self.receipts.held.get(&transfer.id)
            && usize::from(receipt.outputs) == transfer.outputs.len()
        {
            return Ok(Plan::Replay(self.replay(receipt, &transfer.outputs)));
        }
        let last = self.ledger.last_position();
        let mut furthest = 0;
        let mut drawn = Vec::with_capacity(transfer.inputs.len());
        for note in &transfer.inputs {
            let serial = note.serial();
            let position = match self.ledger.status(serial) {
                Status::Live { position } if position == last => return Err(Error::Frozen(serial)),
                Status::Live { position } => position,
                Status::Spent => return Err(Error::Spent(serial)),
                Status::Expired => return Err(Error::Expired(serial)),
                Status::Unissued => return Err(Error::Unissued(serial)),
            };
            furthest = furthest.max(position);
            drawn.push((position, note.amount()));
        }
        if let Some(position) = self.overdrawn(&drawn) {
            return Err(Error::Overdrawn { position });
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
        self.check_room()?;
        Ok(Plan::Apply(Event::Transfer {
            id: transfer.id,
            spent: spent(&transfer.inputs),
            issued,
        }))
    }

    /// Plans a verified burn against the ledger: every input must be live,
    /// and no layer may hold less live value than the inputs burn from it.
    /// Notes frozen in the last layer can be burned, since nothing moves.
    /// A burn needs room for its receipt as a transfer does (see
    /// [`plan_transfer`](Self::plan_transfer)).
    pub fn plan_burn(&self, burn: &VerifiedBurn) -> Result<Plan, Error> {
        self.check_running()?;
        if burn.issuer != self.public {
            return Err(Error::Note {
                input: 0,
                error: NoteError::WrongIssuer,
            });
        }
        self.policy.check_burn(burn.inputs.len())?;
        if self
            .receipts
            .held
            .get(&burn.id)
            .is_some_and(|receipt| receipt.outputs == 0)
        {
            return Ok(Plan::Replay(Vec::new()));
        }
        let mut drawn = Vec::with_capacity(burn.inputs.len());
        for note in &burn.inputs {
            let serial = note.serial();
            match self.ledger.status(serial) {
                Status::Live { position } => drawn.push((position, note.amount())),
                Status::Spent => return Err(Error::Spent(serial)),
                Status::Expired => return Err(Error::Expired(serial)),
                Status::Unissued => return Err(Error::Unissued(serial)),
            }
        }
        if let Some(position) = self.overdrawn(&drawn) {
            return Err(Error::Overdrawn { position });
        }
        self.check_room()?;
        Ok(Plan::Apply(Event::Burn {
            id: burn.id,
            spent: spent(&burn.inputs),
        }))
    }

    /// Plans a rotation whose new last layer `next` will sign.
    ///
    /// The event carries the issuer key's certificate for `next`, and `next`
    /// joins the [`key_ring`](Self::key_ring) at once, since applying the
    /// event needs it. Save the ring before logging the event: a log
    /// that names a key nobody kept cannot be replayed. Refused while the
    /// issuer is paused, and then `next` is not kept.
    pub fn plan_rotate(&mut self, next: GenerationKey) -> Result<Event, Error> {
        self.plan_rotation(next, None)
    }

    /// Plans a rotation a schedule calls for, which was due at `due` seconds
    /// since the Unix epoch. It must be due after the last scheduled
    /// rotation, so a schedule that resumes after a restart or a failover
    /// never rotates twice for one period. Otherwise as
    /// [`plan_rotate`](Self::plan_rotate).
    pub fn plan_scheduled_rotation(
        &mut self,
        next: GenerationKey,
        due: u64,
    ) -> Result<Event, Error> {
        self.plan_rotation(next, Some(due))
    }

    fn plan_rotation(&mut self, next: GenerationKey, due: Option<u64>) -> Result<Event, Error> {
        self.check_running()?;
        if let (Some(due), Some(last)) = (due, self.last_due)
            && due <= last
        {
            return Err(Error::StaleRotation { due, last });
        }
        let mut report = self.rotation();
        report.key = Some(KeyCertificate::sign(
            &self.key,
            report.created,
            next.public_key(),
        ));
        report.due = due;
        self.ring.insert(next);
        Ok(Event::Rotate(report))
    }

    /// Plans pausing the issuer, or resuming it if `paused` is false. `None`
    /// if it already is.
    pub fn plan_pause(&self, paused: bool) -> Option<Event> {
        let event = if paused { Event::Pause } else { Event::Resume };
        // Planned exactly when it applies.
        self.check(&event).is_ok().then_some(event)
    }

    fn check_running(&self) -> Result<(), Error> {
        if self.paused {
            return Err(Error::Paused);
        }
        Ok(())
    }

    /// Refuses as [`Error::Busy`] a new receipt that would push out one
    /// younger than the policy's minimum age, if the issuer has a clock.
    fn check_room(&self) -> Result<(), Error> {
        let Some(clock) = &self.clock else {
            return Ok(());
        };
        let wait = self.receipts.wait(
            self.policy.receipt_capacity as usize,
            self.policy.receipt_min_age_secs,
            clock.now(),
        );
        if wait > 0 {
            return Err(Error::Busy {
                retry_after_secs: wait,
            });
        }
        Ok(())
    }

    /// What the next rotation drops and creates, without its key.
    fn rotation(&self) -> RotationReport {
        let first = self
            .ledger
            .layer(0)
            .expect("a ledger has at least two layers");
        RotationReport {
            dropped: first.generation,
            created: self.ledger.newest_generation() + 1,
            destroyed_notes: first.live,
            destroyed_value: self.value[0],
            key: None,
            due: None,
        }
    }

    /// Checks, changing nothing, that an event fits the current state.
    /// [`apply`](Self::apply) makes this same check first, so an event that
    /// passes applies for as long as the state stays as it is.
    pub fn check(&self, event: &Event) -> Result<(), ApplyError> {
        match event {
            Event::Pause | Event::Resume => {
                let paused = matches!(event, Event::Pause);
                if paused == self.paused {
                    return Err(ApplyError(format!(
                        "the issuer is already {}",
                        if paused { "paused" } else { "running" }
                    )));
                }
                Ok(())
            }
            _ if self.paused => Err(ApplyError("the issuer is paused".into())),
            Event::Mint(issued) => self.check_issued(std::slice::from_ref(issued)),
            Event::Transfer { spent, issued, .. } => self.check_transfer(spent, issued),
            Event::Rotate(report) => self.check_rotate(report),
            Event::Burn { spent, .. } => self.check_burn(spent),
            Event::AuthorizedMint {
                authorization,
                issued,
            } => self.check_authorized_mint(authorization, issued),
        }
    }

    /// Applies an event, or changes nothing and explains why it does not
    /// fit, as [`check`](Self::check) finds.
    pub fn apply(&mut self, event: &Event) -> Result<(), ApplyError> {
        self.check(event)?;
        match event {
            Event::Pause => self.paused = true,
            Event::Resume => self.paused = false,
            Event::Mint(issued) => self.apply_mint(issued),
            Event::Transfer { id, spent, issued } => self.apply_transfer(*id, spent, issued),
            Event::Rotate(report) => self.apply_rotate(report),
            Event::Burn { id, spent } => self.apply_burn(*id, spent),
            Event::AuthorizedMint {
                authorization,
                issued,
            } => self.apply_authorized_mint(authorization, issued),
        }
        self.sequence += 1;
        Ok(())
    }

    /// The notes an event created, signed. Empty for a rotation or a burn.
    ///
    /// Ed25519 signatures are deterministic, so signing the same event again
    /// yields byte-identical notes.
    pub fn notes(&self, event: &Event) -> Vec<Note> {
        match event {
            Event::Mint(issued) | Event::AuthorizedMint { issued, .. } => {
                self.sign_all(std::slice::from_ref(issued))
            }
            Event::Transfer { issued, .. } => self.sign_all(issued),
            Event::Rotate(_) | Event::Burn { .. } | Event::Pause | Event::Resume => Vec::new(),
        }
    }

    /// Mints a note in memory: plan, apply and sign in one step.
    pub fn mint(&mut self, amount: u64, holder: PublicKey) -> Result<Note, Error> {
        let event = self.plan_mint(amount, holder)?;
        self.apply(&event)?;
        Ok(self.notes(&event).remove(0))
    }

    /// Verifies and carries out a transfer in memory.
    pub fn transfer(&mut self, request: &TransferRequest) -> Result<Vec<Note>, Error> {
        let verified = self.verifier().verify(request)?;
        match self.plan_transfer(&verified)? {
            Plan::Replay(notes) => Ok(notes),
            Plan::Apply(event) => {
                self.apply(&event)?;
                Ok(self.notes(&event))
            }
        }
    }

    /// Verifies and carries out a burn in memory, returning the value
    /// destroyed. A resent burn that was already applied returns the same
    /// value and changes nothing.
    pub fn burn(&mut self, request: &BurnRequest) -> Result<u128, Error> {
        let verified = self.verifier().verify_burn(request)?;
        if let Plan::Apply(event) = self.plan_burn(&verified)? {
            self.apply(&event)?;
        }
        Ok(verified.value())
    }

    /// Verifies and carries out a signed mint request in memory. Resending
    /// the authority's last request returns the same note.
    pub fn authorized_mint(&mut self, request: &MintRequest) -> Result<Note, Error> {
        let verified = self.verifier().verify_mint(request)?;
        let mut notes = match self.plan_authorized_mint(&verified)? {
            Plan::Replay(notes) => notes,
            Plan::Apply(event) => {
                self.apply(&event)?;
                self.notes(&event)
            }
        };
        Ok(notes.remove(0))
    }

    /// Rotates in memory, with a fresh generation key. Refused while paused.
    ///
    /// # Panics
    ///
    /// If the system random number generator fails.
    pub fn rotate(&mut self) -> Result<RotationReport, Error> {
        let event = self.plan_rotate(GenerationKey::generate())?;
        self.apply(&event)?;
        match event {
            Event::Rotate(report) => Ok(report),
            _ => unreachable!("plan_rotate plans a rotation"),
        }
    }

    /// Pauses the issuer in memory, or resumes it if `paused` is false.
    pub fn set_paused(&mut self, paused: bool) {
        if let Some(event) = self.plan_pause(paused) {
            self.apply(&event).expect("a freshly planned event applies");
        }
    }

    /// Encodes the issuer's whole state except its secret keys and policy.
    ///
    /// Layout, big-endian: the magic `WORGLSNP`, a `u16` format version, the
    /// issuer public key, the event sequence number, the ledger (see
    /// [`Ledger::to_bytes`]), a byte that is 1 if serial indexes are permuted
    /// and 0 if they are the bits, the live value of each layer as `u128`, value
    /// minted, destroyed and burned as `u128`, the receipts (oldest first, as
    /// a `u32` count and for each the request's id, the generation and first
    /// bit its outputs took as `u64` and `u32`, and their count as `u8`, all
    /// three 0 for a burn), each mint authority's last request and note
    /// (sorted by authority key), the certificates of the live generations'
    /// keys (oldest first), a byte that is 1 if the issuer is paused, a byte
    /// that is 1 if a scheduled rotation was logged followed by the time it
    /// was due as `u64` (0 if none), the oldest generation, live or dropped,
    /// whose notes still verify as `u64`, the certificates of the dropped
    /// generations' keys from it on (oldest first), and finally the SHA-256
    /// of everything before it. When receipts were made is not part of it.
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
        w.u128(self.minted).u128(self.destroyed).u128(self.burned);
        w.u32(self.receipts.order.len() as u32);
        for id in &self.receipts.order {
            let receipt = &self.receipts.held[id];
            w.bytes(&id.to_bytes())
                .u64(receipt.generation)
                .u32(receipt.first)
                .u8(receipt.outputs);
        }
        w.u32(self.authorities.len() as u32);
        for (authorization, issued) in self.authorities.values() {
            w.bytes(authorization.authority.as_bytes())
                .u64(authorization.sequence)
                .bytes(&authorization.signature.to_bytes());
            write_issued(&mut w, issued);
        }
        w.u32(self.keys.certificates().len() as u32);
        for certificate in self.keys.certificates() {
            certificate.write(&mut w);
        }
        w.u8(u8::from(self.paused))
            .u8(u8::from(self.last_due.is_some()))
            .u64(self.last_due.unwrap_or(0))
            .u64(self.recent.oldest());
        let dropped = self.dropped_keys();
        w.u32(dropped.len() as u32);
        for certificate in dropped {
            certificate.write(&mut w);
        }
        let mut bytes = w.finish();
        let checksum: [u8; 32] = Sha256::digest(&bytes).into();
        bytes.extend_from_slice(&checksum);
        bytes
    }

    /// Rebuilds an issuer from a [`snapshot`](Self::snapshot), its key, the
    /// [`key_ring`](Self::key_ring) that holds its generation keys and the
    /// policy to enforce from now on.
    ///
    /// The ring may hold more keys than the snapshot names, such as those of
    /// rotations logged after it, but must hold the key of every live
    /// generation it names. Dropped generations need none: only their public
    /// keys are kept.
    pub fn restore(
        key: IssuerKey,
        ring: KeyRing,
        snapshot: &[u8],
        policy: Policy,
    ) -> Result<Self, SetupError> {
        if ring.issuer() != &key.public_key() {
            return Err(SetupError::KeyMismatch);
        }
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
            1 | 2 => false,
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
        let burned = if version >= 2 { r.u128()? } else { 0 };
        let mut receipts = Receipts::default();
        for _ in 0..r.u32()? {
            let id = TransferId::from_bytes(r.array()?);
            let receipt = if version >= 8 {
                let receipt = Receipt {
                    generation: r.u64()?,
                    first: r.u32()?,
                    outputs: r.u8()?,
                    at: 0,
                };
                Some(receipt).filter(|receipt| receipt.fits(&ledger))
            } else {
                Receipt::of_notes(&ledger, &read_list(&mut r, 52, read_issued)?)
            };
            let receipt = receipt.ok_or(DecodeError::Invalid("receipt"))?;
            if receipts.held.insert(id, receipt).is_some() {
                return Err(DecodeError::Invalid("duplicate receipt").into());
            }
            receipts.order.push_back(id);
        }
        let mut authorities = BTreeMap::new();
        if version >= 4 {
            let count = r.u32()?;
            for _ in 0..count {
                let authorization = MintAuthorization {
                    authority: read_key(&mut r)?,
                    sequence: r.u64()?,
                    signature: Signature::from_bytes(&r.array()?),
                };
                let issued = read_issued(&mut r)?;
                if authorities
                    .insert(authorization.authority, (authorization, issued))
                    .is_some()
                {
                    return Err(DecodeError::Invalid("duplicate mint authority").into());
                }
            }
        }
        let certificates = if version >= 5 {
            read_list(&mut r, 104, KeyCertificate::read)?
        } else {
            Vec::new()
        };
        let flag = |r: &mut Reader<'_>, what| match r.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError::Invalid(what)),
        };
        let (paused, last_due) = if version >= 6 {
            let paused = flag(&mut r, "paused flag")?;
            let scheduled = flag(&mut r, "schedule flag")?;
            let due = r.u64()?;
            (paused, scheduled.then_some(due))
        } else {
            (false, None)
        };
        let oldest = ledger.oldest_generation();
        let (since, dropped) = if version >= 7 {
            (r.u64()?, read_list(&mut r, 104, KeyCertificate::read)?)
        } else {
            (oldest, Vec::new())
        };
        r.finish()?;
        let public = key.public_key();
        let live = certificates.first().is_none_or(|c| c.generation >= oldest)
            && certificates
                .last()
                .is_none_or(|c| c.generation == ledger.newest_generation());
        if !live {
            return Err(DecodeError::Invalid("generation keys").into());
        }
        // At most one fewer dropped generation than there are layers, and
        // their keys, if any, lead straight on to the live generations'.
        let leads_on = dropped.last().is_none_or(|c| {
            c.generation.checked_add(1) == Some(oldest)
                && certificates.first().map(|c| c.generation) == Some(oldest)
        });
        if since > oldest || oldest - since > u64::from(ledger.last_position()) || !leads_on {
            return Err(DecodeError::Invalid("dropped generation keys").into());
        }
        // One set checks each certificate once; the live set is its part
        // from the oldest live generation on.
        let recent = KeySet::new(public, since, [dropped, certificates].concat())?;
        let keys = recent.rotated(oldest, None);
        if let Some(missing) = keys
            .certificates()
            .iter()
            .find(|c| ring.get(&c.key).is_none())
        {
            return Err(SetupError::MissingKey {
                generation: missing.generation,
            });
        }
        receipts.shrink_to(policy.receipt_capacity as usize);
        Ok(Self {
            public,
            key,
            keys,
            recent,
            ring,
            policy,
            ledger,
            sequence,
            value,
            minted,
            destroyed,
            burned,
            receipts,
            authorities,
            paused,
            last_due,
            clock: None,
        })
    }

    /// The serial the `k`-th next allocation in the layer at `position` would get.
    fn next_serial(&self, position: u32, k: u32) -> Option<Serial> {
        self.ledger.next_serial(position, k)
    }

    /// The certificates of the dropped generations whose public keys are
    /// kept, oldest first.
    fn dropped_keys(&self) -> &[KeyCertificate] {
        let certificates = self.recent.certificates();
        let live = self.keys.oldest();
        &certificates[..certificates.partition_point(|c| c.generation < live)]
    }

    /// The notes of the transfer `receipt` keeps, signed again: its outputs,
    /// in order, took the bits from the receipt's first on.
    fn replay(&self, receipt: &Receipt, outputs: &[Output]) -> Vec<Note> {
        let issued: Vec<Issued> = (receipt.first..)
            .zip(outputs)
            .map(|(bit, output)| Issued {
                serial: self.ledger.serial_of(receipt.generation, bit),
                amount: output.amount,
                holder: output.holder,
            })
            .collect();
        self.sign_all(&issued)
    }

    /// The time by the issuer's clock, or 0 without one.
    fn now(&self) -> u64 {
        self.clock.as_ref().map_or(0, Clock::now)
    }

    /// Signs notes of live generations, each with its generation's key.
    fn sign_all(&self, issued: &[Issued]) -> Vec<Note> {
        issued
            .iter()
            .map(|i| {
                let key = signing_key(&self.key, &self.keys, &self.ring, i.serial.generation)
                    .expect("every live generation has its key in the ring");
                Note::issue(self.public, key, i.serial, i.amount, i.holder)
            })
            .collect()
    }

    /// Mints a note [`check`](Self::check) accepted.
    fn apply_mint(&mut self, issued: &Issued) {
        self.allocate(std::slice::from_ref(issued));
        self.minted += u128::from(issued.amount);
    }

    fn check_authorized_mint(
        &self,
        authorization: &MintAuthorization,
        issued: &Issued,
    ) -> Result<(), ApplyError> {
        if let Some((last, _)) = self.authorities.get(&authorization.authority)
            && authorization.sequence <= last.sequence
        {
            return Err(ApplyError(format!(
                "mint sequence {} is not above {}",
                authorization.sequence, last.sequence
            )));
        }
        self.check_issued(std::slice::from_ref(issued))
    }

    /// Mints at an authority's request [`check`](Self::check) accepted.
    fn apply_authorized_mint(&mut self, authorization: &MintAuthorization, issued: &Issued) {
        self.apply_mint(issued);
        self.authorities
            .insert(authorization.authority, (*authorization, *issued));
    }

    fn check_transfer(&self, spent: &[Spent], issued: &[Issued]) -> Result<(), ApplyError> {
        if spent.is_empty() || issued.is_empty() {
            return Err(ApplyError(
                "a transfer spends and issues at least one note".into(),
            ));
        }
        // As many as a policy can allow, and a receipt can count.
        if u8::try_from(issued.len()).is_err() {
            return Err(ApplyError("a transfer issues at most 255 notes".into()));
        }
        self.check_spent(spent)?;
        let total_in: u128 = spent.iter().map(|s| u128::from(s.amount)).sum();
        let total_out: u128 = issued.iter().map(|i| u128::from(i.amount)).sum();
        if total_in != total_out {
            return Err(ApplyError(format!(
                "spends {total_in} but issues {total_out}"
            )));
        }
        self.check_issued(issued)
    }

    /// Carries out a transfer [`check`](Self::check) accepted.
    fn apply_transfer(&mut self, id: TransferId, spent: &[Spent], issued: &[Issued]) {
        let generation = issued[0].serial.generation;
        let receipt = Receipt {
            generation,
            // The outputs take the next bits, from the count handed out on.
            first: self
                .ledger
                .position(generation)
                .and_then(|position| self.ledger.layer(position))
                .expect("checked in the ledger")
                .issued,
            outputs: u8::try_from(issued.len()).expect("checked at most 255"),
            at: self.now(),
        };
        self.spend(spent);
        self.allocate(issued);
        let capacity = self.policy.receipt_capacity as usize;
        self.receipts.insert(capacity, id, receipt);
    }

    fn check_burn(&self, spent: &[Spent]) -> Result<(), ApplyError> {
        if spent.is_empty() {
            return Err(ApplyError("a burn spends at least one note".into()));
        }
        self.check_spent(spent)
    }

    /// Carries out a burn [`check`](Self::check) accepted.
    fn apply_burn(&mut self, id: TransferId, spent: &[Spent]) {
        self.spend(spent);
        self.burned += spent.iter().map(|s| u128::from(s.amount)).sum::<u128>();
        let capacity = self.policy.receipt_capacity as usize;
        let receipt = Receipt {
            at: self.now(),
            ..Receipt::BURN
        };
        self.receipts.insert(capacity, id, receipt);
    }

    /// Checks that every spent note is live, appears once, and that its
    /// layer holds at least the value spent from it.
    fn check_spent(&self, spent: &[Spent]) -> Result<(), ApplyError> {
        let mut drawn = Vec::with_capacity(spent.len());
        let mut serials = Vec::with_capacity(spent.len());
        for s in spent {
            let Status::Live { position } = self.ledger.status(s.serial) else {
                return Err(ApplyError(format!("note {} is not live", s.serial)));
            };
            drawn.push((position, s.amount));
            serials.push(s.serial);
        }
        serials.sort_unstable();
        if serials.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ApplyError("a note is spent twice".into()));
        }
        if let Some(position) = self.overdrawn(&drawn) {
            return Err(ApplyError(format!(
                "layer {position} holds less value than is spent"
            )));
        }
        Ok(())
    }

    /// The position of a layer that holds less live value than `drawn`
    /// takes from it, if any. Each of `drawn` is a live note's position and
    /// amount. Planning a spend and applying it both ask this, so they agree.
    fn overdrawn(&self, drawn: &[(u32, u64)]) -> Option<u32> {
        let mut totals: Vec<(u32, u128)> = Vec::new();
        for &(position, amount) in drawn {
            match totals.iter_mut().find(|(p, _)| *p == position) {
                Some((_, total)) => *total += u128::from(amount),
                None => totals.push((position, u128::from(amount))),
            }
        }
        totals
            .into_iter()
            .find(|&(position, total)| self.value[position as usize] < total)
            .map(|(position, _)| position)
    }

    /// Clears the bits of notes [`check_spent`](Self::check_spent) accepted.
    fn spend(&mut self, spent: &[Spent]) {
        for s in spent {
            let from = self
                .ledger
                .invalidate(s.serial)
                .expect("checked live above");
            self.value[from as usize] -= u128::from(s.amount);
        }
    }

    fn check_rotate(&self, report: &RotationReport) -> Result<(), ApplyError> {
        let expected = RotationReport {
            key: report.key,
            due: report.due,
            ..self.rotation()
        };
        if *report != expected {
            return Err(ApplyError(format!(
                "rotation {report:?} does not match the ledger"
            )));
        }
        if let (Some(due), Some(last)) = (report.due, self.last_due)
            && due <= last
        {
            return Err(ApplyError(format!(
                "a rotation due at {due} follows one due at {last}"
            )));
        }
        match &report.key {
            Some(key) => {
                if key.generation != report.created || !key.verify(&self.public) {
                    return Err(ApplyError(format!(
                        "generation {} has no valid key certificate",
                        report.created
                    )));
                }
                if self.ring.get(&key.key).is_none() {
                    return Err(ApplyError(format!(
                        "the key ring lacks the key for generation {}",
                        report.created
                    )));
                }
                // A key signs one generation only, so erasing it when that
                // generation is dropped never strands another.
                if key.key == self.public
                    || self.keys.certificates().iter().any(|c| c.key == key.key)
                {
                    return Err(ApplyError(format!(
                        "generation {}'s key already signs another generation",
                        report.created
                    )));
                }
            }
            None if self.keys.first_keyed().is_some() => {
                return Err(ApplyError(
                    "a rotation without a generation key follows one with a key".into(),
                ));
            }
            None => {}
        }
        Ok(())
    }

    /// Rotates as [`check`](Self::check) accepted.
    fn apply_rotate(&mut self, report: &RotationReport) {
        self.ledger.rotate();
        self.value.remove(0);
        self.value.push(0);
        self.destroyed += report.destroyed_value;
        let oldest = self.ledger.oldest_generation();
        self.receipts.prune(oldest);
        // Erase the dropped generation's key. Keys of rotations still to be
        // replayed stay, so this never touches a key no certificate names.
        for dropped in self
            .keys
            .certificates()
            .iter()
            .filter(|c| c.generation < oldest)
        {
            self.ring.remove(&dropped.key);
        }
        self.keys = self.keys.rotated(oldest, report.key);
        // A receipt lasts until its outputs' generation is dropped. Outputs
        // land at most `layers − 1` positions past an input, so an input's
        // generation is dropped at most that many rotations before them:
        // keep the public keys of that many dropped generations. A replica
        // restored from a snapshot from before they were kept holds fewer for
        // a while, so whether a rotation applies does not depend on them: a
        // new key that a kept one names ends the kept keys there instead, and
        // no key names two generations.
        let mut since = oldest
            .saturating_sub(u64::from(self.ledger.last_position()))
            .max(self.recent.oldest());
        if let Some(reused) = report
            .key
            .and_then(|key| self.recent.certificates().iter().find(|c| c.key == key.key))
        {
            since = since.max(reused.generation + 1);
        }
        self.recent = self.recent.rotated(since, report.key);
        if report.due.is_some() {
            self.last_due = report.due;
        }
    }

    /// Makes this an issuer from before generation keys, whose own key signs
    /// every generation.
    #[cfg(test)]
    pub(crate) fn forget_generation_keys(&mut self) {
        self.keys = KeySet::new(self.public, self.ledger.oldest_generation(), Vec::new())
            .expect("an empty set is valid");
        self.recent = self.keys.clone();
        self.ring = KeyRing::new(self.public);
    }

    /// Erases every secret in the ring that signs no live generation, such
    /// as one left by a rotation that was planned but never applied. Call it
    /// only once no logged rotation remains to be replayed.
    pub fn retire_keys(&mut self) {
        let live = &self.keys;
        self.ring
            .retain(|public| live.certificates().iter().any(|c| &c.key == public));
    }

    /// Checks that `issued` are exactly the next allocations of one layer.
    fn check_issued(&self, issued: &[Issued]) -> Result<(), ApplyError> {
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
        Ok(())
    }

    /// Sets the bits of notes [`check_issued`](Self::check_issued) accepted.
    fn allocate(&mut self, issued: &[Issued]) {
        let position = self
            .ledger
            .position(issued[0].serial.generation)
            .expect("checked in the ledger");
        for i in issued {
            let serial = self.ledger.allocate(position).expect("checked free");
            debug_assert_eq!(serial, i.serial);
            self.value[position as usize] += u128::from(i.amount);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::keys::{HolderKey, MintKey};
    use crate::transfer::Output;

    fn issuer(layers: u32, policy: Policy) -> Issuer {
        let config = LedgerConfig {
            layers,
            layer_size: 64,
        };
        Issuer::new(IssuerKey::generate(), config, policy).unwrap()
    }

    /// Where the receipts start in a snapshot: after the magic, the version,
    /// the issuer key, the sequence number, the ledger, the serial order
    /// byte, the layers' values and the three totals.
    fn receipts_at(i: &Issuer) -> usize {
        let layers = i.ledger.config().layers as usize;
        8 + 2 + 32 + 8 + i.ledger.to_bytes().len() + 1 + 16 * layers + 48
    }

    /// The receipts as snapshots before version 8 wrote them: each request's
    /// id and the notes it issued, none for a burn.
    fn notes_section(receipts: &[(TransferId, Vec<Note>)]) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(receipts.len() as u32);
        for (id, notes) in receipts {
            w.bytes(&id.to_bytes()).u32(notes.len() as u32);
            for note in notes {
                let issued = Issued {
                    serial: note.serial(),
                    amount: note.amount(),
                    holder: *note.holder(),
                };
                write_issued(&mut w, &issued);
            }
        }
        w.finish()
    }

    /// The snapshot as version 7 wrote it, without its checksum: version 8
    /// kept the notes of each receipt, which are `receipts`, the issuer's
    /// own, oldest first.
    fn version_7_body(i: &Issuer, receipts: &[(TransferId, Vec<Note>)]) -> Vec<u8> {
        let ids: Vec<TransferId> = receipts.iter().map(|(id, _)| *id).collect();
        assert!(i.receipts.order.iter().eq(&ids));
        let mut body = i.snapshot();
        body.truncate(body.len() - 32);
        let at = receipts_at(i);
        let end = at + 4 + 45 * receipts.len();
        assert_eq!(body[at..at + 4], (receipts.len() as u32).to_be_bytes());
        body.splice(at..end, notes_section(receipts));
        body[8..10].copy_from_slice(&7u16.to_be_bytes());
        body
    }

    /// The snapshot as version 6 wrote it, without its checksum: version 7
    /// added the oldest generation that still verifies and the dropped
    /// generations' keys.
    fn version_6_body(i: &Issuer, receipts: &[(TransferId, Vec<Note>)]) -> Vec<u8> {
        let mut body = version_7_body(i, receipts);
        let since = body.len() - 12 - 104 * i.dropped_keys().len();
        assert_eq!(body[since..since + 8], i.recent.oldest().to_be_bytes());
        body.truncate(since);
        body[8..10].copy_from_slice(&6u16.to_be_bytes());
        body
    }

    /// The snapshot as version 5 wrote it, without its checksum, of an issuer
    /// that holds no receipts: version 6 added the pause and schedule, which
    /// must be unset.
    fn version_5_body(i: &Issuer) -> Vec<u8> {
        let mut body = version_6_body(i, &[]);
        assert_eq!(body.split_off(body.len() - 10), [0; 10]);
        body[8..10].copy_from_slice(&5u16.to_be_bytes());
        body
    }

    /// `body` with its checksum.
    fn sealed(mut body: Vec<u8>) -> Vec<u8> {
        let checksum: [u8; 32] = Sha256::digest(&body).into();
        body.extend_from_slice(&checksum);
        body
    }

    /// Restores `body`, sealed with its checksum, under `i`'s key and key
    /// ring.
    fn restore_body(i: &Issuer, body: Vec<u8>, policy: Policy) -> Result<Issuer, SetupError> {
        Issuer::restore(i.key.clone(), i.key_ring().clone(), &sealed(body), policy)
    }

    fn without_generation_keys(i: &mut Issuer) {
        i.forget_generation_keys();
    }

    /// Makes this an issuer restored from a snapshot older than version 7,
    /// which keeps no dropped generation's key.
    fn without_dropped_keys(i: &mut Issuer) {
        i.recent = i.keys.clone();
    }

    /// A request that pays `note` on to a fresh key.
    fn payment(note: Note, key: &HolderKey) -> TransferRequest {
        let output = Output {
            amount: note.amount(),
            holder: HolderKey::generate().public_key(),
        };
        TransferRequest::single(note, key, output).unwrap()
    }

    /// A request that splits `note` into `parts` notes of equal amounts,
    /// each to a fresh key.
    fn split(note: Note, key: &HolderKey, parts: u64) -> TransferRequest {
        let outputs = (0..parts)
            .map(|_| Output {
                amount: note.amount() / parts,
                holder: HolderKey::generate().public_key(),
            })
            .collect();
        TransferRequest::sign(&[(note, key)], outputs).unwrap()
    }

    /// Keeps 4 receipts, each for at least 100 s.
    fn floor() -> Policy {
        Policy {
            receipt_capacity: 4,
            receipt_min_age_secs: 100,
            ..Policy::cash_like()
        }
    }

    /// Gives `i` a clock that reads `start` until the test moves it, with
    /// the receipts it holds `age` seconds old.
    fn fake_clock(i: &mut Issuer, start: u64, age: u64) -> Arc<AtomicU64> {
        let now = Arc::new(AtomicU64::new(start));
        let read = Arc::clone(&now);
        i.set_clock(Clock::new(move || read.load(Ordering::SeqCst)), age);
        now
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
        i.rotate().unwrap();
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
            i.rotate().unwrap();
        }
        assert_eq!(i.status(&idle_note), Ok(NoteStatus::Expired));
        assert!(matches!(i.status(&busy_note), Ok(NoteStatus::Live { .. })));
        assert_eq!(
            i.supply(),
            Supply {
                minted: 6,
                destroyed: 4,
                burned: 0,
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
        i.rotate().unwrap();
        i.rotate().unwrap();
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
        i.rotate().unwrap();
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
        i.rotate().unwrap();
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
        let strict = Issuer::restore(
            key,
            loose.key_ring().clone(),
            &loose.snapshot(),
            Policy::cash_like(),
        )
        .unwrap();
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
        let empty = Event::Burn {
            id: TransferId::from_bytes([1; 32]),
            spent: Vec::new(),
        };
        assert!(i.apply(&empty).is_err(), "a burn spends something");
        let unissued = Event::Burn {
            id: TransferId::from_bytes([2; 32]),
            spent: vec![Spent {
                serial: Serial::new(issued.serial.generation, issued.serial.index + 1),
                amount: 8,
            }],
        };
        assert!(i.apply(&unissued).is_err(), "only live notes burn");
        assert_eq!(i.snapshot(), before);

        let (mut i, authority) = minting_issuer();
        i.authorized_mint(&mint_request(&i, &authority, 5, 8))
            .unwrap();
        let before = i.snapshot();
        let Plan::Apply(stale) = i
            .plan_authorized_mint(
                &i.verifier()
                    .verify_mint(&mint_request(&i, &authority, 6, 8))
                    .unwrap(),
            )
            .unwrap()
        else {
            panic!("fresh mint")
        };
        let Event::AuthorizedMint {
            mut authorization,
            issued,
        } = stale
        else {
            unreachable!()
        };
        authorization.sequence = 5;
        let stale = Event::AuthorizedMint {
            authorization,
            issued,
        };
        assert!(i.apply(&stale).is_err(), "sequence numbers must rise");
        assert_eq!(i.snapshot(), before);
    }

    fn minting_issuer() -> (Issuer, MintKey) {
        let authority = MintKey::generate();
        let policy = Policy {
            mint_authorities: vec![authority.public_key()],
            ..Policy::cash_like()
        };
        (issuer(3, policy), authority)
    }

    fn mint_request(
        issuer: &Issuer,
        authority: &MintKey,
        sequence: u64,
        amount: u64,
    ) -> MintRequest {
        let holder = HolderKey::generate().public_key();
        MintRequest::sign(authority, issuer.public_key(), sequence, amount, holder)
    }

    #[test]
    fn authorized_mints_need_rising_sequence_numbers() {
        let (mut i, authority) = minting_issuer();
        assert_eq!(i.mint_sequence(&authority.public_key()), None);
        let first = mint_request(&i, &authority, 1, 8);
        let note = i.authorized_mint(&first).unwrap();
        assert_eq!(note.amount(), 8);
        assert_eq!(note.holder(), &first.holder);
        assert!(matches!(
            i.status(&note),
            Ok(NoteStatus::Live { position: 1, .. })
        ));
        assert_eq!(i.mint_sequence(&authority.public_key()), Some(1));

        // Resending the request returns the same note and mints nothing.
        assert_eq!(i.authorized_mint(&first), Ok(note));
        assert_eq!(i.supply().minted, 8);

        // A different request under a used number is refused.
        for sequence in [0, 1] {
            assert_eq!(
                i.authorized_mint(&mint_request(&i, &authority, sequence, 8)),
                Err(Error::StaleMint { sequence, last: 1 })
            );
        }
        // Numbers may skip, but never go back.
        i.authorized_mint(&mint_request(&i, &authority, 7, 4))
            .unwrap();
        assert_eq!(
            i.authorized_mint(&first),
            Err(Error::StaleMint {
                sequence: 1,
                last: 7
            })
        );
        assert_eq!(i.supply().minted, 12);
    }

    #[test]
    fn authorities_answer_to_the_current_policy_and_keep_their_numbers() {
        let (mut i, authority) = minting_issuer();
        let request = mint_request(&i, &authority, 3, 8);
        let note = i.authorized_mint(&request).unwrap();

        // Restored under a policy that drops the authority, it cannot mint.
        let policy = i.policy().clone();
        let snapshot = i.snapshot();
        let mut without = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &snapshot,
            Policy::cash_like(),
        )
        .unwrap();
        assert_eq!(
            without.authorized_mint(&mint_request(&without, &authority, 4, 8)),
            Err(Error::UnknownAuthority(authority.public_key()))
        );
        // A request verified under the old policy is checked again.
        let verified = i
            .verifier()
            .verify_mint(&mint_request(&i, &authority, 4, 8))
            .unwrap();
        assert_eq!(
            without.plan_authorized_mint(&verified),
            Err(Error::UnknownAuthority(authority.public_key()))
        );

        // Listed again, the authority's old requests stay spent.
        let mut again =
            Issuer::restore(i.key.clone(), i.key_ring().clone(), &snapshot, policy).unwrap();
        assert_eq!(again.mint_sequence(&authority.public_key()), Some(3));
        assert_eq!(again.authorized_mint(&request), Ok(note));
        assert!(matches!(
            again.authorized_mint(&mint_request(&again, &authority, 2, 8)),
            Err(Error::StaleMint { .. })
        ));
        assert_eq!(again.snapshot(), snapshot);
    }

    #[test]
    fn version_3_snapshots_restore_with_no_authorities() {
        let mut i = issuer(4, Policy::cash_like());
        without_generation_keys(&mut i);
        let key = HolderKey::generate();
        i.mint(8, key.public_key()).unwrap();
        // Version 3 ended with the receipts: drop the empty authority and
        // certificate lists.
        let mut body = version_5_body(&i);
        assert_eq!(body.split_off(body.len() - 8), [0; 8]);
        body[8..10].copy_from_slice(&3u16.to_be_bytes());
        let checksum: [u8; 32] = Sha256::digest(&body).into();
        body.extend_from_slice(&checksum);
        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &body,
            Policy::cash_like(),
        )
        .unwrap();
        assert_eq!(restored.snapshot(), i.snapshot());
    }

    #[test]
    fn burning_destroys_value_even_in_the_last_layer() {
        let mut i = issuer(4, Policy::cash_like());
        let alice = HolderKey::generate();
        let note = i.mint(8, alice.public_key()).unwrap();
        let (frozen, bob) = pay(&mut i, note, &alice).unwrap();
        assert!(matches!(
            i.status(&frozen),
            Ok(NoteStatus::Live { hops_left: 0, .. })
        ));

        let burn = BurnRequest::single(frozen, &bob).unwrap();
        assert_eq!(i.burn(&burn), Ok(8));
        assert_eq!(i.status(&frozen), Ok(NoteStatus::Spent));
        assert_eq!(
            i.supply(),
            Supply {
                minted: 8,
                destroyed: 0,
                burned: 8,
                outstanding: 0,
            }
        );
        assert_eq!(i.layers()[3].value, 0);

        // A resent burn answers the same, even after rotations, and changes nothing.
        i.rotate().unwrap();
        let sequence = i.sequence();
        assert_eq!(i.burn(&burn), Ok(8));
        assert_eq!(i.sequence(), sequence);

        // A burned note cannot be spent, and a spent one cannot be burned.
        let output = Output {
            amount: 8,
            holder: alice.public_key(),
        };
        let request = TransferRequest::single(frozen, &bob, output).unwrap();
        assert_eq!(i.transfer(&request), Err(Error::Spent(frozen.serial())));
        let carol = HolderKey::generate();
        let note = i.mint(4, carol.public_key()).unwrap();
        pay(&mut i, note, &carol).unwrap();
        let late = BurnRequest::single(note, &carol).unwrap();
        assert_eq!(i.burn(&late), Err(Error::Spent(note.serial())));
    }

    #[test]
    fn planning_refuses_to_take_more_from_a_layer_than_it_holds() {
        // Two issuers restored from one snapshot hand out the same serials
        // under the same keys, as a mint and a restored copy of it do.
        let original = issuer(4, Policy::flexible());
        let restore = || {
            Issuer::restore(
                original.key.clone(),
                original.key_ring().clone(),
                &original.snapshot(),
                Policy::flexible(),
            )
            .unwrap()
        };
        let (mut first, mut second) = (restore(), restore());
        let (alice, bob) = (HolderKey::generate(), HolderKey::generate());
        let alices = [32, 8].map(|amount| first.mint(amount, alice.public_key()).unwrap());
        let bobs = [4, 32].map(|amount| second.mint(amount, bob.public_key()).unwrap());
        assert_eq!(alices.map(|n| n.serial()), bobs.map(|n| n.serial()));
        let position = second.mint_position();
        assert_eq!(second.layers()[position as usize].value, 36);
        let overdrawn = Error::Overdrawn { position };

        // Each of Alice's notes is worth less than the layer holds, but not
        // both together.
        let both = [(alices[0], &alice), (alices[1], &alice)];
        let output = Output {
            amount: 40,
            holder: alice.public_key(),
        };
        let transfer = TransferRequest::sign(&both, vec![output]).unwrap();
        let burn = BurnRequest::sign(&both).unwrap();
        let before = second.snapshot();
        let verified = second.verifier().verify(&transfer).unwrap();
        assert_eq!(second.plan_transfer(&verified).unwrap_err(), overdrawn);
        let verified = second.verifier().verify_burn(&burn).unwrap();
        assert_eq!(second.plan_burn(&verified).unwrap_err(), overdrawn);
        assert_eq!(second.transfer(&transfer).unwrap_err(), overdrawn);
        assert_eq!(second.burn(&burn).unwrap_err(), overdrawn);
        assert_eq!(second.snapshot(), before);

        // Alice's 32 alone fits, so it spends and Bob's 4 on its serial is
        // refused as spent. That leaves the layer 4, less than Bob's own 32.
        pay(&mut second, alices[0], &alice).unwrap();
        assert_eq!(
            pay(&mut second, bobs[0], &bob).unwrap_err(),
            Error::Spent(bobs[0].serial())
        );
        assert_eq!(pay(&mut second, bobs[1], &bob).unwrap_err(), overdrawn);
        let burn = BurnRequest::single(bobs[1], &bob).unwrap();
        assert_eq!(second.burn(&burn).unwrap_err(), overdrawn);
    }

    #[test]
    fn version_1_snapshots_restore_with_nothing_burned() {
        // An issuer from before serials were permuted, as version 1 knew them.
        let mut i = issuer(4, Policy::cash_like());
        without_generation_keys(&mut i);
        i.ledger = Ledger::new(i.ledger.config()).unwrap();
        i.mint(8, HolderKey::generate().public_key()).unwrap();
        // With no receipts, authorities or certificates the body ends in the
        // burned value and three zero counts; version 1 had neither the
        // burned value nor the last two lists, and no serial order byte.
        let body = &version_5_body(&i)[..];
        let flag = 8 + 2 + 32 + 8 + i.ledger.to_bytes().len();
        let burned_at = body.len() - 12 - 16;
        let mut v1 = [
            &body[..flag],
            &body[flag + 1..burned_at],
            &body[burned_at + 16..body.len() - 8],
        ]
        .concat();
        v1[8..10].copy_from_slice(&1u16.to_be_bytes());
        let checksum: [u8; 32] = Sha256::digest(&v1).into();
        v1.extend_from_slice(&checksum);

        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &v1,
            Policy::cash_like(),
        )
        .unwrap();
        assert_eq!(restored.supply(), i.supply());
        assert_eq!(restored.snapshot(), i.snapshot());
    }

    #[test]
    fn snapshot_round_trips_and_detects_corruption() {
        let mut i = issuer(4, Policy::cash_like());
        let key = HolderKey::generate();
        let note = i.mint(8, key.public_key()).unwrap();
        pay(&mut i, note, &key).unwrap();
        i.rotate().unwrap();
        let snapshot = i.snapshot();
        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &snapshot,
            Policy::cash_like(),
        )
        .unwrap();
        assert_eq!(restored.snapshot(), snapshot);
        assert_eq!(restored.supply(), i.supply());

        let mut corrupt = snapshot.clone();
        corrupt[20] ^= 1;
        assert_eq!(
            Issuer::restore(
                i.key.clone(),
                i.key_ring().clone(),
                &corrupt,
                Policy::cash_like()
            )
            .unwrap_err(),
            SetupError::Checksum
        );
        assert_eq!(
            Issuer::restore(
                IssuerKey::generate(),
                i.key_ring().clone(),
                &snapshot,
                Policy::cash_like()
            )
            .unwrap_err(),
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
        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &i.snapshot(),
            Policy::cash_like(),
        )
        .unwrap();
        for note in &notes {
            assert!(matches!(restored.status(note), Ok(NoteStatus::Live { .. })));
        }
        assert_eq!(restored.next_serial(2, 0), i.next_serial(2, 0));
    }

    #[test]
    fn version_2_snapshots_restore_with_bits_as_indexes() {
        // An issuer from before serials were permuted.
        let mut i = issuer(3, Policy::cash_like());
        without_generation_keys(&mut i);
        i.ledger = Ledger::new(i.ledger.config()).unwrap();
        let key = HolderKey::generate();
        let first = i.mint(4, key.public_key()).unwrap();
        assert_eq!(first.serial(), Serial::new(1, 0));

        // Its snapshot as version 2 wrote it: no serial order byte and no
        // authority or certificate list.
        let flag = 8 + 2 + 32 + 8 + i.ledger.to_bytes().len();
        let mut body = version_5_body(&i);
        assert_eq!(body.split_off(body.len() - 8), [0; 8]);
        assert_eq!(body[flag], 0);
        body.remove(flag);
        body[8..10].copy_from_slice(&2u16.to_be_bytes());
        let checksum: [u8; 32] = Sha256::digest(&body).into();
        body.extend_from_slice(&checksum);

        let mut restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &body,
            Policy::cash_like(),
        )
        .unwrap();
        assert!(!restored.ledger().is_permuted());
        assert!(matches!(
            restored.status(&first),
            Ok(NoteStatus::Live { .. })
        ));
        let second = restored.mint(4, key.public_key()).unwrap();
        assert_eq!(second.serial(), Serial::new(1, 1));
        // Written again, it stays unpermuted.
        let again = Issuer::restore(
            i.key.clone(),
            restored.key_ring().clone(),
            &restored.snapshot(),
            Policy::cash_like(),
        );
        assert!(!again.unwrap().ledger().is_permuted());

        // Any other order byte is refused.
        let mut bad = i.snapshot();
        bad.truncate(bad.len() - 32);
        bad[flag] = 2;
        let checksum: [u8; 32] = Sha256::digest(&bad).into();
        bad.extend_from_slice(&checksum);
        assert!(
            Issuer::restore(
                i.key.clone(),
                i.key_ring().clone(),
                &bad,
                Policy::cash_like()
            )
            .is_err()
        );
    }

    #[test]
    fn each_generation_signs_with_its_own_key_until_it_is_dropped() {
        let mut i = issuer(3, Policy::cash_like());
        assert_eq!(i.key_ring().len(), 3);
        let alice = HolderKey::generate();
        let note = i.mint(8, alice.public_key()).unwrap();
        let generation = note.serial().generation;
        // The note's own generation key signed it, not the issuer key.
        assert_eq!(note.verify(i.keys()), Ok(()));
        let identity_only = KeySet::new(i.public, i.keys().oldest(), Vec::new()).unwrap();
        assert_eq!(note.verify(&identity_only), Err(NoteError::BadSignature));
        let signers: BTreeSet<PublicKey> = (0..3).map(|g| *i.keys().signer(g).unwrap()).collect();
        assert_eq!(signers.len(), 3);

        // A rotation certifies a fresh key for the new generation and erases
        // the dropped generation's key.
        let before = i.verifier();
        let dropped = *i.keys().signer(0).unwrap();
        let report = i.rotate().unwrap();
        let certificate = report.key.unwrap();
        assert_eq!(certificate.generation, report.created);
        assert!(certificate.verify(&i.public));
        assert_eq!(i.keys().signer(report.created), Some(&certificate.key));
        assert_eq!(i.key_ring().len(), 3);
        assert!(i.key_ring().get(&dropped).is_none());
        assert!(i.key_ring().get(&certificate.key).is_some());

        // Notes of the new generation need a verifier made after the rotation.
        let (moved, bob) = pay(&mut i, note, &alice).unwrap();
        let (moved, _) = pay(&mut i, moved, &bob).unwrap();
        assert_eq!(moved.serial().generation, report.created);
        assert_eq!(
            moved.verify(before.keys()),
            Err(NoteError::UnknownGeneration(report.created))
        );
        assert_eq!(moved.verify(i.verifier().keys()), Ok(()));

        // Once the note's generation is dropped, it is expired whoever signed
        // it. Its key's public half is kept for resent requests for as many
        // rotations as there are layers, less one, so meanwhile a note it
        // did not sign is refused as forged.
        i.rotate().unwrap();
        i.rotate().unwrap();
        assert!(generation < i.keys().oldest());
        let carol = HolderKey::generate();
        let note = Note::issue(i.public, &i.key, note.serial(), 8, carol.public_key());
        assert_eq!(i.status(&note), Ok(NoteStatus::Expired));
        let output = Output {
            amount: 8,
            holder: alice.public_key(),
        };
        let request = TransferRequest::single(note, &carol, output).unwrap();
        assert_eq!(
            i.transfer(&request),
            Err(Error::Note {
                input: 0,
                error: NoteError::BadSignature
            })
        );
        assert!(matches!(i.status(&moved), Ok(NoteStatus::Live { .. })));
        i.rotate().unwrap();
        assert_eq!(i.transfer(&request), Err(Error::Expired(note.serial())));
    }

    #[test]
    fn a_forged_or_unsaved_generation_key_does_not_rotate() {
        let mut i = issuer(3, Policy::cash_like());
        let before = i.snapshot();

        // Certified by another issuer key.
        let mut other = i.clone();
        other.key = IssuerKey::generate();
        let forged = other.plan_rotate(GenerationKey::generate()).unwrap();
        assert!(i.apply(&forged).is_err());

        // Certified, but its secret is in nobody's ring.
        let mut planner = i.clone();
        let Event::Rotate(report) = planner.plan_rotate(GenerationKey::generate()).unwrap() else {
            panic!("a rotation")
        };
        assert!(i.apply(&Event::Rotate(report)).is_err());

        // With a key that already signs a live generation.
        let mut reuse = i.clone();
        let again = reuse
            .key_ring()
            .get(i.keys().signer(0).unwrap())
            .unwrap()
            .clone();
        assert!(
            reuse
                .apply(&reuse.clone().plan_rotate(again.clone()).unwrap())
                .is_err()
        );
        let event = reuse.plan_rotate(again).unwrap();
        assert!(reuse.apply(&event).is_err());

        // Without a key, once generations have keys of their own.
        let bare = Event::Rotate(RotationReport {
            key: None,
            ..report
        });
        assert!(i.apply(&bare).is_err());
        assert_eq!(i.snapshot(), before);
    }

    #[test]
    fn sealed_keys_restore_a_snapshot_on_a_replica_without_its_ring() {
        let mut i = issuer(3, Policy::cash_like());
        let holder = HolderKey::generate();
        let note = i.mint(8, holder.public_key()).unwrap();
        i.rotate().unwrap();
        let kek = crate::kek::FileKek::from_bytes(&[9; 32]);
        let sealed = i.seal_keys(&kek).unwrap();
        let generations: Vec<_> = sealed.iter().map(|s| s.generation).collect();
        let live: Vec<_> = i
            .keys()
            .certificates()
            .iter()
            .map(|c| c.generation)
            .collect();
        assert_eq!(generations, live);

        let mut ring = KeyRing::new(i.public);
        for sealed in &sealed {
            ring.insert(sealed.unseal(&kek, &i.public).unwrap());
        }
        let restored =
            Issuer::restore(i.key.clone(), ring, &i.snapshot(), Policy::cash_like()).unwrap();
        assert_eq!(restored.status(&note).unwrap(), i.status(&note).unwrap());
        let next = restored.plan_mint(4, holder.public_key()).unwrap();
        assert_eq!(restored.notes(&next), i.notes(&next));
    }

    #[test]
    fn a_snapshot_restores_only_with_its_key_ring() {
        let mut i = issuer(3, Policy::cash_like());
        let key = HolderKey::generate();
        let note = i.mint(8, key.public_key()).unwrap();
        i.rotate().unwrap();
        let snapshot = i.snapshot();
        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &snapshot,
            Policy::cash_like(),
        )
        .unwrap();
        assert_eq!(restored.keys(), i.keys());
        assert_eq!(
            restored.notes(&Event::Mint(Issued {
                serial: note.serial(),
                amount: 8,
                holder: key.public_key(),
            })),
            vec![note]
        );

        let empty = KeyRing::new(i.public);
        assert_eq!(
            Issuer::restore(i.key.clone(), empty, &snapshot, Policy::cash_like()).unwrap_err(),
            SetupError::MissingKey {
                generation: i.keys().oldest()
            }
        );
        let foreign = issuer(3, Policy::cash_like()).key_ring().clone();
        assert_eq!(
            Issuer::restore(i.key.clone(), foreign, &snapshot, Policy::cash_like()).unwrap_err(),
            SetupError::KeyMismatch
        );

        // A certificate altered in the snapshot is caught even under a fixed
        // checksum, a live generation's or a dropped one's. After the last
        // live one come the pause and schedule (10 bytes), the oldest
        // generation that verifies and the count of dropped ones (12), and
        // the one dropped generation's certificate (104).
        assert_eq!(i.dropped_keys().len(), 1);
        let body = &snapshot[..snapshot.len() - 32];
        for at in [body.len() - 127, body.len() - 1] {
            let mut body = body.to_vec();
            body[at] ^= 1;
            let checksum: [u8; 32] = Sha256::digest(&body).into();
            body.extend_from_slice(&checksum);
            assert!(matches!(
                Issuer::restore(
                    i.key.clone(),
                    i.key_ring().clone(),
                    &body,
                    Policy::cash_like()
                ),
                Err(SetupError::Snapshot(_))
            ));
        }
    }

    #[test]
    fn issuers_from_before_generation_keys_start_using_them_at_the_next_rotation() {
        let mut i = issuer(3, Policy::cash_like());
        without_generation_keys(&mut i);
        let alice = HolderKey::generate();
        let old = i.mint(8, alice.public_key()).unwrap();
        assert_eq!(old.verify(i.keys()), Ok(()));

        // A log from before generation keys replays its rotations as they were.
        let legacy = Event::Rotate(i.rotation());
        assert_eq!(Event::from_bytes(&legacy.to_bytes()), Ok(legacy.clone()));
        let mut replayed = i.clone();
        replayed.apply(&legacy).unwrap();
        assert_eq!(replayed.keys().certificates(), &[]);

        // From the next rotation on, new generations have keys of their own.
        let report = i.rotate().unwrap();
        let created = report.key.unwrap();
        assert_eq!(i.keys().certificates(), &[created]);
        assert_eq!(i.key_ring().len(), 1);
        let (moved, bob) = pay(&mut i, old, &alice).unwrap();
        assert!(moved.serial().generation < created.generation);
        let (moved, _) = pay(&mut i, moved, &bob).unwrap();
        assert_eq!(moved.serial().generation, created.generation);
        assert_eq!(moved.verify(i.keys()), Ok(()));
        // The issuer key no longer signs for that generation, even if it leaks.
        let forged = Note::issue(
            i.public,
            &i.key,
            moved.serial(),
            1 << 20,
            HolderKey::generate().public_key(),
        );
        assert_eq!(forged.verify(i.keys()), Err(NoteError::BadSignature));
        assert!(matches!(i.status(&forged), Err(NoteError::BadSignature)));
        // Notes in the older generations still verify under it.
        let carol = HolderKey::generate();
        let minted = i.mint(4, carol.public_key()).unwrap();
        assert!(minted.serial().generation < created.generation);
        assert_eq!(minted.verify(i.keys()), Ok(()));

        // A rotation without a key is refused from now on.
        assert!(i.clone().apply(&Event::Rotate(i.rotation())).is_err());
        // And the state survives a snapshot.
        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &i.snapshot(),
            Policy::cash_like(),
        )
        .unwrap();
        assert_eq!(restored.snapshot(), i.snapshot());
    }

    #[test]
    fn a_resent_mint_whose_note_expired_says_so() {
        let (mut i, authority) = minting_issuer();
        let request = mint_request(&i, &authority, 1, 8);
        let note = i.authorized_mint(&request).unwrap();
        assert_eq!(i.authorized_mint(&request), Ok(note));
        for _ in 0..2 {
            i.rotate().unwrap();
        }
        assert_eq!(
            i.authorized_mint(&request),
            Err(Error::Expired(note.serial()))
        );
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
        let burn = BurnRequest::single(note, &key).unwrap();
        let Plan::Apply(burn) = i
            .plan_burn(&i.verifier().verify_burn(&burn).unwrap())
            .unwrap()
        else {
            panic!("fresh burn")
        };
        let authority = MintKey::generate();
        let authorized = Event::AuthorizedMint {
            authorization: MintAuthorization {
                authority: authority.public_key(),
                sequence: 9,
                signature: authority.sign(b"anything"),
            },
            issued: Issued {
                serial: Serial::new(2, 1),
                amount: 4,
                holder: key.public_key(),
            },
        };
        let legacy = Event::Rotate(i.rotation());
        let rotate = i.plan_rotate(GenerationKey::generate()).unwrap();
        let scheduled = i
            .plan_scheduled_rotation(GenerationKey::generate(), 3600)
            .unwrap();
        let scheduled_bare = Event::Rotate(RotationReport {
            due: Some(60),
            ..i.rotation()
        });
        for event in [
            mint,
            transfer,
            burn,
            rotate,
            legacy,
            authorized,
            Event::Pause,
            Event::Resume,
            scheduled,
            scheduled_bare.clone(),
        ] {
            assert_eq!(Event::from_bytes(&event.to_bytes()), Ok(event.clone()));
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), event);
        }
        assert!(Event::from_bytes(&[9]).is_err());
        assert!(Event::from_bytes(&[10]).is_err());
        // The key flag after a scheduled rotation's counts is 0 or 1.
        let mut bad = scheduled_bare.to_bytes();
        bad[1 + 8 + 8 + 4 + 16] = 2;
        assert!(Event::from_bytes(&bad).is_err());
    }

    #[test]
    fn a_paused_issuer_changes_nothing_until_it_resumes() {
        let mut i = issuer(4, Policy::cash_like());
        let alice = HolderKey::generate();
        let note = i.mint(8, alice.public_key()).unwrap();
        let output = Output {
            amount: 8,
            holder: HolderKey::generate().public_key(),
        };
        let request = TransferRequest::single(note, &alice, output).unwrap();
        let verified = i.verifier().verify(&request).unwrap();
        let burn = BurnRequest::single(note, &alice).unwrap();
        let burn = i.verifier().verify_burn(&burn).unwrap();
        // Planned while running, arriving after the pause.
        let Plan::Apply(transfer) = i.plan_transfer(&verified).unwrap() else {
            panic!("fresh transfer")
        };
        let mint = i.plan_mint(4, alice.public_key()).unwrap();

        let pause = i.plan_pause(true).unwrap();
        assert_eq!(pause, Event::Pause);
        i.apply(&pause).unwrap();
        assert!(i.is_paused());
        assert_eq!(i.plan_pause(true), None);
        let before = i.snapshot();
        assert!(i.apply(&Event::Pause).is_err());
        assert_eq!(i.plan_mint(4, alice.public_key()), Err(Error::Paused));
        assert!(matches!(i.plan_transfer(&verified), Err(Error::Paused)));
        assert!(matches!(i.plan_burn(&burn), Err(Error::Paused)));
        let ring = i.key_ring().len();
        assert_eq!(i.plan_rotate(GenerationKey::generate()), Err(Error::Paused));
        assert_eq!(i.rotate(), Err(Error::Paused));
        assert_eq!(i.key_ring().len(), ring);
        assert!(i.apply(&transfer).is_err());
        assert!(i.apply(&mint).is_err());
        assert!(i.apply(&Event::Rotate(i.rotation())).is_err());
        assert_eq!(i.snapshot(), before);

        // The pause is part of the state a snapshot keeps.
        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &before,
            Policy::cash_like(),
        )
        .unwrap();
        assert!(restored.is_paused());

        let resume = i.plan_pause(false).unwrap();
        assert_eq!(resume, Event::Resume);
        i.apply(&resume).unwrap();
        assert!(!i.is_paused());
        assert!(i.apply(&Event::Resume).is_err());
        i.apply(&transfer).unwrap();
        i.apply(&mint).unwrap();
        i.set_paused(true);
        i.set_paused(true);
        assert!(i.is_paused());
    }

    #[test]
    fn a_scheduled_rotation_must_be_due_after_the_last() {
        let mut i = issuer(4, Policy::cash_like());
        assert_eq!(i.last_scheduled_rotation(), None);
        let first = i
            .plan_scheduled_rotation(GenerationKey::generate(), 600)
            .unwrap();
        i.apply(&first).unwrap();
        assert_eq!(i.last_scheduled_rotation(), Some(600));

        // Refused when planned, saying why, and when applied.
        assert_eq!(
            i.plan_scheduled_rotation(GenerationKey::generate(), 600),
            Err(Error::StaleRotation {
                due: 600,
                last: 600
            })
        );
        let Event::Rotate(mut report) = i
            .plan_scheduled_rotation(GenerationKey::generate(), 1200)
            .unwrap()
        else {
            panic!("a rotation")
        };
        let before = i.snapshot();
        for due in [300, 600] {
            report.due = Some(due);
            assert!(i.apply(&Event::Rotate(report)).is_err());
        }
        assert_eq!(i.snapshot(), before);
        report.due = Some(1200);
        i.apply(&Event::Rotate(report)).unwrap();
        assert_eq!(i.last_scheduled_rotation(), Some(1200));

        // A rotation on request leaves the schedule alone.
        i.rotate().unwrap();
        assert_eq!(i.last_scheduled_rotation(), Some(1200));
        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &i.snapshot(),
            Policy::cash_like(),
        )
        .unwrap();
        assert_eq!(restored.last_scheduled_rotation(), Some(1200));
        assert_eq!(restored.snapshot(), i.snapshot());
    }

    #[test]
    fn version_5_snapshots_restore_running_with_no_schedule() {
        let mut i = issuer(4, Policy::cash_like());
        i.mint(8, HolderKey::generate().public_key()).unwrap();
        i.rotate().unwrap();
        without_dropped_keys(&mut i);
        let mut body = version_5_body(&i);
        let checksum: [u8; 32] = Sha256::digest(&body).into();
        body.extend_from_slice(&checksum);
        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &body,
            Policy::cash_like(),
        )
        .unwrap();
        assert!(!restored.is_paused());
        assert_eq!(restored.last_scheduled_rotation(), None);
        assert_eq!(restored.snapshot(), i.snapshot());
    }

    #[test]
    fn a_transfer_resent_after_its_input_was_dropped_gets_the_same_notes() {
        let mut i = issuer(4, Policy::cash_like());
        let alice = HolderKey::generate();
        let note = i.mint(8, alice.public_key()).unwrap();
        let request = payment(note, &alice);
        // Applied, but the answer is lost.
        let paid = i.transfer(&request).unwrap();
        while i.ledger().oldest_generation() <= note.serial().generation {
            i.rotate().unwrap();
        }
        assert!(matches!(
            i.status(&paid[0]),
            Ok(NoteStatus::Live { position: 0, .. })
        ));
        let sequence = i.sequence();
        assert_eq!(i.transfer(&request), Ok(paid));
        assert_eq!(i.sequence(), sequence, "a replay changes nothing");

        // Its receipt goes when the notes it issued expire.
        i.rotate().unwrap();
        assert_eq!(i.transfer(&request), Err(Error::Expired(note.serial())));
    }

    #[test]
    fn a_refresh_resent_across_the_rotation_gets_the_same_note() {
        let mut i = issuer(4, Policy::cash_like());
        let alice = HolderKey::generate();
        let note = i.mint(8, alice.public_key()).unwrap();
        i.rotate().unwrap();
        i.rotate().unwrap();
        assert!(matches!(
            i.status(&note),
            Ok(NoteStatus::Live { position: 0, .. })
        ));
        // Refreshed just before the rotation that drops the note, and resent
        // after it.
        let request = payment(note, &alice);
        let refreshed = i.transfer(&request).unwrap();
        i.rotate().unwrap();
        assert_eq!(i.status(&note), Ok(NoteStatus::Expired));
        assert_eq!(i.transfer(&request), Ok(refreshed));
    }

    #[test]
    fn a_merge_resent_after_its_oldest_input_was_dropped_gets_the_same_note() {
        let mut i = issuer(4, Policy::flexible());
        let key = HolderKey::generate();
        let old = i.mint(4, key.public_key()).unwrap();
        i.rotate().unwrap();
        i.rotate().unwrap();
        let young = i.mint(4, key.public_key()).unwrap();
        // The inputs sit as far apart as inputs can, and the output lands in
        // the last layer.
        assert!(matches!(
            i.status(&old),
            Ok(NoteStatus::Live { position: 0, .. })
        ));
        assert!(matches!(
            i.status(&young),
            Ok(NoteStatus::Live { position: 2, .. })
        ));
        let output = Output {
            amount: 8,
            holder: HolderKey::generate().public_key(),
        };
        let request = TransferRequest::sign(&[(old, &key), (young, &key)], vec![output]).unwrap();
        let merged = i.transfer(&request).unwrap();
        assert!(matches!(
            i.status(&merged[0]),
            Ok(NoteStatus::Live { position: 3, .. })
        ));

        // The merged note outlives the older input's generation by three
        // rotations, one fewer than there are layers, and so does that
        // generation's key.
        for _ in 0..3 {
            i.rotate().unwrap();
            assert_eq!(i.transfer(&request), Ok(merged.clone()));
        }
        assert_eq!(i.status(&old), Ok(NoteStatus::Expired));
        i.rotate().unwrap();
        assert_eq!(i.status(&merged[0]), Ok(NoteStatus::Expired));
        assert_eq!(i.transfer(&request), Err(Error::Expired(old.serial())));
    }

    #[test]
    fn a_dropped_note_is_expired_whether_or_not_its_key_is_kept() {
        let mut i = issuer(4, Policy::cash_like());
        let alice = HolderKey::generate();
        let note = i.mint(8, alice.public_key()).unwrap();
        let request = payment(note, &alice);
        let burn = BurnRequest::single(note, &alice).unwrap();
        while i.ledger().oldest_generation() <= note.serial().generation {
            i.rotate().unwrap();
        }

        // Its key is still kept, so the request verifies, but nothing was
        // applied for it and the ledger says the note expired.
        assert!(i.verifier().verify(&request).is_ok());
        assert_eq!(i.status(&note), Ok(NoteStatus::Expired));
        assert_eq!(i.transfer(&request), Err(Error::Expired(note.serial())));
        assert_eq!(i.burn(&burn), Err(Error::Expired(note.serial())));
        // The keys anyone is shown are still only the live generations'.
        assert_eq!(i.keys().oldest(), i.ledger().oldest_generation());
        assert_eq!(i.verifier().keys(), i.keys());

        // Past the keys kept, it is expired before any signature is checked.
        while i.recent.oldest() <= note.serial().generation {
            i.rotate().unwrap();
        }
        assert_eq!(
            i.verifier().verify(&request),
            Err(Error::Expired(note.serial()))
        );
        assert_eq!(i.transfer(&request), Err(Error::Expired(note.serial())));
        assert_eq!(i.burn(&burn), Err(Error::Expired(note.serial())));
        let forged = Note::issue(i.public, &i.key, note.serial(), 8, alice.public_key());
        let request = payment(forged, &alice);
        assert_eq!(i.transfer(&request), Err(Error::Expired(note.serial())));
    }

    #[test]
    fn a_burn_resent_after_its_input_was_dropped_burns_nothing_more() {
        let mut i = issuer(4, Policy::cash_like());
        let alice = HolderKey::generate();
        let note = i.mint(8, alice.public_key()).unwrap();
        let burn = BurnRequest::single(note, &alice).unwrap();
        assert_eq!(i.burn(&burn), Ok(8));

        // A burn issues nothing, so its receipt stays, and it is answered
        // while the note's generation is live or one of the last three
        // dropped.
        while i.recent.oldest() <= note.serial().generation {
            let sequence = i.sequence();
            assert_eq!(i.burn(&burn), Ok(8));
            assert_eq!(i.sequence(), sequence, "a replay changes nothing");
            assert_eq!(i.supply().burned, 8);
            i.rotate().unwrap();
        }
        assert_eq!(i.ledger().oldest_generation(), note.serial().generation + 4);
        assert_eq!(i.burn(&burn), Err(Error::Expired(note.serial())));
    }

    #[test]
    fn a_restored_issuer_keeps_the_dropped_generations_keys() {
        let mut i = issuer(4, Policy::cash_like());
        let alice = HolderKey::generate();
        let note = i.mint(8, alice.public_key()).unwrap();
        let request = payment(note, &alice);
        let paid = i.transfer(&request).unwrap();
        for _ in 0..3 {
            i.rotate().unwrap();
        }
        assert_eq!(i.dropped_keys().len(), 3);
        // Their secrets are erased all the same.
        assert_eq!(i.key_ring().len(), 4);
        assert!(
            i.dropped_keys()
                .iter()
                .all(|c| i.key_ring().get(&c.key).is_none())
        );

        let snapshot = i.snapshot();
        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &snapshot,
            Policy::cash_like(),
        )
        .unwrap();
        assert_eq!(restored.snapshot(), snapshot);
        assert_eq!(restored.dropped_keys(), i.dropped_keys());
        assert_eq!(restored.keys(), i.keys());
        // A verifier from the restored issuer lets the resend meet its receipt.
        let verified = restored.verifier().verify(&request).unwrap();
        assert_eq!(restored.plan_transfer(&verified), Ok(Plan::Replay(paid)));

        // The keys restored must lead on to the live ones, start no later
        // than the oldest live generation and no earlier than one fewer
        // generations than there are layers before it.
        let first = i.recent.certificates()[0];
        i.rotate().unwrap();
        let dropped = i.dropped_keys().to_vec();
        assert_eq!(dropped[0].generation, 1);
        let with = |since: u64, dropped: &[KeyCertificate]| {
            let mut body = i.snapshot();
            body.truncate(body.len() - 32 - 12 - 104 * 3);
            let mut w = Writer::default();
            w.u64(since).u32(dropped.len() as u32);
            for certificate in dropped {
                certificate.write(&mut w);
            }
            body.extend(w.finish());
            let checksum: [u8; 32] = Sha256::digest(&body).into();
            body.extend_from_slice(&checksum);
            Issuer::restore(
                i.key.clone(),
                i.key_ring().clone(),
                &body,
                Policy::cash_like(),
            )
        };
        assert_eq!(with(1, &dropped).unwrap().snapshot(), i.snapshot());
        assert!(with(2, &dropped[1..]).is_ok());
        assert!(with(4, &[]).is_ok());
        assert!(with(1, &dropped[..2]).is_err());
        assert!(with(2, &dropped).is_err());
        assert!(with(5, &[]).is_err());
        assert!(with(0, &[[first].as_slice(), dropped.as_slice()].concat()).is_err());
    }

    #[test]
    fn version_6_snapshots_restore_keeping_no_dropped_generations_key() {
        let mut i = issuer(4, Policy::cash_like());
        let alice = HolderKey::generate();
        let note = i.mint(8, alice.public_key()).unwrap();
        let request = payment(note, &alice);
        let paid = i.transfer(&request).unwrap();
        for _ in 0..3 {
            i.rotate().unwrap();
        }
        assert_eq!(i.dropped_keys().len(), 3);
        let mut body = version_6_body(&i, &[(request.id(&i.public), paid)]);
        let checksum: [u8; 32] = Sha256::digest(&body).into();
        body.extend_from_slice(&checksum);
        let mut restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &body,
            Policy::cash_like(),
        )
        .unwrap();
        assert_eq!(restored.dropped_keys(), &[]);
        assert_eq!(restored.recent, restored.keys);
        // So the resend is refused as it was before version 7.
        assert_eq!(
            restored.transfer(&request),
            Err(Error::Expired(note.serial()))
        );
        without_dropped_keys(&mut i);
        assert_eq!(restored.snapshot(), i.snapshot());

        // From the next rotation on it keeps the keys of what it drops.
        let next = restored.keys().certificates()[0];
        restored.rotate().unwrap();
        assert_eq!(restored.dropped_keys(), &[next]);
    }

    #[test]
    fn a_dropped_generation_from_before_generation_keys_verifies_under_the_issuer_key() {
        let mut i = issuer(4, Policy::cash_like());
        without_generation_keys(&mut i);
        let alice = HolderKey::generate();
        let note = i.mint(8, alice.public_key()).unwrap();
        let request = payment(note, &alice);
        let paid = i.transfer(&request).unwrap();
        // A rotation from before generation keys, then two with keys: the
        // note's generation is dropped, and the issuer key signed it.
        i.apply(&Event::Rotate(i.rotation())).unwrap();
        let first = i.rotate().unwrap().key.unwrap();
        i.rotate().unwrap();
        assert!(note.serial().generation < i.keys().oldest());
        assert!(note.serial().generation < first.generation);
        assert_eq!(i.dropped_keys(), &[]);
        assert_eq!(i.transfer(&request), Ok(paid.clone()));
        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &i.snapshot(),
            Policy::cash_like(),
        )
        .unwrap();
        assert_eq!(restored.snapshot(), i.snapshot());
        let verified = restored.verifier().verify(&request).unwrap();
        assert_eq!(restored.plan_transfer(&verified), Ok(Plan::Replay(paid)));

        // Once the first generation with a key of its own is dropped too, the
        // keys kept run from issuer-signed generations into certified ones.
        while i.ledger().oldest_generation() <= first.generation {
            i.rotate().unwrap();
        }
        assert_eq!(i.dropped_keys(), &[first]);
        assert!(i.recent.oldest() < first.generation);
        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &i.snapshot(),
            Policy::cash_like(),
        )
        .unwrap();
        assert_eq!(restored.snapshot(), i.snapshot());
        let issuer_signed = Serial::new(first.generation - 1, 0);
        let keyed = Serial::new(first.generation, 0);
        let holder = HolderKey::generate();
        for (serial, verifies) in [(issuer_signed, true), (keyed, false)] {
            let forged = Note::issue(i.public, &i.key, serial, 8, holder.public_key());
            let burn = BurnRequest::single(forged, &holder).unwrap();
            let verified = restored.verifier().verify_burn(&burn);
            assert_eq!(verified.is_ok(), verifies, "{serial}");
        }
    }

    #[test]
    fn a_rotation_reusing_a_dropped_generations_key_applies_on_every_replica() {
        let mut i = issuer(4, Policy::cash_like());
        let second = i
            .key_ring()
            .get(i.keys().signer(1).unwrap())
            .unwrap()
            .clone();
        for _ in 0..3 {
            i.rotate().unwrap();
        }
        assert_eq!(i.dropped_keys().len(), 3);
        assert_eq!(i.dropped_keys()[1].key, second.public_key());

        // Whether it applies depends on the live generations' keys alone,
        // which every replica shares, and not on the dropped ones kept: a
        // replica restored from a version 6 snapshot keeps none.
        let event = i.plan_rotate(second.clone()).unwrap();
        let mut body = version_6_body(&i, &[]);
        let checksum: [u8; 32] = Sha256::digest(&body).into();
        body.extend_from_slice(&checksum);
        let mut upgraded = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &body,
            Policy::cash_like(),
        )
        .unwrap();
        upgraded.apply(&event).unwrap();
        i.apply(&event).unwrap();
        assert_eq!(upgraded.keys(), i.keys());
        assert_eq!(i.keys().signer(7), Some(&second.public_key()));
        let kept: Vec<u64> = i.dropped_keys().iter().map(|c| c.generation).collect();
        assert_eq!(kept, [2, 3]);
        let restored = Issuer::restore(
            i.key.clone(),
            i.key_ring().clone(),
            &i.snapshot(),
            Policy::cash_like(),
        )
        .unwrap();
        assert_eq!(restored.snapshot(), i.snapshot());
    }

    #[test]
    fn a_resent_split_gets_the_same_notes_from_where_they_went() {
        for permuted in [true, false] {
            let mut i = issuer(4, Policy::cash_like());
            if !permuted {
                i.ledger = Ledger::new(i.ledger.config()).unwrap();
            }
            let key = HolderKey::generate();
            // The outputs share their layer with notes paid before and after.
            let before = i.mint(4, key.public_key()).unwrap();
            pay(&mut i, before, &key).unwrap();
            let note = i.mint(16, key.public_key()).unwrap();
            let request = split(note, &key, 4);
            let paid = i.transfer(&request).unwrap();
            let after = i.mint(4, key.public_key()).unwrap();
            pay(&mut i, after, &key).unwrap();
            let indexes: Vec<u32> = paid.iter().map(|n| n.serial().index).collect();
            assert_eq!(indexes == [1, 2, 3, 4], !permuted, "{indexes:?}");

            let sequence = i.sequence();
            assert_eq!(i.transfer(&request), Ok(paid.clone()));
            assert_eq!(i.sequence(), sequence, "a replay changes nothing");
            i.rotate().unwrap();
            assert_eq!(i.transfer(&request), Ok(paid.clone()));

            // The snapshot keeps where the outputs went, the second of three
            // receipts, and a restored issuer signs them again.
            let snapshot = i.snapshot();
            let at = receipts_at(&i) + 4 + 45;
            let mut receipt = Writer::default();
            receipt
                .bytes(&request.id(&i.public).to_bytes())
                .u64(paid[0].serial().generation)
                .u32(1)
                .u8(4);
            assert_eq!(snapshot[at..at + 45], receipt.finish());
            let restored = Issuer::restore(
                i.key.clone(),
                i.key_ring().clone(),
                &snapshot,
                Policy::cash_like(),
            )
            .unwrap();
            assert_eq!(restored.snapshot(), snapshot);
            let verified = restored.verifier().verify(&request).unwrap();
            assert_eq!(restored.plan_transfer(&verified), Ok(Plan::Replay(paid)));
        }
    }

    #[test]
    fn version_7_and_6_receipts_restore_from_their_notes() {
        let mut i = issuer(4, Policy::cash_like());
        let key = HolderKey::generate();
        let first = payment(i.mint(4, key.public_key()).unwrap(), &key);
        let first_paid = i.transfer(&first).unwrap();
        let note = i.mint(16, key.public_key()).unwrap();
        let request = split(note, &key, 4);
        let paid = i.transfer(&request).unwrap();
        let burned = i.mint(8, key.public_key()).unwrap();
        let burn = BurnRequest::single(burned, &key).unwrap();
        i.burn(&burn).unwrap();
        let receipts = vec![
            (first.id(&i.public), first_paid.clone()),
            (request.id(&i.public), paid.clone()),
            (burn.id(&i.public), Vec::new()),
        ];
        for body in [version_7_body(&i, &receipts), version_6_body(&i, &receipts)] {
            let mut restored = restore_body(&i, body, Policy::cash_like()).unwrap();
            assert_eq!(restored.snapshot(), i.snapshot());
            let sequence = restored.sequence();
            assert_eq!(restored.transfer(&first), Ok(first_paid.clone()));
            assert_eq!(restored.transfer(&request), Ok(paid.clone()));
            assert_eq!(restored.burn(&burn), Ok(8));
            assert_eq!(restored.sequence(), sequence, "a replay changes nothing");
        }

        // Notes that are not the bits handed out from the first one's on
        // are refused.
        let refused = |notes: Vec<Note>| {
            let mut receipts = receipts.clone();
            receipts[1].1 = notes;
            restore_body(&i, version_7_body(&i, &receipts), Policy::cash_like()).unwrap_err()
        };
        let invalid = SetupError::Snapshot(DecodeError::Invalid("receipt"));
        assert_eq!(refused(vec![paid[1], paid[0], paid[2], paid[3]]), invalid);
        let generation = paid[0].serial().generation;
        let holder = key.public_key();
        let unissued = i.ledger.serial_of(generation, 5);
        for serial in [
            unissued,
            Serial::new(generation, 64),
            Serial::new(generation + 1, 0),
        ] {
            let note = Note::issue(i.public, &i.key, serial, 1, holder);
            let mut notes = paid.clone();
            notes.push(note);
            assert_eq!(refused(notes), invalid, "{serial}");
            assert_eq!(refused(vec![note]), invalid, "{serial}");
        }
    }

    #[test]
    fn receipts_naming_bits_the_ledger_never_handed_out_are_refused() {
        let mut i = issuer(4, Policy::cash_like());
        let key = HolderKey::generate();
        let note = i.mint(16, key.public_key()).unwrap();
        let paid = i.transfer(&split(note, &key, 4)).unwrap();
        let burned = i.mint(8, key.public_key()).unwrap();
        i.burn(&BurnRequest::single(burned, &key).unwrap()).unwrap();
        let generation = paid[0].serial().generation;

        // The transfer's receipt, then the burn's: each an id, then the
        // generation, the first bit and the count of the outputs.
        let transfer = receipts_at(&i) + 4;
        let burn = transfer + 45;
        let snapshot = i.snapshot();
        let mut fields = Writer::default();
        fields.u64(generation).u32(0).u8(4);
        assert_eq!(snapshot[transfer + 32..transfer + 45], fields.finish());
        assert_eq!(snapshot[burn + 32..burn + 45], [0; 13]);
        let with = |at: usize, bytes: &[u8]| {
            let mut body = snapshot[..snapshot.len() - 32].to_vec();
            body[at..at + bytes.len()].copy_from_slice(bytes);
            restore_body(&i, body, Policy::cash_like())
        };
        assert!(with(transfer, &snapshot[transfer..transfer + 32]).is_ok());

        let invalid = SetupError::Snapshot(DecodeError::Invalid("receipt"));
        let edits: [(&str, usize, Vec<u8>); 8] = [
            (
                "a generation not in the ledger",
                transfer + 32,
                99u64.to_be_bytes().to_vec(),
            ),
            (
                "bits past those handed out",
                transfer + 40,
                1u32.to_be_bytes().to_vec(),
            ),
            ("more outputs than were issued", transfer + 44, vec![5]),
            (
                "a first bit that overflows",
                transfer + 40,
                u32::MAX.to_be_bytes().to_vec(),
            ),
            ("a transfer kept as a burn", transfer + 44, vec![0]),
            (
                "a burn with a generation",
                burn + 32,
                generation.to_be_bytes().to_vec(),
            ),
            (
                "a burn with a first bit",
                burn + 40,
                1u32.to_be_bytes().to_vec(),
            ),
            ("a burn with outputs", burn + 44, vec![1]),
        ];
        for (what, at, bytes) in edits {
            assert_eq!(with(at, &bytes).unwrap_err(), invalid, "{what}");
        }
        assert_eq!(
            with(burn, &snapshot[transfer..transfer + 32]).unwrap_err(),
            SetupError::Snapshot(DecodeError::Invalid("duplicate receipt"))
        );
    }

    #[test]
    fn a_full_table_of_young_receipts_holds_new_transfers_back() {
        let mut i = issuer(4, floor());
        let now = fake_clock(&mut i, 0, 0);
        let alice = HolderKey::generate();
        let bob = HolderKey::generate();
        let to = |holder: &HolderKey| Output {
            amount: 8,
            holder: holder.public_key(),
        };
        let notes: Vec<Note> = (0..6)
            .map(|_| i.mint(8, alice.public_key()).unwrap())
            .collect();
        let requests: Vec<TransferRequest> = notes
            .iter()
            .map(|note| TransferRequest::single(*note, &alice, to(&bob)).unwrap())
            .collect();
        // Receipts made at 0, 10, 20 and 30 s fill the table.
        let mut paid = Vec::new();
        for (t, request) in (0..).step_by(10).zip(&requests[..4]) {
            now.store(t, Ordering::SeqCst);
            paid.push(i.transfer(request).unwrap());
        }

        // At 40 s the oldest is 40 s old, so a fifth waits 60 s more, and
        // nothing is done meanwhile.
        now.store(40, Ordering::SeqCst);
        let sequence = i.sequence();
        let busy = |retry_after_secs| Err(Error::Busy { retry_after_secs });
        assert_eq!(i.transfer(&requests[4]), busy(60));
        assert!(matches!(i.status(&notes[4]), Ok(NoteStatus::Live { .. })));
        // A resend is answered, and a spent or frozen input refused as such.
        assert_eq!(i.transfer(&requests[0]), Ok(paid[0].clone()));
        let again = TransferRequest::single(notes[0], &alice, to(&alice)).unwrap();
        assert_eq!(i.transfer(&again), Err(Error::Spent(notes[0].serial())));
        let frozen = paid[1][0];
        let onward = TransferRequest::single(frozen, &bob, to(&alice)).unwrap();
        assert_eq!(i.transfer(&onward), Err(Error::Frozen(frozen.serial())));
        now.store(99, Ordering::SeqCst);
        assert_eq!(i.transfer(&requests[4]), busy(1));
        assert_eq!(i.sequence(), sequence);

        // At 100 s the oldest may go: the fifth applies, and the first's
        // resend finds no receipt.
        now.store(100, Ordering::SeqCst);
        assert!(i.transfer(&requests[4]).is_ok());
        assert_eq!(
            i.transfer(&requests[0]),
            Err(Error::Spent(notes[0].serial()))
        );
        assert_eq!(i.transfer(&requests[1]), Ok(paid[1].clone()));
        // The next waits for the receipt made at 10 s.
        assert_eq!(i.transfer(&requests[5]), busy(10));
    }

    #[test]
    fn a_full_table_of_young_receipts_holds_new_burns_back() {
        let policy = Policy {
            max_inputs: 2,
            ..floor()
        };
        let mut i = issuer(4, policy);
        let now = fake_clock(&mut i, 0, 0);
        let key = HolderKey::generate();
        let notes: Vec<Note> = (0..5)
            .map(|_| i.mint(8, key.public_key()).unwrap())
            .collect();
        let burns: Vec<BurnRequest> = notes
            .iter()
            .map(|note| BurnRequest::single(*note, &key).unwrap())
            .collect();
        for (t, burn) in (0..).step_by(10).zip(&burns[..4]) {
            now.store(t, Ordering::SeqCst);
            assert_eq!(i.burn(burn), Ok(8));
        }

        now.store(40, Ordering::SeqCst);
        let sequence = i.sequence();
        assert_eq!(
            i.burn(&burns[4]),
            Err(Error::Busy {
                retry_after_secs: 60
            })
        );
        assert_eq!(i.burn(&burns[0]), Ok(8));
        let with_spent = BurnRequest::sign(&[(notes[4], &key), (notes[0], &key)]).unwrap();
        assert_eq!(i.burn(&with_spent), Err(Error::Spent(notes[0].serial())));
        assert_eq!(i.sequence(), sequence);
        assert_eq!(i.supply().burned, 32);

        now.store(100, Ordering::SeqCst);
        assert_eq!(i.burn(&burns[4]), Ok(8));
        assert_eq!(i.burn(&burns[0]), Err(Error::Spent(notes[0].serial())));
        assert_eq!(i.burn(&burns[1]), Ok(8));
        assert_eq!(i.supply().burned, 40);
    }

    #[test]
    fn a_full_layer_is_refused_as_full_before_the_floor_is_checked() {
        let mut i = issuer(4, floor());
        fake_clock(&mut i, 0, 0);
        let key = HolderKey::generate();
        // Four splits into 16 fill the table and the last layer's 64 bits.
        for _ in 0..4 {
            let note = i.mint(16, key.public_key()).unwrap();
            i.transfer(&split(note, &key, 16)).unwrap();
        }
        let note = i.mint(16, key.public_key()).unwrap();
        assert_eq!(
            i.transfer(&payment(note, &key)),
            Err(Error::LayerFull { position: 3 })
        );
    }

    #[test]
    fn without_a_clock_or_a_floor_receipts_are_kept_by_count_alone() {
        let key = HolderKey::generate();
        let unfloored = Policy {
            receipt_min_age_secs: 0,
            ..floor()
        };
        for (clock, policy) in [(false, floor()), (true, unfloored)] {
            let mut i = issuer(4, policy);
            if clock {
                fake_clock(&mut i, 0, 0);
            }
            let notes: Vec<Note> = (0..5)
                .map(|_| i.mint(8, key.public_key()).unwrap())
                .collect();
            let requests: Vec<TransferRequest> =
                notes.iter().map(|note| payment(*note, &key)).collect();
            let paid: Vec<Vec<Note>> = requests.iter().map(|r| i.transfer(r).unwrap()).collect();
            assert_eq!(
                i.transfer(&requests[0]),
                Err(Error::Spent(notes[0].serial()))
            );
            assert_eq!(i.transfer(&requests[1]), Ok(paid[1].clone()));
        }

        // With no receipts kept, nothing waits for one to age.
        let mut i = issuer(
            4,
            Policy {
                receipt_capacity: 0,
                ..floor()
            },
        );
        fake_clock(&mut i, 0, 0);
        for _ in 0..5 {
            let note = i.mint(8, key.public_key()).unwrap();
            let request = payment(note, &key);
            i.transfer(&request).unwrap();
            assert_eq!(i.transfer(&request), Err(Error::Spent(note.serial())));
        }
    }

    #[test]
    fn a_restored_issuer_counts_its_receipts_as_old_as_it_is_told() {
        let mut i = issuer(4, floor());
        let key = HolderKey::generate();
        let notes: Vec<Note> = (0..5)
            .map(|_| i.mint(8, key.public_key()).unwrap())
            .collect();
        for note in &notes[..4] {
            i.transfer(&payment(*note, &key)).unwrap();
        }
        let fifth = payment(notes[4], &key);
        let snapshot = i.snapshot();
        for (age, wait) in [(0, 100), (30, 70), (100, 0), (1000, 0)] {
            let mut restored =
                Issuer::restore(i.key.clone(), i.key_ring().clone(), &snapshot, floor()).unwrap();
            let now = fake_clock(&mut restored, 1000, age);
            let verified = restored.verifier().verify(&fifth).unwrap();
            let planned = restored.plan_transfer(&verified);
            if wait > 0 {
                let busy = Error::Busy {
                    retry_after_secs: wait,
                };
                assert_eq!(planned, Err(busy), "{age}");
            } else {
                assert!(matches!(planned, Ok(Plan::Apply(_))), "{age}");
            }
            now.store(1000 + u64::from(wait), Ordering::SeqCst);
            assert!(matches!(
                restored.plan_transfer(&verified),
                Ok(Plan::Apply(_))
            ));
            // When the receipts were made stays out of the snapshot.
            assert_eq!(restored.snapshot(), snapshot);
        }
    }
}
