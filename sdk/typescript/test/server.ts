// Runs a real worgl-server with one mint for the tests, and a faucet for it.
// Build them first with `cargo build -p worgl-server -p worgl-faucet`, or
// point WORGL_SERVER and WORGL_FAUCET_BINARY at the binaries.

import { type ChildProcess, execFileSync, spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { Client, type Purse } from '../src/index.ts';

const binary =
  process.env.WORGL_SERVER ??
  fileURLToPath(new URL('../../../target/debug/worgl-server', import.meta.url));
const faucetBinary =
  process.env.WORGL_FAUCET_BINARY ??
  fileURLToPath(new URL('../../../target/debug/worgl-faucet', import.meta.url));

export class TestServer {
  readonly url: string;
  readonly mint: string;
  readonly token: string;
  readonly #mintKey: string;
  readonly #dir: string;
  #process: ChildProcess;
  #faucet: ChildProcess | undefined;
  #sequence = 0;

  private constructor(url: string, mint: string, token: string, mintKey: string, dir: string, child: ChildProcess) {
    this.url = url;
    this.mint = mint;
    this.token = token;
    this.#mintKey = mintKey;
    this.#dir = dir;
    this.#process = child;
  }

  /**
   * Creates a state directory with one four-layer mint and serves it. The
   * mint's policy fields not in `policy` take their cash-like defaults. With
   * `rotateEvery`, the mint also rotates every that many seconds. Each layer
   * holds `layerSize` notes.
   */
  static async start(
    policy: Record<string, unknown> = {},
    rotateEvery?: number,
    layerSize = 1024,
  ): Promise<TestServer> {
    const dir = mkdtempSync(join(tmpdir(), 'worgl-ts-'));
    const state = join(dir, 'state');
    execFileSync(binary, ['init', '--state', state], { stdio: 'ignore' });
    const policyFile = join(dir, 'policy.json');
    writeFileSync(policyFile, JSON.stringify(policy));
    const created = execFileSync(
      binary,
      [
        'create-mint', '--state', state, '--layers', '4', '--layer-size', String(layerSize),
        '--policy', policyFile,
        ...(rotateEvery === undefined ? [] : ['--rotate-every', String(rotateEvery)]),
      ],
      { encoding: 'utf8' },
    );
    const field = (name: string): string => {
      const line = created.split('\n').find((l) => l.startsWith(name));
      if (line === undefined) {
        throw new Error(`create-mint printed no ${name}: ${created}`);
      }
      return line.slice(name.length).trim().split(' ')[0] ?? '';
    };
    const mint = field('mint ');
    const token = readFileSync(field('admin token'), 'utf8').trim();
    const mintKey = field('mint key');

    const port = await freePort();
    const child = spawn(binary, ['serve', '--state', state, '--listen', `127.0.0.1:${port}`], {
      stdio: 'ignore',
    });
    const url = `http://127.0.0.1:${port}`;
    await healthy(url, child, 'worgl-server');
    return new TestServer(url, mint, token, mintKey, dir, child);
  }

  /**
   * Serves a faucet for the mint, signing with its authority key, and
   * returns a client for it. It hands out notes of 64, 3 an hour per address.
   * Its sequence numbers are far above {@link mintTo}'s, which the mint then
   * refuses, so give a faucet a server of its own.
   */
  async startFaucet(): Promise<Client> {
    const port = await freePort();
    const child = spawn(
      faucetBinary,
      [
        '--mint', this.mint,
        '--key', this.#mintKey,
        '--server', this.url,
        '--listen', `127.0.0.1:${port}`,
        '--per-address', '3',
      ],
      { stdio: 'ignore' },
    );
    this.#faucet = child;
    const url = `http://127.0.0.1:${port}`;
    await healthy(url, child, 'worgl-faucet');
    return new Client(url);
  }

  client(): Client {
    return new Client(this.url);
  }

  /** Mints `amount` to `holder`, signed with the mint's authority key. */
  async mintTo(amount: bigint, holder: string): Promise<string> {
    this.#sequence += 1;
    const request = execFileSync(
      binary,
      [
        'sign-mint',
        '--key', this.#mintKey,
        '--issuer', this.mint,
        '--sequence', String(this.#sequence),
        '--amount', String(amount),
        '--holder', holder,
      ],
      { encoding: 'utf8' },
    );
    return this.client().mint(this.mint, request);
  }

  /** Gives `purse` `amount` in notes of at most `largest`. */
  async fund(purse: Purse, amount: bigint, largest: bigint): Promise<void> {
    const request = await purse.requestAtMost(this.mint, amount, largest);
    const notes: string[] = [];
    for (const note of request.notes) {
      notes.push(await this.mintTo(note.amount, note.holder));
    }
    if ((await purse.accept(notes)) !== amount) {
      throw new Error('funding was not taken in');
    }
  }

  async rotate(): Promise<void> {
    await this.client().rotate(this.mint, this.token);
  }

  /**
   * Stops serving, sets the fields in `policy` in the mint's policy on disk
   * as its operator would, and serves again at the same address.
   */
  async restart(policy: Record<string, unknown>): Promise<void> {
    const exited = new Promise((resolve) => this.#process.once('exit', resolve));
    this.#process.kill();
    await exited;
    const state = join(this.#dir, 'state');
    const file = join(state, 'mints', this.mint, 'policy.json');
    writeFileSync(file, JSON.stringify({ ...JSON.parse(readFileSync(file, 'utf8')), ...policy }));
    this.#process = spawn(binary, ['serve', '--state', state, '--listen', new URL(this.url).host], {
      stdio: 'ignore',
    });
    await healthy(this.url, this.#process, 'worgl-server');
  }

  async stop(): Promise<void> {
    for (const child of [this.#faucet, this.#process]) {
      if (child !== undefined && child.exitCode === null) {
        const exited = new Promise((resolve) => child.once('exit', resolve));
        child.kill();
        await exited;
      }
    }
    rmSync(this.#dir, { recursive: true, force: true });
  }
}

/** Waits until `url` answers its health check. */
async function healthy(url: string, child: ChildProcess, name: string): Promise<void> {
  for (let tries = 0; ; tries += 1) {
    try {
      if ((await fetch(`${url}/healthz`)).ok) {
        return;
      }
    } catch {
      // Not listening yet.
    }
    if (tries > 200) {
      child.kill();
      throw new Error(`${name} did not start`);
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const probe = createServer();
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const address = probe.address();
      probe.close(() => {
        if (address !== null && typeof address === 'object') {
          resolve(address.port);
        } else {
          reject(new Error('no port'));
        }
      });
    });
  });
}
