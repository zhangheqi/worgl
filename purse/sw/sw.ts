// The purse's service worker: it keeps the purse's own files, so that the
// purse opens with no network. Nothing else: requests to mints go past it,
// and what a mint last said is kept by the page (src/purses.ts, `keepAnswer`),
// so that a purse with no worker reads it too.
//
// Each build is one cache, filled whole when the worker installs, and the
// page is served from it. A newer worker waits until the page says it may
// take over (src/install.ts): never in the middle of a send.
//
// A script, not a module: the build puts `const BUILD = …;` before it.

const sw = self as unknown as ServiceWorkerGlobalScope;

/** The caches of the purse's builds, by version; the page's cache of mint answers is another name. */
const PREFIX = 'purse-app-';
const CACHE = `${PREFIX}${BUILD.version}`;
const KEPT = new Set(BUILD.files);

sw.addEventListener('install', (event) => {
  event.waitUntil(fill());
});

/**
 * Fills this build's cache. The build names what it emits by content, so
 * those come from the browser's own cache when it has them; the rest are
 * checked with the server. Any file that fails fails the install, and the
 * browser tries again on a later load.
 */
async function fill(): Promise<void> {
  const cache = await caches.open(CACHE);
  try {
    await Promise.all(
      BUILD.files.map(async (path) => {
        const response = await fetch(new Request(path, { cache: path.startsWith('/assets/') ? 'default' : 'no-cache' }));
        if (!response.ok) {
          throw new Error(`${path}: ${response.status}`);
        }
        await cache.put(path, await whole(response));
      }),
    );
  } catch (error) {
    // a cache half filled would pass, in `drop`, for the build before the next one
    await caches.delete(CACHE);
    throw error;
  }
}

/** A response a navigation may be answered with: one that came by a redirect may not be. */
async function whole(response: Response): Promise<Response> {
  if (!response.redirected) {
    return response;
  }
  return new Response(await response.blob(), { status: response.status, statusText: response.statusText, headers: response.headers });
}

sw.addEventListener('activate', (event) => {
  event.waitUntil(drop());
});

/**
 * Drops all but this build's cache and the one before it, then takes the
 * open pages. A page of the previous build, still open in another tab, may
 * yet load one of its parts; the one before keeps those.
 */
async function drop(): Promise<void> {
  // caches.keys() lists the caches in the order they were made
  const builds = (await caches.keys()).filter((name) => name.startsWith(PREFIX) && name !== CACHE);
  await Promise.all(builds.slice(0, -1).map((name) => caches.delete(name)));
  await sw.clients.claim();
}

sw.addEventListener('message', (event) => {
  if (event.data === 'skip-waiting') {
    event.waitUntil(sw.skipWaiting());
  }
});

sw.addEventListener('fetch', (event) => {
  const request = event.request;
  if (request.method !== 'GET') {
    return;
  }
  const url = new URL(request.url);
  if (url.origin !== sw.location.origin) {
    return;
  }
  if (request.mode === 'navigate') {
    // the purse is one page, its views told apart by the hash, which a request never carries
    event.respondWith(kept(KEPT.has(url.pathname) ? url.pathname : '/', request));
  } else if (url.pathname.startsWith('/assets/')) {
    // this build's parts, or the build before's, for a page of it still open
    event.respondWith(fromAny(url.pathname, request));
  } else if (KEPT.has(url.pathname)) {
    event.respondWith(kept(url.pathname, request));
  }
});

/** The file at `path` as this build kept it; from the network if the cache has lost it. */
async function kept(path: string, request: Request): Promise<Response> {
  const cache = await caches.open(CACHE);
  return (await cache.match(path)) ?? fetch(request);
}

/** A part named by its content, from whichever build's cache has it. */
async function fromAny(path: string, request: Request): Promise<Response> {
  return (await caches.match(path)) ?? fetch(request);
}
