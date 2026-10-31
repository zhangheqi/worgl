# Worgl Purse

A purse for [Worgl](../README.md) tokens that lives in the browser: no
sign-up, nothing on a server of its own. It is a static page, to be hosted
at `purse.worgl.1y.io` on Cloudflare Pages, built with Vite and TypeScript
and no UI framework, on the [TypeScript SDK](../sdk/typescript/README.md),
which runs the Rust core as WebAssembly.

This version is the shell, Holdings, keep-alive, Receive, Send and History:

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
  when to open the purse again, and a short log of what happened. It shows
  the mint used last: the one added, opened by its link, taken from, asked
  in or sent from most recently.
- **Keep-alive on open**: while the purse is on screen, it moves on the
  tokens that would fade at the next rotation (below).
- **Receive** (`#receive`): ask for an amount, with the open requests listed
  above how it goes. The request is a link and a QR code to hand to the
  sender, who sends and hands a link back; take it by pasting it or with the
  camera. An open request can be cancelled, and Receive opens on the newest
  one for half an hour.
- **Send** (`#send`): open a request by its link or its code, see how the
  amount goes out of what is ready (or why it cannot, under "Not enough
  ready", with what can be done), and send. Each note goes as the mint
  allows, counted down; the hand-back link and code come at the end, for
  the receiver. A send cut off before every note is answered is settled
  by keep-alive's next pass (on open, or right after the send), and its
  hand-back is kept until then.
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

- **History** (`#history`): the shown mint's register, by day, newest
  first: each entry's time, what it was, what it changed and what the
  purse held after it, under "What came and went". Each row opens to what
  was behind it: the notes a take brought and the request it answered, what
  keep-alive moved and why, what faded and when, a send's transfers, what
  went and what came back; a send whose hand-back or send can still be shown
  opens it on Send from there. Under the register: what faded and was lost
  since the oldest entry kept, and that the register is kept in this
  browser alone. Holdings' Recent leads there, and so does the desktop bar.

- **Backup** (`#backup`): what the last backup covers of what the purse
  holds, under "Keep a copy": each note held, largest first, solid when the
  backup names it and dashed when it moved or came since, and "Your last
  backup covers 128 of 187 tokens". Below it a new backup is saved as an
  encrypted file, and a file is restored. Holdings' Backup link on the
  phone carries a mark while the last backup covers less than half of what
  is held, and the desktop's mint panel has a Backup row. With no mint yet
  it is the first run's way to restore ("Have a backup file? Restore it").

Not yet: the Mints screen (Holdings shows the mint used last), a menu on
the phone, and a service worker.

## History

History is the purse's own log (below), the shown mint's entries, by when
each happened rather than when it was written: a fade is written on the
open after it, one entry for each layer that went, at the time of the
rotation that took that layer by the mint's schedule. When that time falls before its notes came (a mint rotated by
hand), the fade is put just after the last of them came and says "some
time after"; with no schedule to go by, it is put when the purse saw the
notes gone and says "or before" (the entry's `fadeAt`). What is held after
each row is not kept with the entry but worked back from what the purse
holds now (the SDK's `Balance.total`, which counts a send's change while it
is on its way and leaves out what goes to the receiver): the newest row's
number is the one on Holdings, and each row before it is its next row's,
less that row's change, the number in its Tokens column. So a send that
ended hours after it began counts once, as it ended, and the column always
adds up. A send's change counts what its answered transfers gave the
receiver and every note a transfer the mint dropped spent; a transfer it
turned away changes nothing, as its note came back. So a send's entry keeps
what was dropped (`lost`, `lostTheirs`, `dropped`) and how many transfers
were turned away (`turnedAway`) from the first submit on, and is `lost`
once nothing of it is on its way if any was dropped. A lost send that part
went through is "Sent 16 of 20 · 8 lost". A send some of whose transfers
ended where the purse never saw (a settle that kept no answer for them and
counted them neither dropped nor turned away) is marked `blind`. A row
whose change is not known (part of a send whose answers gave no value, one
the mint turned part of away while the rest waits, or a `blind` one) shows
"?", as its Recent row on Holdings does, and leaves the rows before it
without a number, and so does a number that would go below
nothing, which only a log entry that could not be written would cause. The
column is empty until the shown mint is read, and again from when an entry
is logged until a read that began after it comes back.

Each entry since this version also carries the values of its notes, so a
row can say "As 4 + 1": a take's and keep-alive's in `values`, beside
their serials; a fade's in `gone`; a send's spent notes in `spentValues`,
from the read its plan was checked on. An entry written before says the
tokens instead.

## Backup

A backup is the SDK's `Purse.backup`: every note of every mint the purse
holds, with its secret key, and the mint list (each mint's server, and the
name and unit the purse gave it), sealed with a passphrase of 8 characters
or more, typed twice (Argon2id over 64 MiB, then XChaCha20-Poly1305: the
`worgl` command line's purse file, which it opens too). Each mint's entry
also keeps when it was sealed (`saved`), which a restore takes as that
backup's time: a file's own date is only when that copy was made. A
passphrase is composed (Unicode NFC) before it is counted, compared and
sealed, so an accented letter typed as one character or as a letter and a
mark seals alike; opening tries it composed, then as typed, then
decomposed, for a file sealed elsewhere.

Where the browser has a save dialog (`showSaveFilePicker`) it asks where
first, while the press still counts, and a second press while the dialog
is open does nothing; the file written there is the last backup at once.
Elsewhere the file downloads, named `worgl-purse-2026-10-30-1413.worgl`,
and a download says nothing back (the browser may ask first, or be told
no), so it counts as a backup only once the person presses "It is saved";
until then Save waits, and "Download again" sends the same file. The
passphrases are kept in the app, not in the state the view is drawn from,
and put back into their fields after each draw, so they are never in the
page's markup; what was typed when Save or Restore was pressed is what
seals or opens, and they are forgotten once used and on leaving Backup. A
save or restore that ends while the person is elsewhere keeps what it came
to for their next visit to Backup, and moves no focus.

A backup covers exactly the notes held now whose serials it names: a note
that moves (a send, keep-alive) gets a new key, and the backup no longer
brings it back. So each mint's record keeps the time and the serials of the
notes held both before and after the seal (`backup`), what Backup draws
and Holdings' Backup row say are those still held, and History gets "Saved
a backup", which changes nothing.

Restoring merges (the SDK's `Purse.restore`). The file is checked before
any passphrase is asked for (not empty, at most 8 MiB, a sealed purse
file); a wrong passphrase, or a file changed since, is refused with one
message. A mint the purse has goes to its own server; a new one goes to
the server the file names, only if a link could name it (https, or http on
the machine itself), and is read there first and named by that server, as
a link's mint would be (the name and unit a file gives are never taken: a
file can come from anyone); a
mint at no such server is left out, and the purse says to add it by its
link and restore again. Each server's mints are restored holding
`purse:transfers`, and what came of it is logged before the lock goes:
"Restored 192" (the notes it brought, which this purse did not hold), with
the notes the backup had spent that this purse still held dropped, and its
outgoing transfers brought along for keep-alive to settle. A note that had
faded meanwhile is dropped by the next pass and logged as a fade "from a
backup", after the restore. A send it brought that the mint turns away
comes back as "Restored", and one whose answer the mint no longer keeps is
"Lost", as keep-alive's are.

A restored note comes in unchecked: a copy of the purse may have spent it
since. The purse asks the mint about each, one at a time and two to six
seconds apart, from one tab (the Web Lock `purse:checks`, taken with
`ifAvailable`), on Backup and on any route, now and on each open until
every one is answered. Each question is asked holding `purse:transfers`
(with `ifAvailable`: when a send or keep-alive holds it, the check waits a
gap and asks again), and its answer is logged before the lock goes, so
History's Held column steps with each drop. A spent note is dropped and the
mint's answers go in one entry per mint and run: "Dropped 64 already
spent", or "Checked 128 restored" when none was. Once anything else of the
mint is logged after it, a note found spent starts a new entry, and one
found live joins the earlier entry where it stands, so no drop is dated
past a row that came after it. For the same reason "Saved a backup" is
logged when it is written, with when it was sealed beside it (a download
said saved later), and a restored file's time is never taken as later
than the restore. Until it is answered, keep-alive
leaves a restored note where it is (a hop it planned for one is cancelled),
and a send first asks about its mint's unchecked notes as the store keeps
them, since the SDK picks the notes a send spends: it logs each answer as
it comes, and sends nothing if the mint did not answer for one.

Below that, Backup says whether the browser agreed to keep the purse's
data (`navigator.storage.persisted()`), and, outside an installed purse,
that Safari keeps it only once added to the Home Screen.

## Keep-alive

Keep-alive is the SDK's refresh and nothing more. Each note that the next
rotation would drop (the first layer, "fading") moves one layer on, alone
in its own transfer, to a fresh key; the purse calls `Purse.planRefresh`,
spreads its transfers over a few seconds, and sends them with `submit`.
One visit keeps tokens for one to two rotations: 8 to 16 hours on the
playground. Holdings then says "Open again before" and the date, and
Recent gets "Kept 128 alive", one entry for each mint and rotation: a
later pass in the same rotation adds to it. The date is the earliest
across every mint added; when it is for another mint's tokens than those
shown, the row names that mint ("For your tokens at …"). A mint the page
has not read yet, or that does not answer, counts by the first fade the
purse saved with it; so does the shown mint when its read fails on open.

When it runs: on open, when the page becomes visible again or comes back
from the back/forward cache, at each rotation while the page is on
screen, after any read of Holdings that finds fading tokens, and each
minute on the other screens. Each pass, for each server: read, drop and
log what faded, settle every outgoing transfer (one a closed page left
half sent included, and what a send left for later, whose Recent entry
then says what it came to), and read again after a settle, since a
resent transfer's note may have landed in the layer that fades next;
then for each mint that has fading tokens plan the refresh, send its
transfers 1 to 3 seconds in and 2 to 5 seconds apart, within two minutes
and before the SDK's five-minute margin before the rotation, log what was
kept, and read again each mint it moved, so its words and its date are of
now.
While it runs, the phone's back row and the desktop's come-back column
read "Keeping 128 alive · 1 of 2" with a box for each transfer, the
notes on their way stay in the fading lane, and each one that lands
slides one lane on in pink. The screen is kept awake while transfers go
(Screen Wake Lock), where the browser allows it. Off Holdings a pass's
run is not spoken and the screen is not kept awake; a send it settles is
spoken on any screen ("The send of 5 went through"), as the send's own
end would be.

What it does not do:

- **It never runs in the background.** A hidden tab (another tab in
  front, the browser minimised, the phone locked) stops at the next safe
  step, never in the middle of a transfer, and a closed page stops at
  once; a transfer cut off stays outgoing, and the next open settles it.
  Left closed or hidden, tokens fade at the date on screen, and the copy
  says "on screen", not "open". There is no service worker, no push and
  no notification.
- **It moves only what fades next**, one layer: no lift to the last layers,
  no split, and no other note moves.
- **No reminders** beyond the date on screen: no calendar entry, no keeper
  to hand tokens to.
- **No switch to turn it off**: the purse says it keeps tokens alive while
  on screen, and does.
- **It never runs during a send.** A send asks the pass in any tab to
  end: the pass sends the transfers it has in hand at once rather than a
  few seconds apart, starts nothing more, and so leaves no hop unsent
  (one the mint does not answer, and those after it, stay outgoing for the
  next pass). The send waits for that, which takes seconds unless the mint
  is slow to answer, since a transfer on its way is answered first, and
  then for the lock. Another tab's pass, once it has ended, waits ten
  seconds before it tries again, so that the send takes the lock first,
  and it runs again as soon as the sending tab says its send is over
  (`sent`). While Send shows a request's plan, not yet sent, that tab's
  keep-alive leaves the mint alone too (a pass at its server ends that way
  before Send reads, and later ones neither move its notes nor resend what
  is outgoing that spends them; the other mints' are settled as ever), so
  that the notes signed are the notes shown. It moves them all the same
  once the mint's rotation is less than a quarter of an hour off. Another
  tab's keep-alive knows nothing of the plan and may move them too. Either
  way, the check under the lock sees it, and Send says that what is ready
  changed.

When it cannot keep tokens, it says why and when they fade: the mint did
not answer ("They fade at 16:00 unless the purse reaches it before then.
The purse asks again each minute while it is on screen.", from the last
read this tab made, or on a fresh open from the first fade the purse
saved with the mint), the next layer is full, the mint refused by its
policy, the mint applied a transfer but no longer keeps its answer ("Lost
64" in Recent), or the browser's storage refused. A mint that rotates in
under four hours gets a line on the first run that a visit keeps its tokens
only for a few hours. The mint sees which notes one visit moved; the
fading lane's tip says so.

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
| `npm test` | `node --test` over the pure logic in `test/`: mint links, request and hand-back links, deflate, QR codes, words, times, amounts, the planner, open requests, notes and transfers, the field's geometry, errors, which SDK purse is current, keep-alive's spacing and decoding, the keeper's passes with stand-ins, History's register, Backup's words and arithmetic, the grain |
| `npm run mint` | the local mint and faucet (`scripts/dev-mint.mjs`) |
| `npm run screenshots` | `scripts/preview.mjs`: the first run and Holdings, below |
| `npm run screenshots:send` | `scripts/preview-send.mjs`: Receive and Send between two purses, below |
| `npm run screenshots:history` | `scripts/preview-history.mjs`: History, from the entries of real sends, takes, keep-alive and a fade |
| `npm run screenshots:backup` | `scripts/preview-backup.mjs`: Backup: a save, a send after it, a restore in a new browser and the checks |
| `npm run plan-check` | `scripts/plan-check.mjs`: the planner's choice of notes, checked against the SDK's own `pay` on a local mint |
| `npm run grain` | draws `public/grain-*.png` again (`scripts/grain.ts`) |

`scripts/preview.mjs` starts a mint that rotates every two minutes and a
faucet on free ports, builds the purse twice (as hosted, and with that mint
as the playground), serves both under the policy, and walks them in
Chromium: the first run, takes from the faucet and rotations, until the
field holds fading, ready and resting tokens and keep-alive has moved the
first ones on. Two more mints that rotate every 8 hours, on a server of
their own with a faucet each, and a third build with the first of them as
its playground, are for the keep-alive walk: a run with its transfers held
so each step can be shot, the landing slide, the result, a page closed in
the middle of a transfer and the open that settles it, a run in another
tab, a mint that does not answer its transfers, and one that does not
answer at all on open; then a purse that holds both of those mints, the
second one's tokens going first while the first is shown, and the second
not answering. It writes PNGs of each
view at 360 x 780 and 1280 x 860, light and dark, and of the tips; any
console error, page error or policy violation fails it. It needs
`playwright` where Node can require it (`NODE_PATH`); `--out <dir>` says
where the PNGs go (`target/purse-preview/shots/`), and `--quick` rotates
the mint by hand instead of waiting.

`scripts/preview-send.mjs` is the walk of Receive and Send, on the same
plumbing (`scripts/preview-kit.mjs`) and with its own `--out`
(`target/purse-preview/send-shots/`): two purses in their own browser
contexts ask, send and take between them, on mints of their own that
rotate by hand (their schedule's next rotation is set well after the walk),
through every view and state of Receive and Send (the
camera from a fake video of a request's code, a slow mint, a dropped
answer, a send turned away, lost or cut short, links that do not read).
After every view it checks the mint's supply and the camera's
Permissions-Policy, and it fails on any console error, page error or
policy violation, or on a view wider than the phone. For the camera it
needs Playwright's full Chromium rather than its headless shell.

`scripts/preview-history.mjs` makes a register between two purses: takes
from the faucet, a send from one note, one the mint turns away and one
from two notes, the hand-backs taken in, a keep-alive run, and a fade over
rotations with the page closed. It shoots the sender's Holdings and
History, closed and with rows open, and the receiver's History with a
received row open, into `target/purse-preview/history-shots/` unless
`--out` says. It checks that the Held column ends at Holdings' total, that
every row has a number, that each is the one after it and its change, and
that the oldest is its own change, as each purse began empty.

`scripts/preview-backup.mjs` saves a backup in one purse (after a short
passphrase and two that differ are refused), sends from one of its notes so
the backup covers less, then restores the file in a new browser from the
first run: a wrong passphrase first, then the right one, and the mint's
checks drop the note that was spent. The first purse restores the file too,
and nothing in it is new. It shoots Holdings, Backup at each step and both
Histories with the new rows open into `target/purse-preview/backup-shots/`
unless `--out` says, checks the Held column as the History walk does, and
fails on a passphrase anywhere in the page's markup.

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
  that change saves what it kept (`src/current.ts`).
- **One lock for every transfer.** A keep-alive pass, a send, "Try
  again now" and a restore each hold the Web Lock `purse:transfers` (`src/lock.ts`), so
  no two of them, in this tab or another, run at once. A pass takes it
  with `ifAvailable` and never steals it: a second tab finds it taken and
  shows the first one's run ("In another tab") instead of starting its
  own, and a tab that hears nothing for 30 seconds tries again. The lock
  is held per pass, not for the tab's life. A send first tells every tab
  to end its pass (`keeper.stop()`, which has the pass send what it has
  in hand at once and start nothing more), then waits for the lock; once
  it is sent, this tab's next pass runs and settles anything the send left
  for later. "Try again now" resends only sends' transfers (those of every
  send still on its way at that server), and leaves keep-alive's to
  keep-alive, which logs them.
  Taking from the faucet, asking and taking a hand-back in need no such
  lock, since every change of a store takes the SDK's own lock
  (`worgl:<server URL>`) and reads it again first.
- **Its own records** are in an IndexedDB database named `purse`, at
  version 4 (a tab on an older version closes its records when a newer one
  opens): the mints added (key, server, name, the info last read, when last
  used, when and how much fades first as last read, so a fresh open
  with the mint down can still say it, and what the last backup named), the requests made (what Receive
  shows and lists; the SDK keeps their keys; open ones are never dropped,
  and of the ended ones the newest 50 stay), and a log of up to 200
  entries, every write strict: `faucet` ("Took 64 from the faucet"),
  `faded` ("Faded 5"), `kept` ("Kept 128 alive", one for each mint and
  rotation, with the serials and values of the new notes; a later pass adds to it,
  and a token moved twice counts once), `lost` ("Lost 64": a transfer the
  mint applied whose answer it no longer keeps), `sent` ("Sent 5", or "Sent
  16 of 20 · 8 lost" when the mint dropped part of it),
  `received` ("Received 20"), `backup` ("Saved a backup"), `restored`
  ("Restored 192") and `checked` ("Dropped 64 already spent"). A send still on its way is never trimmed,
  however old, since its settle writes what it came to there.
- **Sends and hand-backs.** The SDK keeps each payment and what it owes the
  receiver (`payments()`, `owed`), so a hand-back is never lost: Send shows
  it again from Recent, and the purse never forgets it. A send that ends
  before every note is answered is settled by keep-alive's next pass, on
  open or right after the send (the SDK's `settle`), and its Recent entry
  then says what it came to: the change is read from the answers to the
  send's own transfers, never from what else came to the mint in the same
  settle, and each note of it keeps the time its own answer came.
- **Other tabs** hear of each change on the `BroadcastChannel` named
  `purse`, and read the stores again; so does a tab that comes back into
  view. A running pass also posts its progress there (`keeping`, then
  `kept`), and a send posts `yield`, which ends a pass in every other tab,
  and `sent` once it is over, when their passes run again.
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
| `src/app.ts` | state, routes, actions, what keeps Holdings current, and keep-alive's wiring (the lock, the channel, the wake lock) |
| `src/keep.ts` | keep-alive's pure logic: the time budget and spacing, which note a transfer spends, moving and kept notes, when a mint is due, the step count and boxes |
| `src/keeper.ts` | keep-alive's runner: when passes run and stop (`Keeper`), and one pass over a server (`Keeping`), every browser and SDK dependency injected |
| `src/lock.ts` | the one Web Lock that keep-alive passes, sends, settles and restores take, and the one the checks of restored notes take |
| `src/first.ts`, `src/home.ts`, `src/receive.ts`, `src/send.ts`, `src/history.ts`, `src/backup.ts`, `src/shell.ts` | the views, and the bars and brand they share |
| `src/register.ts` | History's register: the rows by day, what was held after each, and what each row says |
| `src/cover.ts` | Backup's words and arithmetic: the passphrase, the file, what a backup covers, where a restore puts each mint, and what a restore and the checks came to |
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
