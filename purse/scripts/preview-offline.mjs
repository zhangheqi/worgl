// Screenshots of the purse installed and offline, on the plumbing of
// scripts/preview-kit.mjs: a mint with a faucet, served as the playground.
// A's first run shows the browser's install offer (one the walk makes, as
// Chromium would), which A takes; its service worker installs, and the
// manifest is read as Chromium reads it. A starts with the Playground; B,
// another purse, asks A for 4. Backup shows the offer too. Then this device
// goes offline: Holdings shows what A holds as the mint last said, "as
// at" when, and says the device is offline; the page loads again from the
// service worker alone; Receive asks by what the purse kept, Send says what
// waits for the network, and Mints shows its card. Back online, Holdings
// reads the mint again. Last, a newer purse is published: it waits while A
// is on Mints out of sight, and takes over once A is on Holdings out of
// sight, which loads again as the newer purse on the same view. Each step
// is shot at 360 x 780 and 1280 x 860, light and dark; and the walk fails
// on any page error, console error or policy violation, on a view wider
// than the phone, and on any failed check.
//
//   node scripts/preview-offline.mjs [--out <dir>]
//
// Its mint lives in target/purse-preview/offline/ (PREVIEW_HOME), made
// afresh each run; the PNGs go to target/purse-preview/offline-shots/
// unless --out says.

import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import { chromiumOf, clearPeriod, failingRequests, launch, newContext, options, repo, report, setup, shooter, step, stop } from './preview-kit.mjs';

const { out } = options(join(repo, 'target/purse-preview/offline-shots/'));
const home = process.env.PREVIEW_HOME ?? join(repo, 'target/purse-preview/offline/');
const chromium = chromiumOf();

// a schedule of about 8 hours, its next rotation well after the walk
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
  if (hash === '#receive' || hash === '#send') {
    await go(page, '#home');
  }
  await page.evaluate((to) => {
    location.hash = to;
  }, hash);
  await page.waitForTimeout(150);
}

const text = (page, selector) => page.$eval(selector, (el) => (el.textContent ?? '').replace(/\s+/g, ' ').trim());
const has = async (page, selector) => (await page.$(selector)) !== null;

