// Runs a real worgl-server with one mint for the tests. Build it first with
// `cargo build -p worgl-server`, or point WORGL_SERVER at the binary.

import { type ChildProcess, execFileSync, spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { Client, type Purse } from '../src/index.ts';

const binary =
  process.env.WORGL_SERVER ??
  fileURLToPath(new URL('../../../target/debug/worgl-server', import.meta.url));

export class TestServer {
  readonly url: string;
  readonly mint: string;
  readonly token: string;
  readonly #mintKey: string;
  readonly #dir: string;
  readonly #process: ChildProcess;
  #sequence = 0;

  private constructor(url: string, mint: string, token: string, mintKey: string, dir: string, child: ChildProcess) {
    this.url = url;
    this.mint = mint;
    this.token = token;
    this.#mintKey = mintKey;
    this.#dir = dir;
    this.#process = child;
  }

  /** Creates a state directory with one four-layer mint and serves it. */
  static async start(): Promise<TestServer> {
    const dir = mkdtempSync(join(tmpdir(), 'worgl-ts-'));
    const state = join(dir, 'state');
    execFileSync(binary, ['init', '--state', state], { stdio: 'ignore' });
    const created = execFileSync(
      binary,
      ['create-mint', '--state', state, '--layers', '4', '--layer-size', '1024'],
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
    for (let tries = 0; ; tries += 1) {
      try {
        if ((await fetch(`${url}/healthz`)).ok) {
          break;
        }
      } catch {
        // Not listening yet.
      }
      if (tries > 200) {
        child.kill();
        throw new Error('worgl-server did not start');
      }
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    return new TestServer(url, mint, token, mintKey, dir, child);
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

  async stop(): Promise<void> {
    const exited = new Promise((resolve) => this.#process.once('exit', resolve));
    this.#process.kill();
    await exited;
    rmSync(this.#dir, { recursive: true, force: true });
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
