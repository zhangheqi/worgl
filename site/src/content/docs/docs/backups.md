---
title: Backups
description: What a server's state directory holds, which files are secret, and what a backup must include.
---

Everything a server knows is in its state directory. Back up the whole
directory, `keys.bin` included.

## What to keep

| File | Secret | Why it matters |
|---|---|---|
| `operator.token` | yes | Creates and lists mints. |
| `kek.key` | yes | Unseals the generation keys that rotations carry in the log. |
| `mints/<mint>/issuer.key` | yes | The mint's identity. Without it there is no mint. |
| `mints/<mint>/admin.token` | yes | Rotates and pauses the mint. |
| `mints/<mint>/keys.bin` | yes | The live generations' signing keys. Without it the store does not open. |
| `mints/<mint>/snapshot.bin`, `log.bin` | no | The ledger: which notes are live, the supply, every change. |
| `mints/<mint>/policy.json`, `mint.json` | no | The mint's rules and rotation period. |

The snapshot and the log hold only public keys and certificates, so they can
be copied or audited without exposing a secret. The secrets are the files in
mode 0600. `mint.key`, if it is still in the mint's directory, is a mint
authority key the server never reads; keep it wherever you mint from instead.

On a server on its own, a rotation writes its new key to `keys.bin` before
logging the rotation, and the snapshot the server writes right after it
takes the keys of dropped generations out of the file and the rotation out
of the log. A copy of `keys.bin` older than the log it goes with can lack
the newest generation's key; given the key-encryption key, the store
unseals it from the rotation's log entry when it opens. A copy of `keys.bin`
older than the snapshot it goes with can lack a key that nothing recovers,
since the rotation has left the log, and the store then does not open. On a
cluster member, `keys.bin` is written only with a snapshot, so the keys of
rotations logged since the member's last snapshot are in the log alone,
sealed, and the member cannot apply them without `kek.key`. Keep `kek.key`
with the rest.

## A consistent copy

Copy the directory while the server is stopped, so the snapshot, log and
keys match. A command that backs up a mint while it is served is not
available yet. `export-log` reads a log safely while the mint is served, but
it leaves out the sealed keys, so it is for an audit, not a restore. See
[Exporting a log for audit](../audit/).

## Why a lost state matters

The mint stores no notes, only their bits. If its state is lost or rolled
back, notes it signed after the copy are unknown to the ledger: spending one
is refused with `unissued`, and notes spent after the copy are live again.

## How storage fails

The log is tied to its issuer's key and every record is checksummed. A
damaged record is dropped only when nothing readable follows it, which is
the one place a crash can leave a torn write. Damage anywhere else stops the
store from opening rather than silently losing acknowledged transfers.

## On a cluster

Each mint is kept on all three members, and a member that was away catches
up from the others. That covers a failed server, not a deleted mint or an
operator's mistake, so back up the members' directories as well.

Those copies restore the whole cluster to one moment: every member from a
copy of that moment, with every member stopped, before any of them starts.
Either copy every member with every member stopped and restore each from
its own copy, or restore all three from the copy of one member that held
every change another member held; see
[Running a cluster](../running-a-cluster/#backups-and-a-failed-member) for
how to check. Never restore one member on its own, whether the others run
or not. Raft counts on a member never forgetting what it acknowledged to
the others, and one restored from an older copy has forgotten it. Nothing
prevents that, and at most the leader logs a warning containing
`follower log is reverted`. Should it help a member that lacks the same
changes become leader, as when the leader fails and the third member was
away while those changes were made, they are lost: transfers the cluster
answered are undone, and a note paid away can be spent again. To replace a
member that failed, copy one that runs instead; see the same page.
