---
title: Pausing a mint
description: Stop one mint during an incident without costing anyone a note, and resume it.
---

Pausing stops a mint without costing anyone a note. A paused mint refuses
transfers, burns, mints and rotations, scheduled or requested, and still
verifies notes, so nothing moves and nothing expires until it resumes. It is
for incidents such as a leaked key or an attack on one mint; the others
carry on.

## Pause and resume

With the mint's admin token:

```sh
curl -X POST -H "Authorization: Bearer $(cat server/mints/<mint>/admin.token)" \
  http://127.0.0.1:8080/v1/mints/<mint>/pause
curl -X POST -H "Authorization: Bearer $(cat server/mints/<mint>/admin.token)" \
  http://127.0.0.1:8080/v1/mints/<mint>/resume
```

They answer `{"paused": true}` and `{"paused": false}`. From Rust,
`worgl_client::Client` has `pause` and `resume`.

## What everyone sees

- The mint's `info` shows `"paused": true` to everyone, and `worgl info`
  prints `state paused`.
- Refused requests are answered `503 paused`, which clients treat as worth
  trying again later.
- The pause is logged, so it survives restarts, and every replica of a mint
  on a cluster agrees on it.
- Scheduled rotations due while the mint is paused are skipped, not made up
  after it resumes, so no note expires while its holder cannot refresh it.
