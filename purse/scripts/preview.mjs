// Screenshots of the purse against a real mint: starts worgl-server with a
// short-period mint and a faucet for it on free ports, builds the purse
// twice (as hosted, and with that mint as the build's playground), serves
// both under the hosted Content-Security-Policy, and walks them in
// Chromium: the first run, a take from the faucet, rotations and more
// takes, until keep-alive moves the first fading note on. Then it writes
// PNGs of each view at 360 x 780 and 1280 x 860, light and dark, and of a
// note's tip open. Receive and Send have a walk of their own,
// scripts/preview-send.mjs, with its own --out.
//
// Two more mints on a server of their own, rotating every 8 hours, with a
// faucet each and a build that has the first as its playground, are for the
// keep-alive walk: a run with its transfers held, so each step can be
// shot; a run whose page closes mid-transfer, and the open that settles
// it; a run in another tab; a mint that does not answer its transfers, and
// one that does not answer at all on open. Then a purse that holds both
// mints, the second one's tokens going first while the first is shown, and
// the second one not answering.
//
//   node scripts/preview.mjs [--quick] [--out <dir>]
//
// --quick rotates the mint by hand instead of waiting for its schedule,
// on a mint that rotates every 8 hours as the playground does, for a fast
// look; without it the mint rotates every two minutes, and once on its
// own while the purse is on screen, as it would for a person, and the keep
// walk waits for the purse to recover from the mint that did not answer.
// Any page error, console error or policy violation fails the walk, as
// does a 360 px shot that scrolls sideways. What it needs is in
// scripts/preview-kit.mjs. Its mints live in holdings/ and keep/ under
// target/purse-preview/ (PREVIEW_HOME), made afresh each run;
// PREVIEW_PERIOD sets the first mint's rotation in seconds (120, or 28800
// with --quick).

import { join } from 'node:path';

import { THEMES, chromiumOf, failingRequests, launch, newContext, options, repo, report, setup, shooter, step, stop } from './preview-kit.mjs';

const { quick, out } = options(join(repo, 'target/purse-preview/shots/'));
const home = process.env.PREVIEW_HOME ?? join(repo, 'target/purse-preview/');
const PERIOD = Number(process.env.PREVIEW_PERIOD ?? (quick ? 28_800 : 120));
const chromium = chromiumOf();

const { mint, builds } = await setup({ home: join(home, 'holdings'), period: PERIOD });
const link = mint.link;
const rotate = () => mint.rotate();
const nextRotation = () => mint.nextRotation();
const plain = builds.plain;
const play = builds.playground;

/** The keep walk's mints: a long period, so only rotations by hand move them while the walk runs. */
const KEEP_PERIOD = 28_800;
const keeps = await setup({ home: join(home, 'keep'), period: KEEP_PERIOD, mints: 2, serve: ['playground'] });
const keepMint = keeps.mint;
const keepLink = keepMint.link;
const keepBuild = keeps.builds.playground;
/** The keep server's second mint, so the keep build can add it as a second mint. */
const other = keepMint.issuers[1];
const otherLink = keepMint.mintLink(other);
/** The keep server as the purse names it. */
const keepHost = new URL(keepMint.server).host;

// ---------- the walk ----------

