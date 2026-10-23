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

```sh
cargo install worgl-cli --locked
```

The crate is `worgl-cli` and the command it installs is `worgl`. Or build it
in a clone with `cargo build --release -p worgl-cli`.

## Settings

| Setting | Flag | Environment | Default |
|---|---|---|---|
| The server | `--server` | `WORGL_SERVER` | `http://127.0.0.1:8080` |
| The purse file | `--purse` | `WORGL_PURSE` | `purse.worgl` |
| The passphrase | | `WORGL_PASSPHRASE` | asked for |

`-` in place of a file reads standard input or writes standard output.
`-o` writes a new file beside the one it names and renames it over that
one, so a failure leaves no half-written file; the file keeps the
permissions it had. A link, such as `/dev/stdout`, or a device is written
in place.
`worgl help <command>` explains each command.

## A payment

```sh
worgl init                                             # each side, once
worgl playground --faucet $FAUCET                      # a note from a playground faucet; prints its mint
worgl mint $MINT 8 --authority-key mint.key            # as a mint authority, for testing
worgl request $MINT 5 -o request.json                  # payee → send request.json
worgl pay request.json -o payment.json                 # payer → send payment.json
worgl receive request.json payment.json -o change.json # payee: checks, submits, keeps → send change.json
worgl accept change.json                               # payer: keeps the change
worgl balance $MINT
worgl refresh $MINT                                    # once per rotation period
```

