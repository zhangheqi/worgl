// Screenshots of the purse against a real mint: starts worgl-server with a
// short-period mint and a faucet for it on free ports, builds the purse
// twice (as hosted, and with that mint as the build's playground), serves
// both under the hosted Content-Security-Policy, and walks them in
// Chromium: the first run, a take from the faucet, rotations and more
// takes, until the field holds fading, ready and resting tokens. Then it
// writes PNGs of each view at 360 x 780 and 1280 x 860, light and dark,
// and of a note's tip open. Receive and Send have a walk of their own,
// scripts/preview-send.mjs, with its own --out.
//
//   node scripts/preview.mjs [--quick] [--out <dir>]
//
// --quick rotates the mint by hand instead of waiting for its schedule,
// on a mint that rotates every 8 hours as the playground does, for a fast
// look; without it the mint rotates every two minutes, and once on its
// own while the purse is open, as it would for a person. What it needs is
// in scripts/preview-kit.mjs. Its mint lives in holdings/ under
// target/purse-preview/ (PREVIEW_HOME), made afresh each run;
// PREVIEW_PERIOD sets the mint's rotation in seconds (120, or 28800 with
// --quick).

import { join } from 'node:path';

import { THEMES, chromiumOf, launch, newContext, options, repo, report, setup, shooter, step, stop } from './preview-kit.mjs';

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

// ---------- the walk ----------

const { shots, size, shoot, shootAll } = shooter(out);
const problems = [];
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
    step(`waiting for the mint's own rotation at ${new Date(next).toTimeString().slice(0, 8)}`);
    // the open purse reads the mint again just after it, and its notes slide a lane up
    await page.waitForSelector('.f-note[data-lane="fade"]', { timeout: next - Date.now() + 30_000 });
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
    await page.waitForTimeout(6500);
  }
  await page.mouse.move(0, 0);
  await shootAll(page, 'home');

  step('tips: a tap on a note on the phone, the mouse over one on the desktop, a lane label');
  for (const theme of THEMES) {
    await size(page, 'm360', theme);
    await tapNote(page, 'fade');
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
} finally {
  await browser.close();
  stop();
}

process.exit(report(out, shots, problems));

// ---------- helpers ----------

/** Opens the mint link again and takes from the faucet once more, then waits for Holdings. */
async function takeAgain(page) {
  const before = await page.$$eval('.f-note', (notes) => notes.length);
  await page.evaluate((hash) => {
    location.hash = hash;
  }, link);
  await page.waitForSelector('[data-fk="first-take"]');
  await page.click('[data-fk="first-take"]');
  await page.waitForFunction((n) => document.querySelectorAll('.f-note').length > n, before);
  await page.waitForTimeout(800);
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
