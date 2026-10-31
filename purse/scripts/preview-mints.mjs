// Screenshots of Mints between real purses, on the plumbing of
// scripts/preview-kit.mjs: two mints on one server, the first with a faucet
// and served as the playground. A starts with the Playground, opens Mints
// from Holdings' top bar, takes 64 from the card, is refused an empty name
// and names the mint; is refused a link that names no mint, reads the
// second mint by its link and adds it; types the first by server and key
// (a key a digit short, then the whole key at its own server and at
// another), and is told the purse holds it. A second tab names the second
// mint, and A's Mints shows it at once. Show on Holdings brings the first
// back to Holdings. Then the server goes down and each card says so. Mints
// is shot at each step at 360 x 780 and 1280 x 860, light and dark, as is
// Holdings with its ways there; and it fails on any page error, console
// error or policy violation, on a view wider than the phone, and on any
// failed check.
//
//   node scripts/preview-mints.mjs [--out <dir>]
//
// Its mints live in target/purse-preview/mints/ (PREVIEW_HOME), made
// afresh each run; the PNGs go to target/purse-preview/mints-shots/
// unless --out says.

import { join } from 'node:path';

import { chromiumOf, clearPeriod, failingRequests, launch, newContext, options, repo, report, setup, shooter, step, stop } from './preview-kit.mjs';

const { out } = options(join(repo, 'target/purse-preview/mints-shots/'));
const home = process.env.PREVIEW_HOME ?? join(repo, 'target/purse-preview/mints/');
const chromium = chromiumOf();

// a schedule of about 8 hours, its next rotation well after the walk
const { mint, builds } = await setup({ home, period: clearPeriod(28_800, 60 * 60_000), perAddress: 1000, mints: 2, serve: ['playground'] });
const play = builds.playground;
const [FIRST, SECOND] = mint.issuers;
const host = new URL(mint.server).host;
const groups = (key) => key.match(/.{8}/g).join(' ');

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

/** Every view at both sizes and in both themes, nothing wider than the phone; then back to the phone, light. */
async function shootAll(page, view) {
  await page.mouse.move(0, 0);
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
  await page.evaluate((to) => {
    location.hash = to;
  }, hash);
  await page.waitForTimeout(150);
}

const text = (page, selector) => page.$eval(selector, (el) => el.textContent ?? '');
const texts = (page, selector) => page.$$eval(selector, (els) => els.map((el) => el.textContent ?? ''));
const card = (key) => `article.mcard[aria-labelledby="mc-${key}"]`;
const held = (page, key) => text(page, `${card(key)} .mc-held b`);
const focused = (page) => page.evaluate(() => document.activeElement?.getAttribute('data-fk') ?? document.activeElement?.tagName ?? '');

