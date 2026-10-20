# Playground deploy

A public playground mint on one machine with Docker Compose: three cluster
members, the faucet, and [Caddy](https://caddyserver.com) in front for TLS.
Everything is served on one address. The faucet answers `/v1/faucet`, and
the members answer everything else, so `worgl playground` finds the faucet
at the server's own URL.

```sh
cd deploy/playground
PLAYGROUND_HOST=playground.example.com docker compose up -d --build
docker compose logs setup        # prints the playground mint's issuer key
```

Point the host's DNS at the machine and open ports 80 and 443; Caddy gets a
certificate on its own. Without `PLAYGROUND_HOST` it serves `localhost` with
a certificate from its own local authority, for trying it out.

```sh
export WORGL_SERVER=https://playground.example.com
worgl init
worgl playground        # keeps a note and prints the mint
```

## What starts

- **setup** runs once, before the others. It creates the mint (4 layers of
  4,194,304, rotating every 8 hours), moves its mint authority key into the
  faucet's volume, and copies the state to each member's volume with that
  member's `cluster.json`. When it finds a member's state already there, it
  does nothing, so `up` again keeps the mint.
- **member1** to **member3** serve the mint as a cluster. A member that
  fails, or whose `/readyz` says its copy of the mint has stopped, is
  skipped by Caddy and replaced as leader by the others within a few
  seconds; while they elect, requests are answered `503 no_leader` and can
  be sent again.
- **faucet** hands out notes of 64, 10 an hour per address and 10,000 a
  day, counting clients by the address Caddy forwards. It keeps its counts
  in memory, so they start again when it restarts.
- **proxy** is Caddy. It refuses `/v1/admin/*` and the mint's `rotate`,
  `pause` and `resume`, which only the operator needs, and spreads the rest
  over the members.

The members and the faucet are on an internal network with no route to the
internet, so the members' Raft and API ports, which carry the cluster token
and the requests members pass on to each other, are reachable only by them
and Caddy.

Settings, as environment variables for `docker compose up`:

| Variable | Default | |
|---|---|---|
| `PLAYGROUND_HOST` | `localhost` | The address Caddy serves and gets a certificate for. |
| `WORGL_LAYERS`, `WORGL_LAYER_SIZE`, `WORGL_ROTATE_EVERY` | `4`, `4194304`, `28800` | The mint's ledger and rotation period, read once by setup. |
| `FAUCET_AMOUNT`, `FAUCET_PER_ADDRESS`, `FAUCET_PER_DAY` | `64`, `10`, `10000` | The faucet's limits. |
| `RUST_LOG` | `info` | Log level of the members and the faucet. |

A new note sits in the second-to-last layer, so one nobody moves dies at the
third rotation after it was minted: 16 to 24 hours with the defaults. Notes
that keep moving keep living. The issuer key never changes, so examples and
SDK defaults pointed at it keep working.

## Operating it

- `docker compose exec member1 cat /state/operator.token` shows the operator
  token, and `/state/mints/<mint>/admin.token` the mint's admin token.
- `docker compose exec member1 worgl-server export-log --mint /state/mints/<mint>`
  prints what member1's log of the mint still holds. Each member purges its
  own log after a snapshot, so two members' exports can start at different
  entries; an entry with the same index and term is the same on both. See
  [When the log is purged](../../site/src/content/docs/docs/audit.md#when-the-log-is-purged).
- Back up one of the `member*` volumes: it holds the whole mint, its keys
  and the key-encryption key. Copy a member that holds every change another
  member holds, checked as
  [Running a cluster](../../site/src/content/docs/docs/running-a-cluster.md#backups-and-a-failed-member)
  describes, and stop it for the copy while the others serve. The copy
  restores all three members, never one alone, with every change answered
  before the check: with all three stopped, put it in every member's volume
  and run `worgl-server join-cluster` on each with its own `--node`, as
  `setup.sh` does, then start them. A member restored on its own can undo
  transfers the cluster answered; to replace one, see the same page. Back
  up the `faucet` volume too, since only it holds the mint authority key;
  without that key nothing can mint.
- `docker compose down -v` deletes the mint for good, with every note.

The proxy refuses the routes the admin token opens, so rotate, pause or
resume the mint from inside a member. The image has no curl, so this speaks
HTTP through bash, and the token never reaches the host or the proxy:

```sh
mint=$(docker compose exec -T faucet cat /state/mint)
docker compose exec -T member1 bash -c '
  exec 3<>/dev/tcp/127.0.0.1/8080
  printf "%s\r\n" "POST /v1/mints/$1/$2 HTTP/1.1" "Host: member1" \
    "Authorization: Bearer $(cat "/state/mints/$1/admin.token")" \
    "Content-Length: 0" "Connection: close" "" >&3
  cat <&3' bash "$mint" pause    # or resume, or rotate
```

Any member will do, since one that does not lead the mint passes the request
on; use `member2` or `member3` if `member1` is down. A pause or resume can be
sent again unchanged. A rotation cannot: one answered `503 timeout` may
still be made, though the answer says to send the same request again, so
check as
[Sending a request again](../../site/src/content/docs/docs/error-codes.md#sending-a-request-again)
says before sending it again, to any member.

Pause the mint this way during an incident. If the admin token itself may
have leaked, pause at once all the same, then write the same new token into
`/state/mints/<mint>/admin.token` on all three members and restart them, as
[Replacing a token](../../site/src/content/docs/docs/keys.md#replacing-a-token)
says. The pause survives the restart, and after it the old token can no
longer resume the mint.

The three members share one machine here, so the cluster survives a member
process failing, not the machine. Spread them over three machines for that,
following [Running a cluster](../../README.md#running-a-cluster).
