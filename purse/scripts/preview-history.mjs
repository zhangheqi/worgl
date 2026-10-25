// Screenshots of History between real purses, on the plumbing of
// scripts/preview-kit.mjs: a mint that rotates by hand, its faucet, and
// the purse built and served as hosted and as the playground. Two browser
// contexts, R (the receiver) and S (the sender), take from the faucet, ask,
// send (one note, a send the mint turns away, two notes) and take in
// between them; then rotations by hand bring S's notes to the first layer,
// which keep-alive moves on, and two more with S's page closed fade them.
// Each purse's History is shot at 360 x 780 and 1280 x 860, light and dark,
// closed and with rows open, beside Holdings with its way there. After each
// it checks that the Held column ends at the total on Holdings and that
// each row's number is the one after it and its change, and it fails on any
// page error, console error or policy violation, on a view wider than the
// phone, and on any failed check.
//
//   node scripts/preview-history.mjs [--out <dir>]
//
// Its mint lives in target/purse-preview/history/ (PREVIEW_HOME), made
// afresh each run; the PNGs go to target/purse-preview/history-shots/
// unless --out says.

import { join } from 'node:path';

import { chromiumOf, clearPeriod, failingRequests, launch, newContext, options, repo, report, setup, shooter, step, stop } from './preview-kit.mjs';

const { out } = options(join(repo, 'target/purse-preview/history-shots/'));
const home = process.env.PREVIEW_HOME ?? join(repo, 'target/purse-preview/history/');
const chromium = chromiumOf();

// rotated by hand: its schedule (about 8 hours) has its next rotation well after the walk, which takes ten minutes or so
const { mint, builds } = await setup({ home, period: clearPeriod(28_800, 60 * 60_000), perAddress: 1000, serve: ['playground'] });
const play = builds.playground;

const problems = [];
const { shots, shootAll: shootEach } = shooter(out);
let failed;

function check(ok, words) {
  if (!ok) {
    throw new Error(`check failed: ${words}`);
  }
  console.log(`preview:   ok: ${words}`);
}

/** Stops the walk on the first problem, so its cause is the step that showed it. */
function clean(where) {
  if (problems.length > 0) {
    throw new Error(`problems at ${where}`);
  }
}

/** Every view at both sizes and in both themes; nothing wider than the phone; then back to the phone, light. */
async function shootAll(page, view) {
  await shootEach(page, view, async (each, name) => {
    if (name === 'm360') {
      const wide = await each.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      if (wide > 0) {
        problems.push(`${view}: ${wide} px wider than the phone`);
      }
    }
  });
  await page.setViewportSize({ width: 360, height: 780 });
  await page.emulateMedia({ colorScheme: 'light' });
  clean(view);
}

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
const total = (page) => page.evaluate(() => Number(document.querySelector('.total b')?.textContent ?? 'NaN'));
const quiet = () => mint.proxy.quiet(6500);

async function rotate() {
  await quiet();
  await mint.rotate();
}