`change.json` holds the payer's change from the mint's answers, nothing of
the payee's. Keeping it with `accept` also settles the payer's transfer.
The signatures `accept` checks show that the notes are the mint's, not that
the payment went through: a payee could pay those keys from notes of its own
and leave the payment unsubmitted. Without the change, the payer runs
`worgl settle` once the payee has submitted the payment: it sends the
transfers again, and the mint answers with the same notes for as long as it
keeps its answers (see [Error codes](../error-codes/)). On a mint that
rotates on a schedule, `pay` says by when to settle: before the rotation
that drops a note the payment spends, with the time left counted by the
server's clock. `refresh` resends a payment still outgoing in the period
before it (see [Keeping notes alive](#keeping-notes-alive)). A resend the
mint no longer answers is reported as possibly applied, with the value that
may be lost, and dropped (see [Refused transfers](#refused-transfers)): the
change can then only come from the payee, and `accept` still keeps it.

`pay` prints the payment's id, and the purse keeps the payment until it is
done with (see [Payments kept](#payments-kept)). If `pay` cannot write the
payment, it is made all the same: `worgl export ID -o payment.json` writes
it again. `pay` refuses a request it has paid already, so a request sent
twice is paid once, and names the payment that paid it while the purse
keeps it. It also refuses one for a note the mint does not make, such as a
note of 3 from a mint whose notes are powers of two.

## Commands

| Command | What it does |
|---|---|
| `init` | Creates an empty purse. |
| `info MINT` | Shows a mint's layers, rotation schedule and supply. |
| `balance MINT` | Shows the purse's total at a mint, what it holds plus what comes back to it as change and refreshes, then parts that do not overlap: what it holds (spendable and frozen, with what of it expires at the next rotation), change on its way back, what it is sending to payees, and what it asked for. Refreshes not yet answered and notes newer than the mint's info show when there are some. So does unconfirmed change of dropped or cancelled transfers, which is in no total: after a cancel it comes only if the mint spent the note taken back. |
| `notes` | Lists the notes held and what each can do now. |
| `request MINT AMOUNT` | Asks to be paid: writes a payment request to send to the payer. `--at-most N` asks for notes no larger than N, for a payer whose notes are small. `-o` names the file (default `-`). |
| `withdraw REQUEST` | Stops expecting the notes of a request that will not be paid. |
| `pay REQUEST` | Answers a payment request: writes a payment to send to the payee, prints its id, and says by when to settle it if the mint rotates on a schedule. Refuses a request it has paid already. `-o` names the file. |
| `receive REQUEST PAYMENT` | Takes a payment: checks it, submits each transfer at its time and keeps the notes. Writes the payer's change from the mint's answers, to send back to the payer. `-o` names the file (default `-`). |
| `submit PAYMENT` | Submits a payment as the payer and writes the payee's notes, which the payee keeps with `accept`. `-o` names the file. |
| `accept NOTES` | Keeps notes the other side handed over, after checking their signatures: the payee's from `submit`, or the payer's change from `receive`, which settles the transfers it answers. Notes whose layer the mint has dropped are skipped. |
| `settle` | Sends the transfers not yet seen answered again and takes in their change, keeping the payee's notes from the answers to a payment in case the payee has not submitted it. One the mint may have applied but no longer answers is reported as possibly applied. A transfer refused for good is dropped or cancelled; see [Refused transfers](#refused-transfers). |
| `outgoing` | Lists the payments kept, the transfers not yet seen answered and the notes kept for payees, each payment and transfer with its id. |
| `export PAYMENT` | Writes a kept payment again, as `pay` wrote it, unless a transfer of it was cancelled or dropped. `--notes` writes instead the notes kept for its payee, for the payee to keep with `accept`, and stops keeping them. `-o` names the file (default `-`). |
| `cancel TRANSFER` | Takes back the note of an outgoing transfer the mint never applied. Only for a transfer nobody will submit; see [Payments kept](#payments-kept). |
| `forget TRANSFER` | Drops an outgoing transfer whose note is gone. |
| `refresh MINT` | Pays the notes that die at the next rotation back to the purse, each at a random moment before it, and resends the transfers not yet seen answered that must land before it, as `settle` does. Waits until the last is done. `--now` submits them all at once: faster, but the mint can then tell the notes belong together. |
| `prune MINT` | Drops notes whose layer the mint has dropped, and the unconfirmed change and notes kept for payees that sit in one. |
| `playground` | Asks a playground [faucet](../running-a-faucet/) for a note and keeps it, then prints the faucet's mint. `--faucet URL` (or `WORGL_FAUCET`) names the faucet when it is not at the server's URL. |
| `mint MINT AMOUNT` | Mints notes into the purse, signing as one of the mint's authorities with `--authority-key FILE`. `--sequence N` sets the first sequence number, above every one this key has used; it defaults to the time in microseconds. `--at-most N` mints notes no larger than N. |

## Payments kept

The purse keeps each payment it makes while one of its transfers is
outgoing, or while it keeps notes for the payee. `worgl outgoing` lists them
by mint, each with its id and its transfers still outgoing, and then the
other outgoing transfers, such as refreshes. A transfer's id is the first 12
hex digits of its identity as the mint computes it. A payment's is the first
12 hex digits of the first key its request asks to be paid to, so it stays
the id `pay` printed whatever becomes of its transfers; `export` takes the
id of one of its transfers too. Commands take any first digits that name one
alone.

- `worgl export PAYMENT -o payment.json` writes the payment again, for a
  payee who lost it. A transfer cancelled or dropped leaves the payment,
  which then pays only part of the request, as `outgoing` shows: `export`
  refuses it rather than hand that transfer out again, and the payee would
  refuse it anyway.
- Whenever `settle` or `refresh` gets the mint's answer to a payment's
  transfer, the purse keeps the payee's notes from it, and says so. It
  cannot tell whether the payee submitted the payment and has them already,
  so it keeps them after the usual `settle` once the payee has submitted,
  too. A payee that has not submitted would get them otherwise only by
  submitting while the mint keeps the answer.
  `worgl export PAYMENT --notes -o notes.json` writes them for the payee,
  who keeps them with `worgl accept`, and the purse then stops keeping
  them; `-o /dev/null` just stops keeping them, for a payee known to have
  them. `prune` drops them once their layer is dropped, and `submit` stops
  keeping them once it has written them for the payee; if it cannot write
  them, they stay kept, and it names the payment to export them from. For a
  payment the purse has no record of, such as one made by an earlier
  release, it keeps nothing, and says that the payee gets its notes with
  `worgl receive` while the mint keeps the answer.
- `pay` refuses a request for a key of a payment still kept, or of one among
  the last 1,024 keys the purse paid. Ask the payee for a new request.
- `worgl cancel TRANSFER` takes back the note of an outgoing transfer the
  mint never applied, such as a payment the payee will never submit. Use it
  only for a transfer nobody will submit: if the mint applies it, or already
  had, the payee's notes and the note taken back claim the same value, and
  the mint refuses whichever is spent second.
- `worgl forget TRANSFER` drops an outgoing transfer whose note is gone,
  such as one a mint that lost its state refuses as `unissued`.

Both `cancel` and `forget` keep the keys of the transfer's change, so change
handed back later is still kept with `accept`. `balance` counts it as
unconfirmed until then. While the purse keeps unconfirmed change or notes
for payees, it is saved in a format an earlier release refuses to open,
rather than open without them.

## Refused transfers

`settle` and `refresh` name each transfer the mint refuses by its id, as
`worgl outgoing` lists it, and say what they did with it:

| Refused as | What they do |
|---|---|
| `spent` or `expired`, with HTTP status 409 | Drop it: the note it spends is gone, whether the transfer was applied or not. Its change keys stay, so change the payee hands back is still kept with `accept`. |
| `layer_full`, with HTTP status 503 | Cancel it, keeping its change keys: the mint checks that the note it spends is live before it looks for room, so it never applied the transfer, and the layer its notes would land in gets no room back before that note expires. |
| `policy` | Cancel it if the policy the mint publishes now refuses it too, keeping its change keys: the mint checks its policy before its record of answers, so a transfer applied under a looser policy is refused this way too. Keep it otherwise. |
| `paused`, `busy`, `storage`, `internal`, another 5xx status, or no answer | Keep it, for the next `settle`: this may pass later. |
| `frozen` | Keep it: the note it spends moves after the next rotation. |
| Anything else, such as `unknown_mint`, `unissued` or `unauthorized` | Keep it: the refusal shows nothing about the transfer, such as a server that is not the mint's, or one that lost its state. The mint answers `spent` and `expired` with 409 and `layer_full` with 503, and never refuses a transfer the purse signed as `unauthorized` or `duplicate_input`, so such answers come from something else, such as a gateway or another version. `cancel` or `forget` ends it once you know which applies. |

They exit with status 0 when no refused transfer is kept, and with 1 while
one is. The error ends with "try again later" only when every transfer kept
was refused by something the row for `paused` lists.
`receive` and `submit` exit with 1 on any refusal, since the payment did not
go through.

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
rotation to fresh keys of the purse, spread over the time left. The time
left is counted by the server's clock, from the `Date` of its answer (by
yours only if the answer has none), so a clock of yours that runs slow or
fast moves none of them past the rotation; once the rotation's time has
come and the mint has not made it yet, they all go at once. It also resends
the transfers not yet seen answered whose spent note dies at that rotation,
or died at the last one. The change of a payment
the payee has submitted then comes back while the mint still has the answer.
A payment the payee has not submitted yet still goes through: `refresh` keeps
the payee's notes from the answer and says so, for
`worgl export PAYMENT --notes` (see [Payments kept](#payments-kept)), unless
the purse has no record of the payment, such as one made by an earlier
release. The payee can also get them by running `worgl receive` while the
mint keeps the answer (see [Error codes](../error-codes/)).
`worgl info` and `worgl balance` say when the next rotation is, by the
server's clock too.
`worgl prune` drops the notes whose layer the mint's info says it has
dropped, keys and all. It goes by the info alone, not by your clock, so it
drops no note the mint still holds live.
