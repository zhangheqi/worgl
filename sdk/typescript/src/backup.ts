import { Holder, Vault } from './core.ts';

/**
 * A mint a backup holds anything of or lists, where the purse that sealed it
 * reached it, and what else that purse wrote of it.
 */
export interface BackupMint {
  /** The mint's issuer key, in hex. */
  mint: string;
  /**
   * The URL of its server, as the purse that sealed the backup named it, or
   * `undefined` when the backup does not say, as in a purse file the
   * `worgl` command line created.
   */
  server: string | undefined;
  /**
   * The other members of its entry in the backup's mint list, such as the
   * name and unit a person gave the mint, as the purse that sealed the backup
   * wrote them; empty when it wrote none. The `worgl` command line keeps
   * them as they are each time it saves; pass them to {@link Purse.backup}
   * to keep them in the next backup.
   */
  extra: Record<string, unknown>;
}

/** An entry of a purse file's mint list, as it is sealed. */
type MintEntry = Record<string, unknown> & { mint: string; server: string };

/**
 * A backup of a purse, opened: what {@link Purse.backup} sealed, or the
 * purse file of the `worgl` command line, which is the same format.
 * {@link Purse.restore} merges it into a purse.
 *
 * It holds the secret key of every note it has: keep it no longer than it
 * takes to restore.
 */
export class Backup {
  /** Every mint it holds anything of or lists, in key order. */
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
    let listed: MintEntry[];
    try {
      const contents = vault.unseal(sealed);
      holder = contents.holder;
      listed = JSON.parse(contents.mints) as MintEntry[];
      contents.free();
    } finally {
      vault.free();
    }
    try {
      const keys = [...new Set([...holder.mints(), ...listed.map((entry) => entry.mint)])].sort();
      const mints = keys.map((mint): BackupMint => {
        // The first entry for a mint names it, as the Rust SDK reads it.
        const entry = listed.find((listedEntry) => listedEntry.mint === mint);
        if (entry === undefined) {
          return { mint, server: undefined, extra: {} };
        }
        const { mint: _, server, ...extra } = entry;
        return { mint, server: normalized(server), extra };
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

/** Seals `holder` under `passphrase`, with the mint list `mints`. */
export function seal(holder: Holder, passphrase: string, mints: MintEntry[]): string {
  const vault = new Vault(passphrase);
  try {
    return vault.seal(holder, JSON.stringify(mints));
  } finally {
    vault.free();
  }
}

/**
 * The mint list of a backup of `held`, each purse's mints with its server,
 * and `listed`, entries such as a person's mints with the names given them.
 * A mint a purse holds is named with that purse's server and the other
 * members of its entry in `listed`; a listed one no purse holds is sealed as
 * listed, if it names a server. Throws a `TypeError` for a mint two purses
 * hold.
 */
export function mintList(
  held: { server: string; mints: string[] }[],
  listed: BackupMint[],
): MintEntry[] {
  const servers = new Map<string, string>();
  for (const { server, mints } of held) {
    for (const mint of mints) {
      if (servers.has(mint)) {
        throw new TypeError(`mint ${mint} is in two of the purses`);
      }
      servers.set(mint, server);
    }
  }
  const extras = new Map<string, Record<string, unknown>>();
  const entries: MintEntry[] = [];
  for (const { mint, server, extra } of listed) {
    if (extras.has(mint)) {
      continue;
    }
    extras.set(mint, extra);
    if (!servers.has(mint) && server !== undefined) {
      entries.push({ ...extra, mint, server });
    }
  }
  for (const [mint, server] of servers) {
    entries.push({ ...extras.get(mint), mint, server });
  }
  return entries.sort((a, b) => (a.mint < b.mint ? -1 : a.mint > b.mint ? 1 : 0));
}

/** A server URL as a `Client` writes its `baseUrl`, if it is one. */
function normalized(server: string): string {
  try {
    return new URL(server).href.replace(/\/+$/, '');
  } catch {
    return server;
  }
}
