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
//! created it would survive a crash. It [checks](Issuer::check) the event
//! first, so the log never holds one that does not replay.
//! [`compact`](PersistentIssuer::compact) writes a fresh snapshot and purges
//! the log up to it; call it after rotations or whenever
//! [`log_events`](PersistentIssuer::log_events) grows large.
//!
//! A flush costs far more than the work around it, so a busy issuer can share
//! one among many requests with a [`Batch`]: each change takes effect in
//! memory as it is made, and all of them reach the disk in one log entry and
//! one flush when the batch commits. Nothing a batch returns may be handed
//! out before then. If the flush fails, the issuer is ahead of its log and
//! refuses all further changes until
//! [`recover`](PersistentIssuer::recover) rebuilds it from the disk or the
//! store is reopened, either of which restores exactly what reached the
//! disk. If the log could not even be cut back after the failed write, the
//! store is stopped instead: the file may not hold what the store knows of
//! it, so only reopening the store reads it again. See [`StoreState`].
//!
//! Given a [`Kek`], a [`PersistentIssuer`] also seals each new generation
//! key into the log entry of the rotation that certifies it, for replicas
//! that will apply the entry without having made the key. Opening a store
//! with the KEK unseals any such key that `keys.bin` lacks.
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
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::codec::DecodeError;
use crate::error::{ApplyError, Error, SetupError};
use crate::issuer::{Clock, Event, Issuer, Plan, RotationReport};
use crate::kek::{Kek, KekError, SealedKey};
use crate::keys::{GenerationKey, IssuerKey, PUBLIC_KEY_LEN, PublicKey};
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
    /// A logged rotation's key is missing from `keys.bin`, and the sealed
    /// copy in its entry could not be opened.
    #[error(
        "log entry {index} seals the key of generation {generation}, which cannot be unsealed: {error}"
    )]
    Unseal {
        /// The entry's index.
        index: u64,
        /// The generation the key signs.
        generation: u64,
        /// Why it could not be unsealed.
        error: KekError,
    },
}

/// A store's directory apart from its log, for a replication layer that
/// keeps the log itself in a [`FileLog`] and applies entries as they commit.
///
/// It holds the directory's lock while open, so no [`FileStore`] or other
/// `StateFiles` opens the same store.
#[derive(Debug)]
pub struct StateFiles {
    dir: PathBuf,
    _lock: Lock,
}

/// What `snapshot.bin` holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotFile {
    /// The id of the last entry the snapshot holds.
    pub applied: Option<LogId>,
    /// The latest membership change the snapshot holds, with its id.
    pub membership: Option<(LogId, Vec<u8>)>,
    /// The issuer's state, as [`Issuer::snapshot`] encodes it.
    pub issuer: Vec<u8>,
}

impl StateFiles {
    /// Locks the store in `dir`, which must exist.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let dir = dir.as_ref().to_path_buf();
        let lock = lock(&dir)?;
        Ok(Self { dir, _lock: lock })
    }

    /// The store's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The key ring in `keys.bin`, or an empty one for a store from before
    /// generation keys.
    pub fn keys(&self, key: &IssuerKey) -> Result<KeyRing, StoreError> {
        read_keys(&self.dir, key)
    }

    /// Replaces `keys.bin` with `ring`, readable by its owner only.
    pub fn save_keys(&self, ring: &KeyRing) -> io::Result<()> {
        write_keys(&self.dir, ring)
    }

    /// Reads `snapshot.bin`. A store from before the log has a snapshot of
    /// another kind; open it once as a [`FileStore`] to convert it.
    pub fn snapshot(&self) -> Result<SnapshotFile, StoreError> {
        let bytes = fs::read(self.dir.join(SNAPSHOT))?;
        let (applied, membership, issuer) = parse_snapshot(&bytes)?;
        Ok(SnapshotFile {
            applied,
            membership,
            issuer: issuer.to_vec(),
        })
    }

    /// Replaces `snapshot.bin`.
    pub fn save_snapshot(&self, snapshot: &SnapshotFile) -> io::Result<()> {
        write_snapshot(
            &self.dir,
            snapshot.applied,
            snapshot.membership.as_ref(),
            &snapshot.issuer,
        )
    }
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
    /// When the files were written before the store opened, or before it
    /// last reread them.
    written: Written,
    /// By when, by the system's wall clock, the issuer had made every
    /// receipt it holds, if known: when the store last logged a transfer or
    /// burn or, if it has logged none, when it was created or what the files
    /// said when it opened.
    newest_receipt: Option<SystemTime>,
    _lock: Lock,
}

/// When a store's files were last written before it opened, by the system's
/// wall clock, as far as their modification times say. Every receipt the
/// issuer held then was made by the time of the file it came from (see
/// [`FileStore::compact`]).
#[derive(Debug, Clone, Copy, Default)]
struct Written {
    /// The time of `snapshot.bin`.
    snapshot: Option<SystemTime>,
    /// When `log.bin` was last written.
    log: Option<SystemTime>,
    /// How many transfers and burns were replayed from the log, each making
    /// at most one receipt: so at most this many of the newest came from it.
    replayed: usize,
}

