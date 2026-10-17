//! Durable storage: a snapshot plus an append-only journal of events.
//!
//! A store is a directory holding
//!
//! - `snapshot.bin`, the issuer's state as of some event (see [`Issuer::snapshot`]),
//! - `journal.bin`, every event applied since then, and
//! - `lock`, held for as long as a [`FileStore`] is open so that two
//!   processes never write the same store.
//!
//! [`PersistentIssuer`] writes each event to the journal and flushes it to
//! disk *before* applying it, so a note is never handed out unless the event
//! that created it would survive a crash. [`compact`](PersistentIssuer::compact)
//! writes a fresh snapshot and empties the journal; call it after rotations
//! or whenever [`journal_len`](PersistentIssuer::journal_len) grows large.
//!
//! A flush costs far more than the work around it, so a busy issuer can share
//! one among many requests with a [`Batch`]: each change takes effect in
//! memory as it is made, and all of them reach the disk in one record and one
//! flush when the batch commits. Nothing a batch returns may be handed out
//! before then. If the flush fails, the issuer is ahead of its journal and
//! refuses all further changes until it is reopened, which restores exactly
//! what reached the disk.
//!
//! The journal starts with a header naming the issuer's public key, so a
//! journal can never be replayed onto another issuer's snapshot. Each record
//! is `len: u32 ‖ first sequence: u64 ‖ count: u32 ‖ (event length: u32 ‖
//! event) × count ‖ checksum`, where `len` counts everything between itself
//! and the checksum, and `checksum` is the first 8 bytes of the SHA-256 of
//! everything before it in the record, length included. Version 1 journals,
//! with one event per record, still open and are rewritten in this format.
//!
//! Records are flushed one at a time, so a crash can only damage the end of
//! the journal. When opening, an unreadable record with no readable record
//! anywhere after it is taken to be such a torn write and dropped; it was
//! never acknowledged. An unreadable record followed by readable ones is
//! corruption, and opening fails rather than lose acknowledged events.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::error::{ApplyError, Error, SetupError};
use crate::issuer::{Event, Issuer, Plan, RotationReport};
use crate::keys::{IssuerKey, PublicKey};
use crate::note::Note;
use crate::policy::Policy;
use crate::transfer::{VerifiedBurn, VerifiedTransfer};

const SNAPSHOT: &str = "snapshot.bin";
const SNAPSHOT_TMP: &str = "snapshot.bin.tmp";
const JOURNAL: &str = "journal.bin";
const JOURNAL_TMP: &str = "journal.bin.tmp";
const LOCK: &str = "lock";
const JOURNAL_MAGIC: &[u8; 8] = b"WORGLJNL";
/// Version 2 groups events into records; version 1 held one event per record.
const JOURNAL_VERSION: u16 = 2;
/// Magic, version and issuer public key.
const HEADER_LEN: u64 = 8 + 2 + 32;
/// Largest payload a record may claim: everything between the length and
/// the checksum.
const MAX_PAYLOAD: usize = 1 << 24;
/// Largest encoded size of the events in one [`Batch`]. The largest event the
/// format allows, with 255 inputs and outputs, is under 19 KB.
pub const MAX_BATCH_BYTES: usize = MAX_PAYLOAD - 12;

/// A store could not be created, opened or replayed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// A file operation failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Another process has the store open.
    #[error("the store is in use by another process")]
    Locked,
    /// [`FileStore::create`] found an existing store.
    #[error("{0} already holds a store")]
    Exists(PathBuf),
    /// The snapshot cannot be restored.
    #[error(transparent)]
    Setup(#[from] SetupError),
    /// The journal is damaged before its last record.
    #[error("journal is corrupt at byte {offset}: {reason}")]
    Corrupt {
        /// Where the damage starts.
        offset: u64,
        /// What is wrong.
        reason: String,
    },
    /// A journaled event does not fit the state it is replayed onto.
    #[error("journal record {sequence} does not replay: {error}")]
    Replay {
        /// The record's sequence number.
        sequence: u64,
        /// Why it does not apply.
        error: ApplyError,
    },
}

