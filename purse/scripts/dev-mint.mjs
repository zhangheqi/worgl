// Serves a local mint for the purse in development: worgl-server, a faucet
// for its mint, and one address in front of both, as the playground's proxy
// serves them (deploy/playground/Caddyfile), so that the purse finds the
// faucet at its mint's server. Build the binaries first, from the
// repository root:
//
//   cargo build --package worgl-server --package worgl-faucet --locked
//
// or point WORGL_SERVER_BINARY and WORGL_FAUCET_BINARY at them.
//
//   node scripts/dev-mint.mjs           serve, creating the mint on first run
//   node scripts/dev-mint.mjs rotate    rotate the mint now, as its schedule would
//   node scripts/dev-mint.mjs --fresh   forget the mint and create a new one
//
// The state lives in target/purse-dev-mint/ at the repository root, or in
// DEV_MINT_HOME, so the mint, and the notes a browser keeps of it, outlast a
// restart. It holds the mint's tokens and authority key, so it stays out of
// the folder the dev server serves.

import { execFileSync, spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer, request } from 'node:http';
import { fileURLToPath } from 'node:url';

const serverBinary =
  process.env.WORGL_SERVER_BINARY ??
  fileURLToPath(new URL('../../target/debug/worgl-server', import.meta.url));
const faucetBinary =
  process.env.WORGL_FAUCET_BINARY ??
  fileURLToPath(new URL('../../target/debug/worgl-faucet', import.meta.url));
const home =
  process.env.DEV_MINT_HOME?.replace(/\/?$/, '/') ??
  fileURLToPath(new URL('../../target/purse-dev-mint/', import.meta.url));

/** The address the purse uses: the server, with the faucet at /v1/faucet. */
const PORT = 8080;
/** The server and the faucet themselves, behind it. */
const SERVER_PORT = 8081;
const FAUCET_PORT = 8091;
/** Seconds between scheduled rotations. */
const PERIOD = Number(process.env.DEV_MINT_PERIOD ?? 300);
/** The purse's origins in development (vite) and preview (vite preview). */
const ORIGINS = [5173, 4173].flatMap((port) => [
  `http://127.0.0.1:${port}`,
  `http://localhost:${port}`,
]);

const [command] = process.argv.slice(2).filter((arg) => !arg.startsWith('--'));
if (process.argv.includes('--fresh')) {
  rmSync(home, { recursive: true, force: true });
}
if (command === 'rotate' && !existsSync(`${home}mint.json`)) {
  console.error(`no dev mint in ${home} yet: run node scripts/dev-mint.mjs first`);
  process.exit(1);
}
const mint = existsSync(`${home}mint.json`) ? read() : create();

if (command === 'rotate') {
  const token = readFileSync(mint.token, 'utf8').trim();
  const answer = await fetch(`http://127.0.0.1:${SERVER_PORT}/v1/mints/${mint.issuer}/rotate`, {
    method: 'POST',
    headers: { authorization: `Bearer ${token}` },
  });
  console.log(answer.status, await answer.text());
  process.exit(answer.ok ? 0 : 1);
}

const children = [
  spawn(
    serverBinary,
    ['serve', '--state', `${home}state`, '--listen', `127.0.0.1:${SERVER_PORT}`],
    { stdio: 'inherit' },
  ),
  spawn(
    faucetBinary,
    [
      '--mint', mint.issuer,
      '--key', mint.key,
      '--server', `http://127.0.0.1:${SERVER_PORT}`,
      '--listen', `127.0.0.1:${FAUCET_PORT}`,
      '--per-address', '1000',
      '--allow-origin', ORIGINS.join(','),
    ],
    { stdio: 'inherit' },
  ),
];
const proxy = createServer((incoming, outgoing) => {
  const { pathname } = new URL(incoming.url ?? '/', 'http://proxy');
  const port = pathname === '/v1/faucet' ? FAUCET_PORT : SERVER_PORT;
  const upstream = request(
    { host: '127.0.0.1', port, method: incoming.method, path: incoming.url, headers: incoming.headers },
    (answer) => {
      outgoing.writeHead(answer.statusCode ?? 502, answer.headers);
      answer.pipe(outgoing);
    },
  );
  upstream.on('error', () => {
    outgoing.writeHead(502, { 'content-type': 'application/json' });
    outgoing.end('{"error":"unavailable","message":"the dev mint is not up yet"}');
  });
  incoming.pipe(upstream);
});
proxy.listen(PORT, '127.0.0.1');

const stop = () => {
  proxy.close();
  for (const child of children) {
    child.kill('SIGTERM');
  }
};
process.on('SIGINT', stop);
process.on('SIGTERM', stop);
for (const child of children) {
  child.on('exit', (code) => {
    if (code !== 0 && code !== null) {
      console.error(`${child.spawnfile} exited with ${code}`);
      stop();
    }
  });
}

const server = `http://127.0.0.1:${PORT}`;
console.log(`mint      ${mint.issuer}`);
console.log(`server    ${server} (the faucet at /v1/faucet)`);
console.log(`rotation  every ${mint.period} s; rotate now with: node scripts/dev-mint.mjs rotate`);
console.log(`mint link http://127.0.0.1:5173/#mint=${mint.issuer}&at=${encodeURIComponent(server)}`);

/** Creates the state directory and its mint, and remembers where they are. */
function create() {
  mkdirSync(home, { recursive: true });
  const state = `${home}state`;
  execFileSync(serverBinary, ['init', '--state', state], { stdio: 'ignore' });
  // New notes land in the last layer, where they rest until the next
  // rotation, so taking from the faucet between rotations fills every lane.
  writeFileSync(`${home}policy.json`, JSON.stringify({ mint_layer: 3 }));
  const created = execFileSync(
    serverBinary,
    [
      'create-mint', '--state', state, '--layers', '4', '--layer-size', '1024',
      '--policy', `${home}policy.json`, '--rotate-every', String(PERIOD),
    ],
    { encoding: 'utf8' },
  );
  // As create-mint prints them: `mint <issuer key>`, `admin token <path>`
  // and `mint key <path> (...)`.
  const field = (name) => new RegExp(`^${name}\\s+(\\S+)`, 'm').exec(created)?.[1];
  const made = {
    issuer: /^mint\s+([0-9a-f]{64})$/m.exec(created)?.[1],
    token: field('admin token'),
    key: field('mint key'),
    period: PERIOD,
  };
  writeFileSync(`${home}mint.json`, JSON.stringify(made, null, 2));
  return made;
}

function read() {
  return JSON.parse(readFileSync(`${home}mint.json`, 'utf8'));
}