impl Written {
    /// When every receipt held was made by, if the files say.
    fn newest(&self) -> Option<SystemTime> {
        match self.replayed {
            0 => self.snapshot,
            _ => Some(self.snapshot?.max(self.log?)),
        }
    }
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
            written: Written::default(),
            newest_receipt: Some(SystemTime::now()),
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
        Self::open_with_kek(dir, key, policy, None)
    }

    /// Opens a store as [`open`](Self::open) does, unsealing with `kek` the
    /// key of any logged rotation that `keys.bin` lacks.
    pub fn open_with_kek(
        dir: impl AsRef<Path>,
        key: IssuerKey,
        policy: Policy,
        kek: Option<&dyn Kek>,
    ) -> Result<(Self, Issuer), StoreError> {
        let dir = dir.as_ref().to_path_buf();
        let lock = lock(&dir)?;
        let (
            OnDisk {
                log,
                applied,
                membership,
                events,
                mut keys,
                written,
            },
            issuer,
        ) = read_store(&dir, key, policy, kek)?;
        // The file may only lose a key that signs no generation once no
        // logged event needs replaying; otherwise the next compaction
        // removes it.
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
                written,
                newest_receipt: written.newest(),
                _lock: lock,
            },
            issuer,
        ))
    }

    /// Reads the snapshot, log and key ring again, as opening does, in
    /// place of what the store knew of them, and returns the issuer they
    /// restore. Unlike opening, it leaves in `keys.bin` a key that signs no
    /// generation, for the next compaction to remove.
    fn reload(
        &mut self,
        key: IssuerKey,
        policy: Policy,
        kek: Option<&dyn Kek>,
    ) -> Result<Issuer, StoreError> {
        let (on_disk, issuer) = read_store(&self.dir, key, policy, kek)?;
        OnDisk {
            log: self.log,
            applied: self.applied,
            membership: self.membership,
            events: self.events,
            keys: self.keys,
            written: self.written,
        } = on_disk;
        Ok(issuer)
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
        self.append_sealed(first, events, &[])
    }

    /// Logs events as [`append_all`](Self::append_all) does, in an entry
    /// that also carries the sealed keys of their rotations.
    pub fn append_sealed(
        &mut self,
        first: u64,
        events: &[Event],
        keys: &[SealedKey],
    ) -> io::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let entry = Entry {
            id: LogId::standalone(self.log.next_index()),
            payload: Payload::Events {
                first,
                events: events.to_vec(),
                keys: keys.to_vec(),
            },
        };
        self.log.append(std::slice::from_ref(&entry))?;
        self.applied = Some(entry.id);
        self.events += events.len() as u64;
        if events.iter().any(makes_receipt) {
            self.newest_receipt = Some(SystemTime::now());
        }
        Ok(())
    }

    /// Replaces the snapshot with `issuer`'s state as of the last entry
    /// applied, purges the log up to that entry and rewrites the key ring
    /// without the keys of dropped generations.
    ///
    /// The snapshot then takes as its modification time the time by which
    /// the issuer had made every receipt it holds, as far as the store
    /// knows: when the store last logged a transfer or burn or, if it has
    /// logged none, when it was created or what the files said when it
    /// opened. So a compaction after changes that make no receipt, such as a
    /// rotation, does not make the receipts look new to the next open's
    /// [`PersistentIssuer::set_clock`]. If the files did not say, the
    /// snapshot keeps the time it was written.
    pub fn compact(&mut self, issuer: &Issuer) -> io::Result<()> {
        write_snapshot(
            &self.dir,
            self.applied,
            self.membership.as_ref(),
            &issuer.snapshot(),
        )?;
        self.backdate_snapshot();
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

    /// Dates `snapshot.bin` back to when the receipts it holds were made by,
    /// if known. If that fails, the next open counts them as made later
    /// than they were, which is safe.
    fn backdate_snapshot(&self) {
        let Some(newest) = self.newest_receipt else {
            return;
        };
        let file = File::options().write(true).open(self.dir.join(SNAPSHOT));
        let _ = file.and_then(|file| file.set_modified(newest));
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

/// What a store's files hold, with its log replayed onto its snapshot.
struct OnDisk {
    log: FileLog,
    applied: Option<LogId>,
    membership: Option<(LogId, Vec<u8>)>,
    events: u64,
    /// What `keys.bin` holds.
    keys: KeyRing,
    written: Written,
}

/// Reads the store in `dir`, whose lock the caller holds, converting one
/// from before the log, and replays its log onto its snapshot.
fn read_store(
    dir: &Path,
    key: IssuerKey,
    policy: Policy,
    kek: Option<&dyn Kek>,
) -> Result<(OnDisk, Issuer), StoreError> {
    // Before anything here writes the log again.
    let log_written = modified(&dir.join(log::LOG));
    let ring = read_keys(dir, &key)?;
    let mut keys = ring.clone();
    let mut bytes = fs::read(dir.join(SNAPSHOT))?;
    if bytes.starts_with(ISSUER_SNAPSHOT_MAGIC) {
        journal::convert(dir, key.clone(), ring.clone(), &bytes, policy.clone())?;
        bytes = fs::read(dir.join(SNAPSHOT))?;
    } else if dir.join(journal::JOURNAL).exists() {
        // Left over from a conversion that stopped once the snapshot held it.
        fs::remove_file(dir.join(journal::JOURNAL))?;
        sync_dir(dir)?;
    }
    // After a conversion, whose snapshot holds what the journal did.
    let mut written = Written {
        snapshot: modified(&dir.join(SNAPSHOT)),
        log: log_written,
        replayed: 0,
    };
    let (mut applied, mut membership, snapshot) = parse_snapshot(&bytes)?;
    let mut issuer = Issuer::restore(key, ring, snapshot, policy)?;
    drop(bytes);

    let (mut log, entries) = FileLog::open(dir, issuer.public_key())?;
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
    let mut unsealed = false;
    for entry in entries {
        // Entries up to the snapshot's are left over from a compaction
        // that stopped before purging them.
        if applied.is_some_and(|applied| entry.id.index <= applied.index) {
            continue;
        }
        unsealed |= unseal_keys(&mut issuer, &entry, kek)?;
        written.replayed += receipts_made(&entry);
        events += replay(&mut issuer, &entry, &mut membership)?;
        applied = Some(entry.id);
    }
    if unsealed {
        keys.extend(issuer.key_ring());
        write_keys(dir, &keys)?;
    }
    if let Some(applied) = applied
        && log.next_index() <= applied.index
    {
        // The log ends before the snapshot's entry, so the next entry
        // must follow that one, not the log's last.
        log.purge(applied)?;
    }
    // A rotation that was planned but never logged leaves its key behind.
    issuer.retire_keys();
    let on_disk = OnDisk {
        log,
        applied,
        membership,
        events,
        keys,
        written,
    };
    Ok((on_disk, issuer))
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
        Payload::Events { first, events, .. } => {
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

/// How many of `entry`'s events are transfers or burns, each of which makes
/// a receipt.
fn receipts_made(entry: &Entry) -> usize {
    let Payload::Events { events, .. } = &entry.payload else {
        return 0;
    };
    events.iter().filter(|event| makes_receipt(event)).count()
}

/// Whether `event` is a transfer or a burn, which makes a receipt.
fn makes_receipt(event: &Event) -> bool {
    matches!(event, Event::Transfer { .. } | Event::Burn { .. })
}

/// When the file at `path` was last written, if its metadata says.
fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

/// Adds to the issuer's ring the sealed keys in `entry` that it lacks, and
/// returns whether there were any.
fn unseal_keys(
    issuer: &mut Issuer,
    entry: &Entry,
    kek: Option<&dyn Kek>,
) -> Result<bool, StoreError> {
    let Payload::Events { keys, .. } = &entry.payload else {
        return Ok(false);
    };
    let mut any = false;
    for sealed in keys {
        if issuer.key_ring().get(&sealed.public).is_some() {
            continue;
        }
        let unseal = |error| StoreError::Unseal {
            index: entry.id.index,
            generation: sealed.generation,
            error,
        };
        let kek = kek.ok_or_else(|| unseal(KekError::Unavailable("none was given".into())))?;
        let key = sealed.unseal(kek, issuer.public_key()).map_err(unseal)?;
        issuer.add_key(key);
        any = true;
    }
    Ok(any)
}

/// A store's lock, held until it is dropped.
#[derive(Debug)]
struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        // A child that another thread of this process is starting shares
        // every open file until it execs, and a lock lasts until every copy
        // of its file is closed. Unlocking ends it for all copies at once, so
        // the store can be opened again straight away.
        let _ = self.0.unlock();
    }
}

fn lock(dir: &Path) -> Result<Lock, StoreError> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))?;
    match file.try_lock() {
        Ok(()) => Ok(Lock(file)),
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

/// Whether a [`PersistentIssuer`] holds in memory what reached its disk.
/// See [`PersistentIssuer::state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreState {
    /// Memory holds what reached the disk, and changes are taken.
    InStep,
    /// A batch took effect in memory but did not reach the disk. Every
    /// change is refused until [`recover`](PersistentIssuer::recover)
    /// rebuilds the issuer from the disk, or the store is reopened. Until
    /// then, [`issuer`](PersistentIssuer::issuer) still shows the batch's
    /// changes.
    Ahead,
    /// A change to the log failed and could not be undone, as when the log
    /// could not be cut back after a failed write, so the file may not hold
    /// what the store knows of it. Every change is refused, and so is
    /// recovery: only reopening the store reads the log again.
    Stopped,
}

/// An [`Issuer`] whose every state change is logged before it takes effect.
#[derive(Debug)]
pub struct PersistentIssuer {
    issuer: Issuer,
    store: FileStore,
    /// Seals the keys of new generations into the log, if set.
    kek: Option<Arc<dyn Kek>>,
    /// Set when a batch took effect in memory but not on disk.
    ahead: bool,
    /// Why the compaction at open failed, until it is
    /// [taken](Self::take_compaction_error).
    compaction_error: Option<io::Error>,
}

impl PersistentIssuer {
    /// Creates a store in `dir` for a new issuer.
    pub fn create(dir: impl AsRef<Path>, issuer: Issuer) -> Result<Self, StoreError> {
        let store = FileStore::create(dir, &issuer)?;
        Ok(Self {
            issuer,
            store,
            kek: None,
            ahead: false,
            compaction_error: None,
        })
    }

    /// Opens an existing store, replays its log and compacts it.
    ///
    /// A compaction that fails, as on a full disk, does not fail the open:
    /// the store opens from the snapshot and log as the compaction left
    /// them, which together hold every event, the log keeps its entries
    /// until the next [`compact`](Self::compact) purges them, and
    /// [`take_compaction_error`](Self::take_compaction_error) tells why it
    /// failed. Only one that leaves the store
    /// [stopped](StoreState::Stopped) fails the open.
    pub fn open(dir: impl AsRef<Path>, key: IssuerKey, policy: Policy) -> Result<Self, StoreError> {
        Self::open_inner(dir, key, policy, None)
    }