/// A snapshot and journal in one directory. See the [module documentation](self).
#[derive(Debug)]
pub struct FileStore {
    dir: PathBuf,
    journal: File,
    /// Bytes of the header and intact records in the journal.
    journal_bytes: u64,
    journal_len: u64,
    /// Set when a failed append could not be rolled back.
    poisoned: bool,
    _lock: File,
}

impl FileStore {
    /// Creates a store in `dir`, creating the directory if needed, holding
    /// `issuer`'s current state. Refuses if a snapshot or journal is already there.
    pub fn create(dir: impl AsRef<Path>, issuer: &Issuer) -> Result<Self, StoreError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let lock = lock(&dir)?;
        if dir.join(SNAPSHOT).exists() || dir.join(JOURNAL).exists() {
            return Err(StoreError::Exists(dir));
        }
        // The journal comes first: a directory with a snapshot is a complete store.
        let mut journal = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(dir.join(JOURNAL))?;
        journal.write_all(&journal_header(issuer.public_key()))?;
        journal.sync_all()?;
        sync_dir(&dir)?;
        write_atomically(&dir, SNAPSHOT, SNAPSHOT_TMP, &issuer.snapshot())?;
        Ok(Self {
            dir,
            journal,
            journal_bytes: HEADER_LEN,
            journal_len: 0,
            poisoned: false,
            _lock: lock,
        })
    }

    /// Opens a store, restores the issuer from its snapshot and replays the
    /// journal onto it.
    pub fn open(
        dir: impl AsRef<Path>,
        key: IssuerKey,
        policy: Policy,
    ) -> Result<(Self, Issuer), StoreError> {
        let dir = dir.as_ref().to_path_buf();
        let lock = lock(&dir)?;
        let snapshot = fs::read(dir.join(SNAPSHOT))?;
        let mut issuer = Issuer::restore(key, &snapshot, policy)?;

        let mut journal = OpenOptions::new()
            .read(true)
            .append(true)
            .open(dir.join(JOURNAL))?;
        let mut bytes = Vec::new();
        journal.read_to_end(&mut bytes)?;
        let (version, records, mut intact) = parse_journal(&bytes, issuer.public_key())?;
        let mut journal_len = 0;
        for (sequence, event) in records {
            // Records at or before the snapshot, ahead of the first one that
            // applies, are left over from a compaction that stopped before
            // emptying the journal.
            if journal_len == 0 && sequence <= issuer.sequence() {
                continue;
            }
            if sequence != issuer.sequence() + 1 {
                return Err(StoreError::Replay {
                    sequence,
                    error: ApplyError(format!("expected record {}", issuer.sequence() + 1)),
                });
            }
            issuer
                .apply(&event)
                .map_err(|error| StoreError::Replay { sequence, error })?;
            journal_len += 1;
        }
        if version < JOURNAL_VERSION {
            // Rewrite in the current format. The snapshot goes first, so the
            // old records are never needed again even if this stops halfway.
            write_atomically(&dir, SNAPSHOT, SNAPSHOT_TMP, &issuer.snapshot())?;
            let header = journal_header(issuer.public_key());
            write_atomically(&dir, JOURNAL, JOURNAL_TMP, &header)?;
            journal = OpenOptions::new()
                .read(true)
                .append(true)
                .open(dir.join(JOURNAL))?;
            intact = header.len();
            journal_len = 0;
        } else if intact < bytes.len() {
            // Drop the torn tail so new records follow the last intact one.
            journal.set_len(intact as u64)?;
            journal.sync_all()?;
        }
        Ok((
            Self {
                dir,
                journal,
                journal_bytes: intact as u64,
                journal_len,
                poisoned: false,
                _lock: lock,
            },
            issuer,
        ))
    }

    /// Appends an event with its sequence number and flushes it to disk.
    ///
    /// If the write fails, the journal is cut back to its previous length; if
    /// even that fails, the store refuses all further appends.
    pub fn append(&mut self, sequence: u64, event: &Event) -> io::Result<()> {
        self.append_all(sequence, std::slice::from_ref(event))
    }

    /// Appends events numbered from `first` as one record and flushes it to
    /// disk, so after a crash either all of them are in the journal or none.
    /// Their encoded size must stay within [`MAX_BATCH_BYTES`].
    ///
    /// Failures are handled as for [`append`](Self::append).
    pub fn append_all(&mut self, first: u64, events: &[Event]) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other(
                "an earlier append failed and could not be undone; reopen the store",
            ));
        }
        if events.is_empty() {
            return Ok(());
        }
        let record = encode_record(first, events);
        if record.len() > 4 + MAX_PAYLOAD + 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the events are too large for one journal record",
            ));
        }
        let written = self
            .journal
            .write_all(&record)
            .and_then(|()| self.journal.sync_data());
        if let Err(error) = written {
            let undone = self
                .journal
                .set_len(self.journal_bytes)
                .and_then(|()| self.journal.sync_data());
            self.poisoned = undone.is_err();
            return Err(error);
        }
        self.journal_bytes += record.len() as u64;
        self.journal_len += events.len() as u64;
        Ok(())
    }

    /// Replaces the snapshot with `issuer`'s state and empties the journal.
    pub fn compact(&mut self, issuer: &Issuer) -> io::Result<()> {
        write_atomically(&self.dir, SNAPSHOT, SNAPSHOT_TMP, &issuer.snapshot())?;
        self.journal.set_len(HEADER_LEN)?;
        // The records are gone from here on, whether or not the flush succeeds.
        self.journal_bytes = HEADER_LEN;
        self.journal_len = 0;
        self.journal.sync_all()
    }

    /// Events in the journal since the last snapshot.
    pub fn journal_len(&self) -> u64 {
        self.journal_len
    }

    /// The store's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

