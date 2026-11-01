import { createHash } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { basename, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { type Plugin, defineConfig, searchForWorkspaceRoot, transformWithOxc } from 'vite';

/** The SDK, linked by `file:`: the dev server serves its wasm/ from here. */
const sdk = fileURLToPath(new URL('../sdk/typescript/', import.meta.url));

/**
 * The mint a preview may call over plain http: the one `scripts/dev-mint.mjs`
 * serves, or another named by `PURSE_DEV_MINT`.
 */
const DEV_MINT = process.env['PURSE_DEV_MINT'] ?? 'http://127.0.0.1:8080';

/** The latin font files the first paint uses, preloaded so that the text does not swap late. */
const FIRST_FONTS = [
  'instrument-serif-latin-400-normal',
  'instrument-sans-latin-wght-normal',
  'martian-mono-latin-wdth-normal',
];

/**
 * The headers that public/_headers gives the hosted purse, so that a preview
 * runs under them: its Content-Security-Policy, with the dev mint added to
 * connect-src, and its Permissions-Policy, so that a wrong `camera=` shows.
 */
function previewHeaders(): Record<string, string> {
  const headers = readFileSync(new URL('public/_headers', import.meta.url), 'utf8');
  const header = (name: string): string => {
    const value = new RegExp(`^\\s+${name}:\\s*(.+)$`, 'm').exec(headers)?.[1];
    if (value === undefined) {
      throw new Error(`public/_headers sets no ${name}`);
    }
    return value;
  };
  return {
    'Content-Security-Policy': header('Content-Security-Policy').replace(/connect-src ([^;]*)/, `connect-src $1 ${DEV_MINT}`),
    'Permissions-Policy': header('Permissions-Policy'),
  };
}

/** Adds a `<link rel="preload">` for each of {@link FIRST_FONTS}, by the name the build gave it. */
function preloadFonts(): Plugin {
  return {
    name: 'purse:preload-fonts',
    apply: 'build',
    transformIndexHtml: {
      order: 'post',
      handler(_html, { bundle }) {
        const files = Object.keys(bundle ?? {});
        return FIRST_FONTS.map((font) => {
          const file = files.find((each) => {
            const name = basename(each);
            return name.startsWith(`${font}-`) && name.endsWith('.woff2');
          });
          if (file === undefined) {
            throw new Error(`the build emitted no ${font}.woff2 to preload`);
          }
          return {
            tag: 'link',
            attrs: { rel: 'preload', as: 'font', type: 'font/woff2', href: `/${file}`, crossorigin: '' },
            injectTo: 'head',
          };
        });
      },
    },
  };
}

/**
 * Whether the service worker keeps a file the build emits: not a source map,
 * and not a font's .woff, which only a browser without .woff2 asks for, and
 * such a browser has no service worker.
 */
const keeps = (file: string): boolean => !file.endsWith('.map') && !file.endsWith('.woff');

/**
 * Emits `sw.js`: sw/sw.ts, with the build's version and the files it keeps
 * before it (sw/build.d.ts). The version is a hash of every one of those
 * files and of the worker itself, so any change to the purse makes a new
 * worker, and the browser installs it.
 */
function serviceWorker(): Plugin {
  let publicDir = '';
  return {
    name: 'purse:service-worker',
    apply: 'build',
    configResolved(config) {
      publicDir = config.publicDir;
    },
    generateBundle: {
      order: 'post',
      async handler(_options, bundle) {
        const source = readFileSync(new URL('sw/sw.ts', import.meta.url), 'utf8');
        const parts: [string, string | Uint8Array][] = [];
        for (const [file, output] of Object.entries(bundle)) {
          if (keeps(file)) {
            parts.push([file === 'index.html' ? '/' : `/${file}`, output.type === 'chunk' ? output.code : output.source]);
          }
        }
        if (!parts.some(([path]) => path === '/')) {
          throw new Error('the service worker found no index.html in the bundle');
        }
        for (const file of readdirSync(publicDir)) {
          if (file !== '_headers') {
            parts.push([`/${file}`, readFileSync(join(publicDir, file))]);
          }
        }
        parts.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
        const hash = createHash('sha256').update(source);
        for (const [path, content] of parts) {
          hash.update(`\0${path}\0`).update(content);
        }
        const build = { version: hash.digest('hex').slice(0, 16), files: parts.map(([path]) => path) };
        const { code } = await transformWithOxc(source, 'sw.ts', { lang: 'ts', target: 'es2022' });
        this.emitFile({ type: 'asset', fileName: 'sw.js', source: `const BUILD = ${JSON.stringify(build)};\n${code}` });
      },
    },
  };
}

export default defineConfig({
  plugins: [preloadFonts(), serviceWorker()],
  // Keep the SDK out of the dependency optimizer: its core finds the wasm
  // relative to its own module (`new URL(..., import.meta.url)`).
  optimizeDeps: { exclude: ['worgl'] },
  server: {
    fs: { allow: [searchForWorkspaceRoot(process.cwd()), sdk] },
  },
  preview: {
    headers: previewHeaders(),
  },
});
