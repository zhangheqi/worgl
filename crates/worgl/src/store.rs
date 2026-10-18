//! Durable storage: a snapshot plus a log of every change since.
//!
//! A store is a directory holding
//!
//! - `snapshot.bin`, the issuer's state as of some log entry (see
//!   [`Issuer::snapshot`]), with that entry's id and the latest membership
//!   change up to it,
//! - `log.bin`, the [`log`] of entries since then,
//! - `keys.bin`, the secrets of the generation keys (see
//!   [`KeyRing`]), readable by its owner only, and
//! - `lock`, held for as long as a [`FileStore`] is open so that two
//!   processes never write the same store.
//!
//! Only `keys.bin` is secret: snapshot and log name public keys and
//! certificates only. Back it up with the rest, since a store whose live
//! generations lost their keys cannot sign their notes. A rotation writes the
//! new key to `keys.bin` before logging the rotation, and a compaction
//! erases the keys of dropped generations from it.
//!
//! [`PersistentIssuer`] writes each event to the log and flushes it to disk
//! *before* applying it, so a note is never handed out unless the event that
//! created it would survive a crash. [`compact`](PersistentIssuer::compact)
//! writes a fresh snapshot and purges the log up to it; call it after
//! rotations or whenever [`log_events`](PersistentIssuer::log_events) grows
//! large.
//!
//! A flush costs far more than the work around it, so a busy issuer can share
//! one among many requests with a [`Batch`]: each change takes effect in
//! memory as it is made, and all of them reach the disk in one log entry and
//! one flush when the batch commits. Nothing a batch returns may be handed
//! out before then. If the flush fails, the issuer is ahead of its log and
//! refuses all further changes until it is reopened, which restores exactly
//! what reached the disk.
//!
//! The split into a log and a snapshot that records the last entry it holds
//! is the one Raft replication needs. A store on its own is the only writer
//! of its log, so every entry it appends is applied at once, under the ids
//! [`LogId::standalone`] gives. Opening it applies every entry after the
//! snapshot's.
//!
//! `snapshot.bin` starts with a header: the magic `WORGLSTA`, a `u16` format
//! version, the id of the last entry applied, the id of the latest membership
//! change applied (each as in the log's header), that change as
//! `len: u32 ‖ bytes`, empty if there is none, and the first 8 bytes of the
//! SHA-256 of everything before them. The issuer's snapshot, which carries
//! its own checksum, fills the rest of the file.
//!
//! Stores from before the log hold the issuer's snapshot alone in
//! `snapshot.bin`, and their changes as events in `journal.bin`. Opening one
//! replays the journal, writes the result as a snapshot with an empty log and
//! removes the journal; a conversion cut short starts over on the next open.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::codec::DecodeError;
use crate::error::{ApplyError, Error, SetupError};
use crate::issuer::{Event, Issuer, Plan, RotationReport};
use crate::keys::{GenerationKey, IssuerKey, PublicKey};
use crate::keyset::KeyRing;
use crate::log::{
    self, ENTRY_HEAD, EVENTS_HEAD, Entry, FileLog, ID_LEN, LogId, MAX_PAYLOAD, Payload, read_id,
    write_id,
};
use crate::mint::VerifiedMint;
use crate::note::Note;
use crate::policy::Policy;
use crate::transfer::{VerifiedBurn, VerifiedTransfer};

const SNAPSHOT: &str = "snapshot.bin";
const SNAPSHOT_TMP: &str = "snapshot.bin.tmp";
const KEYS: &str = "keys.bin";
const KEYS_TMP: &str = "keys.bin.tmp";
const LOCK: &str = "lock";
const SNAPSHOT_MAGIC: &[u8; 8] = b"WORGLSTA";
const SNAPSHOT_VERSION: u16 = 1;
/// How an issuer's snapshot starts, and so how `snapshot.bin` started before
/// the log.
const ISSUER_SNAPSHOT_MAGIC: &[u8; 8] = b"WORGLSNP";
/// Largest encoded size of the events in one [`Batch`], each framed by its
/// length. The largest event the format allows, with 255 inputs and outputs,
/// is under 19 KB.
pub const MAX_BATCH_BYTES: usize = MAX_PAYLOAD - ENTRY_HEAD - EVENTS_HEAD;

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
    /// The log is damaged before its last record, or does not fit the
    /// snapshot.
    #[error("the log is corrupt at byte {offset}: {reason}")]
    Corrupt {
        /// Where the damage starts.
        offset: u64,
        /// What is wrong.
        reason: String,
    },
    /// A logged entry does not fit the state it is replayed onto.
    #[error("log entry {index} does not replay: {error}")]
    Replay {
        /// The entry's index, or for the journal of a store from before the
        /// log, the event's sequence number.
        index: u64,
        /// Why it does not apply.
        error: ApplyError,
    },
}

/// A snapshot and log in one directory. See the [module documentation](self).
#[derive(Debug)]
pub struct FileStore {
    dir: PathBuf,
    log: FileLog,
    /// The last entry applied.
    applied: Option<LogId>,
    /// The latest membership change applied, with its id.
    membership: Option<(LogId, Vec<u8>)>,
    /// Events logged since the snapshot.
    events: u64,
    /// What `keys.bin` holds.
    keys: KeyRing,
    _lock: File,
}

