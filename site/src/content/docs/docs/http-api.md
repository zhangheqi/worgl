---
title: HTTP API
description: Every route worgl-server serves, its token, its body and its answer.
---

All bodies are JSON. Keys and signatures are hex, and notes are unpadded
URL-safe base64. `{mint}` is a mint's issuer key, in hex. A note names its
issuer, so `/v1/verify`, `/v1/transfer` and `/v1/burn` take no mint in their
path: they go to the mint whose key signed the note, or the first input
note.

The request types are the library's own (`TransferRequest`, `BurnRequest`,
`MintRequest`, `Note`, `PublicKey`), so a Rust client signs with the library
and posts the JSON it serializes to. `worgl-client` and the TypeScript SDK
make every call below for you.

## Routes

| Method | Path | Token | Body | Answer |
|---|---|---|---|---|
| GET | `/healthz` | | | `ok` |
| POST | `/v1/verify` | | `{"note"}` | serial, amount, state, position, hops left |
| POST | `/v1/transfer` | | `{"inputs": [{"note", "signature"}], "outputs": [{"amount", "holder"}]}` | `{"notes"}` |
| POST | `/v1/burn` | | `{"inputs": [{"note", "signature"}]}` | `{"value"}` burned |
| GET | `/v1/mints/{mint}/info` | | | layers, supply, policy, rotation schedule, paused |
| GET | `/v1/mints/{mint}/keys` | | | the live generations' keys and their certificates |
| POST | `/v1/mints/{mint}/mint` | | `{"authority", "sequence", "amount", "holder", "signature"}` | `{"note"}` |
| POST | `/v1/mints/{mint}/rotate` | mint | | what was destroyed, and the new generation's key |
| POST | `/v1/mints/{mint}/pause` | mint | | `{"paused": true}` |
| POST | `/v1/mints/{mint}/resume` | mint | | `{"paused": false}` |
| GET | `/v1/admin/mints` | operator | | `{"mints"}` |
| POST | `/v1/admin/mints` | operator | `{"ledger", "policy", "rotate_every"}` | `{"mint", "token"}` |

Routes with a token need `Authorization: Bearer <token>`: a mint's routes
take that mint's own admin token, and `/v1/admin` routes the server's
operator token. No token can mint: a mint request needs a signature from a
key the mint's policy lists in `mint_authorities`.

Request bodies are limited to 64 KiB.

## Errors

Errors answer with a status code and a body like
`{"error": "spent", "message": "note 3:17 is already spent"}`. The `error`
code is stable; the message is for people. A `503 busy` answer also says in
its `Retry-After` header how many seconds to wait before sending the request
again. See [Error codes](../error-codes/).

## Verify

`POST /v1/verify` with `{"note": "<base64>"}` checks a note's signature and
answers its state. It works while the mint is paused.

```json
{"serial":{"generation":2,"index":999119},"amount":8,"state":"spent"}
```

`state` is `live`, `spent`, `expired` or `unissued`. A live note also has
`position` (its layer, 0 for the first), `hops_left` (transfers its lineage
can still make before the next rotation; 0 means frozen) and
`expires_after_rotations` (untouched, it expires in this many rotations
from now).

## Transfer

`POST /v1/transfer` spends input notes and issues new ones. Each input
carries its holder's signature over one message that names every input and
every output; see [Note format](../note-format/#transfers). The answer is
`{"notes": [...]}`, one per output, in order. The outputs land one layer past
the furthest input.

Sending the same request again is safe: the mint keeps a receipt of recent
transfers by the hash of their signed message and answers a resent one with
the same notes, even after a rotation has dropped its inputs' layer, until
those notes expire. It keeps each receipt for at least the policy's
`receipt_min_age_secs`; while it holds `receipt_capacity` receipts all
younger than that, or for a while after a restart counted so, it answers a
new transfer or burn `503 busy` and does nothing.

## Burn

`POST /v1/burn` destroys its input notes and issues nothing, so value that is
redeemed leaves circulation at once. Burns follow the same input limit as
transfers and work on a note frozen in the last layer. Their signatures are
made in their own domain, so a burn signature can never be replayed as a
transfer or the other way round. A resent burn is answered with the same
value and destroys nothing more.

## Mint info

`GET /v1/mints/{mint}/info`:

```json
{
  "issuer": "7e8e…5caf",
  "sequence": 2,
  "mint_layer": 2,
  "layers": [
    {"position": 0, "generation": 0, "issued": 0, "live": 0, "capacity": 1048576, "value": 0},
    {"position": 1, "generation": 1, "issued": 0, "live": 0, "capacity": 1048576, "value": 0},
    {"position": 2, "generation": 2, "issued": 1, "live": 0, "capacity": 1048576, "value": 0},
    {"position": 3, "generation": 3, "issued": 4, "live": 4, "capacity": 1048576, "value": 8}
  ],
  "supply": {"minted": 8, "destroyed": 0, "burned": 0, "outstanding": 8},
  "policy": {"mint_layer": null, "max_inputs": 1, "max_outputs": 16, "denominations": "powers_of_two", "receipt_capacity": 1048576, "receipt_min_age_secs": 128, "mint_authorities": ["b192…5673"]},
  "rotation": null,
  "paused": false
}
```

| Field | Meaning |
|---|---|
| `sequence` | Events applied since the mint was created. |
| `mint_layer` | The position new notes are minted into. |
| `layers` | Every layer, first to last: bits handed out (`issued`), notes still `live`, `capacity` and the live notes' `value`. |
| `supply` | Value `minted`, `destroyed` by rotations and `burned`; `outstanding = minted − destroyed − burned`. |
| `policy` | The rules in force. See [Policy fields](../policy/). |
| `rotation` | `{"period_secs", "next_at"}`, or `null` when the mint rotates only on request. `next_at` is a Unix time in seconds. |
| `paused` | Whether the mint is paused. |

## Keys

`GET /v1/mints/{mint}/keys` answers the keys that sign the live generations'
notes, each certified by the issuer key:

```json
{"issuer":"7e8e…5caf","oldest":0,"keys":[{"generation":0,"key":"7af7…7bff","signature":"4674…5300"}, …]}
```

Anyone who knows the issuer key can check a note offline with these. See
[Note format](../note-format/#generation-keys).

## Mint

`POST /v1/mints/{mint}/mint` takes a request signed by a mint authority and
answers `{"note": "<base64>"}`. `worgl-server sign-mint` prints one, ready to
post; see [Mint authorities and keys](../keys/).

## Rotate, pause and resume

`POST /v1/mints/{mint}/rotate` answers what the rotation did:

```json
{"dropped":0,"created":4,"destroyed_notes":0,"destroyed_value":0,"key":{"generation":4,"key":"5de1…c4ce","signature":"9699…8e08"}}
```

`pause` and `resume` answer `{"paused": true}` and `{"paused": false}`. See
[Pausing a mint](../pausing/).

## Creating mints

`POST /v1/admin/mints` takes a ledger shape, a policy and an optional
period, and answers the new mint's issuer key and admin token:

```json
{"ledger": {"layers": 4, "layer_size": 1048576}, "policy": {"mint_authorities": ["<public key>"]}, "rotate_every": 86400}
```

The policy must list a mint authority, since the server never holds one.
Missing policy fields take their defaults. Without `rotate_every` the mint
rotates only on request. `GET /v1/admin/mints` lists every mint's issuer key.

## On a cluster

Any member takes any request and passes it on to the member that leads the
mint. While a mint has no leader its requests are answered `503 no_leader`;
see [Running a cluster](../running-a-cluster/).
