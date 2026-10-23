import { Holder, Vault } from './core.ts';

/** A mint a backup holds anything of, and where the purse that sealed it reached it. */
export interface BackupMint {
  /** The mint's issuer key, in hex. */
  mint: string;
  /**
   * The URL of its server, as the purse that sealed the backup named it, or
   * `undefined` when the backup does not say, as in a purse file of the
   * `worgl` command line.
   */
  server: string | undefined;
}

/**
 * A backup of a purse, opened: what {@link Purse.backup} sealed, or the
 * purse file of the `worgl` command line, which is the same format.
 * {@link Purse.restore} merges it into a purse.
 *
 * It holds the secret key of every note it has: keep it no longer than it
 * takes to restore.
 */
export class Backup {
  /** Every mint it holds anything of, in key order. */
  readonly mints: BackupMint[];
  readonly #json: string;

  private constructor(json: string, mints: BackupMint[]) {
    this.#json = json;
    this.mints = mints;
  }

  /**
   * Opens `sealed`, a backup or a purse file, with `passphrase`.
   *
   * Deriving its key takes 64 MiB of memory and a moment, on purpose: about
   * a quarter of a second in Node and in Chromium on a desktop processor,
   * longer on a phone, and the thread does nothing else meanwhile. The
   * WebAssembly memory grows by 64 MiB the first time and keeps that size.
   *
   * A wrong passphrase, or a file altered since it was sealed, which the
   * same check catches, is refused with the {@link WorglError}
   * `wrong_passphrase`; text that is no purse file with `not_sealed`; one of
   * a newer version or holding a newer purse with `unknown_version` or
   * `unknown_format`; one that does not read with `damaged`.
   */
  static async open(sealed: string, passphrase: string): Promise<Backup> {
    const vault = Vault.open(sealed, passphrase);
    let holder: Holder;
    let listed: { mint: string; server: string }[];
    try {
      const contents = vault.unseal(sealed);
      holder = contents.holder;
      listed = JSON.parse(contents.mints) as { mint: string; server: string }[];
      contents.free();
    } finally {
      vault.free();
    }
    try {
      const mints = holder.mints().map((mint) => {
        const server = listed.find((entry) => entry.mint === mint)?.server;
        return { mint, server: server === undefined ? undefined : normalized(server) };
      });
      return new Backup(holder.toJson(), mints);
    } finally {
      holder.free();
    }
  }

  /** The whole purse it holds as JSON, secret keys included, as {@link Purse.toJson} gives it. */
  toJson(): string {
    return this.#json;
  }
}

/** Seals `holder` under `passphrase`, naming `server` for each of its mints. */
export function seal(holder: Holder, passphrase: string, server: string): string {
  const vault = new Vault(passphrase);
  try {
    const mints = holder.mints().map((mint) => ({ mint, server }));
    return vault.seal(holder, JSON.stringify(mints));
  } finally {
    vault.free();
  }
}

/** A server URL as a `Client` writes its `baseUrl`, if it is one. */
function normalized(server: string): string {
  try {
    return new URL(server).href.replace(/\/+$/, '');
  } catch {
    return server;
  }
}
