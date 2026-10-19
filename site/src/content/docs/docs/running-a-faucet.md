---
title: Running a faucet
description: Hand out notes of a playground mint to anyone who asks, within limits per address and per day.
---

A faucet hands out notes of one mint to anyone who asks, so people can try
Worgl without a mint of their own. It is one of the mint's authorities, like
any service that mints: it signs a mint request for each note and sends it to
the server, which changes nothing for it.

## Start one

```sh
./target/release/worgl-faucet --mint <mint> --key ./server/mints/<mint>/mint.key \
    --server http://10.0.0.1:8080 --server http://10.0.0.2:8080 --server http://10.0.0.3:8080 \
    --listen 127.0.0.1:8090
curl -sd '{"holder": "<holder public key>"}' -H 'content-type: application/json' \
    http://127.0.0.1:8090/v1/faucet
```

A mint request with no clear answer goes again, unchanged, to the next server
given. Run one faucet per mint key: it numbers its requests by the clock and
keeps no state of its own.

## Limits

Each note is worth 64, each client address gets 10 an hour (IPv6 by its /64),
and the faucet hands out 10,000 a day; `--amount`, `--per-address` and
`--per-day` change them. Past a limit it answers `429` with `Retry-After`.
`GET /v1/faucet` shows the mint, the limits and the notes left today.

Behind a reverse proxy, pass `--behind-proxy` so clients are counted by the
address the proxy appends to `X-Forwarded-For`.

## Taking a note

`worgl playground` asks the faucet at `--faucet`, or at the server's URL when
a proxy serves both on one address, and keeps the note. In Rust,
`Purse::take_from_faucet` does the same; in TypeScript,
`purse.takeFromFaucet(new Client(faucetUrl))`. Each note goes to a fresh key,
like any other.

## A playground mint

A playground mint should let untouched notes expire within about a day rather
than be wiped, so its issuer key never changes: with 4 layers and
`--rotate-every 28800`, a note nobody moves dies 16 to 24 hours after it was
minted.

[`deploy/playground`](https://github.com/zhangheqi/worgl/tree/main/deploy/playground)
runs one with Docker Compose: three members, the faucet and a TLS proxy on one
machine, on one address.
