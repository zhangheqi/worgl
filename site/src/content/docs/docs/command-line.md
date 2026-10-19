---
title: The worgl command
description: A purse on the command line, kept in a passphrase-encrypted file, for scripts and for trying things out.
---

`worgl` holds, pays and receives notes from the command line. It keeps a
purse in one file, encrypted with XChaCha20-Poly1305 under a key derived from
a passphrase with Argon2id (64 MiB, three passes). Payment requests,
payments and handed-over notes are JSON files for the two sides to exchange.
It is `worgl-client`'s purse, so it keeps the same privacy rules as the
[Rust SDK](../rust-sdk/).

## Install

Nothing is on crates.io yet. Install it from git:

```sh
cargo install --git https://github.com/zhangheqi/worgl worgl-cli --locked
```

Or build it in a clone with `cargo build --release -p worgl-cli`, as the
[quickstart](../quickstart/) does.

## Settings

| Setting | Flag | Environment | Default |
|---|---|---|---|
| The server | `--server` | `WORGL_SERVER` | `http://127.0.0.1:8080` |
| The purse file | `--purse` | `WORGL_PURSE` | `purse.worgl` |
| The passphrase | | `WORGL_PASSPHRASE` | asked for |

`-` in place of a file reads standard input or writes standard output.
`worgl help <command>` explains each command.

## A payment

```sh
worgl init                                     # each side, once
worgl playground --faucet $FAUCET              # a note from a playground faucet; prints its mint
worgl mint $MINT 8 --authority-key mint.key    # as a mint authority, for testing
worgl request $MINT 5 -o request.json          # payee → send request.json
worgl pay request.json -o payment.json         # payer → send payment.json
worgl receive request.json payment.json        # payee: checks, submits, keeps
worgl settle                                   # payer, later: takes in the change
worgl balance $MINT
worgl refresh $MINT                            # once per rotation period
```

## Commands

| Command | What it does |
|---|---|
| `init` | Creates an empty purse. |
| `info MINT` | Shows a mint's layers, rotation schedule and supply. |
| `balance MINT` | Shows what the purse holds at a mint: spendable, frozen, expiring, outgoing and expected. |
| `notes` | Lists the notes held and what each can do now. |
| `request MINT AMOUNT` | Asks to be paid: writes a payment request to send to the payer. `--at-most N` asks for notes no larger than N, for a payer whose notes are small. `-o` names the file (default `-`). |
| `withdraw REQUEST` | Stops expecting the notes of a request that will not be paid. |
| `pay REQUEST` | Answers a payment request: writes a payment to send to the payee. `-o` names the file. |
| `receive REQUEST PAYMENT` | Takes a payment: checks it, submits each transfer at its time and keeps the notes. |
| `submit PAYMENT` | Submits a payment as the payer and writes the notes for the payee, who keeps them with `accept`. `-o` names the file. |
| `accept NOTES` | Keeps notes a payer handed over, after checking their signatures. |
| `settle` | Sends the transfers not yet seen answered again and takes in their change. |
| `refresh MINT` | Pays the notes that die at the next rotation back to the purse, each at a random moment before it. Waits until the last is done. `--now` submits them all at once: faster, but the mint can then tell the notes belong together. |
| `prune MINT` | Drops notes whose layer the mint has dropped. |
| `playground` | Asks a playground [faucet](../running-a-faucet/) for a note and keeps it, then prints the faucet's mint. `--faucet URL` (or `WORGL_FAUCET`) names the faucet when it is not at the server's URL. |
| `mint MINT AMOUNT` | Mints notes into the purse, signing as one of the mint's authorities with `--authority-key FILE`. `--sequence N` sets the first sequence number, above every one this key has used; it defaults to the time in microseconds. `--at-most N` mints notes no larger than N. |

## Spacing

`pay` and `settle` take `--spacing MIN..MAX`, the seconds between transfers
that belong together, picked at random in that range. It defaults to
`2..60`. Closer spacing lets the mint guess the notes had one owner; see the
[Privacy model](../privacy/).

## Several commands at once

Commands can run on one purse at the same time, such as a `refresh` waiting
in the background while you pay and receive. Each change locks the purse,
reads it again and saves before unlocking, so no command saves over a note
another one took in. The lock is a file beside the purse,
`.purse.worgl.lock`.

## Keeping notes alive

A note left alone dies when its layer is dropped. Run `worgl refresh MINT`
once per rotation period: it pays the notes that would die at the next
rotation to fresh keys of the purse, spread over the time left. `worgl info`
and `worgl balance` say when the next rotation is. `worgl prune` clears out
notes that died anyway.
