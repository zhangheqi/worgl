//! The log: numbered entries in one append-only file, each stamped with the
//! term and node of the leader that wrote it.
//!
//! A [`FileStore`](crate::FileStore) keeps every change since its snapshot
//! here. An entry carries a batch of issuer [`Event`]s, nothing (a blank
//! entry) or an opaque membership change. The term and node are for
//! replication, as Raft numbers its log: a store used on its own writes
//! every entry under term 0 of node 0, and only ever appends to the log and
//! purges it. Replication also reads entries back for followers and
//! [`truncate`](FileLog::truncate)s entries a new leader overwrote.
//!
//! `log.bin` starts with a header: the magic `WORGLLOG`, a `u16` format
//! version, the issuer public key, the id of the last purged entry (a byte
//! that is 1 if there is one, then its term, node and index as `u64`, all
//! zero if there is none) and the first 8 bytes of the SHA-256 of
//! everything before them. Each entry follows as one record,
//! `len: u32 ‖ term: u64 ‖ node: u64 ‖ index: u64 ‖ kind: u8 ‖ body ‖ checksum`,
//! where `len` counts everything between itself and the checksum, and
//! `checksum` is the first 8 bytes of the SHA-256 of everything before it in
//! the record, length included. Kind 0 is a blank entry with an empty body;
//! kind 1 carries events, its body `first sequence: u64 ‖ count: u32 ‖
//! (event length: u32 ‖ event) × count`; kind 2 is a membership change,
//! whose body the issuer never reads.
//!
//! Indexes count from 0 and follow each other with no gaps: the first entry
//! in the file is the one after the last purged entry. An append writes its
//! records with one write and one flush, so a crash can only damage the end
//! of the log. When opening, an unreadable record with no readable record
//! anywhere after it is taken to be such a torn write and dropped; it was
//! never acknowledged. An unreadable record followed by readable ones is
//! corruption, and opening fails rather than lose acknowledged entries.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::{Bound, RangeBounds};
use std::path::{Path, PathBuf};

use crate::error::SetupError;
use crate::issuer::Event;
use crate::keys::PublicKey;
use crate::store::{StoreError, checksum, sync_dir};

pub(crate) const LOG: &str = "log.bin";
const LOG_TMP: &str = "log.bin.tmp";
const MAGIC: &[u8; 8] = b"WORGLLOG";
const VERSION: u16 = 1;
/// Magic, version, issuer key, last purged id and checksum.
pub(crate) const HEADER_LEN: u64 = 8 + 2 + 32 + ID_LEN as u64 + 8;
/// Bytes of a [`LogId`] with its presence flag.
pub(crate) const ID_LEN: usize = 1 + 24;
/// Bytes of a record's payload ahead of its body: term, node, index and kind.
pub(crate) const ENTRY_HEAD: usize = 24 + 1;
/// Largest payload a record may claim: everything between the length and
/// the checksum.
pub(crate) const MAX_PAYLOAD: usize = 1 << 24;
/// Bytes of an events body ahead of the events: first sequence and count.
pub(crate) const EVENTS_HEAD: usize = 8 + 4;

/// Where an entry sits in the log, and which leader wrote it.
///
/// Raft compares ids by term and node, then by index; two ids with the same
/// index name the same entry only if their terms and nodes match too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LogId {
    /// The term of the leader that wrote the entry.
    pub term: u64,
    /// That leader's node id.
    pub node: u64,
    /// The entry's position in the log, counting from 0.
    pub index: u64,
}

impl LogId {
    /// The id of the entry at `index` in a store used without replication:
    /// term 0 of node 0.
    pub const fn standalone(index: u64) -> Self {
        Self {
            term: 0,
            node: 0,
            index,
        }
    }
}

/// What an entry carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// Nothing, such as the entry a new Raft leader writes to commit its term.
    Blank,
    /// Issuer events, the first numbered `first`, the rest following it.
    Events {
        /// The issuer sequence number of the first event.
        first: u64,
        /// The events, at least one.
        events: Vec<Event>,
    },
    /// A change of the replicas that keep the log, encoded by the
    /// replication layer. The issuer never reads it; the store keeps the
    /// latest one in its snapshot.
    Membership(Vec<u8>),
}

