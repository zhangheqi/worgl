// What the preview walks share: worgl-server with mints on free ports, a
// faucet for each, one address for the server and a faucet behind a proxy
// the walk can slow, cut or take down, the purse built twice and served
// under the hosted headers, and the screenshots with the problems they
// must not show.
//
// Used by scripts/preview.mjs (Holdings) and scripts/preview-send.mjs
// (Receive and Send). Needs the `playwright` package where Node can require
// it (NODE_PATH) and a Chromium it can launch (PREVIEW_CHROMIUM names one).
// Cargo builds worgl-server and worgl-faucet into CARGO_TARGET_DIR (or
// target/), unless WORGL_SERVER_BINARY and WORGL_FAUCET_BINARY name them.

import { execFileSync, spawn, spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer, request } from 'node:http';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const purse = fileURLToPath(new URL('..', import.meta.url));
export const repo = join(purse, '..');
export const SIZES = { m360: { width: 360, height: 780 }, d1280: { width: 1280, height: 860 } };
export const THEMES = ['light', 'dark'];

/** `--quick` and `--out <dir>` from the command line. */
export function options(fallback) {
  const args = process.argv.slice(2);
  const outAt = args.indexOf('--out');
  return {
    quick: args.includes('--quick'),
    out: outAt >= 0 && args[outAt + 1] !== undefined ? args[outAt + 1] : fallback,
  };
}

export function step(words) {
  console.log(`preview: ${words}`);
}

/** Playwright's chromium, from NODE_PATH. */
export function chromiumOf() {
  try {
    return createRequire(import.meta.url)('playwright').chromium;
  } catch {
    console.error('preview: cannot require playwright; put it on NODE_PATH');
    process.exit(1);
  }
}

/**
 * Launches Chromium, the one PREVIEW_CHROMIUM names or Playwright's own,
 * with extra arguments. `full` asks for Playwright's whole Chromium rather
 * than its headless shell, which has no camera, fake or not.
 */
export function launch(chromium, args = [], full = false) {
  const which = process.env.PREVIEW_CHROMIUM ? { executablePath: process.env.PREVIEW_CHROMIUM } : full ? { channel: 'chromium' } : {};
  return chromium.launch({ ...which, args });
}

// ---------- processes ----------

const children = [];
const servers = [];

/** Stops every server and child process started here. */
export function stop() {
  for (const server of servers) {
    server.close();
    server.closeAllConnections?.();
  }
  servers.length = 0;
  for (const child of children) {
    child.kill('SIGTERM');
  }
  children.length = 0;
}
process.on('SIGINT', () => {
  stop();
  process.exit(130);
});

export function run(command, argv, opts) {
  const result = spawnSync(command, argv, { stdio: 'inherit', ...opts });
  if (result.status !== 0) {
    stop();
    throw new Error(`${command} ${argv.join(' ')} exited with ${result.status}`);
  }
}

