// Screenshots of Backup between real purses, on the plumbing of
// scripts/preview-kit.mjs: a mint that rotates by hand, its faucet, and
// the purse served as the playground. A takes 192 from the faucet, is
// refused a short passphrase and two that differ, and saves a backup as a
// download; then it sends 5 to R from one note, so the backup covers less
// of what it holds. C, a new browser, restores the file from the first run:
// first with a wrong passphrase, then the right one; the mint is asked
// about each note it brought, and the one A spent is dropped. A restores
// the file too, and nothing in it is new. Backup is shot at each step at
// 360 x 780 and 1280 x 860, light and dark, as are Holdings with its way
// there and History with the new rows open. The Held column is checked as
// History's walk checks it, and it fails on any page error, console error
// or policy violation, on a view wider than the phone, on a passphrase in
// the page's markup, and on any failed check.
//
//   node scripts/preview-backup.mjs [--out <dir>]
//
// Its mint lives in target/purse-preview/backup/ (PREVIEW_HOME), made
// afresh each run, and the backup file is saved there; the PNGs go to
// target/purse-preview/backup-shots/ unless --out says.

import { join } from 'node:path';

import { chromiumOf, clearPeriod, launch, newContext, options, repo, report, setup, shooter, step, stop } from './preview-kit.mjs';

const { out } = options(join(repo, 'target/purse-preview/backup-shots/'));
const home = process.env.PREVIEW_HOME ?? join(repo, 'target/purse-preview/backup/');
const chromium = chromiumOf();

// rotated by hand: its schedule (about 8 hours) has its next rotation well after the walk
const { mint, builds } = await setup({ home, period: clearPeriod(28_800, 60 * 60_000), perAddress: 1000, serve: ['playground'] });
const play = builds.playground;

// sealed as typed on one keyboard (é as one character), opened as another types it (e and a mark)
const PASSPHRASE = 'quiet harbour café'.normalize('NFC');
const DECOMPOSED = PASSPHRASE.normalize('NFD');

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

/** Every view at both sizes and in both themes; nothing wider than the phone, no passphrase in the markup; then back to the phone, light. */
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
  await bare(page, view);
  clean(view);
}

