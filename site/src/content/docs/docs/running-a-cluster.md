---
title: Running a cluster
description: Run three servers that keep every mint on all three, so each mint carries on when one server fails.
---

Three servers can run as a cluster that keeps every mint on all three, so
each mint carries on when one server fails. Each mint is a Raft group of the
members, built on `worgl-raft`, and a change is answered once two of the
three hold it on disk.

## Start one

Every member starts from a copy of one state directory:

```sh
S=./target/release/worgl-server
MEMBERS="--member 1=10.0.0.1:8080,10.0.0.1:7080 --member 2=10.0.0.2:8080,10.0.0.2:7080 --member 3=10.0.0.3:8080,10.0.0.3:7080"
$S init         --state ./server
$S join-cluster --state ./server --node 1 $MEMBERS
# Copy ./server to the other two machines and run join-cluster there with --node 2 and --node 3.
$S serve        --state ./server --listen 0.0.0.0:8080
```

Each `--member` gives a member's id, the address the other members pass API
requests on to and the address of its Raft port, as `ID=API,RAFT`. Give
every member, this one included. Both addresses must be on a network only
the members and a proxy in front of them can reach; see
[Keep the members private](#keep-the-members-private).

`join-cluster` writes `cluster.json` and, the first time, `cluster.token`
(mode 0600), which the members show each other on their Raft ports. Copy the
directory before any member starts: the copies share the mints, tokens and
key-encryption key, and differ only in `--node`. A directory a server has
already run mints in can join too, and its mints carry on with every note
and setting. On a member, `serve` also listens on its Raft port, or on
`--raft-listen` if given.

## Keep the members private

Members talk to each other in plain HTTP, under the one cluster token they
share. Raft's messages go to each member's Raft address with that token,
several times a second. A member passes each request for a mint it does not
lead, and each mint creation, on to the leader's API address with its
`Authorization` header, so mint admin and operator tokens cross too.

A mint applies whatever its group's log holds without asking who wrote it,
and the cluster token lets anyone who reaches a Raft port append to that
log as if they led. With it they can append events the mints will sign
notes for, so the cluster token is as strong as the mints' issuer keys.

Put every member's API and Raft address on a private network:

- WireGuard between the hosts, with each member's WireGuard address in
  `--member`, is the simplest.
- A TLS sidecar works too, but at both ends. Members dial each other in
  plain HTTP and cannot dial an `https://` address, so each address must
  lead, on every other member's host, to a sidecar there that carries the
  request over TLS to that member.

Choose these addresses before the cluster first starts. The registry's
group and each mint's keep the Raft addresses `cluster.json` held when they
formed, in their logs, and dial those for good. But `serve` listens on the
Raft address `cluster.json` holds now, unless `--raft-listen` is given, so
no group formed before can reach a member restarted with a new one, and once
every member has moved, the registry and every mint stop.

So run `join-cluster` again only to change API addresses, giving every Raft
address exactly as before, and restart every member: members only pass
requests on to the API addresses, and `serve` takes the API on `--listen`.
On a cluster whose Raft addresses are public, keep the traffic to them
private instead, for example by routing it over WireGuard between the hosts
and refusing the Raft ports from anywhere else.

The members' HTTP clients use any proxy set in `HTTP_PROXY`, `ALL_PROXY`
or their lower-case forms, so with one set, Raft's messages and the
requests members pass on, tokens included, go to it in plain HTTP. Leave
them unset where a member runs, or name every member's host in `NO_PROXY`.

## Requests and leaders

Any member takes any request, and one that does not lead the mint passes it
on to the one that does. A read is answered once the leader confirms it
still leads, so no answer is older than one already given.

When a leader fails, the others elect a new one within a few seconds. Until
then, requests are answered `503 no_leader`, and sending the same request
again is safe. A leader cut off from the other members stops taking
requests after two seconds without hearing from them, and a member that
cannot reach the leader says so within a few seconds, so a client that
reaches a cut-off member is answered `503` instead of waiting on it. Sent
again through an address that covers every member, the request can reach
one of the others; see [The address clients use](#the-address-clients-use).
Only the leader makes scheduled rotations.

A mint, transfer or burn the mint already carried out is answered as it was
the first time, so after `503 no_leader` or `503 timeout`, send the same
request again. A rotation is not: one answered `503 timeout` may still be
made, even after an `info` read that does not show it, and sending it again
makes another. See [Error codes](../error-codes/#sending-a-request-again)
for how to check before rotating again.

## The address clients use

`worgl-client`, the TypeScript SDK and the `worgl` command talk to one
address, and a purse sends a failed request again to that same address.
Give them one that covers every member, or a member that fails or is cut
off takes its clients with it while the other two carry on.

Best is a reverse proxy in front of the members that skips a member it
cannot connect to and spreads requests over all of them, so a request sent
again after `503 no_leader` soon reaches another member. The playground's
[`Caddyfile`](https://github.com/zhangheqi/worgl/blob/main/deploy/playground/Caddyfile)
does this:

```text
reverse_proxy member1:8080 member2:8080 member3:8080 {
  lb_policy round_robin
  lb_retries 2
  health_uri /healthz
  health_interval 5s
  fail_duration 30s
}
```

The proxy's requests carry clients' tokens too, so it should reach the
members over the same private network, at their WireGuard addresses for
example.

At the least, use a DNS name that lists every member's address. That lets a
client connect to another member when one refuses, but not move off a
cut-off member that answers `503 no_leader`.

## Creating mints

With the members running, `POST /v1/admin/mints` to any of them creates a
mint on all three. It goes to the member leading the cluster's registry, a
Raft group of the members that lists the mints created this way. A mint's
record holds its policy, period and first state, with its issuer key, admin
token and generation keys sealed by the key-encryption key the members
share. Every member writes the mint's directory from that record, so all of
them start the mint alike, and a member that was away writes it when it
returns.

The answer comes once the mint's group has a leader; a member that has not
yet read the record answers `404 unknown_mint` for a moment.

- A creation answered `503 no_leader` made nothing, so send it again.
- One answered `503 timeout` may have made a mint the answer never named.
  List the mints before creating another, and read its admin token from
  `admin.token` in its directory on any member.

`worgl-server create-mint` works on a member's directory too. Copy the new
mint's directory to every other member before any of them starts; the
command says so when it finishes.

## A member's directory

Beside what any server's state directory holds (see
[Running a server](../running-a-server/)), a member's holds
`cluster.json`, this member's id and every member's addresses;
`cluster.token`; and `registry/`, its copy of the cluster's registry. Each
mint's directory also holds `vote.json`, the replica's Raft vote. Tokens
are read once, not reloaded; to replace one, see
[Replacing a token](../keys/#replacing-a-token).

## Limits

- Members talk in plain HTTP, and the Raft port checks only the cluster
  token, so they need a private network between them; see
  [Keep the members private](#keep-the-members-private). TLS between
  members is not available yet.
- The members are fixed once the cluster starts: members cannot be added or
  removed, and a group keeps the Raft addresses it formed with. Not
  available yet.

## How it is tested

A simulation runs a mint's three-replica group through seeded faults on a simulated clock: replicas
cut off, split apart, crashed and restarted, messages lost and delayed.
Afterwards it checks that the replicas agree, that every answered change is
there, that no note was spent twice and that supply balances.

```sh
cargo test -p worgl-raft --test simulation
```

It runs 32 seeds by default; set `WORGL_SIM_SEEDS=1000` for more, or
`WORGL_SIM_SEED=<n>` to repeat one.