/// One entry of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Where it sits and who wrote it.
    pub id: LogId,
    /// What it carries.
    pub payload: Payload,
}

/// The log in `log.bin`. See the [module documentation](self).
///
/// It takes no lock of its own; whoever opens it must make sure no other
/// process writes the same file, as [`FileStore`](crate::FileStore) does.
#[derive(Debug)]
pub struct FileLog {
    dir: PathBuf,
    file: File,
    issuer: PublicKey,
    purged: Option<LogId>,
    /// The id of every entry in the file and the offset of its record,
    /// oldest first.
    entries: Vec<(LogId, u64)>,
    /// Bytes of the header and intact records.
    end: u64,
    /// Set when a failed change could not be undone, so the file may not
    /// match what is in memory.
    poisoned: bool,
}

impl FileLog {
    /// Creates an empty log in `dir` for `issuer`'s entries, replacing any
    /// log already there.
    pub fn create(dir: impl AsRef<Path>, issuer: &PublicKey) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let header = header(issuer, None);
        replace(&dir, &header)?;
        sync_dir(&dir)?;
        let file = open_file(&dir)?;
        Ok(Self {
            dir,
            file,
            issuer: *issuer,
            purged: None,
            entries: Vec::new(),
            end: HEADER_LEN,
            poisoned: false,
        })
    }

    /// Opens the log in `dir`, which must hold `issuer`'s entries, and
    /// returns it with every entry it holds. A torn final record is cut off.
    pub fn open(
        dir: impl AsRef<Path>,
        issuer: &PublicKey,
    ) -> Result<(Self, Vec<Entry>), StoreError> {
        let dir = dir.as_ref().to_path_buf();
        let mut file = open_file(&dir)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let purged = parse_header(&bytes, issuer)?;
        let mut next = next_after(purged);
        let mut entries = Vec::new();
        let mut ids = Vec::new();
        let mut offset = HEADER_LEN as usize;
        while offset < bytes.len() {
            if let Some((entry, end)) = record_at(&bytes, offset) {
                if entry.id.index != next {
                    return Err(StoreError::Corrupt {
                        offset: offset as u64,
                        reason: format!("entry {} where entry {next} belongs", entry.id.index),
                    });
                }
                ids.push((entry.id, offset as u64));
                entries.push(entry);
                next += 1;
                offset = end;
                continue;
            }
            // Only a torn final write may be unreadable: nothing readable may follow.
            if (offset + 1..bytes.len()).any(|later| record_at(&bytes, later).is_some()) {
                return Err(StoreError::Corrupt {
                    offset: offset as u64,
                    reason: "unreadable record followed by readable ones".into(),
                });
            }
            break;
        }
        if offset < bytes.len() {
            // Drop the torn tail so new records follow the last intact one.
            file.set_len(offset as u64)?;
            file.sync_all()?;
        }
        let log = Self {
            dir,
            file,
            issuer: *issuer,
            purged,
            entries: ids,
            end: offset as u64,
            poisoned: false,
        };
        Ok((log, entries))
    }

    /// Appends `entries`, whose indexes must carry on from
    /// [`next_index`](Self::next_index), and flushes them to disk with one
    /// write. After a crash, a prefix of them may be in the log.
    ///
    /// If the write fails, the log is cut back to its previous length; if
    /// even that fails, the log refuses all further changes.
    pub fn append(&mut self, entries: &[Entry]) -> io::Result<()> {
        self.check_in_step()?;
        let mut bytes = Vec::new();
        let mut added = Vec::with_capacity(entries.len());
        for (next, entry) in (self.next_index()..).zip(entries) {
            if entry.id.index != next {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "entry {} cannot be appended; the next is entry {next}",
                        entry.id.index
                    ),
                ));
            }
            if matches!(&entry.payload, Payload::Events { events, .. } if events.is_empty()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "an events entry needs at least one event",
                ));
            }
            let record = encode_record(entry);
            if record.len() > 4 + MAX_PAYLOAD + 8 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the entry is too large for one record",
                ));
            }
            added.push((entry.id, self.end + bytes.len() as u64));
            bytes.extend_from_slice(&record);
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let written = self
            .file
            .write_all(&bytes)
            .and_then(|()| self.file.sync_data());
        if let Err(error) = written {
            let undone = self
                .file
                .set_len(self.end)
                .and_then(|()| self.file.sync_data());
            self.poisoned = undone.is_err();
            return Err(error);
        }
        self.end += bytes.len() as u64;
        self.entries.extend(added);
        Ok(())
    }

    /// Removes the entry at `index` and every entry after it, as a Raft
    /// follower does with entries a new leader overwrote. Purged entries
    /// cannot be removed this way.
    pub fn truncate(&mut self, index: u64) -> io::Result<()> {
        self.check_in_step()?;
        let first = self.first_index();
        if index >= self.next_index() {
            return Ok(());
        }
        if index < first {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("entries before {first} are purged"),
            ));
        }
        let at = self.entries[(index - first) as usize].1;
        self.file.set_len(at)?;
        // The entries are gone from here on, whether or not the flush succeeds.
        self.entries.truncate((index - first) as usize);
        self.end = at;
        self.file.sync_all()
    }

    /// Removes every entry up to and including `upto`, which a snapshot must
    /// already hold. `upto` may lie past the last entry, as when a snapshot
    /// from the leader replaces the whole log; the next entry then follows
    /// `upto`.
    ///
    /// The kept entries are copied into a new file that replaces the old
    /// one, so this costs as much as they take.
    pub fn purge(&mut self, upto: LogId) -> io::Result<()> {
        self.check_in_step()?;
        if self.purged.is_some_and(|purged| purged.index >= upto.index) {
            return Ok(());
        }
        let first = self.first_index();
        let drop = usize::try_from(upto.index.saturating_add(1).saturating_sub(first))
            .unwrap_or(usize::MAX)
            .min(self.entries.len());
        let from = self.entries.get(drop).map_or(self.end, |&(_, at)| at);
        let mut bytes = header(&self.issuer, Some(upto));
        let kept = (self.end - from) as usize;
        bytes.resize(bytes.len() + kept, 0);
        let mut file = &self.file;
        file.seek(SeekFrom::Start(from))?;
        file.read_exact(&mut bytes[HEADER_LEN as usize..])?;
        replace(&self.dir, &bytes)?;
        // The old file is gone; nothing may be written to it any more.
        match open_file(&self.dir) {
            Ok(file) => self.file = file,
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
        }
        self.purged = Some(upto);
        self.entries.drain(..drop);
        for (_, at) in &mut self.entries {
            *at = *at - from + HEADER_LEN;
        }
        self.end = bytes.len() as u64;
        sync_dir(&self.dir)
    }

    /// The entries whose indexes fall in `range`, as far as the log holds
    /// them.
    pub fn read(&self, range: impl RangeBounds<u64>) -> io::Result<Vec<Entry>> {
        let first = self.first_index();
        let start = match range.start_bound() {
            Bound::Included(&start) => start,
            Bound::Excluded(&start) => start.saturating_add(1),
            Bound::Unbounded => 0,
        }
        .max(first);
        let end = match range.end_bound() {
            Bound::Included(&end) => end.saturating_add(1),
            Bound::Excluded(&end) => end,
            Bound::Unbounded => u64::MAX,
        }
        .min(self.next_index());
        if start >= end {
            return Ok(Vec::new());
        }
        let from = self.entries[(start - first) as usize].1;
        let to = self
            .entries
            .get((end - first) as usize)
            .map_or(self.end, |&(_, at)| at);
        let mut bytes = vec![0; (to - from) as usize];
        let mut file = &self.file;
        file.seek(SeekFrom::Start(from))?;
        file.read_exact(&mut bytes)?;
        let mut entries = Vec::with_capacity((end - start) as usize);
        let mut offset = 0;
        while offset < bytes.len() {
            let (entry, next) = record_at(&bytes, offset).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "the record at byte {} changed on disk",
                        from + offset as u64
                    ),
                )
            })?;
            entries.push(entry);
            offset = next;
        }
        Ok(entries)
    }

    /// The id of the last entry removed by [`purge`](Self::purge).
    pub fn purged(&self) -> Option<LogId> {
        self.purged
    }

    /// The id of the last entry in the log, or of the last purged one if the
    /// log holds none.
    pub fn last_id(&self) -> Option<LogId> {
        self.entries.last().map(|&(id, _)| id).or(self.purged)
    }

    /// The index the next appended entry must have.
    pub fn next_index(&self) -> u64 {
        next_after(self.last_id())
    }

    /// The index of the oldest entry the log holds, or would hold.
    pub fn first_index(&self) -> u64 {
        next_after(self.purged)
    }

    /// How many entries the log holds.
    pub fn len(&self) -> u64 {
        self.entries.len() as u64
    }

    /// Whether the log holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes of the header and the records.
    pub fn bytes(&self) -> u64 {
        self.end
    }

    fn check_in_step(&self) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other(
                "an earlier change to the log failed and could not be undone; reopen it",
            ));
        }
        Ok(())
    }
}