impl FileStore {
    /// Creates a store in `dir`, creating the directory if needed, holding
    /// `issuer`'s current state. Refuses if a snapshot or log is already there.
    pub fn create(dir: impl AsRef<Path>, issuer: &Issuer) -> Result<Self, StoreError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let lock = lock(&dir)?;
        if [SNAPSHOT, log::LOG, journal::JOURNAL]
            .iter()
            .any(|name| dir.join(name).exists())
        {
            return Err(StoreError::Exists(dir));
        }
        // The log comes first: a directory with a snapshot is a complete store.
        let log = FileLog::create(&dir, issuer.public_key())?;
        let keys = issuer.key_ring().clone();
        write_keys(&dir, &keys)?;
        write_snapshot(&dir, None, None, &issuer.snapshot())?;
        Ok(Self {
            dir,
            log,
            applied: None,
            membership: None,
            events: 0,
            keys,
            _lock: lock,
        })
    }

    /// Opens a store, restores the issuer from its snapshot and key ring and
    /// applies the log's entries after the snapshot's onto it. A store from
    /// before the log is converted first.
    ///
    /// A store written before generation keys existed has no `keys.bin`; its
    /// generations are signed by the issuer key until they are dropped.
    pub fn open(
        dir: impl AsRef<Path>,
        key: IssuerKey,
        policy: Policy,
    ) -> Result<(Self, Issuer), StoreError> {
        let dir = dir.as_ref().to_path_buf();
        let lock = lock(&dir)?;
        let ring = read_keys(&dir, &key)?;
        let mut keys = ring.clone();
        let mut bytes = fs::read(dir.join(SNAPSHOT))?;
        if bytes.starts_with(ISSUER_SNAPSHOT_MAGIC) {
            journal::convert(&dir, key.clone(), ring.clone(), &bytes, policy.clone())?;
            bytes = fs::read(dir.join(SNAPSHOT))?;
        } else if dir.join(journal::JOURNAL).exists() {
            // Left over from a conversion that stopped once the snapshot held it.
            fs::remove_file(dir.join(journal::JOURNAL))?;
            sync_dir(&dir)?;
        }
        let (mut applied, mut membership, snapshot) = parse_snapshot(&bytes)?;
        let mut issuer = Issuer::restore(key, ring, snapshot, policy)?;
        drop(bytes);

        let (mut log, entries) = FileLog::open(&dir, issuer.public_key())?;
        if let Some(purged) = log.purged()
            && applied.is_none_or(|applied| purged.index > applied.index)
        {
            return Err(StoreError::Corrupt {
                offset: 0,
                reason: format!(
                    "entry {} was purged but the snapshot lacks it",
                    purged.index
                ),
            });
        }
        let mut events = 0;
        for entry in entries {
            // Entries up to the snapshot's are left over from a compaction
            // that stopped before purging them.
            if applied.is_some_and(|applied| entry.id.index <= applied.index) {
                continue;
            }
            events += replay(&mut issuer, &entry, &mut membership)?;
            applied = Some(entry.id);
        }
        if let Some(applied) = applied
            && log.next_index() <= applied.index
        {
            // The log ends before the snapshot's entry, so the next entry
            // must follow that one, not the log's last.
            log.purge(applied)?;
        }
        // A rotation that was planned but never logged leaves its key
        // behind. The file may only lose it once no logged event needs
        // replaying, though; otherwise the next compaction removes it.
        issuer.retire_keys();
        if events == 0 && issuer.key_ring().len() < keys.len() {
            keys = issuer.key_ring().clone();
            write_keys(&dir, &keys)?;
        }
        Ok((
            Self {
                dir,
                log,
                applied,
                membership,
                events,
                keys,
                _lock: lock,
            },
            issuer,
        ))
    }

    /// Logs an event with its sequence number and flushes it to disk.
    ///
    /// If the write fails, the log is cut back to its previous length; if
    /// even that fails, the store refuses all further appends.
    pub fn append(&mut self, sequence: u64, event: &Event) -> io::Result<()> {
        self.append_all(sequence, std::slice::from_ref(event))
    }

    /// Logs events numbered from `first` as one entry and flushes it to
    /// disk, so after a crash either all of them are in the log or none.
    /// Their encoded size must stay within [`MAX_BATCH_BYTES`].
    ///
    /// Failures are handled as for [`append`](Self::append).
    pub fn append_all(&mut self, first: u64, events: &[Event]) -> io::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let entry = Entry {
            id: LogId::standalone(self.log.next_index()),
            payload: Payload::Events {
                first,
                events: events.to_vec(),
            },
        };
        self.log.append(std::slice::from_ref(&entry))?;
        self.applied = Some(entry.id);
        self.events += events.len() as u64;
        Ok(())
    }

    /// Replaces the snapshot with `issuer`'s state as of the last entry
    /// applied, purges the log up to that entry and rewrites the key ring
    /// without the keys of dropped generations.
    pub fn compact(&mut self, issuer: &Issuer) -> io::Result<()> {
        write_snapshot(
            &self.dir,
            self.applied,
            self.membership.as_ref(),
            &issuer.snapshot(),
        )?;
        // The snapshot holds every event from here on, whether or not the
        // purge succeeds.
        self.events = 0;
        if let Some(applied) = self.applied {
            self.log.purge(applied)?;
        }
        // Only now, since the old snapshot and log named those keys.
        let keys = issuer.key_ring().clone();
        write_keys(&self.dir, &keys)?;
        self.keys = keys;
        Ok(())
    }

    /// Adds `ring`'s keys to `keys.bin` and flushes it to disk. Keys leave
    /// the file only at [`compact`](Self::compact), since until then the
    /// snapshot or log may name them.
    pub fn save_keys(&mut self, ring: &KeyRing) -> io::Result<()> {
        let mut keys = self.keys.clone();
        keys.extend(ring);
        write_keys(&self.dir, &keys)?;
        self.keys = keys;
        Ok(())
    }

    /// Events logged since the last snapshot.
    pub fn log_events(&self) -> u64 {
        self.events
    }

    /// The log.
    pub fn log(&self) -> &FileLog {
        &self.log
    }

    /// The id of the last entry applied, if any was.
    pub fn applied(&self) -> Option<LogId> {
        self.applied
    }

    /// The latest membership change applied, with its id.
    pub fn membership(&self) -> Option<(LogId, &[u8])> {
        self.membership
            .as_ref()
            .map(|(id, bytes)| (*id, bytes.as_slice()))
    }

    /// The store's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Applies an entry after the snapshot's and returns how many events it held.
