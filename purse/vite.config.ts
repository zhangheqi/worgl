import { readFileSync } from 'node:fs';
import { basename } from 'node:path';
import { fileURLToPath } from 'node:url';

import { type Plugin, defineConfig, searchForWorkspaceRoot } from 'vite';

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
 * The Content-Security-Policy that public/_headers gives the hosted purse,
 * with the dev mint added to connect-src, so that a preview runs under it.
 */
function previewPolicy(): string {
  const headers = readFileSync(new URL('public/_headers', import.meta.url), 'utf8');
  const policy = /^\s+Content-Security-Policy:\s*(.+)$/m.exec(headers)?.[1];
  if (policy === undefined) {
    throw new Error('public/_headers sets no Content-Security-Policy');
  }
  return policy.replace(/connect-src ([^;]*)/, `connect-src $1 ${DEV_MINT}`);
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

export default defineConfig({
  plugins: [preloadFonts()],
  // Keep the SDK out of the dependency optimizer: its core finds the wasm
  // relative to its own module (`new URL(..., import.meta.url)`).
  optimizeDeps: { exclude: ['worgl'] },
  server: {
    fs: { allow: [searchForWorkspaceRoot(process.cwd()), sdk] },
  },
  preview: {
    headers: { 'Content-Security-Policy': previewPolicy() },
  },
});
