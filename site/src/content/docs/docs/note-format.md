---
title: Note format
description: The 149 bytes of a note, the messages holders and mint authorities sign, and how a note is checked offline.
---

A note is a fixed 149-byte record signed by its mint. In text it is the
URL-safe, unpadded base64 of its bytes, 199 characters long.

| Bytes | Field |
|---|---|
| 0 | Format version, `1` |
| 1..33 | Issuer public key |
| 33..41 | Serial generation, big-endian `u64` |
| 41..45 | Serial index, big-endian `u32` |
| 45..53 | Amount, big-endian `u64`, never zero |
| 53..85 | Holder public key, one-time |
| 85..149 | Ed25519 signature |

The signature covers `b"worgl/note/v1"` followed by bytes 0..85, and is
made by the key of the note's generation, which the issuer key certifies.
Signatures are Ed25519 with strict verification.

The mint stores no notes. The signature makes a note self-authenticating,
and the mint's bit array says whether it is still live. A serial is written
`generation:index`, as in `3:17`.

## Serials

The generation names the layer the note was issued into. It is never
reused: when a dropped layer's slot is reused, an old note's serial names a
generation that no longer exists, so it can never match the fresh bit that
sits where its bit used to be.

The index is the note's bit under a keyed permutation that differs in every
generation and is derived from the issuer key, run as ten rounds. A layer
hands out its bits in order, but a serial does not say whether a note was
issued before or after another, and the outputs of one split do not carry
consecutive indexes. The generation still tells which layer, and so roughly
which rotation period, a note was issued in.

## Generation keys

The issuer key does not sign notes. At every rotation the mint makes a fresh
key for the new last layer and certifies it with the issuer key over:

```text
b"worgl/generation-key/v1" ‖ issuer key (32) ‖ generation (u64) ‖ generation key (32)
```

`GET /v1/mints/{mint}/keys` answers the certificates of the live
generations. To check a note offline, find the certificate for its
generation, check it against the issuer key the note names, then check the
note's signature with the generation key. Whether the note is still live,
only the mint's ledger knows: ask `POST /v1/verify`.

## Transfers

The holder of every input signs one message that names all inputs and all
outputs:

```text
b"worgl/transfer/v1" ‖ issuer key (32)
  ‖ input count (u32) ‖ for each input:  generation (u64) ‖ index (u32)
  ‖ output count (u32) ‖ for each output: amount (u64) ‖ holder key (32)
```

All integers are big-endian. Because the message names the outputs, nobody
who sees a request in flight can redirect it, and because it names every
input, signatures cannot be moved between requests. Anyone may submit a
signed request: the payer, or the payee the payer handed it to.

The SHA-256 of the message is the request's id. The mint uses it to
recognise a resent request and answer it with the same notes, which works
because Ed25519 signatures are deterministic.

## Burns

A burn's holders sign under a different domain, so a burn signature never
passes as a transfer's or the reverse:

```text
b"worgl/burn/v1" ‖ issuer key (32)
  ‖ input count (u32) ‖ for each input:  generation (u64) ‖ index (u32)
```

## Mint requests

A mint authority signs:

```text
b"worgl/mint/v1" ‖ issuer key (32) ‖ authority key (32)
  ‖ sequence (u64) ‖ amount (u64) ‖ holder key (32)
```

See [Mint authorities and keys](../keys/).
