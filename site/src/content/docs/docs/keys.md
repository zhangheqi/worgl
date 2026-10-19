---
title: Mint authorities and keys
description: Who may mint, how mint requests are numbered, and every key and token a server keeps.
---

## Mint authorities

A mint should not be able to create value on its own say. Its policy lists
the public keys allowed to mint, `mint_authorities`, and each mint request is
signed by one of them over:

```text
b"worgl/mint/v1" ‖ issuer key (32) ‖ authority key (32)
  ‖ sequence (u64) ‖ amount (u64) ‖ holder key (32)
```

The server holds no mint authority key and mints no other way: no token can
mint. Each such mint is logged with its signature, so the log shows who
authorized every unit of supply.

`create-mint` makes a mint authority key when the policy lists none, writes
it to `mint.key` (mode 0600) in the mint's directory and lists it in
`policy.json`. The server never reads it. Move it to wherever you mint from.

To mint, sign a request and post it:

```sh
./target/release/worgl-server sign-mint --key ./server/mints/<mint>/mint.key --issuer <mint> \
    --sequence 1 --amount 8 --holder <holder public key> \
  | curl -sd @- -H 'content-type: application/json' http://127.0.0.1:8080/v1/mints/<mint>/mint
```

`worgl mint MINT AMOUNT --authority-key FILE` signs and posts in one step,
into its own purse.

### Sequence numbers

Each authority numbers its requests, and the mint accepts only a number
above the last one it accepted from that authority. A copied request
therefore cannot mint twice. Resending the latest request, for instance
after a lost response, returns the same note. Send one request at a time and
resend it until it is answered before using the next number. A number at or
below the last is refused with `stale_mint`.

`worgl mint` numbers its requests from the time in microseconds unless you
give `--sequence`.

### Adding and removing authorities

Edit `mint_authorities` in `policy.json` and restart. Removing a key stops
it minting. The mint keeps its sequence number, so adding it back cannot
replay its old requests. A request from a key the policy does not list is
refused with `unknown_authority`.

## The issuer key

A mint's issuer key is its identity: every note names it, and it names the
mint in every route. It lives in `issuer.key` (mode 0600). It does not sign
notes. The issuer key itself never rotates: a new one is a new mint.

## Generation keys

At every rotation the mint makes a fresh key for the new last layer,
certifies it with the issuer key and signs that generation's notes with it.
The key's secret is erased once its generation is dropped. A leaked
generation key therefore forges notes for at most as many rotations as there
are layers, and a leak exposes no key made after it.

The certificates of the live generations are published at
`GET /v1/mints/{mint}/keys`, so anyone who knows the issuer key can check a
note offline. The mint checks notes against its own keys only, so a leaked
issuer key cannot get a forged note past it. It could certify a false key
that fools someone checking offline, though, so a leaked issuer key still
means moving to a new mint.

The secrets live in `keys.bin` (mode 0600), apart from the snapshot and log,
which hold only public keys and certificates. Back `keys.bin` up with the
rest of the state: without it the live generations' notes cannot be signed
and the store does not open.

## The key-encryption key

`kek.key` (mode 0600) in the state directory is the key-encryption key. A
store seals each new generation key with XChaCha20-Poly1305 into the log
entry of the rotation that certifies it, bound to the issuer, the generation
and the key, and unseals it on opening if `keys.bin` lacks it. That is how
replicas that apply a rotation they did not make get its key. The file keeps
the key for development; a cloud KMS can stand behind the same `Kek` trait.
A KMS-backed key is not available yet.

## Tokens

| Token | File | What it allows |
|---|---|---|
| Operator token | `operator.token` | Creating and listing mints: `/v1/admin/mints`. |
| Admin token | `mints/<mint>/admin.token` | Rotating, pausing and resuming that mint only. |
| Cluster token | `cluster.token`, on cluster members | The members' Raft port, and with it every mint's log, so it is as strong as the issuer keys. See [Keep the members private](../running-a-cluster/#keep-the-members-private). |

Routes with a token take `Authorization: Bearer <token>`. A missing or wrong
token is answered `401 admin_unauthorized`. The server refuses to start if a
token is shorter than 32 characters.

### Replacing a token

A server reads its tokens once and never again: when it starts, or, for a
mint created while it runs, when it adds the mint. To replace one, write a
new one of at least 32 characters over its file, which keeps its mode, 0600,
and restart the server:

```sh
openssl rand -hex 32 > server/mints/<mint>/admin.token
```

On a cluster, write the same new token into the file on every member, then
restart them all. A member that does not lead a mint passes the
`Authorization` header on to the leader, which checks it against the token
it read, so until every member has restarted, the old token still works
whenever an unchanged member leads.

A member that writes a mint's directory from the cluster's registry, as one
that was away when the mint was created does when it returns, gets the
mint's first admin token: the registry holds that one, sealed, and never
learns of a replacement. Write the new token there too and restart that
member.

The key-encryption key cannot be replaced yet: nothing seals again what it
has sealed.

If an admin token leaks, pause its mint at once, then replace the token.
The token rotates its mint on request with no limit on how often, so as
many rotations in a row as the mint has layers expire every outstanding
note; a paused mint refuses them. It also resumes a paused mint as readily
as it pauses one, until it is replaced. See
[Pausing a mint](../pausing/).
