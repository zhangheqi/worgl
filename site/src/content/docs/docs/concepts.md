---
title: Concepts
description: Notes, mints, layers, rotation and holders, the five ideas the rest of the docs build on.
---

Worgl is an issuer for tokens. A server hosts mints, each mint signs notes,
and holders keep the notes themselves. The mint tracks every note with one
bit and keeps no accounts.

## Notes

A note is 149 bytes signed by its mint: the mint's issuer key, a serial, an
amount and the public half of a one-time key of whoever holds it. In text it
is unpadded URL-safe base64. See [Note format](../note-format/).

The mint stores no notes. The signature makes a note self-authenticating,
and one bit in the mint's ledger says whether it is live: 1 while it is, 0
once it is spent. A transfer clears its input notes' bits and issues new
notes to the receivers, so no note can be spent twice. A burn clears them
and issues nothing, for value that leaves circulation.

The mint also keeps a receipt of each recent transfer and burn, the last
1,048,576 by default and each for at least 128 seconds, so a client whose
answer was lost can send the same request again and get the same notes back.
See [Policy fields](../policy/#receipt_capacity-and-receipt_min_age_secs).

## Mints

A mint is one kind of token. It has its own issuer key, ledger, policy,
rotation period and directory. One server hosts any number of mints side by
side, and a busy mint does not slow the others.

A mint's name is its issuer key, in hex. Every note names its mint, so the
routes that move notes take no mint in their path and find it from the note.

New value comes only from a mint authority: a key the mint's policy lists,
which signs each mint request. The server holds no such key and mints no
other way. See [Mint authorities and keys](../keys/).

## Layers

A mint's ledger is one bit array split into layers, 4 by default. A note's
serial is `generation:index`: the generation names the layer it was issued
into, and the index names its bit there, through a keyed permutation, so a
serial does not say whether a note was issued before or after another.

Every transfer moves value one layer on: its outputs land one layer past the
furthest input. A note in the last layer cannot move until the next rotation;
it is frozen. New notes are minted into the second to last layer unless the
policy says otherwise, which allows one hop right away.

A note at position `k` of `n` layers can make `n − 1 − k` more hops right
now and, left alone, dies in the `(k + 1)`-th rotation from now. Those two
numbers always add up to `n`, so the layer a note is minted into trades
spending room for lifetime.

A layer hands out each of its bits once per generation, so `layer_size`
caps how many notes can land in one layer per rotation period. A full layer
refuses with `layer_full` until the next rotation.

## Rotation

A rotation drops the first layer, which destroys every note still in it,
wipes its slot and reuses it as the new last layer. Every other layer moves
one position down. Notes that were frozen can move again, and notes nobody
touched for as many rotations as there are layers expire.

A mint rotates when its admin asks, or on a schedule aligned to multiples
of its period since the Unix epoch, so restarts do not shift it. Boundaries
missed while the server is down, or while the mint is paused, are not made
up. A mint's info reports its period and the time of the next rotation.

Once a note reaches the last layer it can move at most once per rotation.
With 4 layers and daily rotation, a note changes hands at most once a day in
steady state, with a burst allowance for notes in lower layers, and a note
left alone dies within 4 days. See
[Choosing a period and layer count](../choosing-a-period/).

Keeping a note alive is a transfer to yourself: paying it to a fresh key of
your own moves it one layer on. `worgl refresh` and the SDKs' `refresh` do
this for the notes about to die, each at a random moment before the
rotation.

## Holders

There are no accounts. A holder is whoever holds the secret half of a
note's one-time key. To be paid, the receiver makes a fresh key for each
note it expects and hands out the public halves. Spending a note takes a
signature from its key over a message that names every input and every
output, so a payer who keeps a copy of a note cannot spend it, and nobody
can redirect a transfer in flight.

A payment takes one round trip between the two holders:

1. The payee makes a payment request: the amount as notes, each to a fresh
   key of its own.
2. The payer answers with a payment: signed transfers, each spending one of
   its notes, with a time to submit each.
3. The payee checks the payment, submits each transfer to the mint at its
   time and keeps the new notes. The payer learns the same answers, from the
   payee or by sending its transfers again, and takes in its change.

The SDKs and the `worgl` command play the holder's part and keep the
privacy rules for you: a fresh key for every note, one note per transfer,
transfers spread out in time and outputs in random order. See the
[Privacy model](../privacy/).

## Words

| Word | Meaning |
|---|---|
| Note | 149 signed bytes worth an amount, bound to a one-time key. |
| Mint | One kind of token: an issuer key, a ledger, a policy and a period. |
| Issuer key | A mint's identity. It certifies the keys that sign notes. |
| Layer | A slice of the ledger's bits. Notes move one layer on per hop. |
| Generation | The number of a layer's current use. It never repeats. |
| Serial | `generation:index`, which names a note's bit. |
| Rotation | Dropping the first layer and opening a new last one. |
| Frozen | In the last layer: cannot move until the next rotation. |
| Expired | In a layer that was dropped. |
| Mint authority | A key whose signed requests a mint accepts to mint. |
| Purse | A holder's notes and keys, saved to a file or a store of yours. |
