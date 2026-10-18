//! The Raft log over a mint's [`FileLog`], and the replica's vote beside it.

use std::fmt::Debug;
use std::fs::{self, File};
use std::io::{self, Write};
use std::ops::RangeBounds;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use openraft::storage::{LogFlushed, RaftLogStorage};
use openraft::{
    CommittedLeaderId, EntryPayload, LogState, RaftLogReader, StorageError, StorageIOError, Vote,
};
use worgl::log::{Entry, Payload};
use worgl::{FileLog, PublicKey, StoreError};

use crate::{NodeId, Request, TypeConfig, blocking};

pub(crate) const VOTE: &str = "vote.json";

/// A replica's log: openraft's entries in the mint's `log.bin`, and its
/// vote in `vote.json`.
///
/// Clones share the file, so a clone serves the reads of replication while
/// the original appends.
#[derive(Debug, Clone)]
pub struct LogStore {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    dir: PathBuf,
    log: FileLog,
    vote: Option<Vote<NodeId>>,
}

impl LogStore {
    /// Opens the log in `dir`, which must hold `issuer`'s entries, and the
    /// vote beside it if there is one. The directory's lock must already be
    /// held.
    pub(crate) fn open(dir: &Path, issuer: &PublicKey) -> Result<Self, StoreError> {
        let (log, _) = FileLog::open(dir, issuer)?;
        let vote = match fs::read(dir.join(VOTE)) {
            Ok(bytes) => Some(serde_json::from_slice(&bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is unreadable: {error}", dir.join(VOTE).display()),
                )
            })?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                dir: dir.to_path_buf(),
                log,
                vote,
            })),
        })
    }

    /// The id of the last purged entry, as the log records it.
    pub(crate) fn purged(&self) -> Option<worgl::LogId> {
        self.lock().log.purged()
    }

    /// The log, for one call. A panic while it was held can only have come
    /// before or after a write, which the log itself guards.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + Send>(
        &mut self,
        range: RB,
    ) -> Result<Vec<openraft::Entry<TypeConfig>>, StorageError<NodeId>> {
        let inner = self.lock();
        let entries =
            blocking(|| inner.log.read(range)).map_err(|e| StorageIOError::read_logs(&e))?;
        entries
            .into_iter()
            .map(from_entry)
            .collect::<Result<_, _>>()
            .map_err(|e| StorageIOError::read_logs(&e).into())
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError<NodeId>> {
        let inner = self.lock();
        Ok(LogState {
            last_purged_log_id: inner.log.purged().map(from_id),
            last_log_id: inner.log.last_id().map(from_id),
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> {
        let mut inner = self.lock();
        let bytes = serde_json::to_vec(vote).map_err(|e| StorageIOError::write_vote(&e))?;
        blocking(|| write_atomically(&inner.dir, VOTE, &bytes))
            .map_err(|e| StorageIOError::write_vote(&e))?;
        inner.vote = Some(*vote);
        Ok(())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        Ok(self.lock().vote)
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<TypeConfig>,
    ) -> Result<(), StorageError<NodeId>>
    where
        I: IntoIterator<Item = openraft::Entry<TypeConfig>> + Send,
        I::IntoIter: Send,
    {
        let entries = entries
            .into_iter()
            .map(to_entry)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageIOError::write_logs(&e))?;
        let mut inner = self.lock();
        blocking(|| inner.log.append(&entries)).map_err(|e| StorageIOError::write_logs(&e))?;
        callback.log_io_completed(Ok(()));
        Ok(())
    }

    async fn truncate(
        &mut self,
        log_id: openraft::LogId<NodeId>,
    ) -> Result<(), StorageError<NodeId>> {
        let mut inner = self.lock();
        blocking(|| inner.log.truncate(log_id.index))
            .map_err(|e| StorageIOError::write_logs(&e).into())
    }

    async fn purge(&mut self, log_id: openraft::LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        let mut inner = self.lock();
        blocking(|| inner.log.purge(to_id(&log_id)))
            .map_err(|e| StorageIOError::write_logs(&e).into())
    }
}

/// The worgl id of an openraft log id: its leader's term and node, and its
/// index.
pub(crate) fn to_id(id: &openraft::LogId<NodeId>) -> worgl::LogId {
    worgl::LogId {
        term: id.leader_id.term,
        node: id.leader_id.node_id,
        index: id.index,
    }
}

/// The openraft log id of a worgl id.
pub(crate) fn from_id(id: worgl::LogId) -> openraft::LogId<NodeId> {
    openraft::LogId::new(CommittedLeaderId::new(id.term, id.node), id.index)
}

fn to_entry(entry: openraft::Entry<TypeConfig>) -> Result<Entry, serde_json::Error> {
    let payload = match entry.payload {
        EntryPayload::Blank => Payload::Blank,
        // The leader never proposes an empty batch, and a log entry of
        // events needs one. Nothing distinguishes it from a blank entry.
        EntryPayload::Normal(request) if request.events.is_empty() => Payload::Blank,
        EntryPayload::Normal(Request {
            first,
            events,
            keys,
        }) => Payload::Events {
            first,
            events,
            keys,
        },
        EntryPayload::Membership(membership) => {
            Payload::Membership(serde_json::to_vec(&membership)?)
        }
    };
    Ok(Entry {
        id: to_id(&entry.log_id),
        payload,
    })
}

fn from_entry(entry: Entry) -> Result<openraft::Entry<TypeConfig>, serde_json::Error> {
    let payload = match entry.payload {
        Payload::Blank => EntryPayload::Blank,
        Payload::Events {
            first,
            events,
            keys,
        } => EntryPayload::Normal(Request {
            first,
            events,
            keys,
        }),
        Payload::Membership(bytes) => EntryPayload::Membership(serde_json::from_slice(&bytes)?),
    };
    Ok(openraft::Entry {
        log_id: from_id(entry.id),
        payload,
    })
}

/// Writes `bytes` to a temporary file, flushes it and renames it over the
/// file `name` in `dir`.
pub(crate) fn write_atomically(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    let mut file = File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, dir.join(name))?;
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use openraft::{BasicNode, Membership};
    use worgl::{FileStore, Issuer, IssuerKey, LedgerConfig, Policy};

    use super::*;

    fn store(dir: &Path) -> (Issuer, LogStore) {
        let config = LedgerConfig {
            layers: 4,
            layer_size: 64,
        };
        let issuer = Issuer::new(IssuerKey::generate(), config, Policy::cash_like()).unwrap();
        drop(FileStore::create(dir, &issuer).unwrap());
        let log = LogStore::open(dir, issuer.public_key()).unwrap();
        (issuer, log)
    }

    fn id(term: u64, node: u64, index: u64) -> openraft::LogId<NodeId> {
        openraft::LogId::new(CommittedLeaderId::new(term, node), index)
    }

    fn entries(issuer: &Issuer) -> Vec<openraft::Entry<TypeConfig>> {
        let members = BTreeMap::from([(1, BasicNode::new("a")), (2, BasicNode::new("b"))]);
        let membership = Membership::new(vec![members.keys().copied().collect()], members);
        let event = issuer
            .plan_mint(8, IssuerKey::generate().public_key())
            .unwrap();
        vec![
            openraft::Entry {
                log_id: id(0, 0, 0),
                payload: EntryPayload::Membership(membership),
            },
            openraft::Entry {
                log_id: id(1, 1, 1),
                payload: EntryPayload::Blank,
            },
            openraft::Entry {
                log_id: id(1, 1, 2),
                payload: EntryPayload::Normal(Request {
                    first: 1,
                    events: vec![event],
                    keys: Vec::new(),
                }),
            },
        ]
    }

    /// Appends without a flush callback, which only openraft can make.
    fn append(log: &LogStore, entries: Vec<openraft::Entry<TypeConfig>>) {
        let entries: Vec<_> = entries.into_iter().map(|e| to_entry(e).unwrap()).collect();
        log.inner.lock().unwrap().log.append(&entries).unwrap();
    }

    #[tokio::test]
    async fn entries_round_trip_and_follow_truncation_and_purging() {
        let dir = tempfile::tempdir().unwrap();
        let (issuer, mut log) = store(dir.path());
        let state = log.get_log_state().await.unwrap();
        assert_eq!((state.last_purged_log_id, state.last_log_id), (None, None));

        let written = entries(&issuer);
        append(&log, written.clone());
        let mut reader = log.get_log_reader().await;
        assert_eq!(reader.try_get_log_entries(..).await.unwrap(), written);
        assert_eq!(
            reader.try_get_log_entries(1..2).await.unwrap(),
            written[1..2]
        );
        assert_eq!(
            log.get_log_state().await.unwrap().last_log_id,
            Some(id(1, 1, 2))
        );

        log.truncate(id(1, 1, 2)).await.unwrap();
        assert_eq!(
            log.get_log_state().await.unwrap().last_log_id,
            Some(id(1, 1, 1))
        );
        log.purge(id(1, 1, 1)).await.unwrap();
        let state = log.get_log_state().await.unwrap();
        assert_eq!(state.last_purged_log_id, Some(id(1, 1, 1)));
        assert_eq!(state.last_log_id, Some(id(1, 1, 1)));
        assert!(reader.try_get_log_entries(..).await.unwrap().is_empty());

        // Reopened, the log is as it was left.
        drop((log, reader));
        let mut log = LogStore::open(dir.path(), issuer.public_key()).unwrap();
        let state = log.get_log_state().await.unwrap();
        assert_eq!(state.last_purged_log_id, Some(id(1, 1, 1)));
        assert_eq!(log.purged(), Some(to_id(&id(1, 1, 1))));
    }

    #[tokio::test]
    async fn the_vote_survives_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let (issuer, mut log) = store(dir.path());
        assert_eq!(log.read_vote().await.unwrap(), None);
        let vote = Vote::new_committed(3, 2);
        log.save_vote(&vote).await.unwrap();
        assert_eq!(log.read_vote().await.unwrap(), Some(vote));
        drop(log);
        let mut log = LogStore::open(dir.path(), issuer.public_key()).unwrap();
        assert_eq!(log.read_vote().await.unwrap(), Some(vote));

        fs::write(dir.path().join(VOTE), "{").unwrap();
        assert!(matches!(
            LogStore::open(dir.path(), issuer.public_key()),
            Err(StoreError::Io(e)) if e.kind() == io::ErrorKind::InvalidData
        ));
    }

    #[test]
    fn an_empty_request_is_stored_as_a_blank_entry() {
        let entry = openraft::Entry::<TypeConfig> {
            log_id: id(2, 1, 7),
            payload: EntryPayload::Normal(Request {
                first: 4,
                events: Vec::new(),
                keys: Vec::new(),
            }),
        };
        let stored = to_entry(entry).unwrap();
        assert_eq!(stored.payload, Payload::Blank);
        assert_eq!(from_entry(stored).unwrap().log_id, id(2, 1, 7));
    }
}