fn replay(
    issuer: &mut Issuer,
    entry: &Entry,
    membership: &mut Option<(LogId, Vec<u8>)>,
) -> Result<u64, StoreError> {
    let index = entry.id.index;
    match &entry.payload {
        Payload::Blank => Ok(0),
        Payload::Membership(bytes) => {
            *membership = Some((entry.id, bytes.clone()));
            Ok(0)
        }
        Payload::Events { first, events } => {
            let next = issuer.sequence() + 1;
            if *first != next {
                return Err(StoreError::Replay {
                    index,
                    error: ApplyError(format!("its first event is {first}, not {next}")),
                });
            }
            for event in events {
                issuer
                    .apply(event)
                    .map_err(|error| StoreError::Replay { index, error })?;
            }
            Ok(events.len() as u64)
        }
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

/// Reads `keys.bin`, or starts an empty ring for a store from before
/// generation keys.
fn read_keys(dir: &Path, key: &IssuerKey) -> Result<KeyRing, StoreError> {
    match fs::read(dir.join(KEYS)) {
        Ok(mut bytes) => {
            let ring = KeyRing::from_bytes(&bytes);
            bytes.fill(0);
            Ok(ring?)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(KeyRing::new(key.public_key())),
        Err(error) => Err(error.into()),
    }
}

/// Writes `snapshot.bin`: the header, then the issuer's snapshot.
fn write_snapshot(
    dir: &Path,
    applied: Option<LogId>,
    membership: Option<&(LogId, Vec<u8>)>,
    issuer: &[u8],
) -> io::Result<()> {
    let change = membership.map_or(&[][..], |(_, bytes)| bytes.as_slice());
    let mut header = Vec::with_capacity(10 + 2 * ID_LEN + 4 + change.len() + 8);
    header.extend_from_slice(SNAPSHOT_MAGIC);
    header.extend_from_slice(&SNAPSHOT_VERSION.to_be_bytes());
    write_id(applied, &mut header);
    write_id(membership.map(|(id, _)| *id), &mut header);
    header.extend_from_slice(&(change.len() as u32).to_be_bytes());
    header.extend_from_slice(change);
    let sum = checksum(&header);
    header.extend_from_slice(&sum);
    write_atomically(dir, SNAPSHOT, SNAPSHOT_TMP, &[&header, issuer])
}

/// The last entry applied, the latest membership change and the issuer's
/// snapshot, from `snapshot.bin`.
#[allow(clippy::type_complexity)]
fn parse_snapshot(
    bytes: &[u8],
) -> Result<(Option<LogId>, Option<(LogId, Vec<u8>)>, &[u8]), StoreError> {
    let malformed = |what| StoreError::Setup(SetupError::Snapshot(DecodeError::Invalid(what)));
    if bytes.get(..8) != Some(SNAPSHOT_MAGIC) {
        return Err(malformed("snapshot magic"));
    }
    if bytes.get(8..10) != Some(&SNAPSHOT_VERSION.to_be_bytes()) {
        return Err(malformed("snapshot version"));
    }
    let truncated = || StoreError::Setup(SetupError::Snapshot(DecodeError::Truncated));
    let id_at = |at: usize| read_id(bytes.get(at..).unwrap_or_default());
    let applied = id_at(10).ok_or_else(truncated)?;
    let changed = id_at(10 + ID_LEN).ok_or_else(truncated)?;
    let at = 10 + 2 * ID_LEN;
    let len = bytes
        .get(at..at + 4)
        .ok_or_else(truncated)?
        .try_into()
        .map(u32::from_be_bytes)
        .expect("4 bytes") as usize;
    let change = bytes.get(at + 4..at + 4 + len).ok_or_else(truncated)?;
    let end = at + 4 + len;
    if bytes.get(end..end + 8).ok_or_else(truncated)? != checksum(&bytes[..end]) {
        return Err(SetupError::Checksum.into());
    }
    let membership = changed.map(|id| (id, change.to_vec()));
    Ok((applied, membership, &bytes[end + 8..]))
}

/// Writes `parts` one after another to a temporary file, flushes it and
/// renames it over `name`.
fn write_atomically(dir: &Path, name: &str, tmp: &str, parts: &[&[u8]]) -> io::Result<()> {
    let tmp = dir.join(tmp);
    let mut file = File::create(&tmp)?;
    for part in parts {
        file.write_all(part)?;
    }
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, dir.join(name))?;
    sync_dir(dir)
}

/// Writes the key ring atomically, readable by its owner only.
fn write_keys(dir: &Path, ring: &KeyRing) -> io::Result<()> {
    let tmp = dir.join(KEYS_TMP);
    let mut bytes = ring.to_bytes();
    let written = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        // The mode only applies to a new file; a stale one is tightened too.
        #[cfg(unix)]
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(&bytes)?;
        file.sync_all()
    })();
    bytes.fill(0);
    written?;
    fs::rename(&tmp, dir.join(KEYS))?;
    sync_dir(dir)
}

