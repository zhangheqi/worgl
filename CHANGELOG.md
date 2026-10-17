# Changelog

## 0.1.0

First release.

- `worgl`: layered bit-array ledger, 149-byte Ed25519 note format, signed
  transfer requests, separable policy (cash-like and flexible presets),
  event-sourced issuer with value accounting and idempotent retries, and a
  crash-safe snapshot-plus-journal store.
- `worgl-server`: HTTP issuer with admin mint and rotate, scheduled
  rotation, and stable error codes.