/// The index of the entry after `id`, or 0 after none.
fn next_after(id: Option<LogId>) -> u64 {
    id.map_or(0, |id| id.index + 1)
}

fn open_file(dir: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .append(true)
        .open(dir.join(LOG))
}

/// Writes `bytes` to a temporary file, flushes it and renames it over the
/// log. The caller syncs the directory.
fn replace(dir: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(LOG_TMP);
    let mut file = File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, dir.join(LOG))
}

fn header(issuer: &PublicKey, purged: Option<LogId>) -> Vec<u8> {
    let mut header = Vec::with_capacity(HEADER_LEN as usize);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&VERSION.to_be_bytes());
    header.extend_from_slice(issuer.as_bytes());
    write_id(purged, &mut header);
    let sum = checksum(&header);
    header.extend_from_slice(&sum);
    header
}

/// Checks the header and returns the last purged id.
fn parse_header(bytes: &[u8], issuer: &PublicKey) -> Result<Option<LogId>, StoreError> {
    let corrupt = |reason: &str| StoreError::Corrupt {
        offset: 0,
        reason: reason.into(),
    };
    let header = bytes
        .get(..HEADER_LEN as usize)
        .ok_or_else(|| corrupt("missing log header"))?;
    let (body, sum) = header.split_at(HEADER_LEN as usize - 8);
    if &body[..8] != MAGIC || u16::from_be_bytes([body[8], body[9]]) != VERSION {
        return Err(corrupt("missing or unknown log header"));
    }
    if sum != checksum(body) {
        return Err(corrupt("damaged log header"));
    }
    if body[10..42] != issuer.as_bytes()[..] {
        return Err(SetupError::KeyMismatch.into());
    }
    read_id(&body[42..]).ok_or_else(|| corrupt("damaged log header"))
}