async function until(ready, ms, what) {
  const end = Date.now() + ms;
  while (!(await ready())) {
    if (Date.now() > end) {
      throw new Error(`waited ${Math.round(ms / 1000)} s for ${what}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
}

/** Finds a mint on Mints by what is typed in the form shown, and waits for what it comes to. */
async function find(page) {
  await page.click('[data-fk="m-find"]');
  await page.waitForFunction(
    () =>
      document.querySelector('[data-fk="m-find"]')?.textContent !== 'Reading the mint…' &&
      document.querySelector('.mints-add .err, .mc-found, .mc-held-note') !== null,
    undefined,
    { timeout: 30_000 },
  );
}

const browser = await launch(chromium);
try {
  const context = await newContext(browser, problems, 'A');
  const A = await context.newPage();

  step('1. A starts with the Playground, and opens Mints from the top bar');
  await A.goto(`${play}/`);
  await A.waitForSelector('[data-fk="first-start"]');
  await A.click('[data-fk="first-start"]');
  await A.waitForSelector('.f-note');
  check((await A.getAttribute('[data-fk="to-mints"]', 'aria-label')) === 'Mints: Playground', 'Holdings’ top bar has the mint’s face, leading to Mints');
  await A.click('[data-fk="to-mints"]');
  await A.waitForSelector(card(FIRST));
  await until(async () => (await held(A, FIRST)) === '64', 30_000, 'the card to say 64 held');
  await A.waitForSelector(`[data-fk="mc-take-${FIRST}"]`, { timeout: 30_000 });
  check((await A.$$('article.mcard')).length === 1, 'one mint');
  check((await A.$$(`${card(FIRST)} .tt li`)).length === 4, 'its next four rotations');
  check((await text(A, `${card(FIRST)} .tt-h`)).replace(/\s+/g, ' ') === 'Next rotations Every 8 h · 4 layers', 'how often it rotates, over its timetable');
  check(
    await A.$eval(`${card(FIRST)} .mc-key`, (code) => [...code.querySelectorAll('span')].every((span) => span.getClientRects().length === 1)),
    'no pair of groups breaks across a line',
  );
  check((await text(A, `${card(FIRST)} .mc-key`)).replace(/\s+/g, ' ') === groups(FIRST), 'its whole key, in groups of eight');
  check((await text(A, `${card(FIRST)} .mc-clock`)).startsWith('Each rotation drops the first layer. A visit keeps this mint’s tokens for'), 'what a rotation does');
  check((await A.$('.mc-tag')) === null && (await A.$('[data-fk^="mc-show-"]')) === null, 'with one mint, nothing to choose for Holdings');
  check((await text(A, `${card(FIRST)} .mc-faucet span`)) === '64 a tap · 1000 taps an hour', 'the faucet’s row');
  await shootAll(A, 'mints-one');

  step('2. A takes 64 from the card');
  await A.click(`[data-fk="mc-take-${FIRST}"]`);
  await A.waitForSelector(`${card(FIRST)} .mc-took`, { timeout: 30_000 });
  const took = await text(A, `${card(FIRST)} .mc-took`);
  check(took.startsWith('Took 64 from the faucet.'), `the card says: ${took}`);
  await until(async () => (await held(A, FIRST)) === '128', 30_000, 'the card to say 128 held');
  check(true, '128 held');

  step('3. A names the mint: an empty name first');
  await A.click(`[data-fk="mc-rename-${FIRST}"]`);
  await A.waitForSelector('#mc-name');
  check((await focused(A)) === 'mc-name' && (await A.inputValue('#mc-name')) === 'Playground', 'the name field opens on the name, focused');
  await A.fill('#mc-name', '   ');
  await A.click('[data-fk="mc-save"]');
  await A.waitForSelector('.mc-rename .err');
  check((await text(A, '.mc-rename .err')).startsWith('Give it a name'), 'an empty name is refused');
  await shootAll(A, 'mints-rename');
  await A.fill('#mc-name', 'Practice  tokens ');
  await A.click('[data-fk="mc-save"]');
  await A.waitForSelector('#mc-name', { state: 'detached' });
  check((await text(A, `${card(FIRST)} .mc-name`)) === 'Practice tokens', 'the name is kept, its spaces tidied');
  check((await focused(A)) === `mc-${FIRST}`, 'focus is on the name');

  step('4. A adds the second mint by its link: a link with a short key first');
  await A.fill('#m-link', `${play}/#mint=8e8efcdf&at=${encodeURIComponent(mint.server)}`);
  await find(A);
  check((await text(A, '.mints-add .err')) === 'Paste the whole mint link, starting with https://', 'a link with a short key is refused');
  await A.fill('#m-link', `${play}/${mint.mintLink(SECOND)}`);
  await find(A);
  await A.waitForSelector('.mc-found');
  check((await text(A, '.mints-add .mc-key')).replace(/\s+/g, ' ') === groups(SECOND), 'the mint read shows its whole key');
  check((await focused(A)) === 'm-add', 'focus is on Add mint');
  await shootAll(A, 'mints-found');
  await A.click('[data-fk="m-add"]');
  await A.waitForSelector(card(SECOND));
  check((await text(A, '.mints-add .result')) === `Added ${host}. Holdings shows it now.`, 'it is added');
  check((await focused(A)) === `mc-${SECOND}`, 'focus is on the new mint');
  check((await text(A, `${card(SECOND)} .mc-tag`)) === 'On Holdings', 'the new mint is the one Holdings shows');
  check((await A.$(`[data-fk="mc-show-${FIRST}"]`)) !== null, 'the first can be shown on Holdings');
  await until(async () => (await held(A, SECOND)) === '0', 30_000, 'the new card to say 0 held');
  // its server's faucet hands out the first mint's notes: the second has none
  await A.waitForTimeout(1500);
  check((await A.$(`[data-fk="mc-take-${SECOND}"]`)) === null, 'no faucet for the second mint');
  await shootAll(A, 'mints-two');

  step('5. A types the first mint by server and key');
  await A.click('[data-fk="m-tab-key"]');
  await A.fill('#m-server', mint.server);
  await A.fill('#m-key', FIRST.slice(1));
  await find(A);
  check((await text(A, '.mints-add .err')) === 'The key has 63 hex digits; a mint’s key has 64.', 'a key a digit short is refused');
  check((await focused(A)) === 'm-key', 'focus is on the key');
  await A.fill('#m-key', groups(FIRST).toUpperCase());
  await find(A);
  check((await text(A, '.mc-held-note .result')) === 'This purse holds this mint already, as Practice tokens.', 'the purse holds it already');
  const elsewhere = mint.server.replace('127.0.0.1', 'localhost');
  await A.fill('#m-server', elsewhere);
  await find(A);
  const words = await text(A, '.mc-held-note .result');
  check(words === `This purse holds this mint already, as Practice tokens, at ${host}, and keeps it there. This names ${new URL(elsewhere).host}.`, `at another server: ${words}`);
  await shootAll(A, 'mints-held');

  step('6. A second tab names the second mint; A shows it at once');
  const B = await context.newPage();
  await B.goto(`${play}/#mints`);
  await B.waitForSelector(card(SECOND));
  await B.click(`[data-fk="mc-rename-${SECOND}"]`);
  await B.fill('#mc-name', 'Club points');
  await B.click('[data-fk="mc-save"]');
  await B.waitForSelector('#mc-name', { state: 'detached' });
  await until(async () => (await text(A, `${card(SECOND)} .mc-name`)) === 'Club points', 10_000, 'A to show the new name');
  check(true, 'A shows "Club points"');
  await B.close();

  step('7. Show on Holdings brings the first back');
  await A.click(`[data-fk="mc-show-${FIRST}"]`);
  await A.waitForSelector('.v-home .total b');
  await until(async () => (await text(A, '.v-home .eyebrow')) === 'Practice tokens · in this browser', 10_000, 'Holdings to show the first mint');
  await until(async () => (await text(A, '.total b')) === '128', 30_000, 'Holdings to say 128');
  check(true, 'Holdings shows Practice tokens, 128');
  check((await A.getAttribute('[data-fk="to-mints"]', 'aria-label')) === 'Mints: Practice tokens', 'its top bar’s face is the first mint’s');
  await A.setViewportSize({ width: 1280, height: 860 });
  check((await texts(A, '.db nav a')).join(' ') === 'Purse History Mints Backup', 'the desktop bar has Mints');
  check((await A.$('[data-fk="to-mints-d"]')) !== null, 'the desktop’s mint column leads to Mints');
  await A.setViewportSize({ width: 360, height: 780 });
  await shootAll(A, 'home-ways');
  await go(A, '#receive');
  await A.waitForSelector('[data-fk="tb-mints"]');
  check((await A.getAttribute('[data-fk="tb-mints"]', 'aria-label')) === 'Mints: Practice tokens', 'Receive’s top bar leads to Mints');

  step('8. The server goes down: each card says so');
  await go(A, '#mints');
  await A.waitForSelector(card(SECOND));
  await failingRequests(async () => {
    mint.proxy.down(true);
    await go(A, '#home');
    await go(A, '#mints');
    await until(async () => (await A.$$('.mc-err')).length === 2, 30_000, 'both cards to say the mint did not answer');
    const said = await texts(A, '.mc-err');
    check(said.every((each) => each === `The mint at ${host} did not answer. The purse asks again in a minute.`), `each card says: ${said[0]}`);
    await shootAll(A, 'mints-down');
    mint.proxy.down(false);
    await go(A, '#home');
    await go(A, '#mints');
    await until(async () => (await A.$$('.mc-err')).length === 0, 30_000, 'the cards to read again');
  });
  check(true, 'back once it answers');
} catch (error) {
  failed = error;
  for (const [c, context] of browser.contexts().entries()) {
    for (const [at, page] of context.pages().entries()) {
      const file = join(home, `failed-${c}-${at}.png`);
      await page.screenshot({ path: file, fullPage: true }).catch(() => undefined);
      const words = await page.evaluate(() => `${location.hash.slice(0, 40)} | ${document.querySelector('main')?.innerText.replace(/\s+/g, ' ').slice(0, 600)}`).catch(() => '');
      console.error(`preview-mints: ${file}: ${words}`);
    }
  }
} finally {
  await browser.close();
  stop();
}

const status = report(out, shots, problems);
if (failed !== undefined) {
  console.error(`\npreview-mints: ${failed.stack ?? failed}`);
  process.exit(1);
}
process.exit(status);