async function until(ready, ms, what) {
  const end = Date.now() + ms;
  while (!(await ready())) {
    if (Date.now() > end) {
      throw new Error(`waited ${Math.round(ms / 1000)} s for ${what}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
}

/** A purse's first run on the playground build: Start with 64. */
async function start(page) {
  await page.goto(`${play}/`);
  await page.waitForSelector('[data-fk="first-start"]');
  await page.click('[data-fk="first-start"]');
  await page.waitForSelector('.f-note');
}

async function take(page) {
  const before = await total(page);
  await go(page, mint.link);
  await page.waitForSelector('[data-fk="first-take"]');
  await page.click('[data-fk="first-take"]');
  await page.waitForFunction((n) => Number(document.querySelector('.total b')?.textContent ?? '0') >= n + 64, before);
  await page.waitForTimeout(300);
}

async function ask(page, amount) {
  await go(page, '#receive');
  await page.waitForSelector('#ask-amount, [data-fk="ask-new"]');
  if ((await page.$('[data-fk="ask-new"]')) !== null) {
    await page.click('[data-fk="ask-new"]');
    await page.waitForSelector('#ask-amount');
  }
  await page.fill('#ask-amount', String(amount));
  await page.click('[data-fk="ask-go"]');
  await page.waitForFunction(() => document.querySelector('#ask-link')?.textContent?.includes('#ask=') === true);
  return text(page, '#ask-link');
}

async function open(page, link) {
  await go(page, '#home');
  await page.goto(link);
  await page.waitForSelector('[data-fk="send-go"]', { timeout: 60_000 });
  await page.waitForTimeout(300);
}

async function send(page) {
  await page.click('[data-fk="send-go"]');
  await page.waitForSelector('[data-fk="copy-take"]', { timeout: 90_000 });
  await page.waitForTimeout(300);
  return text(page, '#take-link-out');
}

async function takeIn(page, link) {
  await page.goto(link);
  await page.waitForSelector('.pm-wrap', { timeout: 30_000 });
  await page.waitForTimeout(300);
}

/** The register's rows as drawn: their titles, changes and Held numbers, newest first. */
const rows = (page) =>
  page.$$eval('.hrow > button', (buttons) =>
    buttons.map((button) => ({
      title: [...(button.querySelector('.t')?.childNodes ?? [])]
        .filter((node) => node.nodeType === Node.TEXT_NODE)
        .map((node) => node.textContent)
        .join(''),
      change: (button.querySelector('b')?.firstChild?.textContent ?? '').replace('−', '-'),
      held: button.querySelector('.held')?.lastChild?.textContent ?? '',
    })),
  );

/**
 * History for `page`: once drawn with the total read, its Held column ends
 * at Holdings' total, every row has its number, each is the next one and
 * its change, and the oldest is its own change, as the purse began empty.
 */
async function history(page, who) {
  await go(page, '#home');
  await page.waitForSelector('.total b');
  await until(async () => Number.isFinite(await total(page)), 30_000, `${who}'s total`);
  const held = await total(page);
  await go(page, '#history');
  await page.waitForSelector('.v-hist .hrow');
  await until(async () => (await rows(page))[0]?.held !== '', 30_000, `${who}'s Held column`);
  const drawn = await rows(page);
  check(Number(drawn[0]?.held) === held, `${who}: Held ends at ${drawn[0]?.held}, the ${held} on Holdings`);
  const changeOf = (row) => (row.change === '—' ? 0 : Number(row.change));
  for (const row of drawn) {
    check(row.held !== '', `${who}: ${row.title} has a Held number`);
  }
  for (let i = 0; i + 1 < drawn.length; i++) {
    const [row, before] = [drawn[i], drawn[i + 1]];
    check(Number(row.held) === Number(before.held) + changeOf(row), `${who}: ${row.title}: ${before.held} ${row.change} = ${row.held}`);
  }
  const oldest = drawn.at(-1);
  check(Number(oldest?.held) === changeOf(oldest ?? { change: '—' }), `${who}: the oldest row, ${oldest?.title}, holds its own ${oldest?.change}`);
  return drawn;
}

/** Opens the row whose title starts with `title`, and waits for what it opens to. */
async function openRow(page, title) {
  const at = (await rows(page)).findIndex((row) => row.title.startsWith(title));
  check(at >= 0, `a row "${title}"`);
  const button = (await page.$$('.hrow > button'))[at];
  await button.click();
  await page.waitForSelector('.hrow.open .det');
  return page.$eval('.hrow.open .det', (el) => [...el.querySelectorAll('p')].map((p) => p.textContent ?? ''));
}

const browser = await launch(chromium);
try {
  const Sx = await newContext(browser, problems, 'S');
  const R = await (await newContext(browser, problems, 'R')).newPage();
  let S = await Sx.newPage();

  step('1. funds: S takes 64 three times, R once, then a rotation');
  await start(S);
  await take(S);
  await take(S);
  await start(R);
  await rotate();
  await go(S, '#home');
  await S.waitForFunction(() => document.querySelector('.total b')?.textContent === '192');

  step('2. R asks for 5, S sends it from one note, R takes it in');
  await open(S, await ask(R, 5));
  await takeIn(R, await send(S));

  step('3. a send of 3 the mint turns away');
  await open(S, await ask(R, 3));
  await failingRequests(async () => {
    await quiet();
    mint.proxy.refuse('layer_full');
    await S.click('[data-fk="send-go"]');
    await S.waitForFunction(() => document.querySelector('h1')?.textContent?.startsWith('The mint turned this send away') === true, undefined, { timeout: 60_000 });
  });

  step('4. R asks for 100, S sends it from two notes, R takes it in');
  await open(S, await ask(R, 100));
  await takeIn(R, await send(S));

  step('5. three rotations bring S’s notes to the first layer: keep-alive moves them on');
  for (let i = 0; i < 3; i++) {
    await rotate();
  }
  await S.goto(`${play}/#home`);
  await S.waitForSelector('.total b');
  await until(async () => (await S.$$eval('.recent-p .act', (els) => els.map((el) => el.textContent ?? ''))).some((row) => row.includes('Kept')), 60_000, 'S’s keep-alive');
  await quiet();

  step('6. S closed over two rotations: what keep-alive moved fades, and S takes 64 again');
  await S.close();
  await rotate();
  await rotate();
  S = await Sx.newPage();
  await S.goto(`${play}/#home`);
  await S.waitForSelector('.total b');
  await until(async () => (await S.$$eval('.recent-p .act', (els) => els.map((el) => el.textContent ?? ''))).some((row) => row.includes('Faded')), 60_000, 'the fade logged');
  await quiet();
  // and 64 more, so Holdings has something to show
  await take(S);
  await quiet();

  step('7. Holdings, with its way to History');
  await go(S, '#home');
  await S.mouse.move(0, 0);
  await shootAll(S, 'home');

  step('8. S’s History, closed, then with rows open');
  const drawn = await history(S, 'S');
  const titles = drawn.map((row) => row.title);
  for (const title of ['Took 64 from the faucet', 'Sent 5', 'Turned away: 3', 'Sent 100', 'Kept', 'Faded']) {
    check(titles.some((each) => each.startsWith(title)), `S’s register has "${title}"`);
  }
  await S.mouse.move(0, 0);
  await shootAll(S, 'history');
  const sent = await openRow(S, 'Sent 5');
  check(sent[0] === 'One transfer, from your note of 64.', `Sent 5 opens to: ${sent.join(' | ')}`);
  check((await S.$('.hrow.open .det .btn')) !== null, 'the send offers its hand-back again');
  await shootAll(S, 'history-sent');
  const two = await openRow(S, 'Sent 100');
  check(two[0]?.startsWith('Two transfers, one per note, from your two notes of 64') === true, `Sent 100 opens to: ${two.join(' | ')}`);
  await shootAll(S, 'history-sent-two');
  const kept = await openRow(S, 'Kept');
  check(kept[1] === 'The mint can tell which notes one visit moved.', `Kept opens to: ${kept.join(' | ')}`);
  await shootAll(S, 'history-kept');
  const faded = await openRow(S, 'Faded');
  // rotated by hand: its schedule puts the rotation hours before the notes came, so the row gives only a bound
  check(/sat in the first layer when the mint rotated, some time after \d\d:\d\d, so/.test(faded[0] ?? ''), `Faded opens to: ${faded.join(' | ')}`);
  await shootAll(S, 'history-faded');

  step('9. the hand-back again, from History');
  await openRow(S, 'Sent 5');
  await S.click('.hrow.open .det .btn');
  await S.waitForSelector('[data-fk="copy-take"]', { timeout: 30_000 });
  check((await S.evaluate(() => location.hash)) === '#send', 'Show the hand-back again opens it on Send');

  step('10. R’s History');
  await history(R, 'R');
  const received = await openRow(R, 'Received 100');
  check(received[0]?.includes('for your request for 100') === true, `Received 100 opens to: ${received.join(' | ')}`);
  await R.mouse.move(0, 0);
  await shootAll(R, 'history-received');
} catch (error) {
  failed = error;
  for (const [c, context] of browser.contexts().entries()) {
    for (const [at, page] of context.pages().entries()) {
      const file = join(home, `failed-${c}-${at}.png`);
      await page.screenshot({ path: file, fullPage: true }).catch(() => undefined);
      const words = await page.evaluate(() => `${location.hash.slice(0, 40)} | ${document.querySelector('main')?.innerText.replace(/\s+/g, ' ').slice(0, 400)}`).catch(() => '');
      console.error(`preview-history: ${file}: ${words}`);
    }
  }
} finally {
  await browser.close();
  stop();
}

const status = report(out, shots, problems);
if (failed !== undefined) {
  console.error(`\npreview-history: ${failed.stack ?? failed}`);
  process.exit(1);
}
process.exit(status);
