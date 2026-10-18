//! The issuer: its key, its ledger, its policy and the rules that tie them.
//!
//! Every state change is an [`Event`]. The issuer *plans* an event from a
//! request without changing anything, the caller makes the event durable if
//! it wants to (see [`store`](crate::store)), and then the issuer *applies*
//! it. Applying is deterministic and checks the event against the state, so
//! replaying a log of events onto the snapshot it started from rebuilds
//! the exact same issuer. [`Issuer::mint`], [`Issuer::transfer`],
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

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::codec::{DecodeError, Reader, Writer};
use crate::error::{ApplyError, Error, SetupError};
use crate::keys::{GenerationKey, IssuerKey, PublicKey, Signature};
use crate::keyset::{KeyCertificate, KeyRing, KeySet, signing_key};
use crate::ledger::{Ledger, LedgerConfig, Serial, Status};
use crate::mint::{MintAuthorization, MintRequest, VerifiedMint};
use crate::note::{Note, NoteError, read_key};
use crate::permutation::Permutation;
use crate::policy::Policy;
use crate::transfer::{
    BurnRequest, TransferId, TransferRequest, VerifiedBurn, VerifiedTransfer, Verifier,
};

const SNAPSHOT_MAGIC: &[u8; 8] = b"WORGLSNP";
/// Version 2 added the burned value, version 3 records whether serial
/// indexes are permuted, version 4 keeps each mint authority's last mint,
/// version 5 the certificates of the generation keys and version 6 whether
/// the issuer is paused and when its last scheduled rotation was due. Older
/// snapshots still restore, with nothing burned, with bits as indexes, with
/// no mint authorities, with every generation signed by the issuer key,
/// running and with no rotation scheduled, as they always had.
const SNAPSHOT_VERSION: u16 = 6;

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

    /// Forgets transfers whose notes have expired. Burns issued nothing, so
    /// they stay until newer requests push them out.
    fn prune(&mut self, oldest_generation: u64) {
        let issued = &mut self.issued;
        self.order.retain(|id| {
            let alive = issued[id]
                .first()
                .is_none_or(|i| i.serial.generation >= oldest_generation);
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
    /// Public keys of the live generations.
    keys: KeySet,
    /// Their secrets, and any key a planned rotation is about to certify.
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

    /// Whether the issuer is paused. See the [module documentation](self).
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// When the last scheduled rotation was due, in seconds since the Unix
    /// epoch, if one was ever logged.
    pub fn last_scheduled_rotation(&self) -> Option<u64> {
        self.last_due
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
        Verifier::new(self.keys.clone(), self.policy.clone())
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

    /// Checks a note's signature and looks up its state. O(1) after the
    /// signature check. A note of a dropped generation is expired, and its
    /// signature is no longer checked, because its key has been erased.
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
    /// live and outside the last layer, and the layer one past the furthest
    /// input must have room for every output.
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
        Ok(Plan::Apply(Event::Transfer {
            id: transfer.id,
            spent: spent(&transfer.inputs),
            issued,
        }))
    }

    /// Plans a verified burn against the ledger: every input must be live.
    /// Notes frozen in the last layer can be burned, since nothing moves.
    pub fn plan_burn(&self, burn: &VerifiedBurn) -> Result<Plan, Error> {
        self.check_running()?;
        if burn.issuer != self.public {
            return Err(Error::Note {
                input: 0,
                error: NoteError::WrongIssuer,
            });
        }
        self.policy.check_burn(burn.inputs.len())?;
        if self.receipts.issued.contains_key(&burn.id) {
            return Ok(Plan::Replay(Vec::new()));
        }
        for note in &burn.inputs {
            let serial = note.serial();
            match self.ledger.status(serial) {
                Status::Live { .. } => {}
                Status::Spent => return Err(Error::Spent(serial)),
                Status::Expired => return Err(Error::Expired(serial)),
                Status::Unissued => return Err(Error::Unissued(serial)),
            }
        }
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
        (paused != self.paused).then_some(if paused { Event::Pause } else { Event::Resume })
    }

    fn check_running(&self) -> Result<(), Error> {
        if self.paused {
            return Err(Error::Paused);
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

    /// Applies an event, or changes nothing and explains why it does not fit.
    pub fn apply(&mut self, event: &Event) -> Result<(), ApplyError> {
        match event {
            Event::Pause | Event::Resume => {
                let paused = matches!(event, Event::Pause);
                if paused == self.paused {
                    return Err(ApplyError(format!(
                        "the issuer is already {}",
                        if paused { "paused" } else { "running" }
                    )));
                }
                self.paused = paused;
            }
            _ if self.paused => return Err(ApplyError("the issuer is paused".into())),
            Event::Mint(issued) => self.apply_mint(issued)?,
            Event::Transfer { id, spent, issued } => self.apply_transfer(*id, spent, issued)?,
            Event::Rotate(report) => self.apply_rotate(report)?,
            Event::Burn { id, spent } => self.apply_burn(*id, spent)?,
            Event::AuthorizedMint {
                authorization,
                issued,
            } => self.apply_authorized_mint(authorization, issued)?,
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

    /// Verifies and carries out a burn in memory, returning the value
    /// destroyed. A resent burn that was already applied returns the same
    /// value and changes nothing.
    pub fn burn(&mut self, request: &BurnRequest) -> Result<u128, Error> {
        let verified = self.verifier().verify_burn(request)?;
        if let Plan::Apply(event) = self.plan_burn(&verified)? {
            self.apply(&event).expect("a freshly planned event applies");
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
                self.apply(&event).expect("a freshly planned event applies");
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
        self.apply(&event).expect("a freshly planned event applies");
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
    /// minted, destroyed and burned as `u128`, the remembered receipts, each
    /// mint authority's last request and note (sorted by authority key), the
    /// certificates of the live generations' keys (oldest first), a byte that
    /// is 1 if the issuer is paused, a byte that is 1 if a scheduled rotation
    /// was logged followed by the time it was due as `u64` (0 if none), and
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
        w.u128(self.minted).u128(self.destroyed).u128(self.burned);
        w.u32(self.receipts.order.len() as u32);
        for id in &self.receipts.order {
            let issued = &self.receipts.issued[id];
            w.bytes(&id.to_bytes()).u32(issued.len() as u32);
            for i in issued {
                write_issued(&mut w, i);
            }
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
            .u64(self.last_due.unwrap_or(0));
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
    /// rotations logged after it, but must hold every key it names.
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
            let issued = read_list(&mut r, 52, read_issued)?;
            if receipts.issued.insert(id, issued).is_some() {
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
        r.finish()?;
        let public = key.public_key();
        let keys = KeySet::new(public, ledger.oldest_generation(), certificates)?;
        if keys
            .certificates()
            .last()
            .is_some_and(|c| c.generation != ledger.newest_generation())
        {
            return Err(DecodeError::Invalid("generation keys").into());
        }
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
        })
    }

    /// The serial the `k`-th next allocation in the layer at `position` would get.
    fn next_serial(&self, position: u32, k: u32) -> Option<Serial> {
        self.ledger.next_serial(position, k)
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

    fn apply_mint(&mut self, issued: &Issued) -> Result<(), ApplyError> {
        let position = self.check_issued(std::slice::from_ref(issued))?;
        self.allocate(position, issued);
        self.minted += u128::from(issued.amount);
        Ok(())
    }

    fn apply_authorized_mint(
        &mut self,
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
        self.apply_mint(issued)?;
        self.authorities
            .insert(authorization.authority, (*authorization, *issued));
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
        self.check_spent(spent)?;
        let total_in: u128 = spent.iter().map(|s| u128::from(s.amount)).sum();
        let total_out: u128 = issued.iter().map(|i| u128::from(i.amount)).sum();
        if total_in != total_out {
            return Err(ApplyError(format!(
                "spends {total_in} but issues {total_out}"
            )));
        }
        let position = self.check_issued(issued)?;

        self.spend(spent);
        for i in issued {
            self.allocate(position, i);
        }
        let capacity = self.policy.receipt_capacity as usize;
        self.receipts.insert(capacity, id, issued.to_vec());
        Ok(())
    }

    fn apply_burn(&mut self, id: TransferId, spent: &[Spent]) -> Result<(), ApplyError> {
        if spent.is_empty() {
            return Err(ApplyError("a burn spends at least one note".into()));
        }
        self.check_spent(spent)?;
        self.spend(spent);
        self.burned += spent.iter().map(|s| u128::from(s.amount)).sum::<u128>();
        let capacity = self.policy.receipt_capacity as usize;
        self.receipts.insert(capacity, id, Vec::new());
        Ok(())
    }

    /// Checks that every spent note is live, appears once, and that its
    /// layer holds at least the value spent from it.
    fn check_spent(&self, spent: &[Spent]) -> Result<(), ApplyError> {
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
        Ok(())
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

    fn apply_rotate(&mut self, report: &RotationReport) -> Result<(), ApplyError> {
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
        if report.due.is_some() {
            self.last_due = report.due;
        }
        Ok(())
    }

    /// Makes this an issuer from before generation keys, whose own key signs
    /// every generation.
    #[cfg(test)]
    pub(crate) fn forget_generation_keys(&mut self) {
        self.keys = KeySet::new(self.public, self.ledger.oldest_generation(), Vec::new())
            .expect("an empty set is valid");
        self.ring = KeyRing::new(self.public);
    }

    /// Erases every secret in the ring that signs no live generation, such
    /// as one left by a rotation that was planned but never applied. Call it
    /// only once no logged rotation remains to be replayed.
    pub(crate) fn retire_keys(&mut self) {
        let live = &self.keys;
        self.ring
            .retain(|public| live.certificates().iter().any(|c| &c.key == public));
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
    use std::collections::BTreeSet;

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

    /// The snapshot as version 5 wrote it, without its checksum: version 6
    /// added the pause and schedule, which must be unset.
    fn version_5_body(i: &Issuer) -> Vec<u8> {
        let mut body = i.snapshot();
        body.truncate(body.len() - 32);
        assert_eq!(body.split_off(body.len() - 10), [0; 10]);
        body[8..10].copy_from_slice(&5u16.to_be_bytes());
        body
    }

    fn without_generation_keys(i: &mut Issuer) {
        i.forget_generation_keys();
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

        // Once the note's generation is dropped, it is expired whoever signed it.
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
        assert_eq!(i.transfer(&request), Err(Error::Expired(note.serial())));
        assert!(matches!(i.status(&moved), Ok(NoteStatus::Live { .. })));
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
        // checksum. The pause and schedule (10 bytes) follow the last one.
        let mut body = snapshot[..snapshot.len() - 32].to_vec();
        let last = body.len() - 11;
        body[last] ^= 1;
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
}
