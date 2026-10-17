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
//! The journal starts with a header naming the issuer's public key, so a
//! journal can never be replayed onto another issuer's snapshot. Each record
//! is `len: u32 ‖ sequence: u64 ‖ event ‖ checksum`, where `len` counts the
//! sequence and event bytes and `checksum` is the first 8 bytes of the
//! SHA-256 of everything before it in the record, length included.
//!
//! Appends are flushed one at a time, so a crash can only damage the end of
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
use crate::mint::VerifiedMint;
use crate::note::Note;
use crate::policy::Policy;
use crate::transfer::{VerifiedBurn, VerifiedTransfer};

const SNAPSHOT: &str = "snapshot.bin";
const SNAPSHOT_TMP: &str = "snapshot.bin.tmp";
const JOURNAL: &str = "journal.bin";
const LOCK: &str = "lock";
const JOURNAL_MAGIC: &[u8; 8] = b"WORGLJNL";
const JOURNAL_VERSION: u16 = 1;
/// Magic, version and issuer public key.
const HEADER_LEN: u64 = 8 + 2 + 32;
/// Bytes of a record that are not the event: length, sequence, checksum.
const RECORD_OVERHEAD: usize = 4 + 8 + 8;
/// Largest sequence-plus-event payload a record may claim. The largest event
/// the format allows with 255 inputs and outputs is under 19 KB.
const MAX_PAYLOAD: usize = 1 << 16;

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
        let (records, intact) = parse_journal(&bytes, issuer.public_key())?;
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
        if intact < bytes.len() {
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
        if self.poisoned {
            return Err(io::Error::other(
                "an earlier append failed and could not be undone; reopen the store",
            ));
        }
        let record = encode_record(sequence, event);
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
        self.journal_len += 1;
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

fn encode_record(sequence: u64, event: &Event) -> Vec<u8> {
    let event = event.to_bytes();
    let mut record = Vec::with_capacity(RECORD_OVERHEAD + event.len());
    record.extend_from_slice(&((8 + event.len()) as u32).to_be_bytes());
    record.extend_from_slice(&sequence.to_be_bytes());
    record.extend_from_slice(&event);
    let sum = checksum(&record);
    record.extend_from_slice(&sum);
    record
}

/// The record starting at `offset`, with the offset just past it, if one is
/// there intact.
fn record_at(bytes: &[u8], offset: usize) -> Option<(u64, Event, usize)> {
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
    let sequence = u64::from_be_bytes(body[4..12].try_into().ok()?);
    let event = Event::from_bytes(&body[12..]).ok()?;
    Some((sequence, event, offset + record.len()))
}

/// Checks the header and parses the records, returning them with the length
/// of the intact prefix.
fn parse_journal(
    bytes: &[u8],
    issuer: &PublicKey,
) -> Result<(Vec<(u64, Event)>, usize), StoreError> {
    let header = HEADER_LEN as usize;
    match bytes.get(..header) {
        Some(found) if found == journal_header(issuer).as_slice() => {}
        Some(found) if found[..10] == journal_header(issuer)[..10] => {
            return Err(SetupError::KeyMismatch.into());
        }
        _ => {
            return Err(StoreError::Corrupt {
                offset: 0,
                reason: "missing or unknown journal header".into(),
            });
        }
    }
    let mut records = Vec::new();
    let mut offset = header;
    while offset < bytes.len() {
        if let Some((sequence, event, next)) = record_at(bytes, offset) {
            records.push((sequence, event));
            offset = next;
            continue;
        }
        // Only a torn final write may be unreadable: nothing readable may follow.
        if (offset + 1..bytes.len()).any(|later| record_at(bytes, later).is_some()) {
            return Err(StoreError::Corrupt {
                offset: offset as u64,
                reason: "unreadable record followed by readable ones".into(),
            });
        }
        break;
    }
    Ok((records, offset))
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
}

impl PersistentIssuer {
    /// Creates a store in `dir` for a new issuer.
    pub fn create(dir: impl AsRef<Path>, issuer: Issuer) -> Result<Self, StoreError> {
        let store = FileStore::create(dir, &issuer)?;
        Ok(Self { issuer, store })
    }

    /// Opens an existing store, replays its journal and compacts it.
    pub fn open(dir: impl AsRef<Path>, key: IssuerKey, policy: Policy) -> Result<Self, StoreError> {
        let (mut store, issuer) = FileStore::open(dir, key, policy)?;
        if store.journal_bytes > HEADER_LEN {
            store.compact(&issuer)?;
        }
        Ok(Self { issuer, store })
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

    /// Mints at a verified mint authority's request once the mint is on disk.
    /// Resending the authority's last request returns the same note and
    /// writes nothing.
    pub fn authorized_mint(&mut self, mint: &VerifiedMint) -> Result<Note, CommitError> {
        let mut notes = match self.issuer.plan_authorized_mint(mint)? {
            Plan::Replay(notes) => notes,
            Plan::Apply(event) => {
                self.commit(&event)?;
                self.issuer.notes(&event)
            }
        };
        Ok(notes.remove(0))
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
        self.store.compact(&self.issuer)
    }

    /// Events journaled since the last snapshot.
    pub fn journal_len(&self) -> u64 {
        self.store.journal_len()
    }

    fn commit(&mut self, event: &Event) -> io::Result<()> {
        self.store.append(self.issuer.sequence() + 1, event)?;
        self.issuer
            .apply(event)
            .expect("an event planned under the same borrow applies");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::HolderKey;
    use crate::ledger::LedgerConfig;
    use crate::transfer::{Output, TransferRequest};

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
        // Cut the last record (a 73-byte mint) short.
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
        let mint = bytes[first..first + 73].to_vec();
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
}
