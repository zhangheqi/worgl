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

Each `--member` gives a member's id, the address of its public API and the
address of its Raft port, as `ID=API,RAFT`. Give every member, this one
included.

`join-cluster` writes `cluster.json` and, the first time, `cluster.token`
(mode 0600), which the members show each other on their Raft ports. Copy the
directory before any member starts: the copies share the mints, tokens and
key-encryption key, and differ only in `--node`. A directory a server has
already run mints in can join too, and its mints carry on with every note
and setting. On a member, `serve` also listens on its Raft port, or on
`--raft-listen` if given.

## Requests and leaders

Any member takes any request, and one that does not lead the mint passes it
on to the one that does. A read is answered once the leader confirms it
still leads, so no answer is older than one already given.

When a leader fails, the others elect a new one within a few seconds. Until
then, requests are answered `503 no_leader`, and sending the same request
again is safe. A leader cut off from the other members stops taking
requests after two seconds without hearing from them, and a member that
cannot reach the leader says so within a few seconds, so clients that still
reach a cut-off member go elsewhere instead of waiting on it. Only the
leader makes scheduled rotations.

A mint, transfer or burn the mint already carried out is answered as it was
the first time, so after `503 no_leader` or `503 timeout`, send the same
request again. See [Error codes](../error-codes/).

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
mint's directory also holds `vote.json`, the replica's Raft vote.

## Backups and a failed member

Back up the members' directories as any server's (see
[Backups](../backups/)), but restore them only together: every member from
a copy of one moment, with every member stopped, before any of them starts.
Take the copies one of two ways:

- Copy every member's directory with every member stopped, and restore
  each member from its own copy.
- Copy one member's directory, with that member stopped, once it holds
  every change another member holds, and restore all three from it. Each
  then needs its own `cluster.json`: run `join-cluster` on each with its
  own `--node`, as when the cluster started.

A member holds every change another holds when, for each mint, the last
entry `export-log` prints on the other, run first, is among those it
prints, and the last `log_id` in the other's `registry/log.json`, its copy
of the registry's log, is in its own. Every change the cluster answered is
on at least two of the three members, so such a member holds every change
answered before the check. A member that was away, cut off or behind may
lack changes the other two made, and restoring all three from its copy
undoes them.

Never restore one member on its own. A member restored from an older copy
has forgotten what it acknowledged to the others, and nothing prevents
that. If it helps a member that also lacks those changes become leader, the
cluster loses changes it answered, and a note paid away can be spent again.
A leader that had counted the member as holding entries the copy lacks logs
a warning containing `follower log is reverted`; a leader elected since has
nothing to compare with, and logs nothing.

To replace a member whose directory is lost, copy one that runs instead:

1. Stop one of the two members still running, once it holds every change
   the other holds. Until it is back, the other alone is not a majority, so
   the mints take no changes.
2. Copy its whole state directory to the machine that takes the failed
   member's place, at the failed member's addresses, and run `join-cluster`
   on the copy with the failed member's id.
3. Start both. The new member catches up from the others.

```sh
# On the new machine, with the copy in ./server, if member 3 failed:
$S join-cluster --state ./server --node 3 $MEMBERS
$S serve        --state ./server --listen 0.0.0.0:8080
```

Deleting a member's mint logs and `vote.json` files so that it receives
snapshots from the others does not work: a member does not start with a
mint whose `log.bin` is missing, or whose snapshot holds entries its group
wrote while its `vote.json` is missing. Never delete a mint's directory or
`registry/` on a member either. When it starts, the member writes a mint's
directory again from the registry's record, if the registry holds the
mint, and opens an empty registry in place of `registry/`; either way it
rejoins the group having forgotten what it acknowledged, as if restored
from an empty copy.

## Limits

- The Raft port speaks plain HTTP and checks only the cluster token, so keep
  it on a network that only the members can reach.
- The members are fixed once the cluster starts: members cannot be added or
  removed. Not available yet.

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
