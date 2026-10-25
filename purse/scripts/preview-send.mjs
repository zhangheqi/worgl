// Screenshots of Receive and Send between real purses: a mint that rotates
// by hand, its faucet, and the purse built and served as in preview.mjs
// (scripts/preview-kit.mjs). Two browser contexts, R (the receiver) and S
// (the sender), each with its own IndexedDB, ask, send and take between
// them through every view and state of Receive and Send; U opens a
// request for a mint it does not hold, and V holds none at all; a second
// browser, C, has a fake camera that shows R's request as a QR code, and
// refuses the camera to a context not granted it. It writes PNGs of each
// view at 360 x 780 and 1280 x 860, light and dark. After every view it
// checks the mint's supply and the camera's Permissions-Policy, and it
// fails on any page error, console error or policy violation, on a view
// wider than the phone, and on any failed check.
//
//   node scripts/preview-send.mjs [--out <dir>]
//
// Its mint lives in target/purse-preview/send/ (PREVIEW_HOME), made afresh
// each run; the PNGs go to target/purse-preview/send-shots/ unless --out
// says, apart from scripts/preview.mjs's shots of Holdings.

import { closeSync, openSync, writeSync } from 'node:fs';
import { join } from 'node:path';
import { deflateRawSync, inflateRawSync } from 'node:zlib';

import qrcode from 'qrcode-generator';

import {
  chromiumOf,
  clearPeriod,
  failingRequests,
  launch,
  newContext,
  options,
  repo,
  report,
  setup,
  shooter,
  step,
  stop,
} from './preview-kit.mjs';

const { out } = options(join(repo, 'target/purse-preview/send-shots/'));
const home = process.env.PREVIEW_HOME ?? join(repo, 'target/purse-preview/send/');
const chromium = chromiumOf();

// mint A, with the faucet, rotates by hand (its schedule is about 8 hours, with its next rotation well after the walk,
// which takes about half an hour); mint B is held by the camera's purses
const { mint, builds } = await setup({ home, period: clearPeriod(28_800, 90 * 60_000), perAddress: 1000, mints: 2 });
const [A, B] = mint.issuers;
const play = builds.playground;
const plain = builds.plain;

const problems = [];
const { shots, size, shootAll: shootEach } = shooter(out);
let failed;

/**
 * Every view at both sizes and in both themes, then: no page error, no
 * console error, no policy violation, nothing wider than the phone; the
 * camera's policy as both builds serve it; and the mint's supply.
 */
async function shootAll(page, view) {
  await shootEach(page, view, async (each, name) => {
    if (name === 'm360') {
      const wide = await each.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      if (wide > 0) {
        problems.push(`${view}: ${wide} px wider than the phone`);
      }
    }
  });
  await phone(page);
  await policies(view);
  clean(view);
  await supply(view);
}

/** Both builds send Permissions-Policy: camera=(self), and nothing looser. */
async function policies(where) {
  for (const build of [play, plain]) {
    const policy = (await fetch(`${build}/`)).headers.get('permissions-policy') ?? '';
    if (!policy.startsWith('camera=(self)')) {
      problems.push(`${where}: ${build} serves Permissions-Policy: ${policy}`);
    }
  }
}

/** Stops the walk on the first problem, so its cause is the step that showed it. */
function clean(where) {
  if (problems.length > 0) {
    throw new Error(`problems at ${where}`);
  }
}

function check(ok, words) {
  if (!ok) {
    throw new Error(`check failed: ${words}`);
  }
  console.log(`preview:   ok: ${words}`);
}

/** Back to the phone, light, for the walk's taps. */
async function phone(page) {
  await page.setViewportSize({ width: 360, height: 780 });
  await page.emulateMedia({ colorScheme: 'light' });
}

/** Goes to `hash`, as a link there would; to a flow, by way of Holdings, so that it opens as it does from there. */
async function go(page, hash) {
  if (hash === '#receive' || hash === '#send') {
    await go(page, '#home');
  }
  await page.evaluate((to) => {
    location.hash = to;
  }, hash);
  await page.waitForTimeout(150);
}

const text = (page, selector) => page.$eval(selector, (el) => el.textContent ?? '');
const hashOf = (link) => new URL(link).hash;
const hashNow = (page) => page.evaluate(() => location.hash);

