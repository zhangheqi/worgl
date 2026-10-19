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
  "receipt_capacity": 65536,
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
| `receipt_capacity` | `65536` | How many recent transfers the mint remembers, so a client whose answer was lost can resend the identical request and get the same notes back. `0` turns this off. Each entry costs about `40 + 52 × outputs` bytes. |
| `mint_authorities` | `[]` | Public keys, in hex, whose signed mint requests the mint accepts. Empty accepts none. |

### denominations

| Value | Allows |
|---|---|
| `"any"` | Any positive amount. |
| `"powers_of_two"` | 1, 2, 4, 8 and so on up to 2⁶³. Every amount splits into at most one note of each size. |
| `{"list": [1, 2, 5, 10]}` | An explicit set, strictly ascending and without zero. |

Input amounts are not checked against the denominations, so notes issued
under an earlier policy still spend.

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