export function freePort() {
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
export async function up(url) {
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

export const origin = (port) => `http://127.0.0.1:${port}`;

let binaries;
/** worgl-server and worgl-faucet: as named, or built by cargo once. */
export function binariesOf() {
  if (binaries !== undefined) {
    return binaries;
  }
  const server = process.env.WORGL_SERVER_BINARY;
  const faucet = process.env.WORGL_FAUCET_BINARY;
  if (server !== undefined && faucet !== undefined) {
    binaries = { server, faucet };
    return binaries;
  }
  step('cargo build worgl-server worgl-faucet');
  run('cargo', ['build', '--package', 'worgl-server', '--package', 'worgl-faucet', '--locked'], { cwd: repo });
  const target = process.env.CARGO_TARGET_DIR ?? join(repo, 'target');
  binaries = { server: join(target, 'debug/worgl-server'), faucet: join(target, 'debug/worgl-faucet') };
  return binaries;
}

// ---------- the mint, its faucet and the proxy ----------

/**
 * A schedule of about `period` seconds whose next rotation is more than
 * `clearMs` off: a mint rotates at whole multiples of its period, so a walk
 * that rotates by hand meets no rotation of the schedule's, nor keep-alive
 * moving fading notes because one is near (as it does for Send's plan within
 * a quarter of an hour of it). Tries the period first, then a few minutes
 * either side of it.
 */
export function clearPeriod(period, clearMs) {
  for (let step = 0; step < 60; step++) {
    const tried = period + (step % 2 === 0 ? 1 : -1) * Math.ceil(step / 2) * 300;
    const next = Math.ceil(Date.now() / 1000 / tried) * tried * 1000;
    if (next - Date.now() > clearMs) {
      return tried;
    }
  }
  return period;
}

/**
 * Starts a fresh state in `home` with `mints` mints (4 layers of 1024, new
 * notes in the last layer, rotating every `period` seconds), a faucet for
 * each, and a proxy that serves the server and the first one's faucet at
 * one address, as the playground does. Then builds the purse (as hosted,
 * and with the first mint as the playground, or only those `serve` names)
 * and serves each build.
 */
export async function setup({ home, period, perAddress = 10, mints = 1, serve = ['plain', 'playground'] }) {
  const bin = binariesOf();
  const [serverPort, proxyPort, plainPort, playPort] = await Promise.all([1, 2, 3, 4].map(freePort));
  const faucetPorts = await Promise.all(Array.from({ length: mints }, freePort));
  const server = origin(proxyPort);

  step(`${mints === 1 ? 'a mint' : `${mints} mints`} rotating every ${period} s, in ${home}`);
  rmSync(home, { recursive: true, force: true });
  mkdirSync(home, { recursive: true });
  const state = join(home, 'state');
  execFileSync(bin.server, ['init', '--state', state], { stdio: 'ignore' });
  // new notes land in the last layer, where they rest until the next rotation
  writeFileSync(join(home, 'policy.json'), JSON.stringify({ mint_layer: 3 }));
  const made = [];
  for (let i = 0; i < mints; i++) {
    const created = execFileSync(
      bin.server,
      ['create-mint', '--state', state, '--layers', '4', '--layer-size', '1024', '--policy', join(home, 'policy.json'), '--rotate-every', String(period)],
      { encoding: 'utf8' },
    );
    const field = (name) => new RegExp(`^${name}\\s+(\\S+)`, 'm').exec(created)?.[1];
    const issuer = /^mint\s+([0-9a-f]{64})$/m.exec(created)?.[1];
    made.push({ issuer, key: field('mint key'), token: readFileSync(field('admin token'), 'utf8').trim() });
  }
  const { issuer } = made[0];
  const mintLink = (mint) => `#mint=${mint}&at=${encodeURIComponent(server)}`;

  children.push(
    spawn(bin.server, ['serve', '--state', state, '--listen', `127.0.0.1:${serverPort}`], { stdio: ['ignore', 'ignore', 'inherit'] }),
    ...made.map((each, i) =>
      spawn(
        bin.faucet,
        [
          '--mint', each.issuer,
          '--key', each.key,
          '--server', origin(serverPort),
          '--listen', `127.0.0.1:${faucetPorts[i]}`,
          '--per-address', String(perAddress),
          '--allow-origin', [plainPort, playPort].map(origin).join(','),
        ],
        { stdio: ['ignore', 'ignore', 'inherit'] },
      ),
    ),
  );

  // ---------- the proxy, and what the walk can make it do ----------
  const control = { hold: undefined, holdKeys: undefined, holdInfo: undefined, refuse: undefined, refuseSkip: 0, drop: false, down: false, transfers: 0, faucets: 0, held: 0, faucet: 0, lastTransfer: 0 };
  /** The gates requests wait at now, by kind, so that a release opens them. */
  const waiting = { transfer: new Set(), keys: new Set(), info: new Set() };
  const wait = async (kind, held) => {
    waiting[kind].add(held);
    await held.opened;
    waiting[kind].delete(held);
  };
  const sockets = new Set();
  /** A gate a request waits at: until `ms` passes or it is let go. */
  const gate = (ms) => {
    let open;
    const opened = new Promise((resolve) => {
      open = resolve;
    });
    const timer = setTimeout(open, ms);
    return { opened, open: () => (clearTimeout(timer), open()), skip: 0 };
  };
  /** What a mint answers when it turns a transfer away. */
  const REFUSALS = {
    layer_full: { status: 503, message: 'the newest layer is full' },
    spent: { status: 409, message: 'a note was spent already' },
  };
  const proxy = createServer(async (incoming, outgoing) => {
    if (control.down) {
      incoming.socket.destroy();
      return;
    }
    const { pathname } = new URL(incoming.url ?? '/', 'http://proxy');
    const transfer = incoming.method === 'POST' && pathname === '/v1/transfer';
    const keys = incoming.method === 'GET' && /^\/v1\/mints\/[0-9a-f]+\/keys$/.test(pathname);
    const info = incoming.method === 'GET' && /^\/v1\/mints\/[0-9a-f]+\/info$/.test(pathname);
    if (transfer) {
      control.lastTransfer = Date.now();
    }
    // read the body first, so a held request can still be forwarded whole
    const body = [];
    for await (const chunk of incoming) {
      body.push(chunk);
    }
    if (transfer && control.hold !== undefined) {
      if (control.hold.skip > 0) {
        control.hold.skip -= 1;
      } else {
        const held = control.hold;
        control.hold = undefined;
        control.held += 1;
        await wait('transfer', held);
        control.held -= 1;
      }
    }
    if (keys && control.holdKeys !== undefined) {
      const held = control.holdKeys;
      control.holdKeys = undefined;
      await wait('keys', held);
    }
    if (info && control.holdInfo !== undefined) {
      const held = control.holdInfo;
      control.holdInfo = undefined;
      await wait('info', held);
    }
    if (process.env.PREVIEW_DEBUG && (control.down || incoming.socket.destroyed)) {
      console.log(`proxy: ${incoming.method} ${pathname} not forwarded (${control.down ? 'down' : 'client gone'})`);
    }
    if (control.down || incoming.socket.destroyed) {
      // down, or the page that asked is gone (closed while held): nothing reaches the mint
      incoming.socket.destroy();
      return;
    }
    if (transfer && control.refuse !== undefined && control.refuseSkip > 0) {
      control.refuseSkip -= 1;
    } else if (transfer && control.refuse !== undefined) {
      const { status, message } = REFUSALS[control.refuse];
      const code = control.refuse;
      control.refuse = undefined;
      outgoing.writeHead(status, { 'content-type': 'application/json', 'access-control-allow-origin': incoming.headers.origin ?? '*' });
      outgoing.end(JSON.stringify({ error: code, message }));
      return;
    }
    const port = pathname === '/v1/faucet' ? faucetPorts[control.faucet] : serverPort;
    const dropping = transfer && control.drop;
    const upstream = request({ host: '127.0.0.1', port, method: incoming.method, path: incoming.url, headers: incoming.headers }, (answer) => {
      if (process.env.PREVIEW_DEBUG) {
        console.log(`proxy: ${incoming.method} ${pathname} ${answer.statusCode}${dropping ? ' (dropped)' : ''}`);
      }
      if (transfer) {
        control.transfers += 1;
      }
      if (incoming.method === 'POST' && pathname === '/v1/faucet' && answer.statusCode === 200) {
        control.faucets += 1;
      }
      if (dropping) {
        // applied by the mint, and the answer never comes
        answer.resume();
        answer.on('end', () => incoming.socket.destroy());
        return;
      }
      outgoing.writeHead(answer.statusCode ?? 502, answer.headers);
      answer.pipe(outgoing);
    });
    upstream.on('error', () => {
      outgoing.writeHead(502);
      outgoing.end();
    });
    upstream.end(Buffer.concat(body));
  });
  proxy.on('connection', (socket) => {
    if (control.down) {
      socket.destroy();
      return;
    }
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
  });
  proxy.listen(proxyPort, '127.0.0.1');
  servers.push(proxy);
  await up(`${server}/v1/mints/${issuer}/info`);
  for (const port of faucetPorts) {
    await up(`${origin(port)}/v1/faucet`);
  }

  const mint = {
    issuer,
    issuers: made.map((each) => each.issuer),
    server,
    link: mintLink(issuer),
    mintLink,
    /** Rotates the mint now, as its schedule would. */
    async rotate(which = issuer) {
      const entry = made.find((each) => each.issuer === which);
      const answer = await fetch(`${origin(serverPort)}/v1/mints/${which}/rotate`, {
        method: 'POST',
        headers: { authorization: `Bearer ${entry.token}` },
      });
      if (!answer.ok) {
        throw new Error(`rotate: ${answer.status} ${await answer.text()}`);
      }
    },
    /** The mint's info, from the server itself. */
    async info(which = issuer) {
      const answer = await fetch(`${origin(serverPort)}/v1/mints/${which}/info`);
      return answer.json();
    },
    /** When the mint rotates next on its schedule: at whole multiples of its period. */
    nextRotation() {
      return Math.ceil(Date.now() / 1000 / period) * period * 1000;
    },
    proxy: {
      /** The next POST /v1/transfer (after `skip` more) waits `ms`, or until released. */
      hold(ms, skip = 0) {
        control.hold = gate(ms);
        control.hold.skip = skip;
      },
      /** How many transfers wait at the gate now. */
      get held() {
        return control.held;
      },
      /** The next POST /v1/transfer (after `skip` more go through) is turned away as the mint would: `layer_full` or `spent`. */
      refuse(code, skip = 0) {
        control.refuse = code;
        control.refuseSkip = skip;
      },
      release() {
        for (const held of waiting.transfer) {
          held.open();
        }
        control.hold?.open();
        control.hold = undefined;
      },
      /** The next GET /v1/mints/…/keys waits `ms`, or until released. */
      holdKeys(ms) {
        control.holdKeys = gate(ms);
      },
      releaseKeys() {
        for (const held of waiting.keys) {
          held.open();
        }
        control.holdKeys?.open();
        control.holdKeys = undefined;
      },
      /** The next GET /v1/mints/…/info waits `ms`, or until released. */
      holdInfo(ms) {
        control.holdInfo = gate(ms);
      },
      releaseInfo() {
        for (const held of waiting.info) {
          held.open();
        }
        control.holdInfo?.open();
        control.holdInfo = undefined;
      },
      /** POST /v1/transfer goes to the mint, and its answer is cut off. */
      drop(on) {
        control.drop = on;
      },
      /** Every connection is cut at once: the mint cannot be reached. */
      down(on) {
        control.down = on;
        if (on) {
          for (const socket of sockets) {
            socket.destroy();
          }
        }
      },
      get transfers() {
        return control.transfers;
      },
      get faucets() {
        return control.faucets;
      },
      /**
       * Resolves once no transfer has reached the proxy, nor waits at its
       * gate, for `ms` from now: a purse's keep-alive sends its hops a few
       * seconds apart, so none is part way through a run.
       */
      async quiet(ms) {
        const from = Date.now();
        while (control.held > 0 || Date.now() - Math.max(from, control.lastTransfer) < ms) {
          await new Promise((resolve) => setTimeout(resolve, 100));
        }
      },
      /** Which mint's faucet answers at the proxy's /v1/faucet: the first's until this names another. */
      faucet(which = issuer) {
        control.faucet = made.findIndex((each) => each.issuer === which);
      },
    },
  };

  // ---------- the purse, built and served twice ----------
  const vite = join(purse, 'node_modules/vite/bin/vite.js');
  const builds = {};
  for (const build of [
    { name: 'plain', port: plainPort, playground: '' },
    { name: 'playground', port: playPort, playground: mint.link },
  ].filter((each) => serve.includes(each.name))) {
    step(`vite build (${build.name})`);
    const dir = join(home, `dist-${build.name}`);
    run(process.execPath, [vite, 'build', '--outDir', dir, '--emptyOutDir', '--logLevel', 'warn'], {
      cwd: purse,
      env: { ...process.env, VITE_PLAYGROUND_LINK: build.playground },
    });
    children.push(
      spawn(process.execPath, [vite, 'preview', '--outDir', dir, '--host', '127.0.0.1', '--port', String(build.port), '--strictPort'], {
        cwd: purse,
        env: { ...process.env, PURSE_DEV_MINT: server },
        stdio: 'ignore',
      }),
    );
    await up(`${origin(build.port)}/`);
    builds[build.name] = origin(build.port);
  }
  return { mint, builds };
}

// ---------- pages, problems and shots ----------

/**
 * A new context whose pages report what the hosted policy refuses, and
 * every page error and console error, into `problems`, each under `who`.
 */
export async function newContext(browser, problems, who, extra = {}) {
  const context = await browser.newContext({ viewport: SIZES.m360, deviceScaleFactor: 2, hasTouch: true, colorScheme: 'light', ...extra });
  // what the hosted policy refuses shows up here, as does any error
  await context.addInitScript(() => {
    document.addEventListener('securitypolicyviolation', (event) => {
      console.error(`policy: ${event.violatedDirective} refused ${event.blockedURI}`);
    });
  });
  context.on('page', (page) => watch(page, problems, who));
  return context;
}

/**
 * Console errors a step causes on purpose (a mint down, an answer cut off
 * or refused, a server asked for a mint it does not list): the browser
 * logs each failed request, and these are not problems while that step
 * runs.
 */
const tolerated = [];

/** Runs `step` with the browser's own lines for failed requests tolerated. */
export async function failingRequests(step) {
  const lines = /^Failed to load resource: (net::ERR_|the server responded with a status of (404|409|503))/;
  tolerated.push(lines);
  try {
    return await step();
  } finally {
    tolerated.splice(tolerated.indexOf(lines), 1);
  }
}

/** Collects `page`'s errors into `problems`. */
export function watch(page, problems, who) {
  page.on('pageerror', (error) => problems.push(`${who}: page error: ${error.message}`));
  page.on('console', (message) => {
    if (message.type() === 'error' && !tolerated.some((pattern) => pattern.test(message.text()))) {
      problems.push(`${who}: console: ${message.text()}`);
    }
  });
}

/** Screenshots into `out`: `${size}-${theme}-${view}.png`. */
export function shooter(out) {
  mkdirSync(out, { recursive: true });
  const shots = [];
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
  async function shootAll(page, view, after) {
    for (const name of Object.keys(SIZES)) {
      for (const theme of THEMES) {
        await size(page, name, theme);
        await shoot(page, name, theme, view);
        await after?.(page, name, theme);
      }
    }
  }
  return { shots, size, shoot, shootAll };
}

/** Prints the shots and the problems; exits 1 when there are problems. */
export function report(out, shots, problems) {
  console.log(`\n${shots.length} screenshots in ${out}`);
  for (const shot of shots) {
    console.log(`  ${shot}`);
  }
  if (problems.length > 0) {
    console.error(`\n${problems.length} problems:`);
    for (const problem of problems) {
      console.error(`  ${problem}`);
    }
    return 1;
  }
  return 0;
}
