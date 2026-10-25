// Screenshots of the purse against a real mint: starts worgl-server with a
// short-period mint and a faucet for it on free ports, builds the purse
// twice (as hosted, and with that mint as the build's playground), serves
// both under the hosted Content-Security-Policy, and walks them in
// Chromium: the first run, a take from the faucet, rotations and more
// takes, until keep-alive moves the first fading note on. Then it writes
// PNGs of each view at 360 x 780 and 1280 x 860, light and dark, and of a
// note's tip open.
//
// A second mint on the same server, rotating every 8 hours, with its own
// faucet and a third build that has it as its playground, is for the
// keep-alive walk: a run with its transfers held, so each step can be
// shot; a run whose page closes mid-transfer, and the open that settles
// it; a run in another tab; a mint that does not answer its transfers, and
// one that does not answer at all on open. Then a purse that holds both
// mints, the first one's tokens going first while the second is shown, and
// the first one not answering.
//
//   node scripts/preview.mjs [--quick] [--out <dir>]
//
// --quick rotates the mint by hand instead of waiting for its schedule,
// on a mint that rotates every 8 hours as the playground does, for a fast
// look; without it the mint rotates every two minutes, and once on its
// own while the purse is on screen, as it would for a person, and the keep
// walk waits for the purse to recover from the mint that did not answer.
// Any page error, console error or policy violation fails the walk, as
// does a 360 px shot that scrolls sideways. Needs the `playwright` package where
// Node can require it (NODE_PATH) and a Chromium it can launch
// (PREVIEW_CHROMIUM names one). Cargo builds worgl-server and worgl-faucet
// into CARGO_TARGET_DIR (or target/), unless WORGL_SERVER_BINARY and
// WORGL_FAUCET_BINARY name them. Its mint lives in target/purse-preview/
// (PREVIEW_HOME), made afresh each run; PREVIEW_PERIOD sets the mint's
// rotation in seconds (120, or 28800 with --quick).

import { execFileSync, spawn, spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer, request } from 'node:http';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const purse = fileURLToPath(new URL('..', import.meta.url));
const repo = join(purse, '..');
const args = process.argv.slice(2);
const quick = args.includes('--quick');
const outAt = args.indexOf('--out');
const out = outAt >= 0 && args[outAt + 1] !== undefined ? args[outAt + 1] : join(repo, 'target/purse-preview/shots/');
const home = process.env.PREVIEW_HOME ?? join(repo, 'target/purse-preview/');
const PERIOD = Number(process.env.PREVIEW_PERIOD ?? (quick ? 28_800 : 120));
const SIZES = { m360: { width: 360, height: 780 }, d1280: { width: 1280, height: 860 } };
const THEMES = ['light', 'dark'];

let chromium;
try {
  ({ chromium } = createRequire(import.meta.url)('playwright'));
} catch {
  console.error('preview: cannot require playwright; put it on NODE_PATH');
  process.exit(1);
}

// ---------- the mint and its faucet ----------

const binaries = (() => {
  const server = process.env.WORGL_SERVER_BINARY;
  const faucet = process.env.WORGL_FAUCET_BINARY;
  if (server !== undefined && faucet !== undefined) {
    return { server, faucet };
  }
  step('cargo build worgl-server worgl-faucet');
  run('cargo', ['build', '--package', 'worgl-server', '--package', 'worgl-faucet', '--locked'], { cwd: repo });
  const target = process.env.CARGO_TARGET_DIR ?? join(repo, 'target');
  return { server: join(target, 'debug/worgl-server'), faucet: join(target, 'debug/worgl-faucet') };
})();

const children = [];
const servers = [];
const stop = () => {
  for (const server of servers) {
    server.close();
  }
  for (const child of children) {
    child.kill('SIGTERM');
  }
};
process.on('SIGINT', () => {
  stop();
  process.exit(130);
});

const ports = await Promise.all([1, 2, 3, 4, 5, 6, 7, 8].map(freePort));
const [serverPort, faucetPort, proxyPort, plainPort, playPort, keepFaucetPort, keepProxyPort, keepPort] = ports;
const origin = (port) => `http://127.0.0.1:${port}`;
const mintServer = origin(proxyPort);
const keepServer = origin(keepProxyPort);
/** The keep walk's mint: a long period, so only rotations by hand move it while the walk runs. */
const KEEP_PERIOD = 28_800;

