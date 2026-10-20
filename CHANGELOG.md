# Changelog

## 0.1.0

First release. `worgl`, `worgl-raft`, `worgl-sdk`, `worgl-client`,
`worgl-server` and `worgl-cli` are on crates.io; `worgl-faucet`,
`worgl-wasm` and the TypeScript SDK are in the repository only.

- `worgl`: layered bit-array ledger, 149-byte Ed25519 note format, signed
  transfer requests, separable policy (cash-like and flexible presets), and
  an event-sourced issuer with value accounting and safe resends, kept as a
  Raft-style log and snapshot in which concurrent requests share one flush.
  Each generation's notes are signed by a key of its own, certified by the
  issuer key and erased when its layer is dropped. Serial indexes are
  scrambled by a keyed permutation, so they do not show issue order. Minting
  needs a mint authority's signature over a rising sequence number, holders
  can burn notes, and pauses and the rotation schedule are events in the
  log.
- `worgl-server`: hosts many mints, routed by issuer key, each with its own
  admin token, pause and rotation schedule, and answers with stable error
  codes. New generation keys are sealed with a key-encryption key. Three
  servers can run as a cluster, with each mint a Raft group of the three,
  and mints can be created while it runs.
- `worgl-raft`: Raft replication of one mint's log across a group of
  replicas, and a registry group that lists a cluster's mints.
- `worgl-sdk`: the holder's side, a note store that requests, pays,
  receives and refreshes notes while keeping the privacy rules.
- `worgl-client`: typed HTTP calls for every route, and a purse.
- `worgl-cli`: `worgl`, a command-line purse in a passphrase-encrypted file.
- `worgl-faucet` hands out notes of a playground mint, within limits. The
  SDKs and `worgl playground` take a note from a faucet, and
  `deploy/playground` runs a playground mint with Docker Compose.
- `worgl-wasm` compiles `worgl-sdk` to WebAssembly for the TypeScript SDK
  in `sdk/typescript`.