/// Writes an id that may be absent: a byte that is 1 if it is there, then
/// its term, node and index, all zero if it is not.
pub(crate) fn write_id(id: Option<LogId>, out: &mut Vec<u8>) {
    out.push(u8::from(id.is_some()));
    let id = id.unwrap_or(LogId::standalone(0));
    for n in [id.term, id.node, id.index] {
        out.extend_from_slice(&n.to_be_bytes());
    }
}

/// Reads an id written by [`write_id`]: `None` inside if absent, `None`
/// outside if malformed.
pub(crate) fn read_id(bytes: &[u8]) -> Option<Option<LogId>> {
    let n = |k: usize| {
        Some(u64::from_be_bytes(
            bytes.get(1 + 8 * k..9 + 8 * k)?.try_into().ok()?,
        ))
    };
    let id = LogId {
        term: n(0)?,
        node: n(1)?,
        index: n(2)?,
    };
    match bytes.first()? {
        0 => Some(None),
        1 => Some(Some(id)),
        _ => None,
    }
}

fn encode_record(entry: &Entry) -> Vec<u8> {
    let mut record = vec![0; 4];
    for n in [entry.id.term, entry.id.node, entry.id.index] {
        record.extend_from_slice(&n.to_be_bytes());
    }
    match &entry.payload {
        Payload::Blank => record.push(0),
        Payload::Events { first, events } => {
            record.push(1);
            record.extend_from_slice(&first.to_be_bytes());
            record.extend_from_slice(&(events.len() as u32).to_be_bytes());
            for event in events {
                let event = event.to_bytes();
                record.extend_from_slice(&(event.len() as u32).to_be_bytes());
                record.extend_from_slice(&event);
            }
        }
        Payload::Membership(bytes) => {
            record.push(2);
            record.extend_from_slice(bytes);
        }
    }
    let len = (record.len() - 4) as u32;
    record[..4].copy_from_slice(&len.to_be_bytes());
    let sum = checksum(&record);
    record.extend_from_slice(&sum);
    record
}

