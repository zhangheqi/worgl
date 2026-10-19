---
title: Choosing a period and layer count
description: How the rotation period, the number of layers and the layer size set how long notes live, how often they move and what a mint stores.
---

Three numbers shape a mint: its rotation period, its number of layers and
the size of each layer. The layers and their size are fixed for the mint's
life; the period is set when you create it.

## What they decide

With `n` layers and a period `P`:

- **How often a note moves.** A note in the last layer is frozen until the
  next rotation, and each hop lands one layer on. So once a note has reached
  the last layer it moves at most once per period. Notes in lower layers can
  make a burst of up to `n − 1 − k` hops from position `k`.
- **How long a note lives untouched.** A note at position `k` dies in the
  `(k + 1)`-th rotation from now. A note that just arrived sits in the last
  layer, so it dies in the `n`-th rotation, within `n × P`.
- **How often holders must act.** A holder who wants to keep a note pays it
  to a fresh key of its own before its layer is dropped. `worgl refresh` and
  the SDKs' `refresh` do that for the notes that die at the next rotation,
  so a holder runs it once per period.

| Period | Layers | A received note, left alone, dies within | A note moves at most |
|---|---|---|---|
| 1 hour | 4 | 4 hours | once an hour |
| 1 day | 4 | 4 days | once a day |
| 1 day | 8 | 8 days | once a day |
| 1 week | 4 | 4 weeks | once a week |

The period sets both how fast tokens can circulate and how long they can sit
still. A short period suits quotas and credits meant to be used soon; a long
one suits tokens people hold for a while.

## Where new notes land

`mint_layer` in the [policy](../policy/) picks the position new notes are
minted into. By default it is the second to last layer: one hop right away,
and the longest life that allows a hop. A lower layer gives more hops now and
a shorter life; the last layer gives the longest life and no hop until the
next rotation.

## Layer size

A layer hands out each of its bits once per generation, so `layer_size` caps
how many notes can land in one layer per period. When a layer runs out it
refuses with `layer_full` until the next rotation.

Every hop moves value to a strictly newer generation, so a unit of value
lands in any generation at most once. While the outstanding value, counted
in the smallest amount, stays at or below `layer_size`, `layer_full` cannot
happen. The property tests check both the bound and the guarantee.

## What it costs

The ledger stores one bit per note: `n × layer_size` bits in all. Four
layers of 2²⁰ bits take 512 KiB and hold over four million notes in flight
([measured in the README](https://github.com/zhangheqi/worgl#efficiency)).

A rotation costs one word write per 64 bits the dropped layer used.

## Limits

| | Allowed |
|---|---|
| Layers | 2 to 65,536 |
| `layer_size` | a positive multiple of 64 |
| Layers × `layer_size` | at most 2³⁶ bits |
| `mint_layer` | below the number of layers |

`worgl-server create-mint` takes `--layers` (default 4), `--layer-size`
(default 1,048,576) and `--rotate-every` in seconds. Without
`--rotate-every` the mint rotates only when its admin asks. Over HTTP, the
same three are `ledger.layers`, `ledger.layer_size` and `rotate_every` in
`POST /v1/admin/mints`; see [Running a server](../running-a-server/).
