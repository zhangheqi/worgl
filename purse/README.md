# Worgl Purse

A purse for [Worgl](../README.md) tokens that lives in the browser: no
sign-up, nothing on a server of its own. It is a static page, to be hosted
at `purse.worgl.1y.io` on Cloudflare Pages, built with Vite and TypeScript
and no UI framework, on the [TypeScript SDK](../sdk/typescript/README.md),
which runs the Rust core as WebAssembly.

This first version is the shell and Holdings:

- **The first run** (`#first`): what the purse is and how its field reads,
  then a way in. A mint link (`#mint=<issuer key hex>&at=<server URL>`)
  shows the mint's stamp, its face and the first eight digits of its key,
  read from its server before anything is kept. Paste a link to get there.
  A build may name a playground mint to start with (below).
- **Take from the faucet**, when the mint's server answers `GET /v1/faucet`
  for that mint, as the playground's does.
- **Holdings** (`#home`): the total, one sentence on what is ready, resting
  and fading, the field (one square per token, one lane per layer, a ruler
  of the current rotation with a needle for now), tips on lanes and notes,
  when the first tokens fade, and a short log of what happened.

Not yet: receiving and sending, keeping tokens alive while the purse is
open (the SDK's refresh), History, Backup, the Mints screen (Holdings shows
the first mint added), and a service worker. Until keep-alive comes, the
purse says that tokens fade unless they move, and gives the date the first
ones fade where it will give the date to open it again by.

## Run it against a local mint

Build the SDK, the server and the faucet once, from the repository root:

```sh
(cd sdk/typescript && npm ci && npm run build)
cargo build --package worgl-server --package worgl-faucet --locked
```

Then, in `purse/`:

```sh
npm ci
npm run mint     # a mint, its faucet and one address for both, on 127.0.0.1:8080
npm run dev      # the purse on http://127.0.0.1:5173
```

`npm run mint` prints the mint link to open. Its mint rotates every 5
minutes (`DEV_MINT_PERIOD` seconds) and puts new notes in its last layer, so
what you take rests until the next rotation; `node scripts/dev-mint.mjs
rotate` rotates it at once, and `--fresh` makes a new mint. Its state lives
in `target/purse-dev-mint/`. It looks for the binaries in `target/debug/`,
or where `WORGL_SERVER_BINARY` and `WORGL_FAUCET_BINARY` say.

`npm run build` writes the site to `dist/`, and `npm run preview` serves it
under the hosted Content-Security-Policy, with the local mint allowed.

## Scripts

| Script | What it does |
|---|---|
| `npm run dev` | Vite's dev server on 127.0.0.1:5173 |
| `npm run build` | the site, in `dist/` |
| `npm run preview` | `dist/` on 127.0.0.1:4173, under the policy in `public/_headers` |
| `npm run check` | `tsc` over the app, and over the tests and scripts |
| `npm test` | `node --test` over the pure logic in `test/`: links, words, times, the field's geometry, errors, which SDK purse is current, the grain |
| `npm run mint` | the local mint and faucet (`scripts/dev-mint.mjs`) |
| `npm run screenshots` | `scripts/preview.mjs`, below |
| `npm run grain` | draws `public/grain-*.png` again (`scripts/grain.ts`) |

`scripts/preview.mjs` starts a mint that rotates every two minutes and a
faucet on free ports, builds the purse twice (as hosted, and with that mint
as the playground), serves both under the policy, and walks them in
Chromium: the first run, takes from the faucet and rotations, until the
field holds fading, ready and resting tokens. It writes PNGs of each view
at 360 x 780 and 1280 x 860, light and dark, and of the tips. It needs
`playwright` where Node can require it (`NODE_PATH`); `--out <dir>` says
where the PNGs go (`target/purse-preview/shots/`), and `--quick` rotates the
mint by hand instead of waiting.

## Settings

`VITE_PLAYGROUND_LINK`, at build time, names a mint by its link. When set,
the first run offers "Start with the Playground", which adds that mint and
takes from its faucet. It is empty by default: the code names no mint and
no server.

## How it keeps things

- **Tokens** live only in the SDK's `IndexedDbStore`, one for each server,
  made once and named after the server's URL as the SDK's `Client` writes
  it (the database and lock `worgl:<server URL>`), behind one SDK `Purse`
  per server. After another tab changed a store, the purse opens a new
  `Purse` on it, but never while a change of the current one runs, since
  that change saves what it kept (`src/current.ts`). The purse takes no
  lock of its own.
- **Its own records** are in an IndexedDB database named `purse`: the mints
  added (key, server, name, the info last read) and a log of up to 200
  entries ("Took 64 from the faucet", "Faded 5"), every write strict.
- **Other tabs** hear of each change on the `BroadcastChannel` named
  `purse`, and read the stores again; so does a tab that comes back into
  view.
- On the first take the purse asks the browser to keep its storage
  (`navigator.storage.persist()`). Clearing the site's data still empties
  it, and a private window forgets it when it closes.

## Hosting

Cloudflare Pages serves `dist/` as it is. `public/_headers` gives every
page its Content-Security-Policy and the usual security headers, and the
hashed assets a year's cache. The policy allows scripts and styles from the
site alone, WebAssembly compiled from them, requests to the site and to any
https server, and requires Trusted Types: the purse builds its DOM with
`createElement` and `textContent`, never from markup, and sets styles
through the CSSOM. The fonts are the site's own `@fontsource` packages,
bundled, and the print grain is two PNGs drawn at build time.

A faucet answers the hosted purse only when it names its origin:
`FAUCET_ALLOW_ORIGIN=https://purse.worgl.1y.io` on the playground
(see [`deploy/playground`](../deploy/playground/README.md)).

## Layout

| File | What |
|---|---|
| `src/main.ts` | the fonts, the styles, then the app |
| `src/app.ts` | state, routes, actions, and what keeps Holdings current |
| `src/first.ts`, `src/home.ts`, `src/shell.ts` | the two views and the brand |
| `src/field.ts` | the field, drawn as SVG, its tips, the needle and the slides |
| `src/layout.ts` | the field's geometry, for any number of layers (at most 8 lanes) and any note (at most 256 squares) |
| `src/holdings.ts`, `src/words.ts`, `src/format.ts` | what is held, in numbers and in words |
| `src/purses.ts`, `src/current.ts` | everything asked of the SDK, loaded after the first paint, and which SDK purse is current |
| `src/records.ts` | the purse's own IndexedDB records |
| `src/link.ts`, `src/errors.ts` | mint links, and every error in plain words |
| `src/styles.css` | the approved look, then what the app adds |