step(`a mint rotating every ${PERIOD} s, and one every ${KEEP_PERIOD} s for keep-alive, in ${home}`);
rmSync(home, { recursive: true, force: true });
mkdirSync(home, { recursive: true });
const state = join(home, 'state');
execFileSync(binaries.server, ['init', '--state', state], { stdio: 'ignore' });
// new notes land in the last layer, where they rest until the next rotation
writeFileSync(join(home, 'policy.json'), JSON.stringify({ mint_layer: 3 }));

/** Creates a mint on the state, before the server serves it: its issuer, admin token and mint authority key. */
function createMint(period) {
  const created = execFileSync(
    binaries.server,
    ['create-mint', '--state', state, '--layers', '4', '--layer-size', '1024', '--policy', join(home, 'policy.json'), '--rotate-every', String(period)],
    { encoding: 'utf8' },
  );
  const field = (name) => new RegExp(`^${name}\\s+(\\S+)`, 'm').exec(created)?.[1];
  return {
    issuer: /^mint\s+([0-9a-f]{64})$/m.exec(created)?.[1],
    token: readFileSync(field('admin token'), 'utf8').trim(),
    key: field('mint key'),
  };
}

const main = createMint(PERIOD);
const kept = createMint(KEEP_PERIOD);
const issuer = main.issuer;
const link = `#mint=${issuer}&at=${encodeURIComponent(mintServer)}`;
const keepLink = `#mint=${kept.issuer}&at=${encodeURIComponent(keepServer)}`;
/** The first mint at the keep mint's address, so the keep build can add it as a second mint. */
const otherLink = `#mint=${issuer}&at=${encodeURIComponent(keepServer)}`;

/** A faucet for `mint`, answering pages from `origins`. */
const faucet = (mint, port, origins) =>
  spawn(
    binaries.faucet,
    [
      '--mint', mint.issuer,
      '--key', mint.key,
      '--server', origin(serverPort),
      '--listen', `127.0.0.1:${port}`,
      '--allow-origin', origins.map(origin).join(','),
    ],
    { stdio: ['ignore', 'ignore', 'inherit'] },
  );

children.push(
  spawn(binaries.server, ['serve', '--state', state, '--listen', `127.0.0.1:${serverPort}`], { stdio: ['ignore', 'ignore', 'inherit'] }),
  // the keep build takes from it too, through the keep mint's address (faucetFor)
  faucet(main, faucetPort, [plainPort, playPort, keepPort]),
  faucet(kept, keepFaucetPort, [keepPort]),
);

/** The faucet each proxy sends /v1/faucet to, by the proxy's port: the walk points the keep mint's at the first mint's faucet for a take. */
const faucetFor = new Map();
/** One address for the mint's server and its faucet, as the playground's proxy serves them: the faucet at /v1/faucet. */
const proxy = (port, faucetAt) => {
  faucetFor.set(port, faucetAt);
  return createServer((incoming, outgoing) => {
    const { pathname } = new URL(incoming.url ?? '/', 'http://proxy');
    const upstream = request(
      { host: '127.0.0.1', port: pathname === '/v1/faucet' ? faucetFor.get(port) : serverPort, method: incoming.method, path: incoming.url, headers: incoming.headers },
      (answer) => {
        outgoing.writeHead(answer.statusCode ?? 502, answer.headers);
        answer.pipe(outgoing);
      },
    );
    upstream.on('error', () => {
      outgoing.writeHead(502);
      outgoing.end();
    });
    incoming.pipe(upstream);
  }).listen(port, '127.0.0.1');
};
servers.push(proxy(proxyPort, faucetPort), proxy(keepProxyPort, keepFaucetPort));
await up(`${mintServer}/v1/mints/${issuer}/info`);
await up(`${keepServer}/v1/mints/${kept.issuer}/info`);
await up(`${origin(faucetPort)}/v1/faucet`);
await up(`${origin(keepFaucetPort)}/v1/faucet`);