const kit = shooter(out);
const { shots, size } = kit;
const problems = [];
/** A shot of `page` as it is, which must not scroll sideways. */
async function shoot(page, name, theme, view) {
  await kit.shoot(page, name, theme, view);
  await sideways(page, `${name}-${theme}-${view}.png`);
}
/** Shots at every size and theme. */
const shootAll = (page, view) => kit.shootAll(page, view, (each, name, theme) => sideways(each, `${name}-${theme}-${view}.png`));
const browser = await launch(chromium);
try {
  const context = await newContext(browser, problems, 'holdings');
  const page = await context.newPage();

  step('the first run, as hosted: the paste row');
  await page.goto(`${plain}/`);
  await page.waitForSelector('.paste-row');
  await shootAll(page, 'first-paste');

  step('the first run on a mint link: its stamp');
  await page.goto(`${plain}/${link}`);
  await page.waitForSelector('.mstamp');
  await shootAll(page, 'first-link');

  step('the first run with the mint as the build’s playground');
  await page.goto(`${play}/`);
  await page.waitForSelector('[data-fk="first-start"]');
  await shootAll(page, 'first');

  // the takes and the rotations by hand that follow must all fall in one rotation of the schedule's
  const due = nextRotation() - Date.now();
  if (due < 20_000) {
    step(`waiting ${Math.ceil(due / 1000)} s for the mint's next rotation to pass`);
    await page.waitForTimeout(due + 2000);
  }
  step('Start with the Playground: the first take');
  await size(page, 'm360', 'light');
  await page.click('[data-fk="first-start"]');
  await page.waitForSelector('.f-note');
  await page.waitForTimeout(1200);
  await shoot(page, 'm360', 'light', 'justtook');

  step('two rotations by hand, then a second take');
  await rotate();
  await rotate();
  await takeAgain(page);
  if (quick) {
    step('one more rotation by hand, then a third take');
    await rotate();
  } else {
    const next = nextRotation();
    step(`waiting for the mint's own rotation at ${new Date(next).toTimeString().slice(0, 8)}, and keep-alive after it`);
    // the open purse reads the mint again just after it; its first note fades, and keep-alive moves it on
    await page.waitForSelector('.back-row .ka-steps', { timeout: next - Date.now() + 30_000 });
    await keptOnce(page, 'Kept 64 alive');
  }
  await takeAgain(page);
  if (!quick) {
    // the needle well into the rotation, so the shots show its run from the last rotation
    const start = Math.floor(Date.now() / 1000 / PERIOD) * PERIOD * 1000;
    const wait = start + PERIOD * 450 - Date.now();
    if (wait > 0) {
      step(`waiting ${Math.round(wait / 1000)} s for the needle`);
      await page.waitForTimeout(wait);
    }
  } else {
    // the take found the first note fading: keep-alive moves it on
    await keptOnce(page, 'Kept 64 alive');
  }
  await page.mouse.move(0, 0);
  await shootAll(page, 'home');

  step('tips: a tap on a note on the phone, the mouse over one on the desktop, a lane label');
  for (const theme of THEMES) {
    await size(page, 'm360', theme);
    // keep-alive has moved what faded: the fading tip is the keep walk's
    await tapNote(page, 'ready');
    await shoot(page, 'm360', theme, 'tip');
    // a click outside closes it, and leaves no focus ring behind
    await page.click('.eyebrow');
    await size(page, 'd1280', theme);
    await hoverNote(page, 'rest');
    await shoot(page, 'd1280', theme, 'tip');
    await page.mouse.move(0, 0);
  }
  await size(page, 'm360', 'light');
  // the label opens the lane's tip; a click on a note the button spans opens the note's
  await page.click('[data-fk="lane-ready"] .f-lab');
  await page.waitForTimeout(300);
  await shoot(page, 'm360', 'light', 'lanetip');
  await page.keyboard.press('Escape');
  await context.close();

  await keepWalk();
} finally {
  await browser.close();
  stop();
}

process.exit(report(out, shots, problems));

// ---------- helpers ----------

/** Opens a mint link again (the first mint's unless `at` names another) and takes from the faucet once more, then waits for Holdings. */
async function takeAgain(page, at = link) {
  const before = await page.$$eval('.f-note', (notes) => notes.length);
  await page.evaluate((hash) => {
    location.hash = hash;
  }, at);
  await page.waitForSelector('[data-fk="first-take"]');
  await page.click('[data-fk="first-take"]');
  await page.waitForFunction((n) => document.querySelectorAll('.f-note').length > n, before);
  await page.waitForTimeout(800);
}

/** A phone page never scrolls sideways, nor a desktop one. */
async function sideways(page, file) {
  const wide = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  if (wide > 0) {
    problems.push(`${file}: the page is ${wide} px wider than the screen`);
  }
}

