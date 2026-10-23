// Node-only parts of the SDK, kept apart so browser bundles never see node:fs.

import { open, readFile, rename } from 'node:fs/promises';
import { basename, dirname, join } from 'node:path';

import type { Store } from './store.ts';

/**
 * A store in one file, replaced whole on every save.
 *
 * A save writes a temporary file beside it, flushes it to disk and renames
 * it over the old one, so a crash leaves either the old store or the new,
 * never a mix. The file is readable by its owner only.
 *
 * It has no {@link Store.lock | lock}, so give each purse a file of its
 * own: two purses on one file save over each other's changes.
 */
export class FileStore implements Store {
  readonly path: string;

  constructor(path: string) {
    this.path = path;
  }

  async load(): Promise<string | undefined> {
    try {
      return await readFile(this.path, 'utf8');
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') {
        return undefined;
      }
      throw error;
    }
  }

  async save(json: string): Promise<void> {
    const dir = dirname(this.path);
    const temporary = join(dir, `.${basename(this.path)}.new`);
    const file = await open(temporary, 'w', 0o600);
    try {
      await file.writeFile(json);
      await file.sync();
    } finally {
      await file.close();
    }
    await rename(temporary, this.path);
    // The rename is durable only once the directory is flushed too.
    if (process.platform !== 'win32') {
      const directory = await open(dir, 'r');
      try {
        await directory.sync();
      } finally {
        await directory.close();
      }
    }
  }
}