/** Rotates a mint now, as its schedule would. */
async function rotateMint(mint) {
  const answer = await fetch(`${origin(serverPort)}/v1/mints/${mint.issuer}/rotate`, {
    method: 'POST',
    headers: { authorization: `Bearer ${mint.token}` },
  });
  if (!answer.ok) {
    throw new Error(`rotate: ${answer.status} ${await answer.text()}`);
  }
}
const rotate = () => rotateMint(main);
const rotateKeep = () => rotateMint(kept);

// ---------- the purse, built and served twice ----------

const vite = join(purse, 'node_modules/vite/bin/vite.js');
const builds = [
  { name: 'plain', port: plainPort, playground: '', mint: mintServer },
  { name: 'playground', port: playPort, playground: link, mint: mintServer },
  { name: 'keep', port: keepPort, playground: keepLink, mint: keepServer },
];
for (const build of builds) {
  step(`vite build (${build.name})`);
  const dir = join(home, `dist-${build.name}`);
  run(process.execPath, [vite, 'build', '--outDir', dir, '--emptyOutDir', '--logLevel', 'warn'], {
    cwd: purse,
    env: { ...process.env, VITE_PLAYGROUND_LINK: build.playground },
  });
  children.push(
    spawn(process.execPath, [vite, 'preview', '--outDir', dir, '--host', '127.0.0.1', '--port', String(build.port), '--strictPort'], {
      cwd: purse,
      env: { ...process.env, PURSE_DEV_MINT: build.mint },
      stdio: 'ignore',
    }),
  );
  await up(`${origin(build.port)}/`);
}

// ---------- the walk ----------

