# Worgl Purse

A purse for [Worgl](../README.md) tokens that lives in the browser: no
sign-up, nothing on a server of its own. It is a static page, to be hosted
at `purse.worgl.1y.io` on Cloudflare Pages, built with Vite and TypeScript
and no UI framework, on the [TypeScript SDK](../sdk/typescript/README.md),
which runs the Rust core as WebAssembly.

This version is the shell, Holdings, Receive and Send:

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
  when the first tokens fade, and a short log of what happened. It shows
  the mint used last: the one added, opened by its link, taken from, asked
  in or sent from most recently.
- **Receive** (`#receive`): ask for an amount, with the open requests listed
  above how it goes. The request is a link and a QR code to hand to the
  sender, who sends and hands a link back; take it by pasting it or with the
  camera. An open request can be cancelled, and Receive opens on the newest
  one for half an hour.
- **Send** (`#send`): open a request by its link or its code, see how the
  amount goes out of what is ready (or why it cannot, under "Not enough
  ready", with what can be done), and send. Each note goes as the mint
  allows, counted down; the hand-back link and code come at the end, for
  the receiver. A send the receiver has not taken yet is settled the next
  time the purse opens, and its hand-back is kept until then.
- **Links.** A request is `#ask=<payload>` and a hand-back `#take=<payload>`,
  where the payload is base64url (unpadded) of raw deflate of a few lines:
  `ask/1`, the server, the SDK's request JSON; or `take/1`, the server, the
  mint's key, the request's id, the amount, then one note per line. The QR
  code carries the whole link, so a phone's own camera opens the purse. A
  link read from the address bar is replaced at once with `#send` or
  `#receive`, so a reload never takes or sends again. A link pasted with
  words round it, or wrapped over lines, still reads. What a hand-back
  carries, and which request it answers, are read from its notes
  themselves (their value, and the one-time key each is paid to), never
  from the link's own amount or request id.

Not yet: keeping tokens alive while the purse is open (the SDK's refresh),
History, Backup, the Mints screen, and a service worker. Until keep-alive comes, the
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
| `npm test` | `node --test` over the pure logic in `test/`: mint links, request and hand-back links, deflate, QR codes, words, times, amounts, the planner, open requests, notes and transfers, the field's geometry, errors, which SDK purse is current, the grain |
| `npm run mint` | the local mint and faucet (`scripts/dev-mint.mjs`) |
| `npm run screenshots` | `scripts/preview.mjs`: the first run and Holdings, below |
| `npm run screenshots:send` | `scripts/preview-send.mjs`: Receive and Send between two purses, below |
| `npm run plan-check` | `scripts/plan-check.mjs`: the planner's choice of notes, checked against the SDK's own `pay` on a local mint |
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

`scripts/preview-send.mjs` is the walk of Receive and Send, on the same
plumbing (`scripts/preview-kit.mjs`) and with its own `--out`
(`target/purse-preview/send-shots/`): two purses in their own browser
contexts ask, send and take between them, on mints of their own that
rotate by hand, through every view and state of Receive and Send (the
camera from a fake video of a request's code, a slow mint, a dropped
answer, a send turned away, lost or cut short, links that do not read).
After every view it checks the mint's supply and the camera's
Permissions-Policy, and it fails on any console error, page error or
policy violation, or on a view wider than the phone. For the camera it
needs Playwright's full Chromium rather than its headless shell.

## Camera

Send and Receive can read a code with the camera, and only on "Turn on the
camera": the purse never starts it unasked, never asks for the permission
before that tap, and stops it the moment the view, the tab or the code no
longer needs it. It needs `Permissions-Policy: camera=(self)`, which
`public/_headers` sends (and `npm run preview` too). It reads with
`BarcodeDetector` where that knows QR codes, and otherwise with `jsqr`,
loaded on first use. Pasting the link always works, with the camera
refused, missing or running.

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
  added (key, server, name, the info last read, when last used), a log of
  up to 200 entries ("Took 64 from the faucet", "Sent 5", "Received 20"),
  and the requests made (what Receive shows and lists; the SDK keeps their
  keys), every write strict. Open requests are never dropped; of the ended
  ones the newest 50 stay.
- **Sends and hand-backs.** The SDK keeps each payment and what it owes the
  receiver (`payments()`, `owed`), so a hand-back is never lost: Send shows
  it again from Recent, and the purse never forgets it. A send that ends
  before every note is answered is settled the next time the purse opens
  (the SDK's `settle`).
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
| `src/first.ts`, `src/home.ts`, `src/receive.ts`, `src/send.ts`, `src/shell.ts` | the views, and the bars and brand they share |
| `src/flow.ts`, `src/flow-words.ts`, `src/slice.ts`, `src/postmark.ts`, `src/icons.ts` | what Receive and Send draw: the stamps, codes, ticket and scanner, their words, the slice of the field a send takes from, the postmark of what came in |
| `src/plan.ts`, `src/amount.ts`, `src/asks.ts`, `src/entries.ts` | which notes a send takes, the amount typed, the open requests, and the log's sends and receipts |
| `src/links.ts`, `src/deflate.ts`, `src/qr.ts`, `src/camera.ts` | request and hand-back links, their deflate, the QR codes, and the camera |
| `src/notes.ts` | what a note and a transfer say, read from their documented formats: a note's value, serial and key, what a send paid to whom |
| `src/field.ts` | the field, drawn as SVG, its tips, the needle and the slides |
| `src/layout.ts` | the field's geometry, for any number of layers (at most 8 lanes) and any note (at most 256 squares) |
| `src/holdings.ts`, `src/words.ts`, `src/format.ts` | what is held, in numbers and in words |
| `src/purses.ts`, `src/current.ts` | everything asked of the SDK, loaded after the first paint, and which SDK purse is current |
| `src/records.ts` | the purse's own IndexedDB records |
| `src/link.ts`, `src/errors.ts` | mint links, and every error in plain words |
| `src/styles.css` | the approved look, then what the app adds |