#[cfg(unix)]
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
pub(crate) fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// The first 8 bytes of the SHA-256 of `bytes`.
pub(crate) fn checksum(bytes: &[u8]) -> [u8; 8] {
    let digest = Sha256::digest(bytes);
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// The journal of stores from before the log, read once to convert them.
///
/// It starts with the magic `WORGLJNL`, a `u16` format version and the
/// issuer public key. Each record is `len: u32 ‖ first sequence: u64 ‖
/// count: u32 ‖ (event length: u32 ‖ event) × count ‖ checksum`, checksummed
/// as log records are; version 1 records held one event with no count.
mod journal {
    use super::*;

    pub(super) const JOURNAL: &str = "journal.bin";
    const MAGIC: &[u8; 8] = b"WORGLJNL";
    /// Version 2 grouped events into records; version 1 held one event per record.
    const VERSION: u16 = 2;
    /// Magic, version and issuer public key.
    pub(super) const HEADER_LEN: usize = 8 + 2 + 32;

    /// Replays the journal onto the issuer snapshot `snapshot` and writes
    /// the result as an empty log and a snapshot, then removes the journal.
    pub(super) fn convert(
        dir: &Path,
        key: IssuerKey,
        ring: KeyRing,
        snapshot: &[u8],
        policy: Policy,
    ) -> Result<(), StoreError> {
        let mut issuer = Issuer::restore(key, ring, snapshot, policy)?;
        let bytes = fs::read(dir.join(JOURNAL))?;
        let mut replayed = 0;
        for (sequence, event) in parse(&bytes, issuer.public_key())? {
            // Records at or before the snapshot, ahead of the first one that
            // applies, are left over from a compaction that stopped before
            // emptying the journal.
            if replayed == 0 && sequence <= issuer.sequence() {
                continue;
            }
            if sequence != issuer.sequence() + 1 {
                return Err(StoreError::Replay {
                    index: sequence,
                    error: ApplyError(format!("expected journal record {}", issuer.sequence() + 1)),
                });
            }
            issuer.apply(&event).map_err(|error| StoreError::Replay {
                index: sequence,
                error,
            })?;
            replayed += 1;
        }
        // Until the new snapshot replaces the old one, opening converts
        // again and replaces this log too.
        drop(FileLog::create(dir, issuer.public_key())?);
        write_snapshot(dir, None, None, &issuer.snapshot())?;
        fs::remove_file(dir.join(JOURNAL))?;
        sync_dir(dir)?;
        Ok(())
    }

    /// Checks the header and parses the intact records, ignoring a torn tail.
    fn parse(bytes: &[u8], issuer: &PublicKey) -> Result<Vec<(u64, Event)>, StoreError> {
        let unknown = || StoreError::Corrupt {
            offset: 0,
            reason: "missing or unknown journal header".into(),
        };
        let found = bytes.get(..HEADER_LEN).ok_or_else(unknown)?;
        let version = u16::from_be_bytes([found[8], found[9]]);
        if &found[..8] != MAGIC || !(1..=VERSION).contains(&version) {
            return Err(unknown());
        }
        if found[10..] != issuer.as_bytes()[..] {
            return Err(SetupError::KeyMismatch.into());
        }
        let mut records = Vec::new();
        let mut offset = HEADER_LEN;
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
                    reason: "unreadable journal record followed by readable ones".into(),
                });
            }
            break;
        }
        Ok(records)
    }

    /// The events of the record starting at `offset`, numbered, with the
    /// offset just past it, if one is there intact.
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

    /// A journal header and records as a store from before the log wrote
    /// them, one record per event group.
    #[cfg(test)]
    pub(super) fn write(issuer: &PublicKey, version: u16, groups: &[(u64, Vec<Event>)]) -> Vec<u8> {
        let mut journal = MAGIC.to_vec();
        journal.extend_from_slice(&version.to_be_bytes());
        journal.extend_from_slice(issuer.as_bytes());
        for (first, events) in groups {
            let mut record = vec![0; 4];
            record.extend_from_slice(&first.to_be_bytes());
            if version == 1 {
                record.extend_from_slice(&events[0].to_bytes());
            } else {
                record.extend_from_slice(&(events.len() as u32).to_be_bytes());
                for event in events {
                    let event = event.to_bytes();
                    record.extend_from_slice(&(event.len() as u32).to_be_bytes());
                    record.extend_from_slice(&event);
                }
            }
            let len = (record.len() - 4) as u32;
            record[..4].copy_from_slice(&len.to_be_bytes());
            let sum = checksum(&record);
            record.extend_from_slice(&sum);
            journal.extend_from_slice(&record);
        }
        journal
    }
}

