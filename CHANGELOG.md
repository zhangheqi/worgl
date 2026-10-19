# Changelog

## Unreleased

Nothing here is tagged or published yet.

- `worgl`: each generation's notes are signed by a key of its own, certified
  by the issuer key and erased when its layer is dropped. Serial indexes are
  scrambled by a keyed permutation, so they no longer show issue order.
  Minting needs a mint authority's signature over a rising sequence number.
  Holders can burn notes. The journal is replaced by a Raft-style log and
  snapshot, concurrent requests share one flush, and pauses and the rotation
  schedule are events in the log.
- `worgl-server`: hosts many mints, routed by issuer key, each with its own
  admin token, pause and rotation schedule. New generation keys are sealed
  with a key-encryption key. Three servers can run as a cluster, with each
  mint a Raft group of the three, and mints can be created while it runs.
- New crates: `worgl-sdk` (the holder's side), `worgl-client` (typed HTTP
  calls and a purse), `worgl-wasm` with the TypeScript SDK, `worgl-cli` (a
  command-line purse in a passphrase-encrypted file) and `worgl-raft`.

## 0.1.0

First release.

- `worgl`: layered bit-array ledger, 149-byte Ed25519 note format, signed
  transfer requests, separable policy (cash-like and flexible presets),
  event-sourced issuer with value accounting and idempotent retries, and a
  crash-safe snapshot-plus-journal store.
- `worgl-server`: HTTP issuer with admin mint and rotate, scheduled
  rotation, and stable error codes.
