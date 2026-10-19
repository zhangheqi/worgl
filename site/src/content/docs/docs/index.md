---
title: Docs
description: How to run a Worgl server, build on its SDKs and operate its mints, with the HTTP API, error codes, note format and policy fields for reference.
---

Start with the [quickstart](quickstart/): run a server on your machine,
create a mint, mint a note and move it between two purses with the `worgl`
command. Then pick the SDK for your language, or read
[How it works](../how-it-works/) first if you want the model before the
code.

## Start

- [Quickstart](quickstart/): a server, a mint and a payment between two
  purses, on your machine.
- [Concepts](concepts/): notes, mints, layers, rotation and holders.
- [Choosing a period and layer count](choosing-a-period/): how long notes
  live, how often they move, and what a ledger costs.

## Build

- [Rust SDK](rust-sdk/): `worgl-sdk`, the holder's side, and `worgl-client`,
  which runs it against a server.
- [TypeScript SDK](typescript-sdk/): the same purse for Node and browsers.
- [The worgl command](command-line/): a purse in an encrypted file, for
  scripts and for trying things out.
- [Embedding the library](embedding/): the issuer itself, inside your own
  program.

## Operate

- [Running a server](running-a-server/), [Running a cluster](running-a-cluster/),
  [Running a faucet](running-a-faucet/)
- [Mint authorities and keys](keys/), [Pausing a mint](pausing/),
  [Backups](backups/), [Exporting a log for audit](audit/)

## Reference

- [HTTP API](http-api/), [Error codes](error-codes/),
  [Note format](note-format/), [Policy fields](policy/),
  [Changelog](changelog/)
- [Privacy model](privacy/): what the mint, a payer and a payee each see.

Worgl is 0.1. It has not had an independent security review, so don't use
it to hold anything you can't afford to lose.