/// A mint or transfer could not be committed.
#[derive(Debug, Error)]
pub enum CommitError {
    /// The issuer refused the request. Nothing was written.
    #[error(transparent)]
    Rejected(#[from] Error),
    /// The event could not be written, so it was not applied.
    #[error("log write failed: {0}")]
    Io(#[from] io::Error),
}

/// An [`Issuer`] whose every state change is logged before it takes effect.
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

    /// Opens an existing store, replays its log and compacts it.
    pub fn open(dir: impl AsRef<Path>, key: IssuerKey, policy: Policy) -> Result<Self, StoreError> {
        let (mut store, issuer) = FileStore::open(dir, key, policy)?;
        if !store.log.is_empty() {
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

    /// Rotates once the rotation is on disk, with a fresh generation key
    /// that is saved before the rotation is logged.
    pub fn rotate(&mut self) -> Result<RotationReport, CommitError> {
        self.check_in_step()?;
        let event = self.plan_rotate()?;
        self.commit(&event)?;
        match event {
            Event::Rotate(report) => Ok(report),
            _ => unreachable!("plan_rotate plans a rotation"),
        }
    }

    /// Writes a fresh snapshot and purges the log up to it.
    pub fn compact(&mut self) -> io::Result<()> {
        self.check_in_step()?;
        self.store.compact(&self.issuer)
    }

    /// Events logged since the last snapshot.
    pub fn log_events(&self) -> u64 {
        self.store.log_events()
    }

    /// Plans a rotation with a fresh key and saves the key ring with it.
    fn plan_rotate(&mut self) -> io::Result<Event> {
        let next = GenerationKey::try_generate().map_err(io::Error::other)?;
        let event = self.issuer.plan_rotate(next);
        self.store.save_keys(self.issuer.key_ring())?;
        Ok(event)
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
/// log entry with one flush. See the [module documentation](self).
///
/// Each change is planned and applied at once, so later changes in the batch
/// see earlier ones, and the issuer's [`Issuer::sequence`] moves ahead of the
/// log until [`commit`](Self::commit). Notes and reports a batch returns
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

    /// Mints at a verified mint authority's request. Resending the
    /// authority's last request returns the same note and adds nothing to
    /// the batch.
    pub fn authorized_mint(&mut self, mint: &VerifiedMint) -> Result<Note, Error> {
        let mut notes = match self.owner.issuer.plan_authorized_mint(mint)? {
            Plan::Replay(notes) => notes,
            Plan::Apply(event) => self.stage(event),
        };
        Ok(notes.remove(0))
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

    /// Rotates, with a fresh generation key. The key is saved to the key
    /// ring at once; if that fails, nothing is staged.
    pub fn rotate(&mut self) -> io::Result<RotationReport> {
        let event = self.owner.plan_rotate()?;
        self.stage(event.clone());
        match event {
            Event::Rotate(report) => Ok(report),
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

    /// Writes every staged event in one log entry and flushes it. Only after
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
        // Each event is framed by a 4-byte length in the entry.
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
    use crate::issuer::NoteStatus;
    use crate::keys::{HolderKey, MintKey};
    use crate::ledger::LedgerConfig;
    use crate::log::HEADER_LEN;
    use crate::mint::MintRequest;
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

    fn log_path(dir: &Path) -> PathBuf {
        dir.join(log::LOG)
    }

    #[test]
    fn reopening_replays_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        assert_eq!(p.log_events(), 4);
        assert_eq!(p.store.applied(), Some(LogId::standalone(3)));
        let expected = p.issuer().snapshot();
        drop(p);
        let p = PersistentIssuer::open(dir.path(), key.clone(), Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
        assert_eq!(p.log_events(), 0, "open compacts");
        assert!(p.store.log().is_empty());
        assert_eq!(p.store.log().purged(), Some(LogId::standalone(3)));

        // The numbering carries on after the snapshot.
        let mut p = p;
        p.mint(2, HolderKey::generate().public_key()).unwrap();
        assert_eq!(p.store.applied(), Some(LogId::standalone(4)));
        let expected = p.issuer().snapshot();
        drop(p);
        let (store, issuer) = FileStore::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(issuer.snapshot(), expected);
        assert_eq!(store.log_events(), 1);
    }

    #[test]
    fn rotations_keep_their_keys_on_disk_until_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        let keys_at = |dir: &Path| KeyRing::from_bytes(&fs::read(dir.join(KEYS)).unwrap()).unwrap();
        assert_eq!(keys_at(dir.path()).len(), 4);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(KEYS))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // More rotations than layers: every generation the snapshot knew is
        // dropped, but replaying from it needs every key since.
        activity(&mut p);
        for _ in 0..5 {
            p.rotate().unwrap();
        }
        assert_eq!(p.issuer().key_ring().len(), 4);
        assert_eq!(keys_at(dir.path()).len(), 10);
        let expected = p.issuer().snapshot();
        drop(p);
        let p = PersistentIssuer::open(dir.path(), key.clone(), Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
        // Opening compacted, which erased the dropped generations' keys.
        assert_eq!(keys_at(dir.path()).len(), 4);
        let live: Vec<_> = p
            .issuer()
            .keys()
            .certificates()
            .iter()
            .map(|c| c.key)
            .collect();
        let ring = keys_at(dir.path());
        assert!(live.iter().all(|key| ring.get(key).is_some()));
    }

    #[test]
    fn a_rotation_that_never_reached_the_log_leaves_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        let expected = p.issuer().snapshot();
        let mut batch = p.batch().unwrap();
        batch.rotate().unwrap();
        // Dropped uncommitted, as if the process died before the flush.
        drop(batch);
        drop(p);
        let ring = KeyRing::from_bytes(&fs::read(dir.path().join(KEYS)).unwrap()).unwrap();
        assert_eq!(ring.len(), 5);
        let p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
        assert_eq!(p.issuer().key_ring().len(), 4);
        let ring = KeyRing::from_bytes(&fs::read(dir.path().join(KEYS)).unwrap()).unwrap();
        assert_eq!(ring.len(), 4);
    }

    #[test]
    fn a_store_from_before_generation_keys_opens_and_starts_keeping_them() {
        let dir = tempfile::tempdir().unwrap();
        let key = IssuerKey::generate();
        let mut issuer = Issuer::new(key.clone(), LEDGER, Policy::cash_like()).unwrap();
        issuer.forget_generation_keys();
        let alice = HolderKey::generate();
        let note = issuer.mint(8, alice.public_key()).unwrap();
        drop(FileStore::create(dir.path(), &issuer).unwrap());
        fs::remove_file(dir.path().join(KEYS)).unwrap();

        let mut p = PersistentIssuer::open(dir.path(), key.clone(), Policy::cash_like()).unwrap();
        assert!(matches!(
            p.issuer().status(&note),
            Ok(NoteStatus::Live { .. })
        ));
        let report = p.rotate().unwrap();
        let ring = KeyRing::from_bytes(&fs::read(dir.path().join(KEYS)).unwrap()).unwrap();
        assert!(ring.get(&report.key.unwrap().key).is_some());
        drop(p);
        let p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().keys().certificates(), &[report.key.unwrap()]);
    }

    #[test]
    fn a_store_whose_keys_were_lost_does_not_open() {
        let dir = tempfile::tempdir().unwrap();
        let (key, p) = fresh(dir.path());
        drop(p);
        fs::remove_file(dir.path().join(KEYS)).unwrap();
        assert!(matches!(
            FileStore::open(dir.path(), key, Policy::cash_like()),
            Err(StoreError::Setup(SetupError::MissingKey { generation: 0 }))
        ));
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
    fn create_refuses_a_leftover_log() {
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
    fn a_torn_final_entry_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        let len = fs::metadata(log_path(dir.path())).unwrap().len();
        drop(p);
        // Cut the last entry (a mint) short.
        let file = OpenOptions::new()
            .write(true)
            .open(log_path(dir.path()))
            .unwrap();
        file.set_len(len - 30).unwrap();
        drop(file);
        let (store, issuer) = FileStore::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(issuer.sequence(), 3);
        assert_eq!(store.log_events(), 3);
        assert_eq!(store.applied(), Some(LogId::standalone(2)));
        assert_eq!(
            fs::metadata(log_path(dir.path())).unwrap().len(),
            store.log().bytes()
        );
    }

    #[test]
    fn damage_before_the_last_entry_is_reported() {
        for offset in [HEADER_LEN as usize + 40, HEADER_LEN as usize] {
            let dir = tempfile::tempdir().unwrap();
            let (key, mut p) = fresh(dir.path());
            activity(&mut p);
            drop(p);
            let mut bytes = fs::read(log_path(dir.path())).unwrap();
            bytes[offset] ^= 0x80;
            fs::write(log_path(dir.path()), bytes).unwrap();
            assert!(matches!(
                FileStore::open(dir.path(), key, Policy::cash_like()),
                Err(StoreError::Corrupt { offset, .. }) if offset == HEADER_LEN
            ));
        }
    }

    #[test]
    fn garbage_after_the_last_entry_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        drop(p);
        let mut bytes = fs::read(log_path(dir.path())).unwrap();
        bytes.extend_from_slice(&[0; 100]);
        fs::write(log_path(dir.path()), bytes).unwrap();
        let (_, issuer) = FileStore::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(issuer.sequence(), 4);
    }

    #[test]
    fn a_log_cannot_replay_onto_another_issuer() {
        let dir = tempfile::tempdir().unwrap();
        let (_, mut p) = fresh(dir.path());
        activity(&mut p);
        drop(p);
        let log = fs::read(log_path(dir.path())).unwrap();
        // A second issuer's store, with the first issuer's log copied in.
        let other = tempfile::tempdir().unwrap();
        let (key, p) = fresh(other.path());
        drop(p);
        fs::write(log_path(other.path()), log).unwrap();
        assert!(matches!(
            FileStore::open(other.path(), key, Policy::cash_like()),
            Err(StoreError::Setup(SetupError::KeyMismatch))
        ));
    }

    #[test]
    fn entries_already_in_the_snapshot_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        let log = fs::read(log_path(dir.path())).unwrap();
        // Simulate a crash after the snapshot was replaced but before the
        // log was purged.
        p.compact().unwrap();
        let expected = p.issuer().snapshot();
        drop(p);
        fs::write(log_path(dir.path()), log).unwrap();
        let (store, issuer) =
            FileStore::open(dir.path(), key.clone(), Policy::cash_like()).unwrap();
        assert_eq!(issuer.snapshot(), expected);
        assert_eq!(store.log_events(), 0);
        assert_eq!(store.log().len(), 4);
        drop(store);
        let mut p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert!(p.store.log().is_empty());
        p.mint(2, HolderKey::generate().public_key()).unwrap();
        assert_eq!(p.store.applied(), Some(LogId::standalone(4)));
    }

    #[test]
    fn a_log_that_ends_before_the_snapshot_carries_on_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        p.compact().unwrap();
        drop(p);
        // An empty log numbered from 0, behind the snapshot of entry 3.
        drop(FileLog::create(dir.path(), &key.public_key()).unwrap());
        let mut p = PersistentIssuer::open(dir.path(), key.clone(), Policy::cash_like()).unwrap();
        assert_eq!(p.store.log().next_index(), 4);
        p.mint(2, HolderKey::generate().public_key()).unwrap();
        let expected = p.issuer().snapshot();
        drop(p);
        let (_, issuer) = FileStore::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(issuer.snapshot(), expected);
    }

    #[test]
    fn a_log_purged_past_the_snapshot_does_not_open() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        drop(p);
        let (mut log, _) = FileLog::open(dir.path(), &key.public_key()).unwrap();
        log.purge(LogId::standalone(1)).unwrap();
        drop(log);
        assert!(matches!(
            FileStore::open(dir.path(), key, Policy::cash_like()),
            Err(StoreError::Corrupt { reason, .. }) if reason.contains("entry 1 was purged")
        ));
    }

    #[test]
    fn membership_and_blank_entries_are_kept_through_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        p.mint(8, HolderKey::generate().public_key()).unwrap();
        let change = LogId {
            term: 1,
            node: 7,
            index: 1,
        };
        let blank = LogId { index: 2, ..change };
        p.store
            .log
            .append(&[
                Entry {
                    id: change,
                    payload: Payload::Membership(b"voters 7, 8, 9".to_vec()),
                },
                Entry {
                    id: blank,
                    payload: Payload::Blank,
                },
            ])
            .unwrap();
        let expected = p.issuer().snapshot();
        drop(p);

        let (store, issuer) =
            FileStore::open(dir.path(), key.clone(), Policy::cash_like()).unwrap();
        assert_eq!(issuer.snapshot(), expected);
        assert_eq!(store.applied(), Some(blank));
        assert_eq!(store.membership(), Some((change, &b"voters 7, 8, 9"[..])));
        drop(store);
        // Opening compacts, and the snapshot keeps both ids.
        drop(PersistentIssuer::open(dir.path(), key.clone(), Policy::cash_like()).unwrap());
        let (store, _) = FileStore::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert!(store.log().is_empty());
        assert_eq!(store.applied(), Some(blank));
        assert_eq!(store.membership(), Some((change, &b"voters 7, 8, 9"[..])));
        assert_eq!(store.log().next_index(), 3);
    }