fn lock(dir: &Path) -> Result<File, StoreError> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(StoreError::Locked),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

fn journal_header(issuer: &PublicKey) -> Vec<u8> {
    let mut header = Vec::with_capacity(HEADER_LEN as usize);
    header.extend_from_slice(JOURNAL_MAGIC);
    header.extend_from_slice(&JOURNAL_VERSION.to_be_bytes());
    header.extend_from_slice(issuer.as_bytes());
    header
}

fn write_atomically(dir: &Path, name: &str, tmp: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(tmp);
    let mut file = File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, dir.join(name))?;
    sync_dir(dir)
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

fn checksum(bytes: &[u8]) -> [u8; 8] {
    let digest = Sha256::digest(bytes);
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

fn encode_record(first: u64, events: &[Event]) -> Vec<u8> {
    let mut record = vec![0; 4];
    record.extend_from_slice(&first.to_be_bytes());
    record.extend_from_slice(&(events.len() as u32).to_be_bytes());
    for event in events {
        let event = event.to_bytes();
        record.extend_from_slice(&(event.len() as u32).to_be_bytes());
        record.extend_from_slice(&event);
    }
    let len = (record.len() - 4) as u32;
    record[..4].copy_from_slice(&len.to_be_bytes());
    let sum = checksum(&record);
    record.extend_from_slice(&sum);
    record
}

/// The events of the record starting at `offset`, numbered, with the offset
/// just past it, if one is there intact.
fn record_at(bytes: &[u8], offset: usize, version: u16) -> Option<(Vec<(u64, Event)>, usize)> {
    let rest = bytes.get(offset..)?;
    let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    if !(9..=MAX_PAYLOAD).contains(&len) {
        return None;
    }
    let record = rest.get(..4 + len + 8)?;
    let (body, sum) = record.split_at(4 + len);
    if sum != checksum(body) {
        return None;
    }
    let first = u64::from_be_bytes(body[4..12].try_into().ok()?);
    let events = if version == 1 {
        vec![(first, Event::from_bytes(&body[12..]).ok()?)]
    } else {
        let mut rest = body.get(12..)?;
        let count = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?);
        rest = &rest[4..];
        let mut events = Vec::new();
        for k in 0..count {
            let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
            let event = Event::from_bytes(rest.get(4..4 + len)?).ok()?;
            events.push((first.checked_add(u64::from(k))?, event));
            rest = &rest[4 + len..];
        }
        if !rest.is_empty() || events.is_empty() {
            return None;
        }
        events
    };
    Some((events, offset + record.len()))
}