/** No passphrase anywhere in the page's markup: typed ones live only in their fields' values. */
async function bare(page, where) {
  const html = await page.evaluate(() => document.documentElement.outerHTML);
  if (html.includes(PASSPHRASE) || html.includes(DECOMPOSED) || html.includes('wrong horse')) {
    problems.push(`${where}: a passphrase is in the page's markup`);
  }
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
const texts = (page, selector) => page.$$eval(selector, (els) => els.map((el) => el.textContent ?? ''));
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

/** Backup's label for what the last backup covers, once it has read what is held. */
async function coverLab(page, want) {
  await until(async () => (await texts(page, '.v-bk .sec-h .lab'))[1] === want, 30_000, `"${want}" on Backup`);
  check(true, `Backup says ${want}`);
}

/** Saves a backup with `first` and `again` typed; with `save`, waits for the download and keeps it at `save`. */
async function saveWith(page, first, again, save) {
  await page.fill('#bk-p1', first);
  await page.fill('#bk-p2', again);
  if (save === undefined) {
    await page.click('[data-fk="bk-save"]');
    await page.waitForSelector('form[data-form="backup"] .err');
    return text(page, 'form[data-form="backup"] .err');
  }
  const [download] = await Promise.all([page.waitForEvent('download', { timeout: 60_000 }), page.click('[data-fk="bk-save"]')]);
  const name = download.suggestedFilename();
  const file = join(save, name);
  await download.saveAs(file);
  // a download counts as a backup only once the person says the file is there
  await page.waitForSelector('[data-fk="bk-kept"]');
  return { name, file, pending: await text(page, 'form[data-form="backup"] .bk-pending .result') };
}

/** Says the downloaded file is there: what the save then says. */
async function keep(page) {
  await page.click('[data-fk="bk-kept"]');
  await page.waitForFunction(() => document.querySelector('form[data-form="backup"] .result')?.textContent?.startsWith('Saved at') === true);
  return text(page, 'form[data-form="backup"] .result');
}

/** Chooses `file` to restore from, through the browser's file chooser. */
async function choose(page, file) {
  const button = (await page.$('[data-fk="bk-other"]')) ?? (await page.$('[data-fk="bk-restore"]'));
  const [chooser] = await Promise.all([page.waitForEvent('filechooser'), button.click()]);
  await chooser.setFiles(file);
  await page.waitForSelector('#bk-open');
}

/** Restores the chosen file with `passphrase`: the error it shows, or the lines of what came of it. */
async function restoreWith(page, passphrase) {
  await page.fill('#bk-open', passphrase);
  await page.click('[data-fk="bk-go"]');
  await page.waitForFunction(() => document.querySelector('.v-bk .bk-low .err, .v-bk .bk-low .result') !== null && document.querySelector('[data-fk="bk-go"]')?.textContent !== 'Opening…', undefined, {
    timeout: 60_000,
  });
  const error = await page.$('.v-bk .bk-low .err');
  if (error !== null) {
    return { error: await error.textContent() };
  }
  return { lines: await texts(page, '.v-bk .bk-low .result p') };
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
  // headless, the save dialog would wait for no one: the file downloads, as where a browser has none
  const noPicker = () => {
    delete window.showSaveFilePicker;
  };
  const Ax = await newContext(browser, problems, 'A');
  await Ax.addInitScript(noPicker);
  const A = await Ax.newPage();
  const R = await (await newContext(browser, problems, 'R')).newPage();

  step('1. A takes 64 three times, then a rotation');
  await start(A);
  await take(A);
  await take(A);
  await rotate();
  await go(A, '#home');
  await A.waitForFunction(() => document.querySelector('.total b')?.textContent === '192');

  step('2. Holdings with no backup: its way to Backup is marked');
  check((await A.$('.tb-bk.wanted .bk-mark')) !== null, 'the phone’s Backup link is marked');
  await until(async () => (await texts(A, '.to-bk')).some((row) => row.includes('None yet')), 10_000, 'the Backup row');
  await A.mouse.move(0, 0);
  await shootAll(A, 'home-none');

  step('3. Backup with none yet');
  await go(A, '#backup');
  await coverLab(A, 'None');
  check((await texts(A, '.v-bk .sec .body')).includes('There is no backup of this purse yet.'), 'Backup says there is none yet');
  await shootAll(A, 'backup-none');

  step('4. a short passphrase, then two that differ, then a save');
  check((await saveWith(A, 'short', 'short')) === 'Use a passphrase of 8 characters or more.', 'a short passphrase is refused');
  check((await A.evaluate(() => document.activeElement?.id)) === 'bk-p1', 'the first field has focus');
  check((await saveWith(A, PASSPHRASE, 'quiet harbour cafe')) === 'The two passphrases differ. Type the same one twice.', 'two that differ are refused');
  check((await A.evaluate(() => document.activeElement?.id)) === 'bk-p2', 'the second field has focus');
  await shootAll(A, 'backup-differ');
  const saved = await saveWith(A, PASSPHRASE, PASSPHRASE, home);
  check(/^worgl-purse-\d{4}-\d\d-\d\d-\d{4}\.worgl$/.test(saved.name), `the file is named ${saved.name}`);
  check(saved.pending === `The browser is saving ${saved.name}. Check that it is with your files, then say so below: until then it does not count as a backup.`, `the download says: ${saved.pending}`);
  check((await A.evaluate(() => document.activeElement?.getAttribute('data-fk'))) === 'bk-kept', '“It is saved” has focus');
  check(await A.$eval('[data-fk="bk-save"]', (el) => el.disabled), 'Save backup waits for it');
  await coverLab(A, 'None');
  await shootAll(A, 'backup-pending');
  const said = await keep(A);
  check(/^Saved at \d\d:\d\d\. The file holds 192 tokens\. Keep it somewhere apart/.test(said), `the save says: ${said}`);
  check((await A.evaluate(() => document.activeElement?.getAttribute('data-fk'))) === 'bk-save', 'Save backup has focus again');
  check((await A.$eval('#bk-p1', (el) => el.value)) === '' && (await A.$eval('#bk-p2', (el) => el.value)) === '', 'the fields are empty once saved');
  await coverLab(A, '192 / 192 covered');
  check((await texts(A, '.v-bk .sec .body')).includes('Your last backup covers all 192 tokens.'), 'it covers all 192');
  await A.mouse.move(0, 0);
  await shootAll(A, 'backup-saved');

  step('5. R asks for 5, A sends it from one note, R takes it in: the backup covers less');
  await start(R);
  await open(A, await ask(R, 5));
  await takeIn(R, await send(A));
  await go(A, '#home');
  await A.waitForFunction(() => document.querySelector('.total b')?.textContent === '187', undefined, { timeout: 30_000 });
  check((await A.$('.tb-bk.wanted')) === null, 'the phone’s Backup link is no longer marked');
  await until(async () => (await texts(A, '.to-bk')).some((row) => row.startsWith('Covers 128 of 187')), 10_000, 'the Backup row on Holdings');
  await A.mouse.move(0, 0);
  await shootAll(A, 'home-partial');
  await go(A, '#backup');
  await coverLab(A, '128 / 187 covered');
  check(
    (await texts(A, '.v-bk .sec .body')).some((line) => line.startsWith('Your last backup covers 128 of 187 tokens. The rest have moved or arrived since')),
    'it covers 128 of 187',
  );
  await shootAll(A, 'backup-partial');

  step('6. A’s History: the backup saved');
  const drawnA = await history(A, 'A');
  check(drawnA.some((row) => row.title === 'Saved a backup'), 'A’s register has "Saved a backup"');
  const backupRow = await openRow(A, 'Saved a backup');
  check(/^A backup holds the notes as they were at \d\d:\d\d: here, three notes of 64, sealed with your passphrase\.$/.test(backupRow[0] ?? ''), `Saved a backup opens to: ${backupRow.join(' | ')}`);
  await A.mouse.move(0, 0);
  await shootAll(A, 'history-backup');

  step('7. C, a new browser: the first run’s way to restore');
  const C = await (await newContext(browser, problems, 'C')).newPage();
  await C.goto(`${play}/`);
  await C.waitForSelector('[data-fk="first-restore"]');
  await C.click('[data-fk="first-restore"]');
  await C.waitForSelector('[data-fk="bk-restore"]');
  check((await C.evaluate(() => location.hash)) === '#backup', 'Restore it opens Backup');
  await shootAll(C, 'backup-first');
  // the desktop's bar has no way back to the first run: the band has one
  await C.setViewportSize({ width: 1280, height: 800 });
  check(await C.isVisible('[data-fk="bk-start"]'), 'the desktop has a way back to the start');
  await C.click('[data-fk="bk-start"]');
  await C.waitForSelector('[data-fk="first-restore"]');
  check((await C.evaluate(() => location.hash)) === '#first', 'Back to the start opens the first run');
  await C.click('[data-fk="first-restore"]');
  await C.waitForSelector('[data-fk="bk-restore"]');
  await C.setViewportSize({ width: 360, height: 780 });
  check(!(await C.isVisible('[data-fk="bk-start"]')), 'the phone has its own way back, in the top bar');

  step('8. C chooses the file: a wrong passphrase, then the right one');
  await choose(C, saved.file);
  check((await text(C, '.bk-file span')) === saved.name, 'the file chosen is named');
  const wrong = await restoreWith(C, 'wrong horse battery');
  check(wrong.error?.startsWith('That passphrase does not open this backup') === true, `a wrong passphrase: ${wrong.error}`);
  await shootAll(C, 'backup-wrong');
  const restored = await restoreWith(C, DECOMPOSED);
  check(restored.lines?.[0] === 'Restored 192 tokens, in three notes. The purse checks each with the mint before they move.', `the restore says: ${restored.lines?.join(' | ') ?? restored.error}`);
  await C.mouse.move(0, 0);
  await shootAll(C, 'backup-restored');

  step('9. C asks the mint about each note: the one A spent is dropped');
  await until(async () => ((await C.$('.v-bk .note-line[role="status"]')) === null ? '' : await text(C, '.v-bk .note-line[role="status"]')).startsWith('Checked'), 60_000, 'the checks');
  const checked = await text(C, '.v-bk .note-line[role="status"]');
  check(checked === 'Checked three restored notes: 128 live, 64 already spent, now dropped.', `the checks say: ${checked}`);
  await coverLab(C, '128 / 128 covered');
  await shootAll(C, 'backup-checked');

  step('10. C’s History and Holdings');
  const drawnC = await history(C, 'C');
  check(drawnC.length === 2 && drawnC[1]?.title === 'Restored 192' && drawnC[0]?.title === 'Dropped 64 already spent', `C’s register: ${drawnC.map((row) => row.title).join(' | ')}`);
  check(drawnC[0]?.held === '128', 'C holds 128');
  const restoredRow = await openRow(C, 'Restored 192');
  check(restoredRow[0]?.includes('from a backup saved at') === true, `Restored 192 opens to: ${restoredRow.join(' | ')}`);
  await C.mouse.move(0, 0);
  await shootAll(C, 'history-restored');
  const droppedRow = await openRow(C, 'Dropped 64 already spent');
  check(droppedRow[0]?.startsWith('The mint says') === true, `Dropped 64 opens to: ${droppedRow.join(' | ')}`);
  await shootAll(C, 'history-checked');
  await go(C, '#home');
  await C.waitForFunction(() => document.querySelector('.total b')?.textContent === '128', undefined, { timeout: 30_000 });
  await C.mouse.move(0, 0);
  await shootAll(C, 'home-restored');

  step('11. A restores its own file: nothing in it is new');
  await go(A, '#backup');
  await choose(A, saved.file);
  const again = await restoreWith(A, PASSPHRASE);
  check(again.lines?.[0] === 'Nothing in the backup was new to this purse.', `A’s restore says: ${again.lines?.join(' | ') ?? again.error}`);
  await go(A, '#home');
  await A.waitForFunction(() => document.querySelector('.total b')?.textContent === '187');
  await history(A, 'A');
} catch (error) {
  failed = error;
  for (const [c, context] of browser.contexts().entries()) {
    for (const [at, page] of context.pages().entries()) {
      const file = join(home, `failed-${c}-${at}.png`);
      await page.screenshot({ path: file, fullPage: true }).catch(() => undefined);
      const words = await page.evaluate(() => `${location.hash.slice(0, 40)} | ${document.querySelector('main')?.innerText.replace(/\s+/g, ' ').slice(0, 600)}`).catch(() => '');
      console.error(`preview-backup: ${file}: ${words}`);
    }
  }
} finally {
  await browser.close();
  stop();
}

const status = report(out, shots, problems);
if (failed !== undefined) {
  console.error(`\npreview-backup: ${failed.stack ?? failed}`);
  process.exit(1);
}
process.exit(status);
