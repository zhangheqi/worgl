---
title: Error codes
description: Every stable error code the server answers with, its HTTP status, what it means and whether to send the request again.
---

Errors answer with a status code and a body like:

```json
{"error": "spent", "message": "note 3:17 is already spent"}
```

`error` is one of the stable codes below, listed on `ApiError::code` in
[`worgl-server`](https://github.com/zhangheqi/worgl/blob/main/crates/worgl-server/src/lib.rs).
`message` is for people and may change. `worgl-client` keeps the code in
`Error::code`, and the TypeScript SDK in `MintError.code`.

## The request itself

| Code | Status | Meaning |
|---|---|---|
| `malformed` | 400, 413, 415 or 422 | The body is not the JSON the route takes, is too large (over 64 KiB), lacks `content-type: application/json`, or spends no notes; or a new mint's ledger, policy or period is refused. |
| `policy` | 400 | The request breaks the mint's policy: too many or too few inputs or outputs, an amount that is zero or not an allowed denomination, or outputs that do not add up to the inputs. |
| `invalid_note` | 400 | An input note is malformed, forged, altered, from another mint, or from a generation the mint has no key for. |
| `duplicate_input` | 400 | The same note appears twice among the inputs. |

## Signatures and tokens

| Code | Status | Meaning |
|---|---|---|
| `unauthorized` | 403 | A holder's signature does not verify or was made by a key that is not the note's, or a mint authority's signature does not verify. |
| `unknown_authority` | 403 | A mint request names a key the policy does not list as a mint authority. |
| `admin_unauthorized` | 401 | The operator or mint admin token is missing or wrong. |
| `unknown_mint` | 404 | No mint here has that issuer key, or the path does not hold an issuer key. On a cluster, also for a moment after a mint is created, until this member has read it. |

## The mint's state

| Code | Status | Meaning |
|---|---|---|
| `spent` | 409 | An input was already spent. |
| `expired` | 409 | An input's layer has been dropped. |
| `frozen` | 409 | An input is in the last layer and cannot move until the next rotation. |
| `unissued` | 409 | The mint signed this note but its ledger has no record of it, which only happens if the mint's state was lost or rolled back. |
| `overdrawn` | 409 | The inputs spend more from a layer than the mint counts as live in it. This happens only after the mint's state was rolled back, as by a restore from a backup, and it handed out the same serials again. See [Backups](../backups/#why-a-lost-state-matters). |
| `stale_mint` | 409 | A mint request reuses a sequence number, or goes below one, that its authority already used. |
| `stale_rotation` | 409 | A scheduled rotation is due no later than the last one logged, which already covered it. |
| `layer_full` | 503 | The layer the outputs would land in has too few free bits left until the next rotation. |
| `paused` | 503 | The mint is paused. Its notes keep their places until it resumes. |
| `busy` | 503 | The mint holds `receipt_capacity` receipts, all younger than `receipt_min_age_secs` or, for a while after a restart, counted so, and takes no new transfer or burn until the oldest may go. The `Retry-After` header says in how many seconds. Nothing was done. |

## The server

| Code | Status | Meaning |
|---|---|---|
| `storage` | 500 | The mint could not record the change, so nothing was applied; or, on a cluster, this member's copy of the mint has stopped. |
| `internal` | 500 | Something else went wrong on the server. |
| `no_leader` | 503 | On a cluster: no member leading the mint, or the cluster's registry, could be reached, or the leader changed while the request was carried out. Nothing was done. |
| `timeout` | 503 | On a cluster: the mint's group or the registry did not answer in time. The change may still be made. |

## Sending a request again

Transfers, burns and mint requests are safe to send again unchanged. The
mint answers a resent transfer or burn from its record of recent ones, and
the latest mint request of an authority with the same note, so nothing
happens twice.

- After `no_leader` or `timeout`, send the same request again.
- After `busy`, send the same request again once the seconds its
  `Retry-After` header gives have passed. Both SDKs wait that long before
  their next try, up to 30 seconds.
- A creation of a mint (`POST /v1/admin/mints`) answered `no_leader` made
  nothing, so send it again. One answered `timeout` may have made a mint the
  answer never named: list the mints before creating another.
- `paused`, `busy`, `layer_full`, `storage` and `internal` may go away
  later. Both SDKs count them, and any other 5xx status, as worth trying
  again: `Error::is_transient` in Rust and `isTransient(error)` in
  TypeScript.
- A transfer or burn sent again is answered as before for at least
  `receipt_min_age_secs` (128 seconds by default), and while it is among the
  last `receipt_capacity` (1,048,576 by default) the mint took. One sent
  again after that is refused as `spent` even though it went through. See
  [receipt_capacity and receipt_min_age_secs](../policy/#receipt_capacity-and-receipt_min_age_secs).
- A rotation does not change a transfer's answer while the mint remembers
  it: a transfer sent again after its inputs' layer was dropped still gets
  the same notes, until those notes expire in turn. A burn sent again is
  answered the same while its inputs' layer is live or one of the last
  `layers − 1` dropped, and refused as `expired` after.
- A transfer sent again and refused as `spent` or `expired` is possibly
  applied: it may have gone through before, sent by the other side or by an
  earlier try whose answer was lost, and the mint no longer keeps the
  answer. For a transfer a purse made or is paid by, both SDKs say what may
  be lost, in `Refused::at_stake` in Rust and `atStake` in TypeScript, and
  `worgl` prints it. If the other side submitted it, it got the mint's
  answer and can hand the notes over: `worgl receive` writes the payer's
  change, and `worgl submit` the payee's notes, each kept with
  `worgl accept` (`Purse::accept` or `purse.accept` in the SDKs).
- A purse acts on the refusal of a transfer it made. One refused as `spent`
  or `expired` with status 409 is dropped, since the note it spends is gone
  either way, but the keys of its change are kept, so change handed over
  later is still taken in. One refused as `policy` while the policy the mint
  publishes refuses it too is cancelled and its note taken back. Any other
  refusal leaves it outgoing, to send again: `unknown_mint` and `unissued`,
  for one, say nothing about the transfer, only about the server, and the
  mint never answers `unauthorized` or `duplicate_input` to a transfer a
  purse signed, so those come from something else, such as a gateway.
  `Refused::outcome` in Rust and `outcome` in TypeScript say what was done,
  and `worgl` prints it (see
  [Refused transfers](../command-line/#refused-transfers)).
