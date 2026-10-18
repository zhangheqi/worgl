//! Readying a mint's directory to open as a replica for the first time.

use std::path::Path;

use worgl::{FileLog, FileStore, IssuerKey, Kek, Policy, StateFiles, StoreError};

use crate::log_store::VOTE;

/// Whether the replica in `dir` has been part of a group: it keeps a vote
/// from the first election it takes part in.
pub fn has_joined(dir: impl AsRef<Path>) -> bool {
    dir.as_ref().join(VOTE).exists()
}

/// Readies the store in `dir` to open as a [`Replica`](crate::Replica), and
/// returns whether it has never been part of a group. If so, the group
/// still has to be formed, with [`Replica::initialize`](crate::Replica::initialize)
/// on one member once it is open. Doing so on more than one with the same
/// membership is safe, but members that start elections together delay
/// each other, so the others should wait to see whether a leader reaches
/// them first.
///
/// openraft forms a group only from replicas that have applied nothing and
/// hold no log, so a store that has never joined one, a new mint's or one a
/// mint ran on alone, is re-based: its snapshot comes to hold every event,
/// with no log entry named, and its log is emptied. The members must be
/// re-based from copies of the same directory, so that they start from the
/// same state. A store that has joined a group is left as it is.
///
/// Every step leaves a store this can run on again, so a crash part way
/// loses nothing. A store whose log holds entries a group wrote but that
/// lacks the vote, as a copy of only some of a member's files would, is
/// refused rather than re-based, since it would then replay the group's
/// entries a second time.
pub fn prepare(
    dir: impl AsRef<Path>,
    key: IssuerKey,
    policy: Policy,
    kek: &dyn Kek,
) -> Result<bool, StoreError> {
    let dir = dir.as_ref();
    if has_joined(dir) {
        return Ok(false);
    }
    // Opening replays the log onto the snapshot, converting older formats;
    // compacting writes the result into the snapshot and purges the log.
    let (mut store, issuer) = FileStore::open_with_kek(dir, key, policy, Some(kek))?;
    if let Some(applied) = store.applied()
        && applied.term > 0
    {
        return Err(StoreError::Corrupt {
            offset: 0,
            reason: format!(
                "entry {} was written by a Raft group, but {} is missing",
                applied.index,
                dir.join(VOTE).display()
            ),
        });
    }
    store.compact(&issuer)?;
    drop(store);
    let files = StateFiles::open(dir)?;
    // A store whose log ends before its snapshot's entry opens as if
    // compacted up to it, so this step can be redone.
    FileLog::create(dir, issuer.public_key())?;
    let mut snapshot = files.snapshot()?;
    snapshot.applied = None;
    snapshot.membership = None;
    files.save_snapshot(&snapshot)?;
    Ok(true)
}