/// The entry whose record starts at `offset`, with the offset just past it,
/// if one is there intact.
fn record_at(bytes: &[u8], offset: usize) -> Option<(Entry, usize)> {
    let rest = bytes.get(offset..)?;
    let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    if !(ENTRY_HEAD..=MAX_PAYLOAD).contains(&len) {
        return None;
    }
    let record = rest.get(..4 + len + 8)?;
    let (body, sum) = record.split_at(4 + len);
    if sum != checksum(body) {
        return None;
    }
    let n = |at: usize| u64::from_be_bytes(body[at..at + 8].try_into().expect("8 bytes"));
    let id = LogId {
        term: n(4),
        node: n(12),
        index: n(20),
    };
    let rest = &body[4 + ENTRY_HEAD..];
    let payload = match body[28] {
        0 if rest.is_empty() => Payload::Blank,
        1 => {
            let first = u64::from_be_bytes(rest.get(..8)?.try_into().ok()?);
            let count = u32::from_be_bytes(rest.get(8..12)?.try_into().ok()?);
            let mut rest = &rest[EVENTS_HEAD..];
            let mut events = Vec::new();
            for _ in 0..count {
                let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
                events.push(Event::from_bytes(rest.get(4..4 + len)?).ok()?);
                rest = &rest[4 + len..];
            }
            if !rest.is_empty() || events.is_empty() {
                return None;
            }
            Payload::Events { first, events }
        }
        2 => Payload::Membership(rest.to_vec()),
        _ => return None,
    };
    Some((Entry { id, payload }, offset + record.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issuer::{Issued, RotationReport};
    use crate::keys::{HolderKey, IssuerKey};
    use crate::ledger::Serial;

    fn mint(sequence: u64) -> Event {
        Event::Mint(Issued {
            serial: Serial::new(2, sequence as u32),
            amount: 8,
            holder: HolderKey::from_secret_bytes(&[sequence as u8; 32]).public_key(),
        })
    }

    fn entry(term: u64, index: u64) -> Entry {
        Entry {
            id: LogId {
                term,
                node: 1,
                index,
            },
            payload: Payload::Events {
                first: index + 1,
                events: vec![mint(index)],
            },
        }
    }

    fn fresh() -> (tempfile::TempDir, PublicKey, FileLog) {
        let dir = tempfile::tempdir().unwrap();
        let issuer = IssuerKey::generate().public_key();
        let log = FileLog::create(dir.path(), &issuer).unwrap();
        (dir, issuer, log)
    }

    fn reopen(dir: &Path, issuer: &PublicKey) -> (FileLog, Vec<Entry>) {
        FileLog::open(dir, issuer).unwrap()
    }

    #[test]
    fn entries_of_every_kind_round_trip() {
        let (dir, issuer, mut log) = fresh();
        let rotation = Event::Rotate(RotationReport {
            dropped: 0,
            created: 4,
            destroyed_notes: 0,
            destroyed_value: 0,
            key: None,
        });
        let entries = vec![
            Entry {
                id: LogId::standalone(0),
                payload: Payload::Membership(b"three voters".to_vec()),
            },
            Entry {
                id: LogId {
                    term: 1,
                    node: 2,
                    index: 1,
                },
                payload: Payload::Blank,
            },
            Entry {
                id: LogId {
                    term: 1,
                    node: 2,
                    index: 2,
                },
                payload: Payload::Events {
                    first: 1,
                    events: vec![mint(0), rotation],
                },
            },
        ];
        log.append(&entries).unwrap();
        assert_eq!(log.read(..).unwrap(), entries);
        assert_eq!(log.read(1..=1).unwrap(), entries[1..2]);
        assert_eq!(log.read(2..9).unwrap(), entries[2..]);
        assert!(log.read(3..).unwrap().is_empty());
        assert_eq!(log.last_id(), Some(entries[2].id));
        drop(log);

        let (log, read) = reopen(dir.path(), &issuer);
        assert_eq!(read, entries);
        assert_eq!(log.next_index(), 3);
        assert_eq!(
            log.bytes(),
            fs::metadata(dir.path().join(LOG)).unwrap().len()
        );
    }

    #[test]
    fn appends_must_carry_on_from_the_last_index() {
        let (_dir, _, mut log) = fresh();
        log.append(&[entry(1, 0), entry(1, 1)]).unwrap();
        for index in [1, 3] {
            let error = log.append(&[entry(1, index)]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
        // A refused batch writes none of its entries.
        assert!(log.append(&[entry(1, 2), entry(1, 4)]).is_err());
        assert_eq!(log.next_index(), 2);
        let empty = Entry {
            id: LogId::standalone(2),
            payload: Payload::Events {
                first: 3,
                events: Vec::new(),
            },
        };
        assert!(log.append(&[empty]).is_err());
        log.append(&[]).unwrap();
        log.append(&[entry(2, 2)]).unwrap();
    }

    #[test]
    fn truncating_removes_the_tail_for_good() {
        let (dir, issuer, mut log) = fresh();
        log.append(&[entry(1, 0), entry(1, 1), entry(1, 2)])
            .unwrap();
        log.truncate(5).unwrap();
        assert_eq!(log.len(), 3);
        log.truncate(1).unwrap();
        assert_eq!(log.last_id(), Some(entry(1, 0).id));
        // A new leader's entry takes the freed index.
        log.append(&[entry(2, 1)]).unwrap();
        drop(log);
        let (mut log, read) = reopen(dir.path(), &issuer);
        assert_eq!(read, [entry(1, 0), entry(2, 1)]);
        log.truncate(0).unwrap();
        assert!(log.is_empty());
        assert_eq!(log.last_id(), None);
        assert_eq!(log.bytes(), HEADER_LEN);
    }

    #[test]
    fn purging_keeps_the_tail_and_its_numbering() {
        let (dir, issuer, mut log) = fresh();
        let entries: Vec<_> = (0..5).map(|index| entry(1, index)).collect();
        log.append(&entries).unwrap();
        log.purge(entries[2].id).unwrap();
        assert_eq!(log.purged(), Some(entries[2].id));
        assert_eq!(log.first_index(), 3);
        assert_eq!(log.read(..).unwrap(), entries[3..]);
        assert_eq!(log.read(0..4).unwrap(), entries[3..4]);
        // Purging again up to an older entry changes nothing.
        log.purge(entries[1].id).unwrap();
        assert_eq!(log.len(), 2);
        log.append(&[entry(1, 5)]).unwrap();
        assert!(
            log.truncate(2).is_err(),
            "purged entries cannot be truncated"
        );
        drop(log);

        let (mut log, read) = reopen(dir.path(), &issuer);
        assert_eq!(read, [&entries[3..], &[entry(1, 5)]].concat());
        assert_eq!(log.purged(), Some(entries[2].id));
        assert!(!dir.path().join(LOG_TMP).exists());

        // A snapshot past the end replaces the whole log.
        let ahead = LogId {
            term: 3,
            node: 2,
            index: 9,
        };
        log.purge(ahead).unwrap();
        assert!(log.is_empty());
        assert_eq!(log.last_id(), Some(ahead));
        assert_eq!(log.next_index(), 10);
        log.append(&[entry(3, 10)]).unwrap();
        drop(log);
        let (log, read) = reopen(dir.path(), &issuer);
        assert_eq!(read, [entry(3, 10)]);
        assert_eq!(log.purged(), Some(ahead));
    }

    #[test]
    fn a_torn_final_record_is_dropped() {
        let (dir, issuer, mut log) = fresh();
        log.append(&[entry(1, 0), entry(1, 1)]).unwrap();
        let len = log.bytes();
        drop(log);
        let file = OpenOptions::new()
            .write(true)
            .open(dir.path().join(LOG))
            .unwrap();
        file.set_len(len - 30).unwrap();
        drop(file);
        let (mut log, read) = reopen(dir.path(), &issuer);
        assert_eq!(read, [entry(1, 0)]);
        assert_eq!(
            log.bytes(),
            fs::metadata(dir.path().join(LOG)).unwrap().len()
        );
        log.append(&[entry(1, 1)]).unwrap();
        drop(log);
        assert_eq!(reopen(dir.path(), &issuer).1.len(), 2);
    }

    #[test]
    fn garbage_after_the_last_record_is_dropped() {
        let (dir, issuer, mut log) = fresh();
        log.append(&[entry(1, 0)]).unwrap();
        drop(log);
        let path = dir.path().join(LOG);
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(&[0; 100]);
        fs::write(&path, bytes).unwrap();
        assert_eq!(reopen(dir.path(), &issuer).1, [entry(1, 0)]);
    }

    #[test]
    fn damage_before_the_last_record_is_reported() {
        let (dir, issuer, mut log) = fresh();
        log.append(&[entry(1, 0), entry(1, 1)]).unwrap();
        drop(log);
        let path = dir.path().join(LOG);
        let first = HEADER_LEN as usize;
        // Inside the first record's event, and in the high byte of its length.
        for offset in [first + 40, first] {
            let mut bytes = fs::read(&path).unwrap();
            bytes[offset] ^= 0x80;
            let copy = tempfile::tempdir().unwrap();
            fs::write(copy.path().join(LOG), bytes).unwrap();
            assert!(matches!(
                FileLog::open(copy.path(), &issuer),
                Err(StoreError::Corrupt { offset, .. }) if offset == HEADER_LEN
            ));
        }
    }

    #[test]
    fn records_out_of_order_are_corruption() {
        let (dir, issuer, mut log) = fresh();
        log.append(&[entry(1, 0), entry(1, 1)]).unwrap();
        drop(log);
        let path = dir.path().join(LOG);
        let mut bytes = fs::read(&path).unwrap();
        // The first record again, after the second.
        let first = HEADER_LEN as usize;
        let len = u32::from_be_bytes(bytes[first..first + 4].try_into().unwrap()) as usize;
        let again = bytes[first..first + 4 + len + 8].to_vec();
        bytes.extend_from_slice(&again);
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            FileLog::open(dir.path(), &issuer),
            Err(StoreError::Corrupt { reason, .. }) if reason.contains("entry 0 where entry 2")
        ));
    }

    #[test]
    fn a_log_belongs_to_one_issuer() {
        let (dir, _, log) = fresh();
        drop(log);
        let other = IssuerKey::generate().public_key();
        assert!(matches!(
            FileLog::open(dir.path(), &other),
            Err(StoreError::Setup(SetupError::KeyMismatch))
        ));
    }

    #[test]
    fn a_damaged_header_is_reported() {
        let (dir, issuer, log) = fresh();
        drop(log);
        let path = dir.path().join(LOG);
        let mut bytes = fs::read(&path).unwrap();
        bytes[50] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            FileLog::open(dir.path(), &issuer),
            Err(StoreError::Corrupt { offset: 0, .. })
        ));
    }
}