/** Waits until `ready()` holds, or fails after `ms`. */
async function until(ready, ms, what) {
  const end = Date.now() + ms;
  while (!(await ready())) {
    if (Date.now() > end) {
      throw new Error(`waited ${Math.round(ms / 1000)} s for ${what}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

/** Waits for a keep-alive run to start and end, and for Recent to say what it kept. */
async function keptOnce(page, title) {
  await page.waitForTimeout(3000);
  await page.waitForSelector('.ka-steps', { state: 'detached', timeout: 60_000 });
  await page.waitForFunction(
    (words) => [...document.querySelectorAll('.recent-p .act span')].some((row) => row.textContent === words),
    title,
    { timeout: 10_000 },
  );
}

/** What the live region last said. */
function live(page) {
  return page.evaluate(() => document.getElementById('live')?.textContent ?? '');
}

/**
 * Holds each `POST /v1/transfer` the page makes until the walk lets it go:
 * one at a time with `release`, or all from then on with `releaseAll`. Each
 * is let go well within the SDK's 30 s timeout.
 */
async function holdTransfers(page) {
  const waiting = [];
  let open = false;
  let arrived = 0;
  const handler = async (route) => {
    arrived += 1;
    if (!open) {
      await new Promise((resolve) => waiting.push(resolve));
    }
    await route.continue().catch(() => undefined);
  };
  await page.route('**/v1/transfer', handler);
  return {
    arrived: (n) => until(() => arrived >= n, 30_000, `transfer ${n}`),
    release: () => waiting.shift()?.(),
    releaseAll: () => {
      open = true;
      while (waiting.length > 0) {
        waiting.shift()?.();
      }
    },
    stop: async () => {
      open = true;
      while (waiting.length > 0) {
        waiting.shift()?.();
      }
      await page.unroute('**/v1/transfer', handler).catch(() => undefined);
    },
  };
}

/** The total the page shows. */
function totalOf(page) {
  return page.$eval('.total b', (b) => Number(b.textContent));
}

/** Opens the keep build's Holdings, a fresh open. */
async function openKeep(page) {
  await page.goto(`${keepBuild}/#home`);
  await page.waitForSelector('.f-note');
}

/**
 * The keep-alive walk, on the 8-hour mint: two notes of 64 fading, their
 * transfers held so each step of the run can be shot; then a page closed
 * mid-transfer and the open that settles it; a run in another tab; and a
 * mint that does not answer. Each rotation by hand happens with the page on
 * about:blank, so each part starts as a fresh open.
 */
async function keepWalk() {
  const rotateKeep = () => keepMint.rotate();
  const next = keepMint.nextRotation();
  if (next - Date.now() < 10 * 60_000) {
    step(`waiting ${Math.ceil((next - Date.now()) / 1000)} s for the keep mints' own rotation to pass`);
    await new Promise((resolve) => setTimeout(resolve, next - Date.now() + 5000));
  }
  const context = await newContext(browser, problems, 'keep');
  let page = await context.newPage();

  step('keep-alive: two notes of 64 taken, two rotations by hand, a third taken, one more rotation');
  await page.goto(`${keepBuild}/`);
  await page.waitForSelector('[data-fk="first-start"]');
  await page.click('[data-fk="first-start"]');
  await page.waitForSelector('.f-note');
  await takeAgain(page, keepLink);
  await page.goto('about:blank');
  await rotateKeep();
  await rotateKeep();
  await openKeep(page);
  await takeAgain(page, keepLink);
  await page.goto('about:blank');
  await rotateKeep();

  step('keep-alive runs on open: its transfers held');
  let held = await holdTransfers(page);
  await openKeep(page);
  await held.arrived(1);
  await page.waitForSelector('.back-row .ka-steps');
  await page.mouse.move(0, 0);
  await shootAll(page, 'keeping');
  for (const theme of THEMES) {
    await size(page, 'm360', theme);
    await tapNote(page, 'fade');
    if ((await page.$('.f-note[data-moving="true"].hov')) === null) {
      problems.push('tipmoving: the note tapped is not a moving one');
    }
    await shoot(page, 'm360', theme, 'tipmoving');
    await page.click('.eyebrow');
  }
  // the fading lane's tip while its notes move: what keep-alive does, and what the mint can tell
  for (const [name, theme] of [['m360', 'light'], ['m360', 'dark'], ['d1280', 'light']]) {
    await size(page, name, theme);
    await page.click('[data-fk="lane-fade"] .f-lab');
    await page.waitForSelector('#field-tip');
    await page.waitForTimeout(300);
    if (!(await page.$eval('#field-tip', (tip) => tip.textContent ?? '')).includes('The mint can tell which notes one visit moved.')) {
      problems.push(`lanetip-fade: the ${name} tip does not say what the mint can tell`);
    }
    await shoot(page, name, theme, 'lanetip-fade');
    // a click outside closes it and leaves no focus ring on the label for the shots after it
    await page.click('.eyebrow');
    await page.mouse.move(0, 0);
  }
  await size(page, 'm360', 'light');

  step('the first transfer answered: its note one lane on, in pink, with its thread');
  held.release();
  await page.waitForSelector('.f-note.pink[data-lane="ready"]', { timeout: 20_000 });
  // most of the slide done, the thread still whole
  await page.waitForTimeout(600);
  if ((await page.$('.f-thread')) === null) {
    problems.push('keeping2: no thread drawn');
  }
  await shoot(page, 'm360', 'light', 'keeping2');
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.waitForTimeout(150);
  await shoot(page, 'm360', 'light', 'keeping2-reduced');
  await page.emulateMedia({ reducedMotion: 'no-preference' });

  step('the second answered: kept');
  held.releaseAll();
  await until(async () => (await live(page)).startsWith('Kept'), 30_000, 'the end of the run');
  for (const [name, theme] of [['m360', 'light'], ['d1280', 'light'], ['m360', 'dark'], ['d1280', 'dark']]) {
    await size(page, name, theme);
    await shoot(page, name, theme, 'kept');
  }
  await held.stop();
  await size(page, 'm360', 'light');
  const total = await totalOf(page);

  step('a page closed mid-transfer: the mint applies it and its answer is lost');
  await page.goto('about:blank');
  await rotateKeep();
  await failingRequests(async () => {
    let cut = false;
    await page.route('**/v1/transfer', async (route) => {
      if (cut) {
        await route.abort('connectionreset').catch(() => undefined);
        return;
      }
      cut = true;
      await route.fetch();
      await route.abort('connectionreset');
    });
    await page.goto(`${keepBuild}/#home`);
    await until(() => cut, 30_000, 'the first transfer');
    await page.close();
  });

  step('the next open settles both: one resent for its answer, one never sent');
  page = await context.newPage();
  held = await holdTransfers(page);
  await openKeep(page);
  await page.waitForSelector('.back-row .ka-steps');
  await held.arrived(1);
  await shoot(page, 'm360', 'light', 'resume');
  held.releaseAll();
  await until(async () => (await live(page)).startsWith('Kept'), 40_000, 'the settle');
  await page.waitForTimeout(500);
  await shoot(page, 'm360', 'light', 'resumed');
  await held.stop();
  if ((await totalOf(page)) !== total) {
    problems.push(`resumed: the total is ${await totalOf(page)}, not ${total}`);
  }
  if ((await page.$$eval('.act span', (rows) => rows.some((row) => /Lost/.test(row.textContent ?? '')))) === true) {
    problems.push('resumed: Recent shows a loss');
  }

  step('a run in another tab');
  await page.goto('about:blank');
  await rotateKeep();
  held = await holdTransfers(page);
  await openKeep(page);
  await page.waitForSelector('.back-row .ka-steps');
  await held.arrived(1);
  const second = await context.newPage();
  await openKeep(second);
  await second.waitForFunction(() => document.querySelector('.back-row')?.textContent?.includes('In another tab') === true, undefined, {
    timeout: 15_000,
  });
  await shoot(second, 'm360', 'light', 'othertab');
  await size(second, 'd1280', 'light');
  await second.waitForSelector('.col-back .ka-steps');
  await shoot(second, 'd1280', 'light', 'othertab');
  held.releaseAll();
  await until(async () => (await live(page)).startsWith('Kept'), 40_000, 'the run in the first tab');
  await held.stop();
  await second.close();

  step('a mint that does not answer: every transfer refused on the way');
  await page.goto('about:blank');
  await rotateKeep();
  await failingRequests(async () => {
    await page.route('**/v1/transfer', (route) => route.abort('connectionrefused'));
    await openKeep(page);
    await page.waitForSelector('.home-err', { timeout: 60_000 });
    await page.waitForSelector('.ka-steps', { state: 'detached', timeout: 90_000 });
    const words = await page.$eval('.home-err', (p) => p.textContent ?? '');
    if (!words.startsWith(`The mint at ${keepHost} did not answer, so `) || !words.endsWith('The purse asks again each minute while it is on screen.')) {
      problems.push(`unreachable: the error says ${JSON.stringify(words)}`);
    }
    const row = await page.$eval('.back-row', (el) => el.textContent ?? '');
    if (!row.startsWith('First tokens fade at ')) {
      problems.push(`unreachable: the row says ${JSON.stringify(row)}`);
    }
    for (const [name, theme] of [['m360', 'light'], ['m360', 'dark'], ['d1280', 'light']]) {
      await size(page, name, theme);
      await shoot(page, name, theme, 'unreachable');
    }
    await size(page, 'm360', 'light');
    await page.unroute('**/v1/transfer');
  });
  if (!quick) {
    step('the mint answers again: the purse tries again within a minute');
    await until(async () => (await live(page)).startsWith('Kept'), 75_000, 'the retry');
    await page.waitForSelector('.home-err', { state: 'detached', timeout: 10_000 });
    // the last hop's slide and thread run out (0.9 s and 2 s); its pink stays for 6 s
    await page.waitForTimeout(2500);
    await shoot(page, 'm360', 'light', 'recovered');
  }

  step('the mint does not answer on open: the lanes and the date from what the purse saved');
  await page.goto('about:blank');
  await failingRequests(async () => {
    await page.route(`**/v1/mints/${keepMint.issuer}/**`, refuse);
    await page.goto(`${keepBuild}/#home`);
    await page.waitForSelector('.home-err', { timeout: 60_000 });
    // the keeper's pass has failed too: its words, from what the purse saved, stand in for the read's
    await page.waitForTimeout(1500);
    const down = await page.$eval('.home-err', (p) => p.textContent ?? '');
    if (!down.startsWith(`The mint at ${keepHost} did not answer. As the purse last saw them, `)) {
      problems.push(`down: the error says ${JSON.stringify(down)}`);
    }
    // the row says the same time, from the same saved fade
    const downRow = await page.$eval('.back-row', (el) => el.textContent ?? '');
    const downAt = /fade at (.+?) unless/.exec(down)?.[1];
    if (downAt === undefined || downRow !== `First tokens fade at ${downAt}`) {
      problems.push(`down: the row says ${JSON.stringify(downRow)}`);
    }
    for (const [name, theme] of [['m360', 'light'], ['m360', 'dark'], ['d1280', 'light']]) {
      await size(page, name, theme);
      await shoot(page, name, theme, 'down');
    }
    await size(page, 'm360', 'light');
    await page.unroute(`**/v1/mints/${keepMint.issuer}/**`, refuse);
  });
  await context.close();

  await elsewhereWalk();
}

/** A request the walk refuses, as a server that is down. */
function refuse(route) {
  return route.abort('connectionrefused');
}

/**
 * A purse that holds two mints, the keep mint shown: the second mint's
 * tokens go first, so the date to come back by is theirs and names them;
 * then, on a fresh open, the second mint does not answer, and the date and
 * the words come from what the purse saved of it.
 */
async function elsewhereWalk() {
  // what this walk does to the second mint must fall in one rotation of its schedule
  const next = keepMint.nextRotation();
  if (next - Date.now() < 10 * 60_000) {
    step(`waiting ${Math.ceil((next - Date.now()) / 1000)} s for the second mint's own rotation to pass`);
    await new Promise((resolve) => setTimeout(resolve, next - Date.now() + 2000));
  }
  const context = await newContext(browser, problems, 'elsewhere');
  const page = await context.newPage();

  step('two mints: the keep mint taken from, then the second mint at the same address, then the keep mint again');
  await page.goto(`${keepBuild}/`);
  await page.waitForSelector('[data-fk="first-start"]');
  await page.click('[data-fk="first-start"]');
  await page.waitForSelector('.f-note');
  keepMint.proxy.faucet(other);
  try {
    await page.evaluate((hash) => {
      location.hash = hash;
    }, otherLink);
    await page.waitForSelector('[data-fk="first-take"]');
    await page.click('[data-fk="first-take"]');
    // Holdings opens on the mint just taken from, with its one note
    await page.waitForSelector('.v-home .f-note');
    await page.waitForTimeout(1500);
  } finally {
    keepMint.proxy.faucet();
  }
  // Holdings shows the mint used last: the keep mint, taken from again
  await takeAgain(page, keepLink);

  step('the second mint rotated twice by hand: its tokens go before the keep mint’s');
  await page.goto('about:blank');
  await keepMint.rotate(other);
  await keepMint.rotate(other);
  await openKeep(page);
  const elsewhereRow = () => page.$eval('.back-row', (el) => el.textContent ?? '');
  await until(async () => (await elsewhereRow()).includes(`For your tokens at ${keepHost}`), 20_000, 'the row naming the other mint');
  // the keeper's pass reads the other mint, and the view draws its date from that read
  await page.waitForTimeout(3000);
  if (!(await elsewhereRow()).startsWith('Open again before ')) {
    problems.push(`elsewhere: the row says ${JSON.stringify(await elsewhereRow())}`);
  }
  await page.mouse.move(0, 0);
  for (const [name, theme] of [['m360', 'light'], ['d1280', 'light']]) {
    await size(page, name, theme);
    await shoot(page, name, theme, 'elsewhere');
  }
  await size(page, 'm360', 'light');

  step('the other mint does not answer on a fresh open: its date and words from what the purse saved');
  await page.goto('about:blank');
  await failingRequests(async () => {
    await page.route(`**/v1/mints/${other}/**`, refuse);
    await openKeep(page);
    await page.waitForSelector('.home-err', { timeout: 60_000 });
    const words = await page.$eval('.home-err', (p) => p.textContent ?? '');
    if (!words.startsWith(`The mint at ${keepHost} did not answer. As the purse last saw them, 64 tokens fade at `)) {
      problems.push(`elsewhere-down: the error says ${JSON.stringify(words)}`);
    }
    const row = await elsewhereRow();
    if (!row.startsWith('First tokens fade at ') || !row.includes(`For your tokens at ${keepHost}`)) {
      problems.push(`elsewhere-down: the row says ${JSON.stringify(row)}`);
    }
    for (const [name, theme] of [['m360', 'light'], ['d1280', 'light']]) {
      await size(page, name, theme);
      await shoot(page, name, theme, 'elsewhere-down');
    }
    await page.unroute(`**/v1/mints/${other}/**`, refuse);
  });
  await context.close();
}

/** The middle of a note's squares in `lane`, in the page. */
async function noteAt(page, lane) {
  const box = await page.locator(`.f-note[data-lane="${lane}"]`).first().boundingBox();
  if (box === null) {
    throw new Error(`no ${lane} note to point at`);
  }
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

async function tapNote(page, lane) {
  const { x, y } = await noteAt(page, lane);
  await page.touchscreen.tap(x, y);
  await page.waitForSelector('.tip-slot .f-tip');
  await page.waitForTimeout(300);
}

async function hoverNote(page, lane) {
  const { x, y } = await noteAt(page, lane);
  await page.mouse.move(x - 3, y);
  await page.mouse.move(x, y);
  await page.waitForSelector('.f-tip.hover');
  await page.waitForTimeout(300);
}
