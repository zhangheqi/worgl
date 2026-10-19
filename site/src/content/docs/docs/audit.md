---
title: Exporting a log for audit
description: Print a mint's log, one JSON object per entry, to check who minted what and that the supply adds up.
---

Every change a mint makes is an event in its log, written and flushed before
it is applied: mints with the authority's signature, transfers, burns,
rotations, pauses and resumes. `export-log` prints the log for an audit, one
JSON object per entry, without the sealed generation keys its rotations
carry. It is safe to run while the server serves the mint.

```sh
worgl-server export-log --mint server/mints/<mint>
```

`--mint` is the mint's directory, named by its issuer key. A torn final
record, as a running server may be writing, is skipped, not cut off.

## What an entry looks like

Each line has the entry's `index`, and the `term` and `node` of the leader
that wrote it, as Raft numbers its log; a server on its own writes every
entry under term 0. An entry of events also has `first`, the sequence number
of its first event, and `events`. Shortened:

```json
{"events":[{"authorized_mint":{"authorization":{"authority":"b192…5673","sequence":1792375561649234,"signature":"086e…0100"},"issued":{"amount":8,"holder":"87d8…4c33","serial":{"generation":2,"index":999119}}}}],"first":1,"index":0,"node":0,"term":0}
{"events":[{"transfer":{"id":"4e63…321f","issued":[{"amount":2,"holder":"31d8…0c1b","serial":{"generation":3,"index":967446}},…]}}],"first":2,"index":1,"node":0,"term":0}
{"events":["pause"],"first":3,"index":2,"node":0,"term":0}
{"events":["resume"],"first":4,"index":3,"node":0,"term":0}
```

The server shares one entry among every change waiting at the moment of a
flush, so a busy mint's entries hold many events each. On a cluster, entries
that change the group's membership carry `membership` instead of `events`.

## What it lets you check

- **Who authorized every unit of supply.** Each mint from a mint request is
  logged with the authority's key, sequence number and signature, so anyone
  holding the log can check that every mint was signed for.
- **That the supply adds up.** The mint keeps the value minted, destroyed by
  rotations and burned, so `outstanding = minted − destroyed − burned` can be
  checked at any time. `GET /v1/mints/{mint}/info` and `worgl info` show
  the same four numbers.
- **Every rotation.** A rotation records the generation it dropped, the notes
  and value it destroyed, and for a scheduled one the time it was due.

The log holds only public keys, certificates and signatures. The secrets of
the generation keys live in `keys.bin`, and the sealed copies in rotation
entries are left out of the export, so an export can be handed to an
auditor.

The log covers the changes since the last snapshot. A server writes a fresh
snapshot after `--compact-every` events (100,000 by default) and purges the
log up to it, so export the log before then if you want every entry.
