---
title: Policy fields
description: Every field of a mint's policy, its default, and the cash_like and flexible presets.
---

A mint's policy holds the rules an operator may change at any restart
without touching stored state. It lives in the mint's `policy.json`, and
`POST /v1/admin/mints` takes it as `policy`. Missing fields take their
`cash_like` values.

On a cluster, each member reads its own `policy.json`, and the rules in
force are those of the member leading the mint, so keep the file the same
on every member. See
[Running a cluster](../running-a-cluster/#changing-the-rules).

```json
{
  "mint_layer": null,
  "max_inputs": 1,
  "max_outputs": 16,
  "denominations": "powers_of_two",
  "receipt_capacity": 1048576,
  "receipt_min_age_secs": 128,
  "mint_authorities": ["dcf3cd0bc01612caf024cce018d6b01b4c1fed8a5dcf881944d13eaa4f2b6229"]
}
```

## Fields

| Field | Default | Meaning |
|---|---|---|
| `mint_layer` | `null` | Position of the layer new notes are minted into. `null` means the second to last layer. It must be below the number of layers. |
| `max_inputs` | `1` | Most notes one transfer or burn may consume. `1` forbids merging. At least 1. |
| `max_outputs` | `16` | Most notes one transfer may create. At least 1. |
| `denominations` | `"powers_of_two"` | Amounts a minted or transferred note may carry. |
| `receipt_capacity` | `1048576` | How many recent transfers and burns the mint keeps a receipt of, so a client whose answer was lost can resend the identical request and get the same answer back. `0` turns this off. Each receipt takes 45 bytes in the mint's snapshots and about 150 in memory, twice that on a cluster's leader. |
| `receipt_min_age_secs` | `128` | Shortest time, in seconds, the mint keeps a receipt. While it holds `receipt_capacity` of them and the oldest is younger, it refuses new transfers and burns as `busy`. `0` turns this off, as does a `receipt_capacity` of `0`. |
| `mint_authorities` | `[]` | Public keys, in hex, whose signed mint requests the mint accepts. Empty accepts none. |

### denominations

| Value | Allows |
|---|---|
| `"any"` | Any positive amount. |
| `"powers_of_two"` | 1, 2, 4, 8 and so on up to 2<sup>63</sup>. Every amount splits into at most one note of each size. |
| `{"list": [1, 2, 5, 10]}` | An explicit set, strictly ascending and without zero. |

Input amounts are not checked against the denominations, so notes issued
under an earlier policy still spend.

### receipt_capacity and receipt_min_age_secs

A client whose answer was lost sends the identical request again, and the
mint answers it from its receipt: the same notes for a transfer, the same
value for a burn. The mint keeps each receipt for at least
`receipt_min_age_secs`, and after that while it is among the last
`receipt_capacity`; a transfer's only until the notes it issued expire. The
default of 128 seconds outlasts how long both SDKs keep resending a request
after a try the mint may have applied. They make four tries of up to 30
seconds each, 1, 2 and 4 seconds apart, the last ending within 127 seconds
of the first. A `busy` answer stretches the wait after it to what its
`Retry-After` asks, up to 30 seconds, but the mint answers `busy` only to a
request it has not applied, so the last try still ends within 127 seconds of
any the mint may have applied.

A burst of up to `receipt_capacity` transfers and burns is taken at once,
except shortly after a restart, as below. Sustained, a mint takes at most
`receipt_capacity / receipt_min_age_secs` a second, about 8,192 at the
defaults: past that its table is full of receipts younger than
`receipt_min_age_secs`, and it answers new transfers and burns `busy`, with a
`Retry-After` header saying when it has room again. Resends of the ones it
took are still answered. Each mint keeps its own receipts, so several mints
on one server are not capped together.

A mint keeps no record on disk of when it made each receipt, so after a
restart it counts each as made as late as its files allow: a server of its
own goes by when it last wrote the mint's log and when it last logged a
transfer or burn before writing the mint's snapshot, and a cluster member
counts the receipts it held when it started, or took in with a snapshot from
the leader, as made then. So for up to `receipt_min_age_secs` after a
restart, a mint that holds `receipt_capacity` receipts may answer every new
transfer and burn `busy`, even if the receipts are older.

On a cluster, give every member the same `receipt_capacity` and
`receipt_min_age_secs`. The leader refuses what would push out a receipt too
young to go, counting on every member to keep as many as it does. A member
that restarted, or took in a snapshot, less than `receipt_min_age_secs` ago
holds new transfers and burns back as above if it becomes the leader, so
leave at least that long between restarts of members.

A mint an older server created with the defaults has
`"receipt_capacity": 65536` written in its `policy.json`. It now keeps each
receipt for 128 seconds too, which caps it at 512 transfers and burns a
second, and the server warns when it starts a mint capped below 1,000 a
second. To take more, raise `receipt_capacity` in the mint's `policy.json`,
on every member of a cluster, and restart: `1048576` is the new default.

### mint_layer

A note at position `k` of `n` layers can make `n − 1 − k` more hops before
the next rotation and, untouched, dies in the `(k + 1)`-th rotation from
now. The two always add up to `n`, so this trades spending room for
lifetime. The second to last layer allows one hop right away and the longest
life that does. See
[Choosing a period and layer count](../choosing-a-period/).

### mint_authorities

Removing a key stops it minting; on a cluster, only once every member has
restarted without it. The mint keeps its sequence number, so adding it back
cannot replay its old requests. The server never holds a mint authority
key, so a mint whose list is empty cannot mint at all.
`create-mint` makes a key and lists it when the policy lists none, and
`POST /v1/admin/mints` refuses a policy without one. See
[Mint authorities and keys](../keys/).

## Presets

The library has two presets, `Policy::cash_like()` and `Policy::flexible()`.

| | `cash_like` | `flexible` |
|---|---|---|
| `max_inputs` | 1 | 16 |
| `max_outputs` | 16 | 16 |
| `denominations` | powers of two | any |
| Everything else | as above | as `cash_like` |

`cash_like`, the default, is chosen for privacy:

- one input per transfer, so the mint never sees two lineages join;
- up to 16 outputs, so one note can be split; all outputs descend from one
  note, so a split reveals nothing new;
- powers-of-two denominations, so notes of one size are interchangeable and
  any amount splits into at most one note of each size.

A payment of 5 is then a 4-note transfer and a 1-note transfer to two fresh
payee keys, and change is the payee paying notes back the same way.

`flexible` allows up to 16 inputs of any amount, so transfers can merge
notes and pay change in one step, for systems that do not need the privacy
property. The mint learns that merged notes shared an owner. To use it from
`policy.json`, write its fields out:

```json
{ "max_inputs": 16, "denominations": "any" }
```