/// Checks the header and parses the records, returning the journal's format
/// version, its events and the length of the intact prefix.
#[allow(clippy::type_complexity)]
fn parse_journal(
    bytes: &[u8],
    issuer: &PublicKey,
) -> Result<(u16, Vec<(u64, Event)>, usize), StoreError> {
    let header = HEADER_LEN as usize;
    let unknown = || StoreError::Corrupt {
        offset: 0,
        reason: "missing or unknown journal header".into(),
    };
    let found = bytes.get(..header).ok_or_else(unknown)?;
    let version = u16::from_be_bytes([found[8], found[9]]);
    if &found[..8] != JOURNAL_MAGIC || !(1..=JOURNAL_VERSION).contains(&version) {
        return Err(unknown());
    }
    if found[10..] != issuer.as_bytes()[..] {
        return Err(SetupError::KeyMismatch.into());
    }
    let mut records = Vec::new();
    let mut offset = header;
    while offset < bytes.len() {
        if let Some((events, next)) = record_at(bytes, offset, version) {
            records.extend(events);
            offset = next;
            continue;
        }
        // Only a torn final write may be unreadable: nothing readable may follow.
        if (offset + 1..bytes.len()).any(|later| record_at(bytes, later, version).is_some()) {
            return Err(StoreError::Corrupt {
                offset: offset as u64,
                reason: "unreadable record followed by readable ones".into(),
            });
        }
        break;
    }
    Ok((version, records, offset))
}

