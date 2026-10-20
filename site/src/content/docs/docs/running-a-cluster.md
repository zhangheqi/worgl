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
MEMBERS="--member 1=member1.internal:8080,member1.internal:7080 --member 2=member2.internal:8080,member2.internal:7080 --member 3=member3.internal:8080,member3.internal:7080"
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

Give host names, as above, rather than IP addresses. The members look a
name up each time they connect, so a member that moves to another machine
under the same name and ports, taking its own state directory with it,
needs only its name pointed there. To change
the addresses themselves, see [Moving a member](#moving-a-member).

`join-cluster` writes `cluster.json` and, the first time, `cluster.token`
(mode 0600), which the members show each other on their Raft ports. Copy the
directory before any member starts: the copies share the mints, tokens and
key-encryption key, and differ only in `--node`. Copy `kek.key` with the
rest, and do not run `init` on the other machines, which would make each a
key of its own: see [The key-encryption key](#the-key-encryption-key). A
directory a server has already run mints in can join too, and its mints
carry on with every note and setting. On a member, `serve` also listens on
its Raft port, or on `--raft-listen` if given.

## Keep the members private

Members talk to each other in plain HTTP, under the one cluster token they
share. Raft's messages go to each member's Raft address with that token,
several times a second. A member passes each request for a mint it does not
lead, and each mint creation, on to the leader's API address with its
`Authorization` header, so mint admin and operator tokens cross too.

A mint applies whatever its group's log holds without asking who wrote it,
and the cluster token, shown with a member's seal of a fixed value under
its key-encryption key (see
[The key-encryption key](#the-key-encryption-key)), lets anyone who reaches
a Raft port append to that log as if they led. Both cross the
network in the clear with every Raft message. With them anyone can append
events the mints will sign notes for, so the cluster token is as strong as
the mints' issuer keys.

Put every member's API and Raft address on a private network:

- WireGuard between the hosts, with each member's name in `--member`
  leading to its WireGuard address, is the simplest.
- A TLS sidecar works too, but at both ends. Members dial each other in
  plain HTTP and cannot dial an `https://` address, so each address must
  lead, on every other member's host, to a sidecar there that carries the
  request over TLS to that member.

A cluster that started on public addresses can move to private ones, one
member at a time; see [Moving a member](#moving-a-member).

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

A member whose copy of a mint stops, as when its disk fails a write,
answers `500 storage` to the requests for that mint it was carrying out
then. Until it is restarted, it passes later ones on to the leader it last
knew of, or answers them `503 no_leader` if that was itself, and its
`GET /readyz` answers `503` naming the mint `stopped`. Point the load
balancer's health check at `/readyz` so that it sends requests to the other
members instead. See [Monitoring](../running-a-server/#monitoring).

A mint, transfer or burn the mint already carried out is answered as it was
the first time, so after `503 no_leader` or `503 timeout`, send the same
request again. A rotation is not: one answered `503 timeout` may still be
made, even after an `info` read that does not show it, and sending it again
makes another. See [Error codes](../error-codes/#sending-a-request-again)
for how to check before rotating again.

Each member keeps its own receipts of recent transfers and burns, timed from
when it applied them, and the leader answers `503 busy` rather than take one
that would push out a receipt younger than the policy's
`receipt_min_age_secs`. A new leader goes on keeping them, so give every
member the same `receipt_capacity` and `receipt_min_age_secs` in the mint's
`policy.json`. A member passing on the leader's `busy` passes on its
`Retry-After` too. A member counts the receipts it held when it started, or
took in with a snapshot from the leader, as made then, so if it becomes the
leader within `receipt_min_age_secs` of that, a mint holding
`receipt_capacity` receipts answers every new transfer and burn `busy` for
the rest of that time. Leave at least that long between restarts of
members. See [Policy fields](../policy/#receipt_capacity-and-receipt_min_age_secs).

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

## The key-encryption key

`kek.key` must be identical on every member. A rotation seals its new
generation key with the leader's `kek.key`, and the registry seals a new
mint's secrets with that of the member leading it, so every member has to
unseal what any other sealed.

A member does not join or start without `kek.key`. `join-cluster` and
`serve` fail with `…/kek.key is missing`, and `join-cluster` writes nothing.
Copy `kek.key` from another member, or from a backup of this member's
directory. Only `init` makes one, and a new key would not open what the old
one sealed.

Members also check each other's key on every Raft request: each request
carries the sender's seal of a fixed value, which the receiver opens once and
remembers. A member whose `kek.key` differs, such as one that ran its own
`init` and was given only the cluster token, is refused by the others, and
refuses them in turn. Both sides log `kek.key differs from member N's`, and
the Raft port answers `403 kek_mismatch` with the same words. To each other
the two look cut off: nothing either sealed reaches the other, so no
member's copy of a mint stops over a key it cannot unseal. The odd member
creates no mint and makes no rotation, even if it led the registry or a mint
before it restarted: a leader proposes nothing until a majority has answered
it, and the others never answer the odd member. The other two carry on while
they are a majority, but until the odd member is fixed the cluster cannot
lose another. To fix it, stop it, copy `kek.key` from a member the others
accept, and start it again; it catches up from the others.

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

A member holds every change another holds when, for each mint and for the
registry, its log holds the other's last entry or starts after it:

- For a mint, run `export-log` on the other first, then on the member. Its
  log holds the other's last entry if it prints one with the same `index`,
  `term` and `node`, and starts after it if the first entry it prints has
  a higher `index`.
- For the registry, read each member's `registry/log.json`, its copy of the
  registry's log. The other's last entry is the last `log_id` under
  `entries`, or `purged` if there are none. The member's log holds it if
  one of its `entries` has the same `log_id`, and starts after it if its
  own `purged` has that `index` or a higher one.

An entry with the same index, term and node is the same on both members,
and so is every entry before it, so a member whose log holds the other's
last entry holds every entry the other holds. A member purges only entries
its snapshot holds, and a snapshot holds only entries the cluster has
carried out, which are the same on every member, so a member whose log
starts after the other's last entry lacks only entries the cluster never
carried out, and so never answered. Every change the cluster answered is on
at least two of the three members, so such a member holds every change
answered before the check. A member that was away, cut off or behind may
lack changes the other two made, and restoring all three from its copy
undoes them.

An export that prints nothing tells nothing: that member has purged its
whole log, as after installing the leader's snapshot. Use another member in
its place, or check again once it has logged the mint's next change.

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

## Changing the rules

Each member reads a mint's `policy.json` and `mint.json`, its rules and
rotation period, from its own directory when it starts serving the mint.
Nothing replicates or compares them. The member leading the mint checks
requests against its own policy and makes scheduled rotations on its own
period, and the others apply what it logs, so the rules in force are those
of whichever member leads. `GET /v1/mints/<mint>/info` on any member shows
the leader's.

Editing one member is therefore not enough: its rules hold only while it
leads, and the lead moves, for instance when the leader restarts. A mint
authority key removed from one member's policy goes on minting whenever a
member that still lists it leads.

To change a mint's rules:

1. [Pause the mint](../pausing/). The pause is replicated, so the mint
   refuses mints, transfers, burns and rotations while the members
   disagree.
2. Make the same edit to `policy.json`, and to `mint.json` if the period
   changes, on every member. Compare the files, for instance with
   `sha256sum`, to be sure they match.
3. Restart the members one at a time, so that two are always up for the
   cluster's other mints.
4. Check that `info` shows the new policy and period, then resume the mint.

A member that has no directory for a mint, because it was stopped or cut
off from the others when the mint was created, writes one from the
registry's record once it has the record, and serves the mint straight
away. It gets the policy and period the mint was created with, not later
edits. So if a member has no `mints/<mint>/` when you change the rules, keep
the mint paused until it has written one, make the edit there too, restart
it, and only then resume.

`receipt_capacity` must match too, though the members apply the same log.
Each member remembers as many recent transfers and burns as its own policy
says, and the leader answers a resent one from what it remembers, so with
different values one leader can answer a resend as before and another with
`spent`. See
[Sending a request again](../error-codes/#sending-a-request-again).

## Moving a member

A member reads `cluster.json` when it starts, and dials the others at the
addresses it gives: their Raft addresses for the registry's group and every
mint's, whichever addresses those groups formed with, and their API
addresses to pass requests on. `serve` listens for Raft on this member's
own Raft address there, unless `--raft-listen` is given, and for the API on
`--listen`.

To give a member new addresses:

1. Write them into `cluster.json` on every member, the same on each: run
   `join-cluster` again on each, with its own `--node` and the same new
   `--member` list, or edit the files by hand. `join-cluster` warns when it
   changes a member's addresses, and it and `serve` refuse a list that
   gives two members the same API or Raft address written alike. They
   cannot tell two spellings of one address apart, such as `localhost` and
   `127.0.0.1`, or a name and its IP address, so check that each address
   leads to the member it names. A member's Raft port refuses what that
   member sends itself, answering `421 misdirected`, so a member that
   dials itself as another finds that one unreachable instead of counting
   itself twice.
2. Restart every member, one at a time and each soon after the last, the
   moved one at its new addresses, but at least `receipt_min_age_secs`
   apart; see [Requests and leaders](#requests-and-leaders). If its API
   address changes, point the proxy in front of the members, and any
   faucet's `--server`, at the new one too.

A member that has not restarted yet still dials the old addresses, and one
that has dials the new ones. So until all have restarted, requests may be
answered `503 no_leader`:

- for a few seconds after each restart;
- if the API address changed, for the mints the moved member leads, and
  for new mints if it leads the registry;
- if the Raft address changed, when they reach the moved member while the
  leader of their mint has restarted and the moved member has not, or the
  other way round. That leader dials the moved member where it does not
  listen, so a few seconds on, the moved member knows of no leader to pass
  the requests on to. While the registry's leader is in that state, mint
  creations that reach the moved member are answered so too, and requests
  for mints created meanwhile `404 unknown_mint`.

Through a proxy that spreads requests over the members, a request sent
again soon reaches another member.

The groups' logs do not change, so the registry and every mint carry on
with every note. Move one member at a time, restarting all three before
moving the next: with two moved at once, a restart can leave two running
members that each dial the other where it does not listen, and the groups
stop until the third is back.

## Limits

- A mint's rules are not replicated: each member keeps its own `policy.json`
  and `mint.json`. See [Changing the rules](#changing-the-rules).
- Members talk in plain HTTP, and what the Raft port checks, the cluster
  token and the members' seals, crosses the network in the clear, so they
  need a private network between them; see
  [Keep the members private](#keep-the-members-private). TLS between
  members is not available yet.
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