/** A link's frame, as the purse wrote it: the lines inside its payload. */
const frameOf = (link) => inflateRawSync(Buffer.from(/#(?:ask|take)=([A-Za-z0-9_-]+)/.exec(link)[1], 'base64url')).toString('utf8');
/** A payload for `frame`, as the purse writes one. */
const payloadOf = (frame) => deflateRawSync(Buffer.from(frame, 'utf8'), { level: 9 }).toString('base64url');

/** After every transfer: the mint's supply adds up, and it minted only what the faucet handed out. */
async function supply(where) {
  const info = await mint.info(A);
  const s = info.supply;
  const layers = info.layers.reduce((sum, layer) => sum + Number(layer.value), 0);
  const ok =
    Number(s.outstanding) === Number(s.minted) - Number(s.destroyed) - Number(s.burned) &&
    layers === Number(s.outstanding) &&
    Number(s.minted) === 64 * mint.proxy.faucets;
  check(ok, `${where}: supply ${JSON.stringify(s)}, ${mint.proxy.faucets} faucet takes`);
}

/** A purse's first run on the playground build: Start with 64. */
async function start(page) {
  await page.goto(`${play}/`);
  await page.waitForSelector('[data-fk="first-start"]');
  await page.click('[data-fk="first-start"]');
  await page.waitForSelector('.f-note');
}

/**
 * Waits until no purse's keep-alive has a transfer at the mint, nor sends
 * one for 6.5 s (its hops go at most 5 s apart): what the proxy is told to
 * hold, drop or refuse next then meets the transfer the walk means, not a
 * hop of another purse's.
 */
const quiet = () => mint.proxy.quiet(6500);

/**
 * Rotates mint A by hand, once no purse's keep-alive is part way through a
 * run: a rotation between two of its hops would drop the second one's note
 * before it goes, which the mint then refuses as expired.
 */
async function rotate() {
  await quiet();
  await mint.rotate();
}

/** Opens mint A's link again and takes 64 from the faucet, then waits for Holdings to show it. */
async function take(page) {
  const before = await page.evaluate(() => Number(document.querySelector('.total b')?.textContent ?? '0'));
  await go(page, mint.link);
  await page.waitForSelector('[data-fk="first-take"]');
  await page.click('[data-fk="first-take"]');
  await page.waitForFunction((n) => Number(document.querySelector('.total b')?.textContent ?? '0') >= n + 64, before);
  await page.waitForTimeout(300);
}

/** Holdings' total. */
const total = (page) => page.evaluate(() => Number(document.querySelector('.total b')?.textContent ?? 'NaN'));

/** R asks for `amount` (in notes of at most `atMost`), and gives the request's link. */
async function ask(page, amount, atMost, shotSmaller) {
  await go(page, '#receive');
  await page.waitForSelector('#ask-amount, [data-fk="ask-new"]');
  if ((await page.$('[data-fk="ask-new"]')) !== null) {
    await page.click('[data-fk="ask-new"]');
    await page.waitForSelector('#ask-amount');
  }
  await page.fill('#ask-amount', String(amount));
  // a new request keeps "Ask in smaller notes" as the last one had it
  if ((await page.$('[data-fk="ask-smaller"][aria-expanded="true"]')) !== null) {
    await page.click('[data-fk="ask-smaller"]');
    await page.waitForSelector('[data-fk="ask-smaller"][aria-expanded="false"]');
  }
  if (atMost !== undefined) {
    await page.click('[data-fk="ask-smaller"]');
    await page.waitForSelector('#ask-atmost');
    await page.selectOption('#ask-atmost', String(atMost));
    await page.waitForTimeout(200);
    if (shotSmaller !== undefined) {
      await shootAll(page, shotSmaller);
    }
  }
  await page.click('[data-fk="ask-go"]');
  await page.waitForFunction(() => document.querySelector('#ask-link')?.textContent?.includes('#ask=') === true);
  return text(page, '#ask-link');
}

/** S opens a request by its link, and waits for the step it leads to. */
async function open(page, link, until) {
  // from Holdings, so that nothing of the view before can answer for the one it leads to
  await go(page, '#home');
  await page.goto(link);
  await page.waitForSelector(until, { timeout: 60_000 });
  await page.waitForTimeout(300);
}

/** S sends the request it shows, and waits for its hand-back. */
async function send(page) {
  await page.click('[data-fk="send-go"]');
  await page.waitForSelector('[data-fk="copy-take"]', { timeout: 90_000 });
  await page.waitForTimeout(300);
  return text(page, '#take-link-out');
}

/** The Recent rows on the phone, as text. */
const recentRows = (page) => page.$$eval('.recent-p .act', (rows) => rows.map((row) => row.textContent ?? ''));

/** A y4m video of `blank` frames of a dim room, then `frames` frames of `link` as a QR code on a light screen, 4 px a module, for Chromium's fake camera. */
function writeY4m(path, link, blank = 80, frames = 40) {
  const W = 640;
  const H = 480;
  const code = qrcode(0, 'L');
  code.addData(link, 'Byte');
  code.make();
  const n = code.getModuleCount();
  const unit = 4;
  const side = (n + 8) * unit;
  const x0 = Math.floor((W - side) / 2);
  const y0 = Math.floor((H - side) / 2);
  // a dim room first, then a phone's light screen held up in it, showing the code with its quiet zone
  const light = Buffer.alloc(W * H, 92);
  const shown = Buffer.alloc(W * H, 92);
  for (let y = y0 - 2 * unit; y < y0 + side + 2 * unit; y++) {
    shown.fill(235, y * W + x0 - 2 * unit, y * W + x0 + side + 2 * unit);
  }
  for (let r = 0; r < n; r++) {
    for (let c = 0; c < n; c++) {
      if (code.isDark(r, c)) {
        for (let y = 0; y < unit; y++) {
          shown.fill(16, (y0 + (r + 4) * unit + y) * W + x0 + (c + 4) * unit, (y0 + (r + 4) * unit + y) * W + x0 + (c + 5) * unit);
        }
      }
    }
  }
  const chroma = Buffer.alloc((W / 2) * (H / 2) * 2, 128);
  const fd = openSync(path, 'w');
  writeSync(fd, `YUV4MPEG2 W${W} H${H} F10:1 Ip A1:1 C420jpeg\n`);
  for (let i = 0; i < blank + frames; i++) {
    writeSync(fd, 'FRAME\n');
    writeSync(fd, i < blank ? light : shown);
    writeSync(fd, chroma);
  }
  closeSync(fd);
}

const policy = await fetch(`${play}/`).then((answer) => answer.headers.get('permissions-policy') ?? '');
const browser = await launch(chromium);
let camera;
try {
  check(policy.startsWith('camera=(self)'), `the preview serves Permissions-Policy: ${policy}`);
  const R = await (await newContext(browser, problems, 'R')).newPage();
  const S = await (await newContext(browser, problems, 'S')).newPage();

  step('1. funds: S and R start with 64 each, a rotation, and 64 more for S');
  await start(S);
  await start(R);
  await rotate();
  await take(S);
  check((await total(S)) === 128, 'S holds 128');
  await go(S, '#home');
  await shootAll(S, 'home-acts');

  step('2. R asks for 5');
  await go(R, '#receive');
  await R.waitForSelector('#ask-amount');
  await shootAll(R, 'receive-ask');
  await R.fill('#ask-amount', '0');
  await R.click('[data-fk="ask-go"]');
  await R.waitForSelector('.v-recv-ask .err');
  check((await text(R, '.v-recv-ask .err')) === 'Type a whole number of tokens, 1 or more.', `an amount of 0: ${await text(R, '.v-recv-ask .err')}`);
  check((await text(R, '#ask-split')) === 'Type a whole number of tokens, 1 or more.', 'the hint says so too');
  await shootAll(R, 'receive-ask-invalid');
  const link5 = await ask(R, 5);
  await R.waitForSelector('svg.qr');
  check(link5.length <= 330, `the request link is ${link5.length} characters`);
  await shootAll(R, 'receive-request');
  await R.click('[data-fk="recv-paste"]');
  await R.waitForSelector('#take-link');
  await shootAll(R, 'receive-request-paste');
  await R.click('[data-fk="ask-new"]');
  await R.waitForSelector('.asks .act.go');
  check((await text(R, '.asks')).includes('5'), 'the 5 is listed');
  await shootAll(R, 'receive-open');
  await R.click('.asks .act.go');
  await R.waitForSelector('#ask-link');

  step('3. S opens the link');
  await open(S, link5, '.split');
  const split = await S.$$eval('.split b', (els) => els.map((el) => el.textContent));
  check(split.join(' ') === '64 5 59', `the split reads ${split.join(' ')}`);
  check((await S.$$('#slice-confirm .f-mark')).length === 1, 'one note is outlined');
  check((await S.evaluate(() => location.hash)) === '#send', 'the hash is #send');
  await shootAll(S, 'send-confirm');

  step('4. S sends 5, the mint slow to answer');
  await quiet();
  mint.proxy.hold(25_000);
  await S.click('[data-fk="send-go"]');
  await S.waitForSelector('.v-send.is-sending');
  await shootAll(S, 'send-sending-one');
  mint.proxy.release();
  await S.waitForSelector('[data-fk="copy-take"]', { timeout: 60_000 });
  const take5 = await text(S, '#take-link-out');
  check((await S.$('.view[data-plan="match"]')) !== null, 'the planner chose what pay spent (data-plan="match")');
  check((await S.$('.changed .sw.live')) !== null, 'the change is pink');
  await shootAll(S, 'send-handback');
  await supply('after the send of 5');

  step('5. R takes the hand-back');
  const before = 64;
  await quiet();
  mint.proxy.holdKeys(25_000);
  await R.goto(take5);
  await R.waitForSelector('.v-take');
  check((await hashNow(R)) === '#receive', 'the hand-back left the address bar before it was taken');
  await shootAll(R, 'receive-taking');
  mint.proxy.releaseKeys();
  await R.waitForSelector('.pm-wrap', { timeout: 30_000 });
  await R.waitForTimeout(600);
  check((await text(R, '.postmark .big')).includes('5'), `the postmark reads ${await text(R, '.postmark .big')}`);
  check((await R.$('.chip')) !== null, 'the chip shows');
  check((await recentRows(R)).some((row) => row.includes('Received 5')), 'Recent has Received 5');
  check((await text(R, '.sentence')).includes(', including the 5 you just took'), 'the sentence names the 5 just taken');
  check((await total(R)) === before + 5, `R's total grew by 5 (${before} to ${await total(R)})`);
  await shootAll(R, 'home-received');

  step('6. R opens the same hand-back again');
  await R.goto(take5);
  await R.waitForFunction(() => document.querySelector('.v-take h1')?.textContent?.includes('already') === true);
  await shootAll(R, 'receive-taken-twice');

  step('7. S afterwards');
  await go(S, '#home');
  await S.waitForSelector('.recent-p .act.go');
  check((await text(S, '.chip')).replace('−', '-').includes('-5'), `the chip reads ${await text(S, '.chip')}`);
  const sent = (await recentRows(S))[0] ?? '';
  check(sent.includes('Sent 5') && sent.includes('hand-back'), `Recent: ${sent}`);
  await shootAll(S, 'home-sent');
  await S.click('.recent-p .act.go');
  await S.waitForSelector('[data-fk="copy-take"]');
  await shootAll(S, 'send-handback-again');
  // the hand-back is R's: S holds the mint, but not the request's keys
  await S.goto(take5);
  await S.waitForFunction(() => document.querySelector('.v-take h1')?.textContent?.startsWith('Not for this purse') === true, undefined, { timeout: 30_000 });
  check((await text(S, '.v-take .body')).includes('someone else’s request'), `not ours: ${await text(S, '.v-take .body')}`);
  await shootAll(S, 'receive-not-ours');

  step('8. nothing ready: R asks 20, S holds 59 and 64, both resting');
  const link20 = await ask(R, 20);
  await open(S, link20, '#slice-short');
  check((await text(S, 'h1')).startsWith('Nothing is ready'), `short: ${await text(S, 'h1')}`);
  await shootAll(S, 'send-short-none');
  // on the 20's request view, R pastes the 5's hand-back again, then a link that does not read
  await R.click('[data-fk="recv-paste"]');
  await R.fill('#take-link', take5);
  await R.click('[data-fk="take-go"]');
  await R.waitForFunction(() => document.querySelector('#recv-panel .err')?.textContent?.startsWith('This purse took these 5 in at') === true, undefined, { timeout: 30_000 });
  await R.fill('#take-link', `${play}/#take=AAAA`);
  await R.click('[data-fk="take-go"]');
  await R.waitForFunction(() => document.querySelector('#recv-panel .err')?.textContent === 'This does not read as a hand-back. Check that you copied the whole link.');
  check((await R.inputValue('#take-link')).endsWith('#take=AAAA'), 'the link that did not read stays in the field');
  await shootAll(R, 'receive-request-take-error');

  step('9. the other short cases');
  await rotate();
  await take(S);
  // in notes of at most 64: a plain 150 asks for a note of 128, which no note here can make
  const link150 = await ask(R, 150, 64);
  await open(S, link150, '#slice-short');
  check((await text(S, 'h1')).startsWith('More than is ready'), `short: ${await text(S, 'h1')}`);
  await shootAll(S, 'send-short-later');
  const link300 = await ask(R, 300);
  await open(S, link300, '[data-fk="short-faucet"]');
  check((await text(S, 'h1')).startsWith('More than this purse holds'), `short: ${await text(S, 'h1')}`);
  await shootAll(S, 'send-short-over');

  step('10. fragmented: R asks one note of 128');
  await rotate();
  const link128 = await ask(R, 128);
  await open(S, link128, '#slice-short');
  check((await text(S, '.options')).includes('at most 64'), 'the way out asks for notes of at most 64');
  await shootAll(S, 'send-short-notes');

  step('11. smaller notes: 128 in notes of at most 64');
  const link128b = await ask(R, 128, 64, 'receive-smaller');
  await open(S, link128b, '.split.many');
  await shootAll(S, 'send-confirm-two');
  await quiet();
  mint.proxy.hold(25_000);
  await S.click('[data-fk="send-go"]');
  await S.waitForSelector('[data-fk="send-now"]');
  await shootAll(S, 'send-sending-two');
  mint.proxy.release();
  await S.waitForSelector('[data-fk="copy-take"]', { timeout: 60_000 });
  check((await text(S, '.changed')).includes('Nothing came back'), `the change: ${await text(S, '.changed')}`);
  check((await S.$('.view[data-plan="match"]')) !== null, 'data-plan="match"');
  await shootAll(S, 'send-handback-whole');
  await supply('after the send of 128');

  step('12. sent already: S opens the 128 again');
  await open(S, link128b, '[data-fk="show-hb"]');
  await shootAll(S, 'send-already');
  await go(S, '#home');
  await go(S, '#send');
  await S.waitForSelector('.samples .sample');
  await shootAll(S, 'send-scan-kept');

  step('13. a purse that holds no mint opens a request');
  const U = await (await newContext(browser, problems, 'U')).newPage();
  await U.goto(`${plain}/${hashOf(link128b)}`);
  await U.waitForSelector('.v-send .mstamp');
  await U.waitForSelector('[data-fk="add-mint"]:not([disabled])');
  await shootAll(U, 'send-unknown');
  await U.click('[data-fk="add-mint"]');
  await U.waitForFunction(() => document.querySelector('h1')?.textContent?.includes('empty') === true, undefined, { timeout: 30_000 });
  await shootAll(U, 'send-empty');
  await go(U, '#home');
  await U.waitForSelector('.home-acts .btn.solid.wide');
  await shootAll(U, 'home-empty-acts');

  step('14. the camera');
  const link20c = await ask(R, 20);
  const video = join(home, 'qr.y4m');
  writeY4m(video, link20c);
  // the full Chromium, since the headless shell has no camera at all; C1 is granted it, and C0, which is
  // not, is refused at once rather than left at a prompt no one answers
  camera = await launch(
    chromium,
    ['--use-fake-device-for-media-stream', `--use-file-for-fake-video-capture=${video}`, '--deny-permission-prompts'],
    true,
  );
  const C1 = await (await newContext(camera, problems, 'C1', { permissions: ['camera'] })).newPage();
  const C0 = await (await newContext(camera, problems, 'C0')).newPage();
  for (const page of [C1, C0]) {
    // each holds mint B, so that mint A's request is for a mint it does not hold
    await page.goto(`${plain}/${mint.mintLink(B)}`);
    await page.waitForSelector('[data-fk="first-addmint"]:not([disabled])');
    await page.click('[data-fk="first-addmint"]');
    await page.waitForSelector('.v-home');
  }
  await go(C1, '#send');
  await C1.waitForSelector('.scan-area [data-fk="cam"]');
  await shootAll(C1, 'send-scan');
  await C1.click('[data-fk="cam"]');
  await C1.waitForSelector('.scanner.live');
  await shootAll(C1, 'send-camera');
  await C1.waitForSelector('.ticket', { timeout: 30_000 });
  check((await text(C1, '.ticket .tk .lab')).startsWith('Scanned request'), `the ticket: ${await text(C1, '.ticket .tk .lab')}`);
  await C1.waitForSelector('[data-fk="add-mint"]');
  check((await C1.$('.scanner')) === null, 'the camera is off once the code is read');
  await go(C0, '#send');
  await C0.waitForSelector('.scan-area [data-fk="cam"]');
  await C0.click('[data-fk="cam"]');
  await C0.waitForFunction(() => document.querySelector('#scan-words')?.textContent?.startsWith('The camera is off for this purse') === true);
  check((await text(C0, '.scan-area [data-fk="cam"]')).includes('Try the camera again'), 'the button offers the camera again');
  check((await C0.getAttribute('.scan-area [data-fk="cam"]', 'aria-describedby')) === 'scan-words', 'the button is described by the words');
  // the words are said once, in the live region, since the scanner is drawn anew
  await C0.waitForFunction(() => document.getElementById('live')?.textContent?.startsWith('The camera is off for this purse') === true);
  await shootAll(C0, 'send-camera-off');
  await ask(C0, 5);
  await C0.click('[data-fk="recv-scan"]');
  await C0.waitForSelector('#recv-panel [data-fk="cam"]');
  await C0.click('#recv-panel [data-fk="cam"]');
  await C0.waitForFunction(() => document.querySelector('#recv-panel-words')?.textContent?.startsWith('The camera is off for this purse') === true);
  await shootAll(C0, 'receive-request-camera-off');
  await camera.close();
  camera = undefined;

  step('15. waiting for the mint, then settled');
  await take(S);
  await take(S);
  await rotate();
  const link5w = await ask(R, 5);
  await open(S, link5w, '[data-fk="send-go"]');
  await failingRequests(async () => {
    await quiet();
    mint.proxy.drop(true);
    await S.click('[data-fk="send-go"]');
    await S.waitForFunction(() => document.querySelector('[data-fk="retry"]')?.textContent?.includes('Try again now') === true, undefined, { timeout: 120_000 });
    await S.waitForTimeout(500);
    await shootAll(S, 'send-waiting');
    mint.proxy.drop(false);
    await S.click('[data-fk="retry"]');
    await S.waitForSelector('[data-fk="copy-take"]', { timeout: 60_000 });
  });
  check((await S.$('.view[data-plan="match"]')) !== null, 'settled as planned (data-plan="match")');
  await supply('after the settled send');

  step('16. a tab closed mid-send, settled on the next open');
  const link69 = await ask(R, 69);
  await open(S, link69, '.split.many');
  await quiet();
  mint.proxy.hold(120_000, 1);
  await S.click('[data-fk="send-go"]');
  // LV7, in the live region, once the first answer is in
  await S.waitForFunction(() => document.getElementById('live')?.textContent === '1 of 2 at the mint.', undefined, { timeout: 60_000 });
  check((await S.evaluate(() => document.getElementById('live')?.textContent)) === '1 of 2 at the mint.', 'the live region says 1 of 2 at the mint.');
  for (let i = 0; i < 600 && mint.proxy.held === 0; i++) {
    await S.waitForTimeout(100);
  }
  check(mint.proxy.held === 1, 'the first transfer was answered, the second waits');
  await failingRequests(async () => {
    await S.close();
    await new Promise((resolve) => setTimeout(resolve, 800));
    mint.proxy.release();
    await new Promise((resolve) => setTimeout(resolve, 300));
  });
  const S2 = await S.context().newPage();
  await S2.goto(`${play}/#home`);
  await S2.waitForFunction(
    () => [...document.querySelectorAll('.recent-p .act')].some((row) => row.textContent?.includes('Sent 69') && row.textContent.includes('hand-back')),
    undefined,
    { timeout: 30_000 },
  );
  await S2.waitForTimeout(600);
  await shootAll(S2, 'home-settled');
  await supply('after the send settled on open');

  step('17. the mint unreachable');
  const link5u = await ask(R, 5);
  await failingRequests(async () => {
    await quiet();
    mint.proxy.down(true);
    await open(S2, link5u, '.v-send .err');
    await shootAll(S2, 'send-unreachable');
    await go(R, '#receive');
    await R.waitForSelector('[data-fk="ask-new"]');
    await R.click('[data-fk="ask-new"]');
    await R.waitForSelector('#ask-amount');
    await R.fill('#ask-amount', '5');
    await R.click('[data-fk="ask-go"]');
    await R.waitForSelector('.v-recv-ask .err', { timeout: 60_000 });
    await shootAll(R, 'receive-unreachable');
    // a hand-back opened while the mint is down: nothing taken, and Try again
    await R.goto(take5);
    await R.waitForSelector('.v-take [data-fk="retry"]', { timeout: 60_000 });
    check((await text(R, '.v-take .body')).startsWith('The mint at') && (await text(R, '.v-take .body')).includes('Taking in needs its keys'), `ER4: ${await text(R, '.v-take .body')}`);
    await shootAll(R, 'receive-take-failed');
    mint.proxy.down(false);
    await R.click('.v-take [data-fk="retry"]');
    await R.waitForFunction(() => document.querySelector('.v-take h1')?.textContent?.startsWith('Taken in already') === true, undefined, { timeout: 30_000 });
    check((await text(R, '.v-take h1')).startsWith('Taken in already'), 'Try again, with the mint back, finds it taken in already');
  });

  step('18. R cancels its open 20');
  await go(R, '#home');
  await go(R, '#receive');
  await R.waitForSelector('#ask-amount, [data-fk="ask-new"]');
  if ((await R.$('[data-fk="ask-new"]')) !== null) {
    await R.click('[data-fk="ask-new"]');
  }
  await R.waitForSelector('.asks .act.go');
  const row20 = await R.$$eval('.asks .act.go', (rows) => rows.find((row) => row.querySelector('b')?.textContent === '20')?.getAttribute('data-fk'));
  check(row20 !== undefined, 'an open 20 is listed');
  await R.click(`[data-fk="${row20}"]`);
  await R.waitForSelector('[data-fk="cancel-ask"]');
  await R.click('[data-fk="cancel-ask"]');
  await R.waitForSelector('[data-fk="do-cancel"]');
  await shootAll(R, 'receive-request-cancel');
  await R.click('[data-fk="do-cancel"]');
  await R.waitForFunction((key) => document.querySelector(`[data-fk="${key}"]`) === null && document.querySelector('#ask-amount') !== null, row20);
  check((await R.$(`[data-fk="${row20}"]`)) === null, 'the 20 left the open list');

  step('19. a dense hand-back: 255 in notes of at most 64');
  for (let i = 0; i < 4; i++) {
    await take(S2);
  }
  await rotate();
  const link255 = await ask(R, 255, 64);
  await open(S2, link255, '.split.many');
  await send(S2);
  check((await S2.$('.hb-mid .qr-note')) !== null, 'the code says it is dense');
  check((await S2.$('[data-fk="copy-take"].solid')) !== null, 'Copy link is the solid button');
  await shootAll(S2, 'send-handback-dense');
  await supply('after the send of 255');

  step('20. no code: 1000 in notes of at most 8');
  await ask(R, 1000, 8);
  await R.waitForSelector('.qstamp .qr-none');
  await shootAll(R, 'receive-request-nocode');

  step('21. R opens its own request');
  const own = await ask(R, 5);
  await open(R, own, '[data-fk="show-own"]');
  await shootAll(R, 'send-own');

  step('22. a send the mint turns away, and one lost');
  await take(S2);
  await take(S2);
  await rotate();
  const linkT = await ask(R, 5);
  await open(S2, linkT, '[data-fk="send-go"]');
  await failingRequests(async () => {
    await quiet();
    mint.proxy.refuse('layer_full');
    await S2.click('[data-fk="send-go"]');
    await S2.waitForFunction(() => document.querySelector('h1')?.textContent?.startsWith('The mint turned this send away') === true, undefined, { timeout: 60_000 });
  });
  await shootAll(S2, 'send-turned');
  await go(S2, '#home');
  await S2.waitForSelector('.total b');
  const beforeLost = await total(S2);
  const linkL = await ask(R, 5);
  await open(S2, linkL, '[data-fk="send-go"]');
  await failingRequests(async () => {
    await quiet();
    mint.proxy.refuse('spent');
    await S2.click('[data-fk="send-go"]');
    await S2.waitForFunction(() => document.querySelector('h1')?.textContent?.startsWith('This send could not be finished') === true, undefined, { timeout: 60_000 });
  });
  await shootAll(S2, 'send-lost');
  const inAll = Number((await text(S2, '.v-send .body')).match(/lost: (\d+) in all/)?.[1] ?? 'NaN');
  await go(S2, '#home');
  await S2.waitForSelector('.recent-p .act');
  const afterLost = await total(S2);
  check(inAll === beforeLost - afterLost && inAll > 5, `the lost send took ${inAll} in all, as the total fell (${beforeLost} to ${afterLost})`);
  check((await recentRows(S2)).some((row) => row.includes(`Lost ${inAll} on a send of 5`)), `Recent says Lost ${inAll} on a send of 5`);
  await shootAll(S2, 'home-lost');

  step('23. the reading step, then what is ready changed: another tab of S spent the note');
  await take(S2);
  await take(S2);
  await rotate();
  const linkX1 = await ask(R, 5);
  const linkX2 = await ask(R, 5);
  await go(S2, '#home');
  // Holdings' own read of the mint first, so that the held one is Send's
  await S2.waitForTimeout(1200);
  await quiet();
  mint.proxy.holdInfo(30_000);
  await S2.goto(linkX1);
  await S2.waitForSelector('.v-send .ticket');
  await S2.waitForFunction(() => document.querySelector('h1')?.textContent?.startsWith('Checking what can go') === true);
  await shootAll(S2, 'send-reading');
  mint.proxy.releaseInfo();
  await S2.waitForSelector('[data-fk="send-go"]', { timeout: 30_000 });
  const S3 = await S2.context().newPage();
  await S3.goto(linkX2);
  await S3.waitForSelector('[data-fk="send-go"]', { timeout: 30_000 });
  await send(S3);
  await S3.close();
  await S2.click('[data-fk="send-go"]');
  await S2.waitForFunction(() => document.querySelector('.v-send')?.textContent?.includes('What is ready changed since you opened this') === true, undefined, { timeout: 30_000 });
  check((await S2.$('[data-fk="send-go"]:not([disabled])')) !== null, 'confirm is drawn again, ready to send');
  await shootAll(S2, 'send-confirm-changed');
  await send(S2);
  check((await S2.$('.view[data-plan="match"]')) !== null, 'sent as planned again (data-plan="match")');

  step('24. part of a send turned away: the hand-back carries what went');
  await take(S2);
  await take(S2);
  await rotate();
  const link69p = await ask(R, 69);
  await open(S2, link69p, '.split.many');
  await failingRequests(async () => {
    await quiet();
    mint.proxy.refuse('layer_full', 1);
    await S2.click('[data-fk="send-go"]');
    await S2.waitForSelector('[data-fk="copy-take"]', { timeout: 90_000 });
  });
  const partial = await text(S2, '.changed');
  check(/\d+ of 69 went\./.test(partial) && partial.includes('turned one transfer away'), `the change line: ${partial}`);
  check((await S2.$('.changed .sw.live')) !== null, 'its swatch is pink while new');
  check((await S2.$('.view[data-plan="match"]')) !== null, 'what went was part of the plan (data-plan="match")');
  await shootAll(S2, 'send-handback-partial');
  await go(S2, '#home');
  await S2.waitForSelector('.recent-p .act.go');
  const partialRow = (await recentRows(S2))[0] ?? '';
  check(/Sent \d+ of 69/.test(partialRow) && partialRow.includes('hand-back'), `Recent: ${partialRow}`);
  await S2.click('.recent-p .act.go');
  await S2.waitForSelector('[data-fk="copy-take"]');
  check((await S2.$('.changed .sw.cold')) !== null, 'shown again, its swatch is cold');
  await shootAll(S2, 'send-handback-partial-again');

  step('25. too many small notes for one transfer: U holds one ready 64');
  await take(U);
  await rotate();
  const link15 = await ask(R, 15, 1);
  // on U's own build: R's link names the playground build, whose origin keeps another store
  await open(U, `${plain}/${hashOf(link15)}`, '#slice-short');
  check((await text(U, 'h1')).startsWith('Too many small notes for one transfer'), `short: ${await text(U, 'h1')}`);
  await shootAll(U, 'send-short-outputs');

  step('26. a hand-back for a request R cancelled meanwhile');
  const linkK = await ask(R, 5);
  await open(S2, linkK, '[data-fk="send-go"]');
  const takeK = await send(S2);
  // R is on that request's view still
  await R.click('[data-fk="cancel-ask"]');
  await R.click('[data-fk="do-cancel"]');
  await R.waitForSelector('#ask-amount');
  await R.goto(takeK);
  await R.waitForFunction(() => document.querySelector('.v-take .body')?.textContent?.startsWith('You cancelled this request at') === true, undefined, { timeout: 30_000 });
  await shootAll(R, 'receive-take-cancelled');

  step('27. a purse that holds no mint opens links, the purse’s code slow to load');
  const V = await (await newContext(browser, problems, 'V')).newPage();
  let letGo;
  const loaded = new Promise((resolve) => {
    letGo = resolve;
  });
  const sdkChunk = /\/assets\/purses-[^/]*\.js$/;
  await V.route(sdkChunk, async (route) => {
    await loaded;
    await route.continue();
  });
  await V.goto(`${plain}/${hashOf(linkK)}`);
  await V.waitForFunction(() => document.querySelector('h1')?.textContent?.startsWith('Checking what can go') === true);
  check((await hashNow(V)) === '#send', 'the request left the address bar before it was read');
  check((await V.$('.ticket')) === null, 'no ticket until the request is read');
  await shootAll(V, 'send-opening');
  letGo();
  await V.waitForSelector('.v-send .mstamp', { timeout: 30_000 });
  await V.unroute(sdkChunk);
  await V.goto(`${plain}/${hashOf(takeK)}`);
  await V.waitForFunction(() => document.querySelector('.v-take h1')?.textContent?.startsWith('Not for this purse') === true, undefined, { timeout: 30_000 });
  check((await text(V, '.v-take .body')).includes('that this purse does not hold'), `unheld: ${await text(V, '.v-take .body')}`);
  await shootAll(V, 'receive-take-unheld');
  await V.goto(`${plain}/#take=AAAA`);
  await V.waitForFunction(() => document.querySelector('.v-take .body')?.textContent === 'This does not read as a hand-back. Check that you copied the whole link.');
  check((await hashNow(V)) === '#receive', 'a hand-back link that does not read shows on the take step, under #receive');
  await shootAll(V, 'receive-take-bad');
  await V.goto(`${plain}/#ask=AAAA`);
  await V.waitForFunction(() => document.querySelector('.v-send .err')?.textContent === 'This does not read as a request. Check that you copied the whole link.');
  check((await V.inputValue('#req-link')).endsWith('#ask=AAAA') && (await hashNow(V)) === '#send', 'a request link that does not read stays in Send’s field, under #send');
  await shootAll(V, 'send-link-bad');

  step('28. a request for a mint its server does not list');
  // the mint's key replaced by one of the request's own one-time keys: a key that reads, and that no server lists
  const frameK = frameOf(linkK);
  const unlisted = frameK.replaceAll(A, /"holder":"([0-9a-f]{64})"/.exec(frameK)[1]);
  await failingRequests(async () => {
    await V.goto(`${plain}/#ask=${payloadOf(unlisted)}`);
    await V.waitForFunction(() => document.querySelector('.v-send .err')?.textContent?.startsWith('The server at') === true, undefined, { timeout: 30_000 });
    check((await text(V, '.v-send .err')).endsWith('does not list the mint this link names.'), `unlisted: ${await text(V, '.v-send .err')}`);
    await shootAll(V, 'send-unknown-unlisted');
  });
  await supply('at the end');
} catch (error) {
  failed = error;
  // what each page showed when the walk stopped: beside the mint, never among the views
  const contexts = [...browser.contexts(), ...(camera?.contexts() ?? [])];
  for (const [c, context] of contexts.entries()) {
    for (const [at, page] of context.pages().entries()) {
      const file = join(home, `failed-${c}-${at}.png`);
      await page.screenshot({ path: file, fullPage: true }).catch(() => undefined);
      const words = await page.evaluate(() => `${location.hash.slice(0, 40)} | ${document.querySelector('main')?.innerText.replace(/\s+/g, ' ').slice(0, 400)}`).catch(() => '');
      console.error(`preview-send: ${file}: ${words}`);
    }
  }
} finally {
  await camera?.close();
  await browser.close();
  stop();
}

const status = report(out, shots, problems);
if (failed !== undefined) {
  console.error(`\npreview-send: ${failed.stack ?? failed}`);
  process.exit(1);
}
process.exit(status);
