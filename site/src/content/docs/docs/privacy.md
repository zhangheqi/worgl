---
title: Privacy model
description: What the mint learns, what it cannot tell, what timing still gives away, and what each party to a payment sees.
---

The mint learns no identities and keeps no accounts. It does see each
transfer's inputs die and its outputs appear, so it can follow the lineage
of every note. What it cannot do is:

- tell who holds a note, or
- tell that two unrelated lineages share an owner,

provided holders use a fresh key for every note they receive and never spend
two notes in one transfer. The default policy enforces the second rule, and
the SDKs and the `worgl` command follow both.

## Why the two rules

**A fresh key for every note.** A note is bound to a one-time key of
whoever receives it. Two notes under one key tell the mint they share an
owner.

**One note per transfer.** A transfer that spends several notes tells the
mint they had one owner. The default policy, `cash_like`, allows one input
per transfer, so the mint never sees two lineages join. A note can still be
split into up to 16, since all the outputs descend from one note and a split
reveals nothing new. Powers-of-two denominations keep notes of one size
interchangeable, so a payment of 5 is a 4-note transfer and a 1-note
transfer to two fresh payee keys, and change goes back the same way.

`Policy::flexible()` allows up to 16 inputs, for systems that do not need
this property. Its mint learns that merged notes shared an owner.

## Timing

What remains visible is timing. A payer who sends three notes to one payee
in the same second invites the guess that they are one payment. Spacing or
batching transfers blurs this.

The SDKs and `worgl` spread the transfers of one payment at random gaps, 2
to 60 seconds apart by default, and spread refreshes across the rotation
period. A refresh made once the rotation's time has come, before the mint
makes it, has no time left to spread over and sends everything at once.
Closer spacing, `worgl refresh --now`, or a spacing of zero in your own code
lets the mint link the notes by when they arrive. Outputs are
shuffled too, so the payee's notes are not always first.

## What each party sees

| Party | Sees | Cannot tell |
|---|---|---|
| The mint | Every note's serial, amount and one-time key; which notes each transfer spent and which it made; when each request arrived; which authority minted what. | Who holds a note. That two unrelated lineages share an owner, while holders keep the two rules. |
| The payee | Its own new notes. The payer's transfers in the payment: the notes they spend, and the change, bound to fresh keys of the payer's. The mint's answers hold the change notes too, so the payee can ask `POST /v1/verify` whether they are still live, and it hands them back to the payer. | The payer's other notes. Where the change goes once it is spent. |
| The payer | The payee's fresh keys in the request, and, from the mint's answers when it submits or resends the payment itself, the payee's new notes. It cannot spend them, since they are bound to the payee's keys, but it can ask whether they are still live. The change the payee hands back holds none of the payee's notes. | Where the value goes once the payee spends it. |
| Anyone holding the log | What the mint saw, change by change, with every mint's authority and signature. No secret keys. | The same as the mint. |

The mint stores no notes, only one bit each, plus a bounded list of recent
transfers so that resent requests get the same answer. The log keeps every
change since the last snapshot, so treat it as you would the mint's view.

Worgl's server does not log where requests come from, but like any HTTP
server it sees each connection's address, and so does a proxy in front of
it. Worgl does nothing to hide that address.

## Next to other designs

This is weaker than blind-signature schemes (Chaum's, Cashu, GNU Taler),
where the issuer cannot link a note to the one it replaced, and stronger
than any account system. Blind signatures do not fit this design: the
issuer must choose each note's serial, because the serial names the note's
bit.

## What it does not cover

- **The mint sees lineage.** That is inherent to the design.
- **No identity checks.** Who may hold or receive notes is out of scope.