    /// Opens an existing store as [`open`](Self::open) does, unsealing with
    /// `kek` the logged keys `keys.bin` lacks, and seals with it the keys of
    /// later rotations. See [`set_kek`](Self::set_kek).
    pub fn open_with_kek(
        dir: impl AsRef<Path>,
        key: IssuerKey,
        policy: Policy,
        kek: Arc<dyn Kek>,
    ) -> Result<Self, StoreError> {
        Self::open_inner(dir, key, policy, Some(kek))
    }

    fn open_inner(
        dir: impl AsRef<Path>,
        key: IssuerKey,
        policy: Policy,
        kek: Option<Arc<dyn Kek>>,
    ) -> Result<Self, StoreError> {
        let (store, issuer) = FileStore::open_with_kek(dir, key, policy, kek.as_deref())?;
        let mut opened = Self {
            issuer,
            store,
            kek,
            ahead: false,
            compaction_error: None,
        };
        if !opened.store.log.is_empty()
            && let Err(error) = opened.compact()
        {
            if opened.state() == StoreState::Stopped {
                return Err(error.into());
            }
            opened.compaction_error = Some(error);
        }
        Ok(opened)
    }

    /// Seals the key of every later rotation with `kek` into the
    /// rotation's log entry, so a replica applying the entry can unseal it.
    /// The key goes to `keys.bin` as well.
    pub fn set_kek(&mut self, kek: Arc<dyn Kek>) {
        self.kek = Some(kek);
    }

    /// Gives the issuer `clock`, by which it keeps each receipt for at least
    /// the policy's `receipt_min_age_secs`; see [`Issuer::set_clock`].
    ///
    /// Every receipt the store held when it opened was made by the time of
    /// the file it came from, by the system's wall clock: those restored
    /// from the snapshot by the time of `snapshot.bin`, which a compaction
    /// dates back to when the receipts in it were made by (see
    /// [`FileStore::compact`]), and the newest, as many as the transfers and
    /// burns replayed from the log, by the time `log.bin` was last written,
    /// if that is later. Each counts as made then, or `receipt_min_age_secs`
    /// ago if that was longer ago, and as made now if the file's time is
    /// unknown or ahead of the wall clock. Call it right after opening the
    /// store, before any change.
    pub fn set_clock(&mut self, clock: Clock) {
        let min_age = u64::from(self.issuer.policy().receipt_min_age_secs);
        let age = |written: Option<SystemTime>| {
            written
                .and_then(|written| SystemTime::now().duration_since(written).ok())
                .map_or(0, |age| age.as_secs().min(min_age))
        };
        let written = self.store.written;
        let snapshot = age(written.snapshot);
        let log = age(written.log).min(snapshot);
        self.issuer
            .set_clock_split(clock, snapshot, written.replayed, log);
    }