/// A mint or transfer could not be committed.
#[derive(Debug, Error)]
pub enum CommitError {
    /// The issuer refused the request. Nothing was written.
    #[error(transparent)]
    Rejected(#[from] Error),
    /// The event could not be written, so it was not applied.
    #[error("journal write failed: {0}")]
    Io(#[from] io::Error),
}

/// An [`Issuer`] whose every state change is journaled before it takes effect.
#[derive(Debug)]
pub struct PersistentIssuer {
    issuer: Issuer,
    store: FileStore,
    /// Set when a batch took effect in memory but not on disk.
    ahead: bool,
}

impl PersistentIssuer {
    /// Creates a store in `dir` for a new issuer.
    pub fn create(dir: impl AsRef<Path>, issuer: Issuer) -> Result<Self, StoreError> {
        let store = FileStore::create(dir, &issuer)?;
        Ok(Self {
            issuer,
            store,
            ahead: false,
        })
    }

    /// Opens an existing store, replays its journal and compacts it.
    pub fn open(dir: impl AsRef<Path>, key: IssuerKey, policy: Policy) -> Result<Self, StoreError> {
        let (mut store, issuer) = FileStore::open(dir, key, policy)?;
        if store.journal_bytes > HEADER_LEN {
            store.compact(&issuer)?;
        }
        Ok(Self {
            issuer,
            store,
            ahead: false,
        })
    }

    /// Starts a group commit. See [`Batch`].
    pub fn batch(&mut self) -> io::Result<Batch<'_>> {
        self.check_in_step()?;
        Ok(Batch {
            first: self.issuer.sequence() + 1,
            owner: self,
            events: Vec::new(),
            bytes: 0,
        })
    }

    /// The issuer, for reading state and verifying requests.
    pub fn issuer(&self) -> &Issuer {
        &self.issuer
    }

    /// The issuer's public key.
    pub fn public_key(&self) -> &PublicKey {
        self.issuer.public_key()
    }

    /// Mints a note once the mint is on disk.
    pub fn mint(&mut self, amount: u64, holder: PublicKey) -> Result<Note, CommitError> {
        let event = self.issuer.plan_mint(amount, holder)?;
        self.commit(&event)?;
        Ok(self.issuer.notes(&event).remove(0))
    }

    /// Carries out a verified transfer once it is on disk. A resent request
    /// that was already applied returns the same notes and writes nothing.
    pub fn transfer(&mut self, transfer: &VerifiedTransfer) -> Result<Vec<Note>, CommitError> {
        match self.issuer.plan_transfer(transfer)? {
            Plan::Replay(notes) => Ok(notes),
            Plan::Apply(event) => {
                self.commit(&event)?;
                Ok(self.issuer.notes(&event))
            }
        }
    }

    /// Carries out a verified burn once it is on disk and returns the value
    /// destroyed. A resent burn that was already applied writes nothing.
    pub fn burn(&mut self, burn: &VerifiedBurn) -> Result<u128, CommitError> {
        if let Plan::Apply(event) = self.issuer.plan_burn(burn)? {
            self.commit(&event)?;
        }
        Ok(burn.value())
    }

    /// Rotates once the rotation is on disk.
    pub fn rotate(&mut self) -> Result<RotationReport, CommitError> {
        let event = self.issuer.plan_rotate();
        self.commit(&event)?;
        match event {
            Event::Rotate(report) => Ok(report),
            _ => unreachable!("plan_rotate plans a rotation"),
        }
    }

    /// Writes a fresh snapshot and empties the journal.
    pub fn compact(&mut self) -> io::Result<()> {
        self.check_in_step()?;
        self.store.compact(&self.issuer)
    }

    /// Events journaled since the last snapshot.
    pub fn journal_len(&self) -> u64 {
        self.store.journal_len()
    }

    fn commit(&mut self, event: &Event) -> io::Result<()> {
        self.check_in_step()?;
        self.store.append(self.issuer.sequence() + 1, event)?;
        self.issuer
            .apply(event)
            .expect("an event planned under the same borrow applies");
        Ok(())
    }

    /// Refuses to go on once memory is ahead of the disk.
    fn check_in_step(&self) -> io::Result<()> {
        if self.ahead {
            return Err(io::Error::other(
                "a batch took effect but was not written; reopen the store",
            ));
        }
        Ok(())
    }
}

/// Changes to a [`PersistentIssuer`] that reach the disk together, in one
/// journal record with one flush. See the [module documentation](self).
///
/// Each change is planned and applied at once, so later changes in the batch
/// see earlier ones, and the issuer's [`Issuer::sequence`] moves ahead of the
/// journal until [`commit`](Self::commit). Notes and reports a batch returns
/// must not leave the process until the commit succeeds. A batch dropped
/// without committing, or whose commit fails, leaves the issuer refusing
/// every further change until the store is reopened.
#[derive(Debug)]
pub struct Batch<'a> {
    owner: &'a mut PersistentIssuer,
    first: u64,
    events: Vec<Event>,
    bytes: usize,
}

impl Batch<'_> {
    /// The issuer as the batch has left it so far.
    pub fn issuer(&self) -> &Issuer {
        &self.owner.issuer
    }

    /// Mints a note.
    pub fn mint(&mut self, amount: u64, holder: PublicKey) -> Result<Note, Error> {
        let event = self.owner.issuer.plan_mint(amount, holder)?;
        Ok(self.stage(event).remove(0))
    }

    /// Carries out a verified transfer. A resent request that was already
    /// applied returns the same notes and adds nothing to the batch.
    pub fn transfer(&mut self, transfer: &VerifiedTransfer) -> Result<Vec<Note>, Error> {
        match self.owner.issuer.plan_transfer(transfer)? {
            Plan::Replay(notes) => Ok(notes),
            Plan::Apply(event) => Ok(self.stage(event)),
        }
    }

    /// Carries out a verified burn and returns the value destroyed. A resent
    /// burn that was already applied adds nothing to the batch.
    pub fn burn(&mut self, burn: &VerifiedBurn) -> Result<u128, Error> {
        if let Plan::Apply(event) = self.owner.issuer.plan_burn(burn)? {
            self.stage(event);
        }
        Ok(burn.value())
    }

    /// Rotates.
    pub fn rotate(&mut self) -> RotationReport {
        let event = self.owner.issuer.plan_rotate();
        self.stage(event.clone());
        match event {
            Event::Rotate(report) => report,
            _ => unreachable!("plan_rotate plans a rotation"),
        }
    }

    /// Events staged so far.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether nothing has been staged.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Encoded size of the events staged so far. Commit before it would pass
    /// [`MAX_BATCH_BYTES`].
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Writes every staged event in one record and flushes it. Only after
    /// this succeeds may the batch's results be handed out.
    pub fn commit(mut self) -> io::Result<()> {
        let events = std::mem::take(&mut self.events);
        let written = self.owner.store.append_all(self.first, &events);
        if written.is_err() && !events.is_empty() {
            self.owner.ahead = true;
        }
        written
    }

    fn stage(&mut self, event: Event) -> Vec<Note> {
        self.owner
            .issuer
            .apply(&event)
            .expect("an event planned under the same borrow applies");
        // Each event is framed by a 4-byte length in the record.
        self.bytes += 4 + event.to_bytes().len();
        let notes = self.owner.issuer.notes(&event);
        self.events.push(event);
        notes
    }
}

