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
| `kek.key` | yes | Unseals the generation keys that rotations carry in the log. The server does not start without it, and on a cluster it must be identical on every member. |
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

A server whose state directory lacks `kek.key` does not start: `serve`
fails with `…/kek.key is missing`. Restore the file from a backup of the
same directory or, on a cluster, copy it from another member. Only `init`
makes one, and a new key would not open what the old one sealed. Every
member of a cluster must hold the same `kek.key`, so restore a member's from
a backup of the same cluster: a member with another key is refused by the
rest, and both sides log `kek.key differs from member N's`. See
[Running a cluster](../running-a-cluster/#the-key-encryption-key).

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

On a cluster each mint is kept on all three members, and a member that was
away catches up from the others. That covers a failed server, not a deleted
mint or an operator's mistake, so back up a member's directory as well.