    #[test]
    fn a_damaged_snapshot_header_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (key, p) = fresh(dir.path());
        drop(p);
        let path = dir.path().join(SNAPSHOT);
        let mut bytes = fs::read(&path).unwrap();
        bytes[20] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            FileStore::open(dir.path(), key, Policy::cash_like()),
            Err(StoreError::Setup(SetupError::Checksum))
        ));
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
    fn a_batch_is_one_entry_and_replays_like_single_events() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        p.mint(2, HolderKey::generate().public_key()).unwrap();
        let before = fs::metadata(log_path(dir.path())).unwrap().len();

        let mut batch = p.batch().unwrap();
        let paid = mint_and_pay(&mut batch);
        let report = batch.rotate().unwrap();
        assert_eq!(batch.len(), 3);
        let bytes = batch.bytes();
        batch.commit().unwrap();
        assert_eq!(report.dropped, 0);
        assert_eq!(p.log_events(), 4);
        assert_eq!(p.store.log().len(), 2);
        // One record: length, id and kind, first sequence and count, the
        // events, checksum.
        let after = fs::metadata(log_path(dir.path())).unwrap().len();
        assert_eq!(
            after - before,
            (4 + ENTRY_HEAD + EVENTS_HEAD + bytes + 8) as u64
        );

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
    fn a_mint_resent_in_the_same_batch_mints_once() {
        let dir = tempfile::tempdir().unwrap();
        let key = IssuerKey::generate();
        let authority = MintKey::generate();
        let policy = Policy {
            mint_authorities: vec![authority.public_key()],
            ..Policy::cash_like()
        };
        let issuer = Issuer::new(key.clone(), LEDGER, policy.clone()).unwrap();
        let mut p = PersistentIssuer::create(dir.path(), issuer).unwrap();
        let holder = HolderKey::generate().public_key();
        let request = MintRequest::sign(&authority, p.public_key(), 1, 8, holder);
        let verified = p.issuer().verifier().verify_mint(&request).unwrap();

        let mut batch = p.batch().unwrap();
        let note = batch.authorized_mint(&verified).unwrap();
        assert_eq!(batch.authorized_mint(&verified).unwrap(), note);
        assert_eq!(batch.len(), 1);
        batch.commit().unwrap();

        let expected = p.issuer().snapshot();
        drop(p);
        let p = PersistentIssuer::open(dir.path(), key, policy).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
        assert!(matches!(
            p.issuer().status(&note),
            Ok(crate::NoteStatus::Live { .. })
        ));
    }

    #[test]
    fn a_torn_batch_is_dropped_whole() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        p.mint(2, HolderKey::generate().public_key()).unwrap();
        let mut batch = p.batch().unwrap();
        mint_and_pay(&mut batch);
        batch.commit().unwrap();
        let len = fs::metadata(log_path(dir.path())).unwrap().len();
        drop(p);
        let file = OpenOptions::new()
            .write(true)
            .open(log_path(dir.path()))
            .unwrap();
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

    /// A store as it was before the log: the issuer's snapshot alone in
    /// `snapshot.bin`, its key ring, and a journal of `version` holding a
    /// mint and a rotation. Returns the issuer as the journal leaves it.
    fn store_from_before_the_log(dir: &Path, key: &IssuerKey, version: u16) -> Issuer {
        let mut issuer = Issuer::new(key.clone(), LEDGER, Policy::cash_like()).unwrap();
        let alice = HolderKey::generate();
        let early = issuer.mint(2, alice.public_key()).unwrap();
        fs::write(dir.join(SNAPSHOT), issuer.snapshot()).unwrap();
        let mint = issuer.plan_mint(8, alice.public_key()).unwrap();
        issuer.apply(&mint).unwrap();
        let rotate = issuer.plan_rotate(GenerationKey::generate());
        // Saved before the rotation applies, which retires the dropped
        // generation's key.
        write_keys(dir, issuer.key_ring()).unwrap();
        issuer.apply(&rotate).unwrap();
        // The first record repeats the snapshot's last event, as a
        // compaction cut short left it.
        let early = Event::Mint(crate::issuer::Issued {
            serial: early.serial(),
            amount: 2,
            holder: alice.public_key(),
        });
        let groups = if version == 1 {
            vec![(1, vec![early]), (2, vec![mint]), (3, vec![rotate])]
        } else {
            vec![(1, vec![early]), (2, vec![mint, rotate])]
        };
        let journal = journal::write(issuer.public_key(), version, &groups);
        fs::write(dir.join(journal::JOURNAL), journal).unwrap();
        issuer
    }

    #[test]
    fn stores_from_before_the_log_are_converted() {
        for version in [1, 2] {
            let dir = tempfile::tempdir().unwrap();
            let key = IssuerKey::generate();
            let issuer = store_from_before_the_log(dir.path(), &key, version);

            let (store, restored) =
                FileStore::open(dir.path(), key.clone(), Policy::cash_like()).unwrap();
            assert_eq!(restored.snapshot(), issuer.snapshot());
            assert!(store.log().is_empty());
            assert_eq!(store.applied(), None);
            assert!(!dir.path().join(journal::JOURNAL).exists());
            drop(store);
            let mut p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
            assert_eq!(p.issuer().snapshot(), issuer.snapshot());
            p.mint(2, HolderKey::generate().public_key()).unwrap();
            assert_eq!(p.store.applied(), Some(LogId::standalone(0)));
        }
    }

    #[test]
    fn a_conversion_cut_short_finishes_on_the_next_open() {
        let dir = tempfile::tempdir().unwrap();
        let key = IssuerKey::generate();
        let issuer = store_from_before_the_log(dir.path(), &key, 2);
        let old_snapshot = fs::read(dir.path().join(SNAPSHOT)).unwrap();
        let old_keys = fs::read(dir.path().join(KEYS)).unwrap();
        let journal = fs::read(dir.path().join(journal::JOURNAL)).unwrap();

        // Stopped after writing the log, before replacing the snapshot: a
        // log with an entry in it, as if a later step had run, is replaced.
        let mut log = FileLog::create(dir.path(), issuer.public_key()).unwrap();
        log.append(&[Entry {
            id: LogId::standalone(0),
            payload: Payload::Blank,
        }])
        .unwrap();
        drop(log);
        let (store, restored) =
            FileStore::open(dir.path(), key.clone(), Policy::cash_like()).unwrap();
        assert_eq!(restored.snapshot(), issuer.snapshot());
        assert!(store.log().is_empty());
        drop(store);

        // Stopped after replacing the snapshot, before removing the journal.
        fs::write(dir.path().join(journal::JOURNAL), &journal).unwrap();
        let (_, restored) = FileStore::open(dir.path(), key.clone(), Policy::cash_like()).unwrap();
        assert_eq!(restored.snapshot(), issuer.snapshot());
        assert!(!dir.path().join(journal::JOURNAL).exists());

        // A journal that does not fit its snapshot is still refused.
        fs::write(dir.path().join(SNAPSHOT), &old_snapshot).unwrap();
        fs::write(dir.path().join(KEYS), &old_keys).unwrap();
        let mint = Event::Mint(crate::issuer::Issued {
            serial: crate::ledger::Serial::new(2, 0),
            amount: 8,
            holder: HolderKey::generate().public_key(),
        });
        let gap = journal::write(issuer.public_key(), 2, &[(9, vec![mint])]);
        fs::write(dir.path().join(journal::JOURNAL), gap).unwrap();
        assert!(matches!(
            FileStore::open(dir.path(), key, Policy::cash_like()),
            Err(StoreError::Replay { index: 9, .. })
        ));
    }
}