async function until(ready, ms, what) {
  const end = Date.now() + ms;
  while (!(await ready())) {
    if (Date.now() > end) {
      throw new Error(`waited ${Math.round(ms / 1000)} s for ${what}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
}

/** Chromium's install offer, as the browser fires it once the purse can be installed: the walk's, so that it comes when asked. */
async function offer(page) {
  await page.evaluate(() => {
    const event = new Event('beforeinstallprompt', { cancelable: true });
    Object.assign(event, {
      prompt: async () => {
        window.__prompted = (window.__prompted ?? 0) + 1;
      },
      userChoice: Promise.resolve({ outcome: 'accepted' }),
    });
    window.dispatchEvent(event);
  });
  await page.waitForTimeout(100);
}

/** The page out of sight, or back in it, as a switch of tab or app makes it: what both the purse and its worker's update read. */
async function sight(page, hidden) {
  await page.evaluate((out) => {
    if (out) {
      Object.defineProperty(document, 'hidden', { configurable: true, get: () => true });
      Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });
    } else {
      delete document.hidden;
      delete document.visibilityState;
    }
    document.dispatchEvent(new Event('visibilitychange'));
  }, hidden);
}

/** The purse's caches: its builds', and the mints' answers it kept. */
const cacheNames = (page) => page.evaluate(() => caches.keys());
const eyebrow = (page) => text(page, '.band-p .lab.muted');

const browser = await launch(chromium);
try {
  const context = await newContext(browser, problems, 'A');
  const A = await context.newPage();

  step('1. A’s first run: the browser offers to install the purse, and A takes the offer');
  await A.goto(`${play}/`);
  await A.waitForSelector('[data-fk="first-start"]');
  check((await text(A, '.first-notes')).includes('On iPhone or iPad, add this purse to your Home Screen'), 'with no offer, the iPhone’s way');
  await offer(A);
  check((await text(A, '.to-install')) === 'Install the purse', 'the offer, as a link in the notes');
  check(
    (await A.$eval('.to-install', (button) => button.parentElement?.textContent ?? '')) === 'Install the purse to open it in its own window, even offline.',
    'what installing does',
  );
  check(!(await text(A, '.first-notes')).includes('iPhone'), 'the iPhone’s way gives place to the offer');
  await shootAll(A, 'first-install');
  await A.click('.to-install');
  await until(() => A.evaluate(() => window.__prompted === 1), 5_000, 'the browser’s prompt');
  check(!(await has(A, '.to-install')), 'a spent offer is not offered again');

  step('2. The service worker installs and takes the page; the manifest reads as Chromium reads it');
  await A.waitForFunction(() => navigator.serviceWorker.controller !== null, undefined, { timeout: 30_000 });
  check(true, 'the worker controls the page');
  const build = await A.evaluate(async () => {
    const source = await (await fetch('/sw.js')).text();
    return JSON.parse(/^const BUILD = (.*);$/m.exec(source)[1]);
  });
  const names = await cacheNames(A);
  check(names.includes(`purse-app-${build.version}`), `its cache, purse-app-${build.version}`);
  const cached = await A.evaluate(async (version) => (await (await caches.open(`purse-app-${version}`)).keys()).length, build.version);
  check(cached === build.files.length, `all ${build.files.length} of the build’s files kept`);
  check(build.files.includes('/') && build.files.some((file) => file.endsWith('.wasm')), 'the page and the SDK’s wasm among them');
  check(!build.files.some((file) => file.endsWith('.map') || file.endsWith('.woff') || file === '/_headers'), 'no source maps, old fonts or headers file');
  const cdp = await context.newCDPSession(A);
  const manifest = await cdp.send('Page.getAppManifest');
  check(manifest.errors.length === 0, `the manifest parses: ${JSON.stringify(manifest.errors)}`);
  const parsed = JSON.parse(manifest.data);
  check(parsed.name === 'Worgl Purse' && parsed.display === 'standalone' && parsed.icons.length === 3, 'Worgl Purse, standalone, three icons');
  const { installabilityErrors } = await cdp.send('Page.getInstallabilityErrors');
  check(installabilityErrors.length === 0, `installable: ${JSON.stringify(installabilityErrors)}`);
  for (const icon of parsed.icons) {
    const answer = await A.evaluate(async (src) => {
      const response = await fetch(src);
      return { ok: response.ok, type: response.headers.get('content-type') };
    }, icon.src);
    check(answer.ok && answer.type === 'image/png', `${icon.src} (${icon.purpose}) is served`);
  }

  step('3. A starts with the Playground; B asks A for 4');
  await A.click('[data-fk="first-start"]');
  await A.waitForSelector('.f-note', { timeout: 30_000 });
  await until(async () => (await text(A, '.total b')) === '64', 30_000, 'Holdings to say 64');
  check((await eyebrow(A)) === 'Playground · in this browser', 'Holdings, read from the mint');
  check((await cacheNames(A)).includes('purse-info'), 'the mint’s answer kept');
  const contextB = await newContext(browser, problems, 'B');
  const B = await contextB.newPage();
  await B.goto(`${play}/`);
  await B.waitForSelector('[data-fk="first-start"]');
  await B.click('[data-fk="first-start"]');
  await B.waitForSelector('.f-note', { timeout: 30_000 });
  await go(B, '#receive');
  await B.waitForSelector('#ask-amount');
  await B.fill('#ask-amount', '4');
  await B.click('[data-fk="ask-go"]');
  await B.waitForFunction(() => document.querySelector('#ask-link')?.textContent?.includes('#ask=') === true);
  const askB = await text(B, '#ask-link');
  await contextB.close();
  check(askB.includes('#ask='), 'B’s request');

  step('4. Backup offers the install too; installed, it stops');
  await offer(A);
  await go(A, '#backup');
  await A.waitForSelector('.to-install');
  check(
    (await A.$eval('.to-install', (button) => button.parentElement?.textContent ?? '')) ===
      'Install the purse to open it in its own window, even offline. Browsers keep an installed purse’s data more readily.',
    'what installing does for the purse’s storage',
  );
  await shootAll(A, 'backup-install');
  await A.evaluate(() => window.dispatchEvent(new Event('appinstalled')));
  await A.waitForTimeout(150);
  check(!(await has(A, '.to-install')), 'installed: no offer');

  step('5. This device goes offline: Holdings shows what A holds as the mint last said');
  await failingRequests(async () => {
    await context.setOffline(true);
    mint.proxy.down(true);
    await go(A, '#home');
    await A.waitForSelector('.home-err', { timeout: 30_000 });
    const err = await text(A, '.home-err');
    check(err.startsWith('This device is offline'), `Holdings says: ${err}`);
    check(!err.includes('did not answer'), 'not that the mint did not answer');
    check((await eyebrow(A)).startsWith('Playground · as at '), `over the total: ${await eyebrow(A)}`);
    check((await text(A, '.total b')) === '64', 'the 64 A holds');
    check((await A.$$('.f-note')).length > 0, 'the field, drawn');
    await shootAll(A, 'offline-home');

    step('6. The page loads again with no network: from the service worker');
    const again = await A.reload();
    check(again?.fromServiceWorker() === true, 'the page came from the service worker');
    await A.waitForSelector('.home-err', { timeout: 30_000 });
    await until(async () => (await has(A, '.total b')) && (await text(A, '.total b')) === '64', 30_000, 'the kept holdings');
    check((await eyebrow(A)).startsWith('Playground · as at '), 'Holdings, as at the mint’s last answer');
    check((await text(A, '.home-err')).startsWith('This device is offline'), 'and why');

    step('7. Receive asks by what the purse kept; Send says what waits for the network');
    await go(A, '#receive');
    await A.waitForSelector('#ask-amount, [data-fk="ask-new"]');
    if (await has(A, '[data-fk="ask-new"]')) {
      await A.click('[data-fk="ask-new"]');
      await A.waitForSelector('#ask-amount');
    }
    await A.fill('#ask-amount', '8');
    await A.click('[data-fk="ask-go"]');
    await A.waitForFunction(() => document.querySelector('#ask-link')?.textContent?.includes('#ask=') === true, undefined, { timeout: 30_000 });
    check(true, 'a request for 8, made offline');
    await shootAll(A, 'offline-receive');
    await go(A, '#home');
    await A.goto(askB);
    await A.waitForSelector('.v-send [role="alert"], .v-send .err', { timeout: 30_000 });
    const sendSays = await text(A, '.v-send [role="alert"], .v-send .err');
    check(sendSays.startsWith('This device is offline.'), `Send says: ${sendSays}`);
    await shootAll(A, 'offline-send');

    step('8. Mints shows the card, as the mint last said');
    await go(A, '#mints');
    await A.waitForSelector('article.mcard', { timeout: 30_000 });
    await until(async () => (await has(A, 'article.mcard .mc-held b')) && (await text(A, 'article.mcard .mc-held b')) === '64', 30_000, 'the card to say 64 held');
    await shootAll(A, 'offline-mints');
  });

  step('9. Back online: Holdings reads the mint again');
  mint.proxy.down(false);
  await context.setOffline(false);
  await go(A, '#home');
  await until(async () => (await eyebrow(A)) === 'Playground · in this browser', 30_000, 'Holdings to read the mint');
  check(!(await has(A, '.home-err')), 'nothing to say');
  check((await text(A, '.total b')) === '64', 'the 64, read from the mint');
  await shootAll(A, 'online-again');

  step('10. A newer purse is published: it waits while A is busy, and takes over once A is calm and out of sight');
  const sw = join(home, 'dist-playground/sw.js');
  const newer = build.version.replace(/^./, (digit) => (digit === '0' ? '1' : '0'));
  writeFileSync(sw, readFileSync(sw, 'utf8').replace(`"version":"${build.version}"`, `"version":"${newer}"`));
  await A.evaluate(async () => (await navigator.serviceWorker.getRegistration())?.update());
  await A.waitForFunction(async () => (await navigator.serviceWorker.getRegistration())?.waiting != null, undefined, { timeout: 30_000, polling: 200 });
  check(true, 'the newer purse installed, and waits');
  check((await cacheNames(A)).includes(`purse-app-${newer}`), 'its files kept already');
  await A.evaluate(() => {
    window.__before = true;
  });
  await go(A, '#mints');
  await A.waitForSelector('article.mcard');
  await sight(A, true);
  await A.waitForTimeout(1500);
  check(await A.evaluate(() => window.__before === true), 'on Mints, out of sight, the page is not loaded again');
  check(await A.evaluate(async () => (await navigator.serviceWorker.getRegistration())?.waiting != null), 'the newer purse still waits');
  await sight(A, false);
  await go(A, '#home');
  await A.waitForSelector('.f-note');
  const loaded = A.waitForEvent('load', { timeout: 30_000 });
  await sight(A, true);
  await loaded;
  check(await A.evaluate(() => window.__before === undefined), 'on Holdings, out of sight, it loads again as the newer purse');
  check((await A.evaluate(() => location.hash)) === '#home', 'on the same view');
  const active = await A.evaluate(async () => {
    const source = await (await fetch('/sw.js')).text();
    return JSON.parse(/^const BUILD = (.*);$/m.exec(source)[1]).version;
  });
  check(active === newer, 'the newer worker answers');
  const after = await cacheNames(A);
  check(after.includes(`purse-app-${newer}`) && after.includes(`purse-app-${build.version}`), 'the newer build’s files, and the one before’s for a tab of it still open');
  await A.waitForSelector('.f-note', { timeout: 30_000 });
  check((await text(A, '.total b')) === '64', 'the 64, as before');
} catch (error) {
  failed = error;
} finally {
  await browser.close();
  stop();
}

if (failed !== undefined) {
  console.error(`\npreview: ${failed.stack ?? failed}`);
}
process.exit(report(out, shots, problems) || (failed === undefined ? 0 : 1));