    /// Starts a group commit. See [`Batch`].
    pub fn batch(&mut self) -> io::Result<Batch<'_>> {
        self.check_in_step()?;
        Ok(Batch {
            first: self.issuer.sequence() + 1,
            owner: self,
            events: Vec::new(),
            keys: Vec::new(),
            bytes: 0,
        })
    }

    /// The issuer, for reading state and verifying requests. While the
    /// store is [ahead](StoreState::Ahead), it holds a batch's changes that
    /// never reached the disk, so [`recover`](Self::recover) first, or
    /// refuse to answer reads from it.
    pub fn issuer(&self) -> &Issuer {
        &self.issuer
    }

    /// The issuer's public key.
    pub fn public_key(&self) -> &PublicKey {
        self.issuer.public_key()
    }

    /// Mints a note once the mint is on disk.
    pub fn mint(&mut self, amount: u64, holder: PublicKey) -> Result<Note, CommitError> {
        let event = self.planner()?.plan_mint(amount, holder)?;
        self.commit(&event)?;
        Ok(self.issuer.notes(&event).remove(0))
    }

    /// Mints at a verified mint authority's request once the mint is on disk.
    /// Resending the authority's last request returns the same note and
    /// writes nothing. While the store is not [in step](StoreState::InStep),
    /// it is refused like every change, resent or not.
    pub fn authorized_mint(&mut self, mint: &VerifiedMint) -> Result<Note, CommitError> {
        let mut notes = match self.planner()?.plan_authorized_mint(mint)? {
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
    /// While the store is not [in step](StoreState::InStep), it is refused
    /// like every change, resent or not.
    pub fn transfer(&mut self, transfer: &VerifiedTransfer) -> Result<Vec<Note>, CommitError> {
        match self.planner()?.plan_transfer(transfer)? {
            Plan::Replay(notes) => Ok(notes),
            Plan::Apply(event) => {
                self.commit(&event)?;
                Ok(self.issuer.notes(&event))
            }
        }
    }

    /// Carries out a verified burn once it is on disk and returns the value
    /// destroyed. A resent burn that was already applied writes nothing.
    /// While the store is not [in step](StoreState::InStep), it is refused
    /// like every change, resent or not.
    pub fn burn(&mut self, burn: &VerifiedBurn) -> Result<u128, CommitError> {
        if let Plan::Apply(event) = self.planner()?.plan_burn(burn)? {
            self.commit(&event)?;
        }
        Ok(burn.value())
    }

    /// Rotates once the rotation is on disk, with a fresh generation key
    /// that is saved before the rotation is logged.
    pub fn rotate(&mut self) -> Result<RotationReport, CommitError> {
        self.rotate_due(None)
    }

    /// Rotates for a schedule's period that was due at `due` seconds since
    /// the Unix epoch, which must be after the last one logged. See
    /// [`Issuer::plan_scheduled_rotation`]; otherwise as
    /// [`rotate`](Self::rotate).
    pub fn rotate_scheduled(&mut self, due: u64) -> Result<RotationReport, CommitError> {
        self.rotate_due(Some(due))
    }

    /// Pauses the issuer once the pause is on disk, or resumes it if
    /// `paused` is false. Writes nothing if it already is. While the store
    /// is not [in step](StoreState::InStep), it is refused like every
    /// change, even where it would write nothing.
    pub fn set_paused(&mut self, paused: bool) -> Result<(), CommitError> {
        if let Some(event) = self.planner()?.plan_pause(paused) {
            self.commit(&event)?;
        }
        Ok(())
    }

    /// Writes a fresh snapshot and purges the log up to it. The snapshot
    /// keeps when its receipts were made by; see [`FileStore::compact`].
    pub fn compact(&mut self) -> io::Result<()> {
        self.check_in_step()?;
        self.store.compact(&self.issuer)
    }

    /// Events logged since the last snapshot.
    pub fn log_events(&self) -> u64 {
        self.store.log_events()
    }

    /// Takes the error of the compaction [`open`](Self::open) tried, if it
    /// failed; later calls return `None`.
    pub fn take_compaction_error(&mut self) -> Option<io::Error> {
        self.compaction_error.take()
    }

    /// Whether memory holds what reached the disk. See [`StoreState`].
    pub fn state(&self) -> StoreState {
        if self.store.log.is_poisoned() {
            StoreState::Stopped
        } else if self.ahead {
            StoreState::Ahead
        } else {
            StoreState::InStep
        }
    }

    /// Rebuilds the issuer from the snapshot and log on disk, as reopening
    /// the store would, once it is [ahead](StoreState::Ahead) of them, so
    /// that it takes changes again. It keeps the lock the store holds and
    /// writes no snapshot. A store in step is left as it is, and a
    /// [stopped](StoreState::Stopped) one refuses, since only reopening it
    /// reads its log again.
    ///
    /// If the issuer was given a clock by [`set_clock`](Self::set_clock),
    /// the rebuilt one keeps it, and counts the receipts it rereads as made
    /// by the times of their files, as on opening.
    ///
    /// If it fails, as when a file cannot be read, the store stays ahead
    /// and this can be tried again.
    pub fn recover(&mut self) -> Result<(), StoreError> {
        if self.state() != StoreState::Ahead {
            // Nothing to do in step; the reason it refuses when stopped.
            return Ok(self.check_in_step()?);
        }
        let key = self.issuer.issuer_key().clone();
        let policy = self.issuer.policy().clone();
        let clock = self.issuer.clock().cloned();
        self.issuer = self.store.reload(key, policy, self.kek.as_deref())?;
        if let Some(clock) = clock {
            self.set_clock(clock);
        }
        self.ahead = false;
        Ok(())
    }

    fn rotate_due(&mut self, due: Option<u64>) -> Result<RotationReport, CommitError> {
        let (report, sealed) = self.plan_rotate(due)?;
        self.commit_sealed(&Event::Rotate(report), sealed.as_slice())?;
        Ok(report)
    }

    /// Plans a rotation with a fresh key, saves the key ring with it and
    /// seals the key if there is a KEK.
    fn plan_rotate(
        &mut self,
        due: Option<u64>,
    ) -> Result<(RotationReport, Option<SealedKey>), CommitError> {
        let issuer = self.planner()?;
        let next = GenerationKey::try_generate().map_err(io::Error::other)?;
        let report = match due {
            Some(due) => issuer.plan_scheduled_rotation(next, due)?,
            None => issuer.plan_rotate(next)?,
        };
        let Event::Rotate(report) = report else {
            unreachable!("a rotation is planned as one")
        };
        let certificate = report.key.expect("a planned rotation certifies its key");
        let sealed = match &self.kek {
            Some(kek) => {
                let key = self
                    .issuer
                    .key_ring()
                    .get(&certificate.key)
                    .expect("planning a rotation adds its key to the ring");
                let sealed = SealedKey::seal(&**kek, self.issuer.public_key(), report.created, key)
                    .map_err(io::Error::other)?;
                Some(sealed)
            }
            None => None,
        };
        self.store.save_keys(self.issuer.key_ring())?;
        Ok((report, sealed))
    }

    /// The issuer, to plan a change on, once the store is in step. Every
    /// change outside a [`Batch`] is planned on it, so that while the store
    /// is not in step the change is refused before it is planned: planning
    /// answers a request that was already applied from what memory holds,
    /// which may then not match the disk. A batch needs no check of its
    /// own, since it starts only in step and stays so until it commits.
    fn planner(&mut self) -> io::Result<&mut Issuer> {
        self.check_in_step()?;
        Ok(&mut self.issuer)
    }

    fn commit(&mut self, event: &Event) -> Result<(), CommitError> {
        self.commit_sealed(event, &[])
    }

    fn commit_sealed(&mut self, event: &Event, keys: &[SealedKey]) -> Result<(), CommitError> {
        // Also checked before planning; this keeps an event planned any
        // other way off a log the store is not in step with.
        self.check_in_step()?;
        // Before it is logged, so the log never holds an event that does
        // not replay.
        self.issuer.check(event).map_err(Error::from)?;
        self.store.append_sealed(
            self.issuer.sequence() + 1,
            std::slice::from_ref(event),
            keys,
        )?;
        self.issuer
            .apply(event)
            .expect("an event that passed the check applies");
        Ok(())
    }

    /// Refuses to go on once memory is ahead of the disk, or the log may
    /// not hold what the store knows of it.
    fn check_in_step(&self) -> io::Result<()> {
        match self.state() {
            StoreState::InStep => Ok(()),
            StoreState::Ahead => Err(io::Error::other(
                "a batch took effect but was not written; recover or reopen the store",
            )),
            StoreState::Stopped => Err(io::Error::other(
                "a change to the log failed and could not be undone; reopen the store",
            )),
        }
    }
}

/// Changes to a [`PersistentIssuer`] that reach the disk together, in one
/// log entry with one flush. See the [module documentation](self).
///
/// Each change is planned and applied at once, so later changes in the batch
/// see earlier ones, and the issuer's [`Issuer::sequence`] moves ahead of the
/// log until [`commit`](Self::commit). Notes and reports a batch returns
/// must not leave the process until the commit succeeds. A batch dropped
/// without committing, or whose commit fails, leaves the issuer
/// [ahead](StoreState::Ahead) of its disk, refusing every further change
/// until it [recovers](PersistentIssuer::recover) or the store is reopened.
/// If the log cannot be cut back after a failed write either, the store is
/// [stopped](StoreState::Stopped) instead and only reopening it helps.
#[derive(Debug)]
pub struct Batch<'a> {
    owner: &'a mut PersistentIssuer,
    first: u64,
    events: Vec<Event>,
    keys: Vec<SealedKey>,
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
        Ok(self.stage(event)?.remove(0))
    }

    /// Mints at a verified mint authority's request. Resending the
    /// authority's last request returns the same note and adds nothing to
    /// the batch.
    pub fn authorized_mint(&mut self, mint: &VerifiedMint) -> Result<Note, Error> {
        let mut notes = match self.owner.issuer.plan_authorized_mint(mint)? {
            Plan::Replay(notes) => notes,
            Plan::Apply(event) => self.stage(event)?,
        };
        Ok(notes.remove(0))
    }

    /// Carries out a verified transfer. A resent request that was already
    /// applied returns the same notes and adds nothing to the batch.
    pub fn transfer(&mut self, transfer: &VerifiedTransfer) -> Result<Vec<Note>, Error> {
        match self.owner.issuer.plan_transfer(transfer)? {
            Plan::Replay(notes) => Ok(notes),
            Plan::Apply(event) => self.stage(event),
        }
    }

    /// Carries out a verified burn and returns the value destroyed. A resent
    /// burn that was already applied adds nothing to the batch.
    pub fn burn(&mut self, burn: &VerifiedBurn) -> Result<u128, Error> {
        if let Plan::Apply(event) = self.owner.issuer.plan_burn(burn)? {
            self.stage(event)?;
        }
        Ok(burn.value())
    }

    /// Rotates, with a fresh generation key. The key is saved to the key
    /// ring at once; if that fails, nothing is staged.
    pub fn rotate(&mut self) -> Result<RotationReport, CommitError> {
        self.rotate_due(None)
    }

    /// Rotates for a schedule's period that was due at `due`. See
    /// [`PersistentIssuer::rotate_scheduled`].
    pub fn rotate_scheduled(&mut self, due: u64) -> Result<RotationReport, CommitError> {
        self.rotate_due(Some(due))
    }

    /// Pauses the issuer, or resumes it if `paused` is false. Stages nothing
    /// if it already is. Changes after a pause in the same batch are refused.
    pub fn set_paused(&mut self, paused: bool) {
        if let Some(event) = self.owner.issuer.plan_pause(paused) {
            self.stage(event)
                .expect("plan_pause plans only what applies");
        }
    }

    fn rotate_due(&mut self, due: Option<u64>) -> Result<RotationReport, CommitError> {
        let (report, sealed) = self.owner.plan_rotate(due)?;
        // The key joins the entry only with its rotation.
        self.stage(Event::Rotate(report))?;
        if let Some(sealed) = sealed {
            // The key count once, then generation, public key, length and
            // the sealed bytes.
            if self.keys.is_empty() {
                self.bytes += 4;
            }
            self.bytes += 8 + PUBLIC_KEY_LEN + 4 + sealed.sealed.len();
            self.keys.push(sealed);
        }
        Ok(report)
    }

    /// Events staged so far.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether nothing has been staged.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Encoded size of the events and sealed keys staged so far. Commit
    /// before it would pass [`MAX_BATCH_BYTES`].
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Writes every staged event in one log entry and flushes it. Only after
    /// this succeeds may the batch's results be handed out.
    pub fn commit(mut self) -> io::Result<()> {
        let events = std::mem::take(&mut self.events);
        let keys = std::mem::take(&mut self.keys);
        let written = self.owner.store.append_sealed(self.first, &events, &keys);
        if written.is_err() && !events.is_empty() {
            self.owner.ahead = true;
        }
        written
    }

    /// Applies `event` and adds it to the batch, or adds nothing if it does
    /// not apply.
    fn stage(&mut self, event: Event) -> Result<Vec<Note>, Error> {
        self.owner.issuer.apply(&event)?;
        // Each event is framed by a 4-byte length in the entry.
        self.bytes += 4 + event.to_bytes().len();
        let notes = self.owner.issuer.notes(&event);
        self.events.push(event);
        Ok(notes)
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
    use crate::kek::FileKek;
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
    fn pauses_and_the_schedule_are_logged() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        let holder = HolderKey::generate().public_key();
        p.rotate_scheduled(600).unwrap();
        p.set_paused(true).unwrap();
        let events = p.log_events();
        p.set_paused(true).unwrap();
        assert_eq!(p.log_events(), events, "pausing again writes nothing");
        assert!(matches!(
            p.mint(1, holder),
            Err(CommitError::Rejected(Error::Paused))
        ));
        assert!(matches!(
            p.rotate(),
            Err(CommitError::Rejected(Error::Paused))
        ));
        drop(p);
        let mut p = PersistentIssuer::open(dir.path(), key.clone(), Policy::cash_like()).unwrap();
        assert!(p.issuer().is_paused());
        assert_eq!(p.issuer().last_scheduled_rotation(), Some(600));

        // In a batch, a pause refuses the changes after it.
        let mut batch = p.batch().unwrap();
        batch.set_paused(false);
        batch.mint(1, holder).unwrap();
        batch.set_paused(true);
        assert!(matches!(batch.mint(1, holder), Err(Error::Paused)));
        assert!(matches!(
            batch.rotate_scheduled(1200),
            Err(CommitError::Rejected(Error::Paused))
        ));
        batch.set_paused(false);
        assert!(matches!(
            batch.rotate_scheduled(600),
            Err(CommitError::Rejected(Error::StaleRotation { .. }))
        ));
        batch.rotate_scheduled(1200).unwrap();
        assert_eq!(batch.len(), 5);
        batch.commit().unwrap();
        let expected = p.issuer().snapshot();
        drop(p);
        let p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
        assert!(!p.issuer().is_paused());
        assert_eq!(p.issuer().last_scheduled_rotation(), Some(1200));
    }

    #[test]
    fn a_sealed_rotation_restores_a_key_the_ring_lacks() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        let alice = HolderKey::generate();
        p.rotate().unwrap();
        let kek: Arc<dyn Kek> = Arc::new(FileKek::from_bytes(&[3; 32]));
        p.set_kek(kek.clone());
        p.mint(8, alice.public_key()).unwrap();
        // The ring as a replica holds it before the next rotations.
        let keys_before = fs::read(dir.path().join(KEYS)).unwrap();
        let first = p.rotate().unwrap();
        let mut batch = p.batch().unwrap();
        let second = batch.rotate().unwrap();
        let rotation = Event::Rotate(second).to_bytes().len();
        assert_eq!(batch.bytes(), 4 + rotation + 4 + 8 + 32 + 4 + 73);
        batch.mint(4, alice.public_key()).unwrap();
        batch.commit().unwrap();
        let expected = p.issuer().snapshot();
        let made: Vec<_> = [first, second]
            .iter()
            .map(|report| {
                let public = report.key.unwrap().key;
                p.issuer().key_ring().get(&public).unwrap().clone()
            })
            .collect();

        // Only the rotations made with the KEK carry their keys, sealed.
        let keys: Vec<_> = p
            .store
            .log()
            .read(..)
            .unwrap()
            .into_iter()
            .map(|entry| match entry.payload {
                Payload::Events { keys, .. } => keys,
                _ => panic!("events"),
            })
            .collect();
        assert_eq!(keys.iter().map(Vec::len).collect::<Vec<_>>(), [0, 0, 1, 1]);
        assert_eq!(keys[2][0].generation, first.created);
        assert_eq!(keys[3][0].public, made[1].public_key());
        let log = fs::read(log_path(dir.path())).unwrap();
        for made in &made {
            let secret = made.to_secret_bytes();
            assert!(!log.windows(32).any(|bytes| bytes == secret));
        }
        drop(p);

        // A replica that never had the keys opens only with the KEK.
        fs::write(dir.path().join(KEYS), &keys_before).unwrap();
        assert!(matches!(
            FileStore::open(dir.path(), key.clone(), Policy::cash_like()),
            Err(StoreError::Unseal {
                index: 2,
                error: KekError::Unavailable(_),
                ..
            })
        ));
        let wrong = FileKek::from_bytes(&[4; 32]);
        assert!(matches!(
            FileStore::open_with_kek(dir.path(), key.clone(), Policy::cash_like(), Some(&wrong)),
            Err(StoreError::Unseal {
                error: KekError::Rejected,
                ..
            })
        ));
        let p = PersistentIssuer::open_with_kek(dir.path(), key, Policy::cash_like(), kek).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
        let ring = KeyRing::from_bytes(&fs::read(dir.path().join(KEYS)).unwrap()).unwrap();
        for made in &made {
            assert!(ring.get(&made.public_key()).is_some());
        }
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
    fn a_closed_store_opens_again_while_a_copy_of_its_lock_file_is_open() {
        // As a child that another thread is starting holds one until it execs.
        let dir = tempfile::tempdir().unwrap();
        let (key, p) = fresh(dir.path());
        let copy = p.store._lock.0.try_clone().unwrap();
        drop(p);
        PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        drop(copy);
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
    fn state_files_read_and_replace_what_a_store_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        activity(&mut p);
        p.compact().unwrap();
        let expected = p.issuer().snapshot();
        let ring = p.issuer().key_ring().clone();
        assert!(matches!(
            StateFiles::open(dir.path()),
            Err(StoreError::Locked)
        ));
        drop(p);

        let files = StateFiles::open(dir.path()).unwrap();
        assert!(matches!(
            FileStore::open(dir.path(), key.clone(), Policy::cash_like()),
            Err(StoreError::Locked)
        ));
        assert_eq!(files.dir(), dir.path());
        let snapshot = files.snapshot().unwrap();
        assert_eq!(snapshot.issuer, expected);
        assert_eq!(snapshot.applied, Some(LogId::standalone(3)));
        assert_eq!(snapshot.membership, None);
        assert_eq!(files.keys(&key).unwrap().to_bytes(), ring.to_bytes());

        let id = LogId {
            term: 2,
            node: 1,
            index: 9,
        };
        let replaced = SnapshotFile {
            applied: Some(id),
            membership: Some((id, b"members".to_vec())),
            ..snapshot
        };
        files.save_snapshot(&replaced).unwrap();
        files.save_keys(&KeyRing::new(key.public_key())).unwrap();
        assert_eq!(files.snapshot().unwrap(), replaced);
        assert!(files.keys(&key).unwrap().is_empty());
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

    /// A store whose policy lists `authority` as a mint authority.
    fn fresh_with_authority(
        dir: &Path,
        authority: &MintKey,
    ) -> (IssuerKey, Policy, PersistentIssuer) {
        let key = IssuerKey::generate();
        let policy = Policy {
            mint_authorities: vec![authority.public_key()],
            ..Policy::cash_like()
        };
        let issuer = Issuer::new(key.clone(), LEDGER, policy.clone()).unwrap();
        (key, policy, PersistentIssuer::create(dir, issuer).unwrap())
    }

    /// A request of each kind that a store answers again from memory once
    /// it has applied it.
    struct Requests {
        mint: VerifiedMint,
        transfer: VerifiedTransfer,
        burn: VerifiedBurn,
    }

    impl Requests {
        /// Mints the notes that the transfer and the burn spend.
        fn new(p: &mut PersistentIssuer, authority: &MintKey) -> Self {
            let request = MintRequest::sign(
                authority,
                p.public_key(),
                1,
                8,
                HolderKey::generate().public_key(),
            );
            let mint = p.issuer().verifier().verify_mint(&request).unwrap();
            let alice = HolderKey::generate();
            let note = p.mint(8, alice.public_key()).unwrap();
            let output = Output {
                amount: 8,
                holder: HolderKey::generate().public_key(),
            };
            let request = TransferRequest::single(note, &alice, output).unwrap();
            let transfer = p.issuer().verifier().verify(&request).unwrap();
            let note = p.mint(4, alice.public_key()).unwrap();
            let request = BurnRequest::single(note, &alice).unwrap();
            let burn = p.issuer().verifier().verify_burn(&request).unwrap();
            Self {
                mint,
                transfer,
                burn,
            }
        }

        fn stage(&self, batch: &mut Batch<'_>) {
            batch.authorized_mint(&self.mint).unwrap();
            batch.transfer(&self.transfer).unwrap();
            assert_eq!(batch.burn(&self.burn).unwrap(), 4);
        }

        /// Asserts that `p` refuses each of them, applied or not.
        fn assert_refused(&self, p: &mut PersistentIssuer) {
            assert!(matches!(
                p.authorized_mint(&self.mint),
                Err(CommitError::Io(_))
            ));
            assert!(matches!(
                p.transfer(&self.transfer),
                Err(CommitError::Io(_))
            ));
            assert!(matches!(p.burn(&self.burn), Err(CommitError::Io(_))));
        }
    }

    #[test]
    fn a_batch_that_does_not_reach_the_disk_stops_all_changes() {
        // Dropped without committing, or refused by the log.
        for refused in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let authority = MintKey::generate();
            let (key, policy, mut p) = fresh_with_authority(dir.path(), &authority);
            let requests = Requests::new(&mut p, &authority);
            p.set_paused(true).unwrap();
            let on_disk = p.issuer().snapshot();
            let sequence = p.issuer().sequence();

            let mut batch = p.batch().unwrap();
            batch.set_paused(false);
            requests.stage(&mut batch);
            if refused {
                // A sealed key too large for any record.
                batch.keys.push(SealedKey {
                    generation: 0,
                    public: key.public_key(),
                    sealed: vec![0; MAX_PAYLOAD],
                });
                assert!(batch.commit().is_err());
            } else {
                drop(batch);
            }
            assert_eq!(p.state(), StoreState::Ahead);
            assert_eq!(
                p.issuer().sequence(),
                sequence + 4,
                "the batch took effect in memory"
            );
            // The batch's own requests sent again are refused too, rather
            // than answered from memory, as is resuming again.
            requests.assert_refused(&mut p);
            assert!(matches!(p.set_paused(false), Err(CommitError::Io(_))));
            assert!(matches!(
                p.mint(2, HolderKey::generate().public_key()),
                Err(CommitError::Io(_))
            ));
            // Rotations are refused before a key is made and saved for them.
            let keys = fs::read(dir.path().join(KEYS)).unwrap();
            assert!(matches!(p.rotate(), Err(CommitError::Io(_))));
            assert!(matches!(p.rotate_scheduled(1), Err(CommitError::Io(_))));
            assert_eq!(fs::read(dir.path().join(KEYS)).unwrap(), keys);
            assert!(p.batch().is_err());
            assert!(
                p.compact().is_err(),
                "a snapshot would save unwritten changes"
            );
            assert_eq!(p.state(), StoreState::Ahead);
            drop(p);
            let mut p = PersistentIssuer::open(dir.path(), key, policy).unwrap();
            assert_eq!(p.issuer().snapshot(), on_disk);

            // An empty batch writes nothing and does no harm.
            p.batch().unwrap().commit().unwrap();
            drop(p.batch().unwrap());
            p.set_paused(false).unwrap();
            p.mint(2, HolderKey::generate().public_key()).unwrap();
        }
    }

    #[test]
    fn a_change_after_a_lost_batch_is_refused_before_it_is_planned() {
        let dir = tempfile::tempdir().unwrap();
        let (_, mut p) = fresh(dir.path());
        let mut batch = p.batch().unwrap();
        batch.set_paused(true);
        drop(batch);
        // Planned on memory, which is paused while the disk is not, a mint
        // or a rotation would be refused as paused, and pausing again
        // would write nothing.
        assert!(matches!(
            p.mint(2, HolderKey::generate().public_key()),
            Err(CommitError::Io(_))
        ));
        assert!(matches!(p.rotate(), Err(CommitError::Io(_))));
        assert!(matches!(p.set_paused(true), Err(CommitError::Io(_))));
    }

    #[test]
    fn requests_resent_after_a_lost_batch_are_made_once_the_store_recovers() {
        // Recovered in place, or reopened.
        for reopen in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let authority = MintKey::generate();
            let (key, policy, mut p) = fresh_with_authority(dir.path(), &authority);
            let requests = Requests::new(&mut p, &authority);
            let mut batch = p.batch().unwrap();
            requests.stage(&mut batch);
            drop(batch);
            // Answered from memory, they would hand out notes and report a
            // burn that are gone once the store is rebuilt from its disk,
            // and the notes' serials would then be issued to others.
            requests.assert_refused(&mut p);
            if reopen {
                drop(p);
                p = PersistentIssuer::open(dir.path(), key.clone(), policy.clone()).unwrap();
            } else {
                p.recover().unwrap();
            }

            // Sent again now, each is made and reaches the disk; sent once
            // more, it is answered as made.
            let minted = p.authorized_mint(&requests.mint).unwrap();
            let paid = p.transfer(&requests.transfer).unwrap();
            assert_eq!(p.burn(&requests.burn).unwrap(), 4);
            assert_eq!(p.transfer(&requests.transfer).unwrap(), paid);
            drop(p);
            let p = PersistentIssuer::open(dir.path(), key, policy).unwrap();
            let issuer = p.issuer();
            assert!(matches!(
                issuer.status(&minted),
                Ok(NoteStatus::Live { .. })
            ));
            assert_eq!(issuer.mint_sequence(&authority.public_key()), Some(1));
            assert!(matches!(
                issuer.status(&paid[0]),
                Ok(NoteStatus::Live { .. })
            ));
            for spent in [requests.transfer.inputs(), requests.burn.inputs()] {
                assert_eq!(issuer.status(&spent[0]), Ok(NoteStatus::Spent));
            }
            assert_eq!(issuer.supply().burned, 4);
        }
    }

    #[test]
    fn recovering_after_a_failed_append_rebuilds_what_reached_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        let holder = HolderKey::generate();
        p.mint(8, holder.public_key()).unwrap();
        let on_disk = p.issuer().snapshot();

        let mut batch = p.batch().unwrap();
        let lost = mint_and_pay(&mut batch);
        batch.rotate().unwrap();
        // A sealed key too large for any record, so the log refuses the
        // batch's entry.
        batch.keys.push(SealedKey {
            generation: 0,
            public: holder.public_key(),
            sealed: vec![0; MAX_PAYLOAD],
        });
        assert!(batch.commit().is_err());
        assert_eq!(p.state(), StoreState::Ahead);
        assert!(matches!(
            p.issuer().status(&lost),
            Ok(NoteStatus::Live { .. })
        ));
        assert!(matches!(
            p.mint(2, holder.public_key()),
            Err(CommitError::Io(_))
        ));
        assert!(p.compact().is_err());

        // Recovering fails while a file cannot be read, and leaves the
        // store ahead, to be tried again.
        let (snapshot, moved) = (dir.path().join(SNAPSHOT), dir.path().join("moved"));
        fs::rename(&snapshot, &moved).unwrap();
        assert!(p.recover().is_err());
        assert_eq!(p.state(), StoreState::Ahead);
        fs::rename(&moved, &snapshot).unwrap();

        let files =
            || [SNAPSHOT, log::LOG, KEYS].map(|name| fs::read(dir.path().join(name)).unwrap());
        let written = files();
        p.recover().unwrap();
        assert_eq!(p.state(), StoreState::InStep);
        assert_eq!(files(), written, "recovering writes nothing");
        assert_eq!(p.issuer().snapshot(), on_disk);
        assert_eq!(p.issuer().status(&lost), Ok(NoteStatus::Unissued));
        // The rotation's key was saved but never logged: the ring drops it,
        // and the next compaction erases it from the file.
        assert_eq!(p.issuer().key_ring().len(), 4);
        assert_eq!(KeyRing::from_bytes(&written[2]).unwrap().len(), 5);
        // A reopen restores the same, from a copy of the files, which
        // nothing writes to now.
        let copy = tempfile::tempdir().unwrap();
        copy_store(dir.path(), copy.path());
        let reopened =
            PersistentIssuer::open(copy.path(), key.clone(), Policy::cash_like()).unwrap();
        assert_eq!(reopened.issuer().snapshot(), p.issuer().snapshot());

        // Once in step, recovering does nothing, and changes are taken again.
        p.recover().unwrap();
        let mut batch = p.batch().unwrap();
        let paid = mint_and_pay(&mut batch);
        batch.commit().unwrap();
        let expected = p.issuer().snapshot();
        drop(p);
        let p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
        assert!(matches!(
            p.issuer().status(&paid),
            Ok(NoteStatus::Live { .. })
        ));
    }

    #[test]
    fn a_log_that_cannot_be_cut_back_after_a_failed_write_stops_the_store() {
        for batched in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let authority = MintKey::generate();
            let (key, policy, mut p) = fresh_with_authority(dir.path(), &authority);
            let holder = HolderKey::generate().public_key();
            let requests = Requests::new(&mut p, &authority);
            let on_disk = p.issuer().snapshot();

            // Neither the write nor the cut-back after it gets through a
            // read-only handle.
            p.store.log.lose_write_access().unwrap();
            if batched {
                let mut batch = p.batch().unwrap();
                requests.stage(&mut batch);
                assert!(batch.commit().is_err());
            } else {
                assert!(matches!(p.mint(2, holder), Err(CommitError::Io(_))));
            }
            assert_eq!(p.state(), StoreState::Stopped);
            assert!(p.recover().is_err());
            assert_eq!(p.state(), StoreState::Stopped);
            requests.assert_refused(&mut p);
            assert!(matches!(p.mint(2, holder), Err(CommitError::Io(_))));
            assert!(p.batch().is_err());
            assert!(p.compact().is_err());
            drop(p);

            // Reopening reads the log again.
            let mut p = PersistentIssuer::open(dir.path(), key, policy).unwrap();
            assert_eq!(p.issuer().snapshot(), on_disk);
            p.mint(2, holder).unwrap();
        }
    }

    #[test]
    fn a_store_whose_compaction_fails_opens_from_its_snapshot_and_log() {
        // A snapshot, and the log purged after it, are written to a
        // temporary file first, which cannot be made where a directory has
        // its name, as it cannot on a full disk. Either way the log keeps
        // every entry.
        for (blocked, snapshot_written) in [(SNAPSHOT_TMP, false), (log::LOG_TMP, true)] {
            let dir = tempfile::tempdir().unwrap();
            let (key, mut p) = fresh(dir.path());
            activity(&mut p);
            let expected = p.issuer().snapshot();
            drop(p);
            let read = |name| fs::read(dir.path().join(name)).unwrap();
            let (snapshot, logged) = (read(SNAPSHOT), read(log::LOG));
            let blocked = dir.path().join(blocked);
            fs::create_dir(&blocked).unwrap();

            let mut p =
                PersistentIssuer::open(dir.path(), key.clone(), Policy::cash_like()).unwrap();
            assert_eq!(p.state(), StoreState::InStep);
            assert!(p.take_compaction_error().is_some());
            assert!(p.take_compaction_error().is_none());
            assert_eq!(p.issuer().snapshot(), expected);
            assert_eq!(read(log::LOG), logged);
            assert_eq!(read(SNAPSHOT) != snapshot, snapshot_written);
            let since_snapshot = if snapshot_written { 0 } else { 4 };
            assert_eq!(p.log_events(), since_snapshot);
            p.mint(2, HolderKey::generate().public_key()).unwrap();
            assert!(p.compact().is_err());
            assert_eq!(p.state(), StoreState::InStep);

            // Once the files can be written, the next compaction goes
            // through.
            fs::remove_dir(&blocked).unwrap();
            p.compact().unwrap();
            assert_eq!(p.log_events(), 0);
            assert!(p.store.log().is_empty());
            let expected = p.issuer().snapshot();
            drop(p);
            let (store, issuer) = FileStore::open(dir.path(), key, Policy::cash_like()).unwrap();
            assert_eq!(issuer.snapshot(), expected);
            assert!(store.log().is_empty());
        }
    }

    #[test]
    fn an_event_that_does_not_apply_is_neither_logged_nor_staged() {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut p) = fresh(dir.path());
        let holder = HolderKey::generate().public_key();
        let mint = p.issuer().plan_mint(8, holder).unwrap();
        p.commit(&mint).unwrap();
        let events = p.log_events();
        let next = p.store.log().next_index();
        let len = fs::metadata(log_path(dir.path())).unwrap().len();
        let state = p.issuer().snapshot();

        // Its bit is taken now, so the same mint does not apply again.
        assert!(matches!(
            p.commit(&mint),
            Err(CommitError::Rejected(Error::Misfit(_)))
        ));
        let mut batch = p.batch().unwrap();
        assert!(matches!(batch.stage(mint.clone()), Err(Error::Misfit(_))));
        assert!(batch.is_empty());
        drop(batch);
        assert_eq!(p.log_events(), events);
        assert_eq!(p.store.log().next_index(), next);
        assert_eq!(fs::metadata(log_path(dir.path())).unwrap().len(), len);
        assert_eq!(p.issuer().snapshot(), state);

        // The store carries on, and opens again.
        p.mint(2, holder).unwrap();
        let expected = p.issuer().snapshot();
        drop(p);
        let p = PersistentIssuer::open(dir.path(), key, Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
    }

    /// Copies the files of a closed store, as a backup does.
    fn copy_store(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }

    #[test]
    fn a_restored_copy_issues_its_serials_again_and_refuses_overdrawn_spends() {
        let dir = tempfile::tempdir().unwrap();
        let (mint, backup) = (dir.path().join("mint"), dir.path().join("backup"));
        let (key, p) = fresh(&mint);
        drop(p);
        copy_store(&mint, &backup);
        let pay = |p: &mut PersistentIssuer, amounts: [u64; 4]| {
            amounts.map(|amount| {
                let holder = HolderKey::generate();
                (p.mint(amount, holder.public_key()).unwrap(), holder)
            })
        };
        let spend = |p: &PersistentIssuer, (note, holder): &(Note, HolderKey)| {
            let output = Output {
                amount: note.amount(),
                holder: HolderKey::generate().public_key(),
            };
            let request = TransferRequest::single(*note, holder, output).unwrap();
            p.issuer().verifier().verify(&request).unwrap()
        };

        let mut p = PersistentIssuer::open(&mint, key.clone(), Policy::cash_like()).unwrap();
        let alice = pay(&mut p, [4, 32, 64, 8]);
        drop(p);

        // Put back, the copy gives Bob's notes the serials of Alice's.
        fs::remove_dir_all(&mint).unwrap();
        copy_store(&backup, &mint);
        let mut p = PersistentIssuer::open(&mint, key.clone(), Policy::cash_like()).unwrap();
        for (note, _) in &alice {
            assert_eq!(p.issuer().status(note), Ok(NoteStatus::Unissued));
        }
        let bob = pay(&mut p, [32, 4, 64, 8]);
        for ((old, _), (new, _)) in alice.iter().zip(&bob) {
            assert_eq!(old.serial(), new.serial());
            assert!(matches!(
                p.issuer().status(old),
                Ok(NoteStatus::Live { .. })
            ));
        }

        // Whichever note of a serial is spent first wins, the new one or the
        // old one, even where the old one is worth more.
        p.transfer(&spend(&p, &bob[3])).unwrap();
        assert!(matches!(
            p.transfer(&spend(&p, &alice[3])),
            Err(CommitError::Rejected(Error::Spent(_)))
        ));
        p.transfer(&spend(&p, &alice[2])).unwrap();
        assert!(matches!(
            p.transfer(&spend(&p, &bob[2])),
            Err(CommitError::Rejected(Error::Spent(_)))
        ));
        p.transfer(&spend(&p, &alice[1])).unwrap();

        // The mint layer holds 4 now, less than Bob's own 32: planning
        // refuses it, and nothing is written.
        let position = p.issuer().mint_position();
        assert_eq!(p.issuer().layers()[position as usize].value, 4);
        let events = p.log_events();
        let transfer = spend(&p, &bob[0]);
        assert_eq!(
            p.issuer().plan_transfer(&transfer),
            Err(Error::Overdrawn { position })
        );
        assert!(matches!(
            p.transfer(&transfer),
            Err(CommitError::Rejected(Error::Overdrawn { .. }))
        ));
        let burn = BurnRequest::single(bob[0].0, &bob[0].1).unwrap();
        let burn = p.issuer().verifier().verify_burn(&burn).unwrap();
        assert_eq!(
            p.issuer().plan_burn(&burn),
            Err(Error::Overdrawn { position })
        );
        assert!(matches!(
            p.burn(&burn),
            Err(CommitError::Rejected(Error::Overdrawn { .. }))
        ));
        assert_eq!(p.log_events(), events);

        let expected = p.issuer().snapshot();
        drop(p);
        let p = PersistentIssuer::open(&mint, key, Policy::cash_like()).unwrap();
        assert_eq!(p.issuer().snapshot(), expected);
    }

    /// Keeps 2 receipts, each for at least 100 s.
    fn floor() -> Policy {
        Policy {
            receipt_capacity: 2,
            receipt_min_age_secs: 100,
            ..Policy::cash_like()
        }
    }

    /// A payment of a newly minted note, verified.
    fn new_payment(issuer: &Issuer, note: Note, key: &HolderKey) -> VerifiedTransfer {
        let output = Output {
            amount: note.amount(),
            holder: HolderKey::generate().public_key(),
        };
        let request = TransferRequest::single(note, key, output).unwrap();
        issuer.verifier().verify(&request).unwrap()
    }

    /// Sets the modification time of `name` in the store in `dir`.
    fn date(dir: &Path, name: &str, written: SystemTime) {
        let file = File::options().write(true).open(dir.join(name)).unwrap();
        file.set_modified(written).unwrap();
    }

    /// How many seconds `p` holds back a payment of `note`: 0 if it takes it.
    fn held_back(p: &PersistentIssuer, note: Note, key: &HolderKey) -> u32 {
        let verified = new_payment(p.issuer(), note, key);
        match p.issuer().plan_transfer(&verified) {
            Err(Error::Busy { retry_after_secs }) => retry_after_secs,
            Ok(Plan::Apply(_)) => 0,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reopening_counts_receipts_as_made_when_their_files_were_written() {
        let dir = tempfile::tempdir().unwrap();
        let key = IssuerKey::generate();
        let issuer = Issuer::new(key.clone(), LEDGER, floor()).unwrap();
        let mut p = PersistentIssuer::create(dir.path(), issuer).unwrap();
        let alice = HolderKey::generate();
        let notes: Vec<Note> = (0..5)
            .map(|_| p.mint(8, alice.public_key()).unwrap())
            .collect();
        for note in &notes[..2] {
            let verified = new_payment(p.issuer(), *note, &alice);
            p.transfer(&verified).unwrap();
        }
        drop(p);
        let open = || {
            let mut p = PersistentIssuer::open(dir.path(), key.clone(), floor()).unwrap();
            p.set_clock(Clock::new(|| 5000));
            p
        };
        let now = SystemTime::now();
        let ago = |secs| now - std::time::Duration::from_secs(secs);

        // Both receipts came from the log, last written 60 s ago, and none
        // from the snapshot, written 1000 s ago.
        date(dir.path(), SNAPSHOT, ago(1000));
        date(dir.path(), log::LOG, ago(60));
        let p = open();
        assert!((39..=40).contains(&held_back(&p, notes[2], &alice)));
        // Opening compacted them into the snapshot, which keeps the log's
        // time: opening again at once counts them as made then, not now.
        drop(p);
        let p = open();
        assert!((39..=40).contains(&held_back(&p, notes[2], &alice)));
        drop(p);
        // By a clock that has since gone back, written 1000 s from now.
        date(
            dir.path(),
            SNAPSHOT,
            now + std::time::Duration::from_secs(1000),
        );
        assert_eq!(held_back(&open(), notes[2], &alice), 100);

        // Old enough, they make room for a third; the log then holds the
        // third's receipt, the newest, and the snapshot the second's.
        date(dir.path(), SNAPSHOT, ago(1000));
        let mut p = open();
        assert_eq!(held_back(&p, notes[2], &alice), 0);
        p.transfer(&new_payment(p.issuer(), notes[2], &alice))
            .unwrap();
        drop(p);
        // The log was written just now, but the oldest receipt came from
        // the snapshot, so it makes room for a fourth, and the third's
        // receipt then holds a fifth back.
        date(dir.path(), SNAPSHOT, ago(1000));
        let mut p = open();
        assert_eq!(held_back(&p, notes[3], &alice), 0);
        p.transfer(&new_payment(p.issuer(), notes[3], &alice))
            .unwrap();
        assert!((99..=100).contains(&held_back(&p, notes[4], &alice)));
    }

    #[test]
    fn a_compaction_keeps_when_its_receipts_were_made() {
        let dir = tempfile::tempdir().unwrap();
        let key = IssuerKey::generate();
        let issuer = Issuer::new(key.clone(), LEDGER, floor()).unwrap();
        let mut p = PersistentIssuer::create(dir.path(), issuer).unwrap();
        let alice = HolderKey::generate();
        let notes: Vec<Note> = (0..4)
            .map(|_| p.mint(8, alice.public_key()).unwrap())
            .collect();
        for note in &notes[..2] {
            let verified = new_payment(p.issuer(), *note, &alice);
            p.transfer(&verified).unwrap();
        }
        drop(p);
        let open = || {
            let mut p = PersistentIssuer::open(dir.path(), key.clone(), floor()).unwrap();
            p.set_clock(Clock::new(|| 5000));
            p
        };
        let ago = SystemTime::now() - std::time::Duration::from_secs(1000);
        date(dir.path(), SNAPSHOT, ago);
        date(dir.path(), log::LOG, ago);

        // A rotation, a mint and a pause make no receipt, so the snapshots
        // written after them keep the time of the two receipts, and the
        // next open counts them as just as old.
        let mut p = open();
        assert_eq!(held_back(&p, notes[2], &alice), 0);
        p.rotate().unwrap();
        p.compact().unwrap();
        p.mint(8, alice.public_key()).unwrap();
        p.set_paused(true).unwrap();
        p.set_paused(false).unwrap();
        p.compact().unwrap();
        drop(p);
        let mut p = open();
        assert_eq!(held_back(&p, notes[2], &alice), 0);

        // A transfer makes one, so the next snapshot counts both as made
        // when it was logged, and holds a fourth back.
        p.transfer(&new_payment(p.issuer(), notes[2], &alice))
            .unwrap();
        p.compact().unwrap();
        drop(p);
        assert!((99..=100).contains(&held_back(&open(), notes[3], &alice)));
    }

    #[test]
    fn a_batch_counts_the_receipts_it_stages() {
        let dir = tempfile::tempdir().unwrap();
        let key = IssuerKey::generate();
        let issuer = Issuer::new(key, LEDGER, floor()).unwrap();
        let mut p = PersistentIssuer::create(dir.path(), issuer).unwrap();
        p.set_clock(Clock::new(|| 0));
        let alice = HolderKey::generate();
        let notes: Vec<Note> = (0..4)
            .map(|_| p.mint(8, alice.public_key()).unwrap())
            .collect();
        let burn = BurnRequest::single(notes[3], &alice).unwrap();
        let burn = p.issuer().verifier().verify_burn(&burn).unwrap();

        let mut batch = p.batch().unwrap();
        for note in &notes[..2] {
            let verified = new_payment(batch.issuer(), *note, &alice);
            batch.transfer(&verified).unwrap();
        }
        let busy = Error::Busy {
            retry_after_secs: 100,
        };
        let verified = new_payment(batch.issuer(), notes[2], &alice);
        assert_eq!(batch.transfer(&verified), Err(busy.clone()));
        assert_eq!(batch.burn(&burn), Err(busy));
        assert_eq!(batch.len(), 2);
        batch.commit().unwrap();
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
        let rotate = issuer.plan_rotate(GenerationKey::generate()).unwrap();
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
