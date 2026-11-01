/**
 * What the build tells the service worker (vite.config.ts `serviceWorker`):
 * a version that changes whenever a file it keeps does, and those files, by
 * the paths the page asks for them, `/` for the page itself.
 */
declare const BUILD: { readonly version: string; readonly files: readonly string[] };