mkdirSync(out, { recursive: true });
const shots = [];
const problems = [];
/** While set, the browser's own line for a transfer the walk cut off on purpose is no problem. */
let cutting = false;
const browser = await chromium.launch(process.env.PREVIEW_CHROMIUM ? { executablePath: process.env.PREVIEW_CHROMIUM } : {});
try {
  const context = await newContext();
  const page = await newPage(context);

  step('the first run, as hosted: the paste row');
  await page.goto(`${origin(plainPort)}/`);
  await page.waitForSelector('.paste-row');
  await shootAll(page, 'first-paste');

  step('the first run on a mint link: its stamp');
  await page.goto(`${origin(plainPort)}/${link}`);
  await page.waitForSelector('.mstamp');
  await shootAll(page, 'first-link');

  step('the first run with the mint as the build’s playground');
  await page.goto(`${origin(playPort)}/`);
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

console.log(`\n${shots.length} screenshots in ${out}`);
for (const shot of shots) {
  console.log(`  ${shot}`);
}
if (problems.length > 0) {
  console.error(`\n${problems.length} problems:`);
  for (const problem of problems) {
    console.error(`  ${problem}`);
  }
  process.exit(1);
}
process.exit(0);

// ---------- helpers ----------

/** When the mint rotates next on its schedule: at whole multiples of its period. */
function nextRotation() {
  return Math.ceil(Date.now() / 1000 / PERIOD) * PERIOD * 1000;
}

/** Opens the mint link again and takes from the faucet once more, then waits for Holdings. */
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

async function size(page, name, theme) {
  await page.setViewportSize(SIZES[name]);
  await page.emulateMedia({ colorScheme: theme });
  // the field redraws 120 ms after a resize; the fonts are in by then
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(450);
}

async function shoot(page, name, theme, view) {
  const file = `${name}-${theme}-${view}.png`;
  // a phone page never scrolls sideways
  const wide = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  if (wide > 0) {
    problems.push(`${file}: the page is ${wide} px wider than the screen`);
  }
  await page.screenshot({ path: join(out, file), fullPage: true });
  shots.push(file);
}

/** A context as the walks use it: a phone in light, touch, and the policy's refusals reported as errors. */
async function newContext() {
  const context = await browser.newContext({ viewport: SIZES.m360, deviceScaleFactor: 2, hasTouch: true, colorScheme: 'light' });
  // what the hosted policy refuses shows up here, as does any error
  await context.addInitScript(() => {
    document.addEventListener('securitypolicyviolation', (event) => {
      console.error(`policy: ${event.violatedDirective} refused ${event.blockedURI}`);
    });
  });
  return context;
}

/** A page whose errors are the walk's problems. */
async function newPage(context) {
  const page = await context.newPage();
  page.on('pageerror', (error) => problems.push(`page error: ${error.message}`));
  page.on('console', (message) => {
    if (message.type() !== 'error') {
      return;
    }
    // a transfer the walk cut off on purpose: the browser says it failed to load, as it should
    if (cutting && /^Failed to load resource: net::ERR_CONNECTION_(RESET|REFUSED)/.test(message.text())) {
      return;
    }
    problems.push(`console: ${message.text()}`);
  });
  return page;
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
  await page.goto(`${origin(keepPort)}/#home`);
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
  const next = Math.ceil(Date.now() / 1000 / KEEP_PERIOD) * KEEP_PERIOD * 1000;
  if (next - Date.now() < 10 * 60_000) {
    step(`waiting ${Math.ceil((next - Date.now()) / 1000)} s for the keep mint's own rotation to pass`);
    await new Promise((resolve) => setTimeout(resolve, next - Date.now() + 5000));
  }
  const context = await newContext();
  let page = await newPage(context);

  step('keep-alive: two notes of 64 taken, two rotations by hand, a third taken, one more rotation');
  await page.goto(`${origin(keepPort)}/`);
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
  let cut = false;
  cutting = true;
  await page.route('**/v1/transfer', async (route) => {
    if (cut) {
      await route.abort('connectionreset').catch(() => undefined);
      return;
    }
    cut = true;
    await route.fetch();
    await route.abort('connectionreset');
  });
  await page.goto(`${origin(keepPort)}/#home`);
  await until(() => cut, 30_000, 'the first transfer');
  await page.close();
  cutting = false;

  step('the next open settles both: one resent for its answer, one never sent');
  page = await newPage(context);
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
  const other = await newPage(context);
  await openKeep(other);
  await other.waitForFunction(() => document.querySelector('.back-row')?.textContent?.includes('In another tab') === true, undefined, {
    timeout: 15_000,
  });
  await shoot(other, 'm360', 'light', 'othertab');
  await size(other, 'd1280', 'light');
  await other.waitForSelector('.col-back .ka-steps');
  await shoot(other, 'd1280', 'light', 'othertab');
  held.releaseAll();
  await until(async () => (await live(page)).startsWith('Kept'), 40_000, 'the run in the first tab');
  await held.stop();
  await other.close();

  step('a mint that does not answer: every transfer refused on the way');
  await page.goto('about:blank');
  await rotateKeep();
  cutting = true;
  await page.route('**/v1/transfer', (route) => route.abort('connectionrefused'));
  await openKeep(page);
  await page.waitForSelector('.home-err', { timeout: 60_000 });
  await page.waitForSelector('.ka-steps', { state: 'detached', timeout: 90_000 });
  const words = await page.$eval('.home-err', (p) => p.textContent ?? '');
  if (!words.startsWith(`The mint at 127.0.0.1:${keepProxyPort} did not answer, so `) || !words.endsWith('The purse asks again each minute while it is on screen.')) {
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
  if (!quick) {
    step('the mint answers again: the purse tries again within a minute');
    await page.unroute('**/v1/transfer');
    cutting = false;
    await until(async () => (await live(page)).startsWith('Kept'), 75_000, 'the retry');
    await page.waitForSelector('.home-err', { state: 'detached', timeout: 10_000 });
    // the last hop's slide and thread run out (0.9 s and 2 s); its pink stays for 6 s
    await page.waitForTimeout(2500);
    await shoot(page, 'm360', 'light', 'recovered');
  }
  await page.unroute('**/v1/transfer');

  step('the mint does not answer on open: the lanes and the date from what the purse saved');
  await page.goto('about:blank');
  cutting = true;
  await page.route(`**/v1/mints/${kept.issuer}/**`, refuse);
  await page.goto(`${origin(keepPort)}/#home`);
  await page.waitForSelector('.home-err', { timeout: 60_000 });
  // the keeper's pass has failed too: its words, from what the purse saved, stand in for the read's
  await page.waitForTimeout(1500);
  const down = await page.$eval('.home-err', (p) => p.textContent ?? '');
  if (!down.startsWith(`The mint at 127.0.0.1:${keepProxyPort} did not answer. As the purse last saw them, `)) {
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
  await page.unroute(`**/v1/mints/${kept.issuer}/**`, refuse);
  cutting = false;
  await context.close();

  await elsewhereWalk();
}

/** A request the walk refuses, as a server that is down. */
function refuse(route) {
  return route.abort('connectionrefused');
}

/**
 * A purse that holds two mints, the keep mint shown: the first mint's
 * tokens go first, so the date to come back by is theirs and names them;
 * then, on a fresh open, the first mint does not answer, and the date and
 * the words come from what the purse saved of it.
 */
async function elsewhereWalk() {
  // what this walk does to the first mint must fall in one rotation of its schedule
  const next = nextRotation();
  if (next - Date.now() < (quick ? 10 * 60_000 : 100_000)) {
    step(`waiting ${Math.ceil((next - Date.now()) / 1000)} s for the first mint's own rotation to pass`);
    await new Promise((resolve) => setTimeout(resolve, next - Date.now() + 2000));
  }
  const context = await newContext();
  const page = await newPage(context);

  step('two mints: the keep mint taken from first, then the first mint at the same address');
  await page.goto(`${origin(keepPort)}/`);
  await page.waitForSelector('[data-fk="first-start"]');
  await page.click('[data-fk="first-start"]');
  await page.waitForSelector('.f-note');
  faucetFor.set(keepProxyPort, faucetPort);
  try {
    await page.evaluate((hash) => {
      location.hash = hash;
    }, otherLink);
    await page.waitForSelector('[data-fk="first-take"]');
    await page.click('[data-fk="first-take"]');
    // the take is kept before Holdings opens, on the keep mint, the first added
    await page.waitForSelector('.v-home .f-note');
    await page.waitForTimeout(1500);
  } finally {
    faucetFor.set(keepProxyPort, keepFaucetPort);
  }

  step('the first mint rotated twice by hand: its tokens go before the keep mint’s');
  await page.goto('about:blank');
  await rotate();
  await rotate();
  await openKeep(page);
  const elsewhereRow = () => page.$eval('.back-row', (el) => el.textContent ?? '');
  await until(async () => (await elsewhereRow()).includes(`For your tokens at 127.0.0.1:${keepProxyPort}`), 20_000, 'the row naming the other mint');
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
  cutting = true;
  await page.route(`**/v1/mints/${issuer}/**`, refuse);
  await openKeep(page);
  await page.waitForSelector('.home-err', { timeout: 60_000 });
  const words = await page.$eval('.home-err', (p) => p.textContent ?? '');
  if (!words.startsWith(`The mint at 127.0.0.1:${keepProxyPort} did not answer. As the purse last saw them, 64 tokens fade at `)) {
    problems.push(`elsewhere-down: the error says ${JSON.stringify(words)}`);
  }
  const row = await elsewhereRow();
  if (!row.startsWith('First tokens fade at ') || !row.includes(`For your tokens at 127.0.0.1:${keepProxyPort}`)) {
    problems.push(`elsewhere-down: the row says ${JSON.stringify(row)}`);
  }
  for (const [name, theme] of [['m360', 'light'], ['d1280', 'light']]) {
    await size(page, name, theme);
    await shoot(page, name, theme, 'elsewhere-down');
  }
  await page.unroute(`**/v1/mints/${issuer}/**`, refuse);
  cutting = false;
  await context.close();
}

async function shootAll(page, view) {
  for (const name of Object.keys(SIZES)) {
    for (const theme of THEMES) {
      await size(page, name, theme);
      await shoot(page, name, theme, view);
    }
  }
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

function freePort() {
  return new Promise((resolve, reject) => {
    const probe = createServer();
    probe.on('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const { port } = probe.address();
      probe.close(() => resolve(port));
    });
  });
}

/** Waits until `url` answers 200. */
async function up(url) {
  for (let i = 0; i < 200; i++) {
    try {
      if ((await fetch(url)).ok) {
        return;
      }
    } catch {
      // not yet
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`${url} did not come up`);
}

function run(command, argv, options) {
  const result = spawnSync(command, argv, { stdio: 'inherit', ...options });
  if (result.status !== 0) {
    stop();
    throw new Error(`${command} ${argv.join(' ')} exited with ${result.status}`);
  }
}

function step(words) {
  console.log(`preview: ${words}`);
}
