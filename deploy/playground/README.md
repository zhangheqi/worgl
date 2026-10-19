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
- **proxy** is Caddy. It refuses `/v1/admin/*`, which only the operator
  needs, and spreads the rest over the members.

The members and the faucet are on an internal network with no route to the
internet, so the Raft ports are reachable only by them and Caddy.

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
  token, and `/state/mints/<mint>/admin.token` the mint's admin token. Pause
  the mint during an incident with `POST /v1/mints/<mint>/pause` and that
  token; the proxy passes it through.
- `docker compose exec member1 worgl-server export-log --mint /state/mints/<mint>`
  prints the mint's log. Every member's should match.
- Back up one of the `member*` volumes: each holds the whole mint, its keys
  and the key-encryption key. Back up the `faucet` volume too, since only it
  holds the mint authority key; without that key nothing can mint.
- `docker compose down -v` deletes the mint for good, with every note.

The three members share one machine here, so the cluster survives a member
process failing, not the machine. Spread them over three machines for that,
following [Running a cluster](../../README.md#running-a-cluster).
