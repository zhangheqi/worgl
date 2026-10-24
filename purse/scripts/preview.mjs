// Screenshots of the purse against a real mint: starts worgl-server with a
// short-period mint and a faucet for it on free ports, builds the purse
// twice (as hosted, and with that mint as the build's playground), serves
// both under the hosted Content-Security-Policy, and walks them in
// Chromium: the first run, a take from the faucet, rotations and more
// takes, until the field holds fading, ready and resting tokens. Then it
// writes PNGs of each view at 360 x 780 and 1280 x 860, light and dark,
// and of a note's tip open.
//
//   node scripts/preview.mjs [--quick] [--out <dir>]
//
// --quick rotates the mint by hand instead of waiting for its schedule,
// on a mint that rotates every 8 hours as the playground does, for a fast
// look; without it the mint rotates every two minutes, and once on its
// own while the purse is open, as it would for a person. Needs the `playwright` package where
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

const [serverPort, faucetPort, proxyPort, plainPort, playPort] = await Promise.all([1, 2, 3, 4, 5].map(freePort));
const origin = (port) => `http://127.0.0.1:${port}`;
const mintServer = origin(proxyPort);

step(`a mint rotating every ${PERIOD} s, in ${home}`);
rmSync(home, { recursive: true, force: true });
mkdirSync(home, { recursive: true });
const state = join(home, 'state');
execFileSync(binaries.server, ['init', '--state', state], { stdio: 'ignore' });
// new notes land in the last layer, where they rest until the next rotation
writeFileSync(join(home, 'policy.json'), JSON.stringify({ mint_layer: 3 }));
const created = execFileSync(
  binaries.server,
  ['create-mint', '--state', state, '--layers', '4', '--layer-size', '1024', '--policy', join(home, 'policy.json'), '--rotate-every', String(PERIOD)],
  { encoding: 'utf8' },
);
const field = (name) => new RegExp(`^${name}\\s+(\\S+)`, 'm').exec(created)?.[1];
const issuer = /^mint\s+([0-9a-f]{64})$/m.exec(created)?.[1];
const token = readFileSync(field('admin token'), 'utf8').trim();
const link = `#mint=${issuer}&at=${encodeURIComponent(mintServer)}`;

children.push(
  spawn(binaries.server, ['serve', '--state', state, '--listen', `127.0.0.1:${serverPort}`], { stdio: ['ignore', 'ignore', 'inherit'] }),
  spawn(
    binaries.faucet,
    [
      '--mint', issuer,
      '--key', field('mint key'),
      '--server', origin(serverPort),
      '--listen', `127.0.0.1:${faucetPort}`,
      '--allow-origin', [plainPort, playPort].map(origin).join(','),
    ],
    { stdio: ['ignore', 'ignore', 'inherit'] },
  ),
);
// one address for both, as the playground's proxy serves them: the faucet at /v1/faucet
servers.push(
  createServer((incoming, outgoing) => {
    const { pathname } = new URL(incoming.url ?? '/', 'http://proxy');
    const port = pathname === '/v1/faucet' ? faucetPort : serverPort;
    const upstream = request(
      { host: '127.0.0.1', port, method: incoming.method, path: incoming.url, headers: incoming.headers },
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
  }).listen(proxyPort, '127.0.0.1'),
);
await up(`${mintServer}/v1/mints/${issuer}/info`);
await up(`${origin(faucetPort)}/v1/faucet`);

/** Rotates the mint now, as its schedule would. */
async function rotate() {
  const answer = await fetch(`${origin(serverPort)}/v1/mints/${issuer}/rotate`, {
    method: 'POST',
    headers: { authorization: `Bearer ${token}` },
  });
  if (!answer.ok) {
    throw new Error(`rotate: ${answer.status} ${await answer.text()}`);
  }
}

// ---------- the purse, built and served twice ----------

const vite = join(purse, 'node_modules/vite/bin/vite.js');
const builds = [
  { name: 'plain', port: plainPort, playground: '' },
  { name: 'playground', port: playPort, playground: link },
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
      env: { ...process.env, PURSE_DEV_MINT: mintServer },
      stdio: 'ignore',
    }),
  );
  await up(`${origin(build.port)}/`);
}

// ---------- the walk ----------

mkdirSync(out, { recursive: true });
const shots = [];
const problems = [];
const browser = await chromium.launch(process.env.PREVIEW_CHROMIUM ? { executablePath: process.env.PREVIEW_CHROMIUM } : {});
try {
  const context = await browser.newContext({ viewport: SIZES.m360, deviceScaleFactor: 2, hasTouch: true, colorScheme: 'light' });
  // what the hosted policy refuses shows up here, as does any error
  await context.addInitScript(() => {
    document.addEventListener('securitypolicyviolation', (event) => {
      console.error(`policy: ${event.violatedDirective} refused ${event.blockedURI}`);
    });
  });
  const page = await context.newPage();
  page.on('pageerror', (error) => problems.push(`page error: ${error.message}`));
  page.on('console', (message) => {
    if (message.type() === 'error') {
      problems.push(`console: ${message.text()}`);
    }
  });

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

async function size(page, name, theme) {
  await page.setViewportSize(SIZES[name]);
  await page.emulateMedia({ colorScheme: theme });
  // the field redraws 120 ms after a resize; the fonts are in by then
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(450);
}

async function shoot(page, name, theme, view) {
  const file = `${name}-${theme}-${view}.png`;
  await page.screenshot({ path: join(out, file), fullPage: true });
  shots.push(file);
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
