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

A rotation writes its new key to `keys.bin` before logging the rotation, and
keys of dropped generations leave the file at the next snapshot. A backup of
`keys.bin` older than the log it goes with can therefore lack the newest
generation's key. Given the key-encryption key, the store unseals any such
key from the rotation's log entry when it opens, so keep `kek.key` with the
rest.

## A consistent copy

Copy the directory while the server is stopped, so the snapshot, log and
keys match. A command that backs up a mint while it is served is not
available yet. `export-log` reads a log safely while the mint is served, but
it leaves out the sealed keys, so it is for an audit, not a restore. See
[Exporting a log for audit](../audit/).

## Why a lost state matters

The mint stores no notes, only their bits. If its state is lost or rolled
back to a copy, notes spent after the copy are live again, and notes it
signed after the copy are unknown to the ledger, so spending one is refused
with `unissued`. That lasts only until the restored mint hands out their
serials again, which it does: each layer hands out its bits in the same
order as before, and the restored `keys.bin` signs them with the same
generation keys. The notes signed between the copy and the restore come
back and collide with new notes on the same serials. Both are genuinely
signed, so whichever is spent first wins and the other is refused as
`spent`. Where the two differ in amount, the layer's count of live value
drifts from what its notes are worth, and a spend that takes more than that
count is refused as `overdrawn`, even with notes that collided with none.
This lasts until every generation live at the copy has rotated out. Notes of
generations created after the copy were signed with keys the restored
`keys.bin` lacks, so they are refused as `invalid_note`.

Until a restore step can tell the two notes of a serial apart, the only
recovery free of collisions is to rotate every restored mint as many times
as it has layers before the server serves anyone: serve on a loopback
address and post to each mint's rotate route (see
[Rotation](../running-a-server/#rotation)). That drops every generation the
copy knew, so every outstanding note expires, those from before the copy
too, and holders get the value back only if it is minted again.

A whole cluster restored from its members' backups is rolled back the same
way, but serving on a loopback address does not work there: a member passes
a rotation of a mint it does not lead to the leader's API address in
`cluster.json`. Serve the members on their usual addresses, keep clients off
their API ports some other way, such as a firewall that lets only the
members through, and post every mint's rotations to any member.

## How storage fails

The log is tied to its issuer's key and every record is checksummed. A
damaged record is dropped only when nothing readable follows it, which is
the one place a crash can leave a torn write. Damage anywhere else stops the
store from opening rather than silently losing acknowledged transfers.

A write that fails, as on a full disk, is cut back off the log, and the
changes in it are answered as not made. The mint then rereads its state
from the disk, so that it answers from what reached it, and takes changes
again once the disk has room. If even cutting the write back fails, the
mint stops until the server is restarted. A restart with the disk still
full opens from the snapshot and log as they are. See
[When the disk fills up](../running-a-server/#when-the-disk-fills-up).

On a cluster each mint is kept on all three members, and a member that was
away catches up from the others. That covers a failed server, not a deleted
mint or an operator's mistake, so back up a member's directory as well.
