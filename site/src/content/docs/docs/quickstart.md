---
title: Quickstart
description: Run a server on your machine, create a mint, mint a note and move it between two purses with the worgl command.
---

You will install the server and the `worgl` command, run a server with one
mint, mint a note into Alice's purse and pay part of it to Bob. Most of the
time goes on compiling.

## Before you start

You need `curl` and Rust 1.89 or later.

## 1. Install

```sh
cargo install --locked worgl-server worgl-cli
```

That installs two programs: `worgl-server`, which hosts mints, and `worgl`, a
purse on the command line.

## 2. Create a mint

```sh
worgl-server init --state ./server
worgl-server create-mint --state ./server
MINT=$(ls server/mints)
```

`init` makes a state directory with an operator token and a key-encryption
key. `create-mint` adds one mint with 4 layers: its issuer key, an admin
token, a mint authority key and an empty ledger. It prints where each went:

```text
mint        5d9f1ede4d9781d525ec3b8b2da4962e83e6ad1950fc50f8b44d6c2866c0e3ce
admin token ./server/mints/5d9f…e3ce/admin.token
policy      ./server/mints/5d9f…e3ce/policy.json
mint key    ./server/mints/5d9f…e3ce/mint.key (the server does not need it; keep it where you mint from)
```

A mint is named by its issuer key, and its directory under `server/mints`
carries that name, so `MINT` now holds it.

## 3. Start the server

```sh
worgl-server serve --state ./server > server.log 2>&1 &
sleep 1
curl http://127.0.0.1:8080/healthz
```

The server listens on `127.0.0.1:8080`, the address `worgl` uses unless you
give it `--server`, and writes its log to `server.log`. `curl` prints `ok`.

## 4. Make two purses

```sh
export WORGL_PASSPHRASE='try it out'
worgl --purse alice.worgl init
worgl --purse bob.worgl init
```

A purse is one file, encrypted under a passphrase. `worgl` reads the
passphrase from `WORGL_PASSPHRASE`, or asks for it when that is not set.
Both purses share one here to keep the steps short; for anything you keep,
let `worgl` ask.

## 5. Mint a note

```sh
worgl --purse alice.worgl mint $MINT 8 --authority-key server/mints/$MINT/mint.key
worgl --purse alice.worgl balance $MINT
```

`mint` signs a mint request with the authority key `create-mint` wrote. The
mint answers with a note of 8, bound to a fresh key in Alice's purse.

```text
Minted 8 in 1 notes.
spendable  8
frozen     0  (moves after the next rotation)
expiring   0  (dies at the next rotation unless refreshed)
outgoing   0  (sent, not yet seen answered; see `worgl settle`)
expected   0  (asked for, not yet received)
```

## 6. Pay Bob 5

Bob asks, Alice pays, Bob takes the payment, and Alice takes in her change.
The request, the payment and the change are JSON files the two sides send
each other.

```sh
worgl --purse bob.worgl request $MINT 5 -o request.json
worgl --purse alice.worgl pay request.json -o payment.json
worgl --purse bob.worgl receive request.json payment.json -o change.json
worgl --purse alice.worgl accept change.json
```

```text
Asked for 5 in 2 notes. Send the request to the payer.
Paid 5 in 1 transfer(s). Send the payment to the payee, then keep the change they send back with `worgl accept`, or run `worgl settle` once they have submitted it.
Submitting 1 transfer(s).
Send the notes to the payer, who keeps the change with `worgl accept`.
Received 5 in 1 transfer(s).
Kept 3.
```

- `request` asks for 5 as two notes, 4 and 1, each to a fresh key of Bob's.
- `pay` signs one transfer that spends Alice's 8: 4 and 1 to Bob's keys, and
  3 in change to fresh keys of her own.
- `receive` checks the payment against the request, submits it to the mint
  and keeps Bob's new notes. The mint's answer holds Alice's change too,
  which it writes to `change.json` for her.
- `accept` checks the change's signatures, keeps it, and settles Alice's
  transfer. Without it, `worgl --purse alice.worgl settle` would send her
  transfer again: the mint answers a resent transfer with the same notes
  while it keeps its answer.

## 7. Rotate

```sh
worgl --purse bob.worgl balance $MINT
```

Bob's 5 is `frozen`. Each hop moves a note one layer on, and his notes
landed in the last layer, which cannot move until the next rotation. This
mint rotates only when its admin asks, so ask:

```sh
curl -X POST -H "Authorization: Bearer $(cat server/mints/$MINT/admin.token)" http://127.0.0.1:8080/v1/mints/$MINT/rotate
worgl --purse bob.worgl notes
```

```text
{"dropped":0,"created":4,"destroyed_notes":0,"destroyed_value":0,"key":{"generation":4,"key":"5de1…c4ce","signature":"9699…8e08"}}
mint 5d9f1ede4d9781d525ec3b8b2da4962e83e6ad1950fc50f8b44d6c2866c0e3ce
      3:425685                     4  spendable, dies in 3 rotation(s)
      3:394684                     1  spendable, dies in 3 rotation(s)
```

The rotation dropped generation 0, which held no notes, and opened
generation 4 as the new last layer, with a key of its own to sign its notes.
Bob can spend both notes now. Left alone, they die in the third rotation
from now; `worgl refresh` pays them to fresh keys of his own before that.

## 8. Stop the server

```sh
kill %1
```

That stops the server you started with `&` in step 3. Everything it knows is
in `./server`, and `serve` carries on from there next time.

## Next

- [Concepts](../concepts/): notes, mints, layers, rotation and holders.
- [The worgl command](../command-line/): every command, including `refresh`.
- [Running a server](../running-a-server/): rotation on a schedule,
  policies, tokens and minting over HTTP.
- [Rust SDK](../rust-sdk/) or [TypeScript SDK](../typescript-sdk/): the same
  purse in your own program.