impl Drop for Batch<'_> {
    fn drop(&mut self) {
        if !self.events.is_empty() {
            self.owner.ahead = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::HolderKey;
    use crate::ledger::LedgerConfig;
    use crate::transfer::{BurnRequest, Output, TransferRequest};

    const LEDGER: LedgerConfig = LedgerConfig {
        layers: 4,
        layer_size: 128,
    };

    fn fresh(dir: &Path) -> (IssuerKey, PersistentIssuer) {
        let key = IssuerKey::generate();
        let issuer = Issuer::new(key.clone(), LEDGER, Policy::cash_like()).unwrap();
        (key, PersistentIssuer::create(dir, issuer).unwrap())
    }

    fn activity(p: &mut PersistentIssuer) {
        let alice = HolderKey::generate();
        let note = p.mint(8, alice.public_key()).unwrap();
        let output = Output {
            amount: 8,
            holder: HolderKey::generate().public_key(),
        };
        let request = TransferRequest::single(note, &alice, output).unwrap();
        let verified = p.issuer().verifier().verify(&request).unwrap();
        p.transfer(&verified).unwrap();
        p.rotate().unwrap();
        p.mint(4, alice.public_key()).unwrap();
    }

    #[test]
    fn reopening_replays_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        assert_eq!(p.journal_len(), 4);
        let expected = p.issuer().snapshot();
        drop(p);
        let p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
        assert_eq!(p.journal_len(), 0, "open compacts");
    }

    #[test]
    fn a_second_process_cannot_open_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let (key, _p) = fresh(dir.path());
        assert!(matches!(
            FileStore::open(dir.path(), key, Policy::cash_like()),
            Err(StoreError::Locked)
        ));
    }

    #[test]
    fn create_refuses_an_existing_store() {
        let dir = tempfile::tempdir().unwrap();
        let (key, p) = fresh(dir.path());
        drop(p);
        let issuer = Issuer::new(key, LEDGER, Policy::cash_like()).unwrap();
        assert!(matches!(
            FileStore::create(dir.path(), &issuer),
            Err(StoreError::Exists(_))
        ));
    }

    #[test]
    fn a_torn_final_record_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        let journal = dir.path().join(JOURNAL);
        let len = fs::metadata(&journal).unwrap().len();
        drop(p);
        // Cut the last record (an 81-byte mint) short.
        let file = OpenOptions::new().write(true).open(&journal).unwrap();
        file.set_len(len - 30).unwrap();
        drop(file);
        let (store, issuer) = FileStore::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(issuer.sequence(), 3);
        assert_eq!(store.journal_len(), 3);
        assert_eq!(fs::metadata(&journal).unwrap().len(), store.journal_bytes);
    }

    /// Flips one bit of the journal and tries to open the store.
    fn open_with_bit_flipped(offset: usize) -> Result<(FileStore, Issuer), StoreError> {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        drop(p);
        let journal = dir.path().join(JOURNAL);
        let mut bytes = fs::read(&journal).unwrap();
        bytes[offset] ^= 0x80;
        fs::write(&journal, bytes).unwrap();
        FileStore::open(dir.path(), key, Policy::cash_like())
    }

    #[test]
    fn damage_before_the_last_record_is_reported() {
        let first = HEADER_LEN as usize;
        // Inside the first record's event, and in the high byte of its length.
        for offset in [first + 20, first] {
            assert!(matches!(
                open_with_bit_flipped(offset),
                Err(StoreError::Corrupt { offset, .. }) if offset == HEADER_LEN
            ));
        }
    }

    #[test]
    fn a_record_repeated_after_newer_ones_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        drop(p);
        let journal = dir.path().join(JOURNAL);
        let mut bytes = fs::read(&journal).unwrap();
        let first = HEADER_LEN as usize;
        let len = u32::from_be_bytes(bytes[first..first + 4].try_into().unwrap()) as usize;
        let mint = bytes[first..first + 4 + len + 8].to_vec();
        bytes.extend_from_slice(&mint);
        fs::write(&journal, bytes).unwrap();
        assert!(matches!(
            FileStore::open(dir.path(), key, Policy::cash_like()),
            Err(StoreError::Replay { sequence: 1, .. })
        ));
    }

    #[test]
    fn garbage_after_the_last_record_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        drop(p);
        let journal = dir.path().join(JOURNAL);
        let mut bytes = fs::read(&journal).unwrap();
        bytes.extend_from_slice(&[0; 100]);
        fs::write(&journal, bytes).unwrap();
        let (_, issuer) = FileStore::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(issuer.sequence(), 4);
    }

    #[test]
    fn a_journal_cannot_replay_onto_another_issuer() {
        let dir = tempfile::tempdir().unwrap();
        let (_, mut p) = fresh(dir.path());
        activity(&mut p);
        drop(p);
        let journal = fs::read(dir.path().join(JOURNAL)).unwrap();
        // A second issuer's store, with the first issuer's journal copied in.
        let other = tempfile::tempdir().unwrap();
        let (key, p) = fresh(other.path());
        drop(p);
        fs::write(other.path().join(JOURNAL), journal).unwrap();
        assert!(matches!(
            FileStore::open(other.path(), key, Policy::cash_like()),
            Err(StoreError::Setup(SetupError::KeyMismatch))
        ));
    }

    #[test]
    fn create_refuses_a_leftover_journal() {
        let dir = tempfile::tempdir().unwrap();
        let (key, p) = fresh(dir.path());
        drop(p);
        fs::remove_file(dir.path().join(SNAPSHOT)).unwrap();
        let issuer = Issuer::new(key, LEDGER, Policy::cash_like()).unwrap();
        assert!(matches!(
            FileStore::create(dir.path(), &issuer),
            Err(StoreError::Exists(_))
        ));
    }

    #[test]
    fn records_already_in_the_snapshot_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        let journal = fs::read(dir.path().join(JOURNAL)).unwrap();
        // Simulate a crash after the snapshot was replaced but before the
        // journal was emptied.
        p.compact().unwrap();
        let expected = p.issuer().snapshot();
        drop(p);
        fs::write(dir.path().join(JOURNAL), journal).unwrap();
        let p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
    }

    fn mint_and_pay(batch: &mut Batch<'_>) -> Note {
        let alice = HolderKey::generate();
        let note = batch.mint(8, alice.public_key()).unwrap();
        let output = Output {
            amount: 8,
            holder: HolderKey::generate().public_key(),
        };
        let request = TransferRequest::single(note, &alice, output).unwrap();
        let verified = batch.issuer().verifier().verify(&request).unwrap();
        batch.transfer(&verified).unwrap().remove(0)
    }

    #[test]
    fn a_batch_is_one_record_and_replays_like_single_events() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        p.mint(2, HolderKey::generate().public_key()).unwrap();
        let journal = dir.path().join(JOURNAL);
        let before = fs::metadata(&journal).unwrap().len();

        let mut batch = p.batch().unwrap();
        let paid = mint_and_pay(&mut batch);
        let report = batch.rotate();
        assert_eq!(batch.len(), 3);
        let bytes = batch.bytes();
        batch.commit().unwrap();
        assert_eq!(report.dropped, 0);
        assert_eq!(p.journal_len(), 4);
        // One record: length, first sequence, count, the events, checksum.
        let after = fs::metadata(&journal).unwrap().len();
        assert_eq!(after - before, (4 + 8 + 4 + bytes + 8) as u64);

        let expected = p.issuer().snapshot();
        drop(p);
        let p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
        assert!(matches!(
            p.issuer().status(&paid),
            Ok(crate::NoteStatus::Live { .. })
        ));
    }

    #[test]
    fn a_burn_resent_in_the_same_batch_burns_once() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        let holder = HolderKey::generate();
        let note = p.mint(8, holder.public_key()).unwrap();
        let request = BurnRequest::single(note, &holder).unwrap();
        let verified = p.issuer().verifier().verify_burn(&request).unwrap();

        let mut batch = p.batch().unwrap();
        assert_eq!(batch.burn(&verified).unwrap(), 8);
        assert_eq!(batch.burn(&verified).unwrap(), 8);
        assert_eq!(batch.len(), 1);
        batch.commit().unwrap();
        assert_eq!(p.issuer().supply().burned, 8);

        let expected = p.issuer().snapshot();
        drop(p);
        let p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
    }

    #[test]
    fn a_torn_batch_is_dropped_whole() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        p.mint(2, HolderKey::generate().public_key()).unwrap();
        let mut batch = p.batch().unwrap();
        mint_and_pay(&mut batch);
        batch.commit().unwrap();
        let journal = dir.path().join(JOURNAL);
        let len = fs::metadata(&journal).unwrap().len();
        drop(p);
        let file = OpenOptions::new().write(true).open(&journal).unwrap();
        file.set_len(len - 10).unwrap();
        drop(file);
        let (_, issuer) = FileStore::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(issuer.sequence(), 1);
    }

    #[test]
    fn a_batch_that_does_not_reach_the_disk_stops_all_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        let mut batch = p.batch().unwrap();
        mint_and_pay(&mut batch);
        drop(batch);
        assert_eq!(p.issuer().sequence(), 2, "the batch took effect in memory");
        assert!(matches!(
            p.mint(2, HolderKey::generate().public_key()),
            Err(CommitError::Io(_))
        ));
        assert!(p.batch().is_err());
        assert!(
            p.compact().is_err(),
            "a snapshot would save unwritten changes"
        );
        drop(p);
        let p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().sequence(), 0);

        // An empty batch writes nothing and does no harm.
        let mut p = p;
        p.batch().unwrap().commit().unwrap();
        drop(p.batch().unwrap());
        p.mint(2, HolderKey::generate().public_key()).unwrap();
    }

    #[test]
    fn version_1_journals_replay_and_are_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let key = IssuerKey::generate();
        let mut issuer = Issuer::new(key.clone(), LEDGER, Policy::cash_like()).unwrap();
        drop(FileStore::create(dir.path(), &issuer).unwrap());

        // A journal as version 1 wrote it: one event per record.
        let mut journal = journal_header(issuer.public_key());
        journal[8..10].copy_from_slice(&1u16.to_be_bytes());
        let alice = HolderKey::generate();
        let mint = issuer.plan_mint(8, alice.public_key()).unwrap();
        for event in [mint, issuer.plan_rotate()] {
            issuer.apply(&event).unwrap();
            let event = event.to_bytes();
            let mut record = ((8 + event.len()) as u32).to_be_bytes().to_vec();
            record.extend_from_slice(&issuer.sequence().to_be_bytes());
            record.extend_from_slice(&event);
            let sum = checksum(&record);
            record.extend_from_slice(&sum);
            journal.extend_from_slice(&record);
        }
        fs::write(dir.path().join(JOURNAL), journal).unwrap();

        let (store, restored) =
            FileStore::open(dir.path(), key.clone(), Policy::cash_like()).unwrap();
        assert_eq!(restored.snapshot(), issuer.snapshot());
        assert_eq!(store.journal_len(), 0);
        drop(store);
        let rewritten = fs::read(dir.path().join(JOURNAL)).unwrap();
        assert_eq!(rewritten, journal_header(issuer.public_key()));
        let p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().snapshot(), issuer.snapshot());
    }
}
