// The purse as an app: its service worker (sw/sw.ts), which keeps its files
// so that it opens with no network, and the browser's offer to install it,
// where the browser makes one.
//
// A newer purse never takes over a page in use. Its worker waits until the
// page is out of sight and calm (nothing sending, finishing or kept alive,
// nothing typed), then takes over, and the page loads again as the newer
// purse, on the same view, before the person comes back to it.

/** The Trusted Types policy the page may register its worker by: it passes only {@link WORKER}. */
const POLICY = 'purse-worker';
const WORKER = '/sw.js';
/** How long after the last check for a newer purse a page coming back into sight checks again. */
const CHECK_EVERY = 60 * 60 * 1000;
/** How often a page out of sight looks again for a calm moment, while a newer purse waits. */
const CALM_EVERY = 15_000;

/** Chromium's install offer, held until the person asks for it. */
interface InstallOffer extends Event {
  prompt(): Promise<void>;
  readonly userChoice: Promise<{ outcome: 'accepted' | 'dismissed' }>;
}

/** The Trusted Types API, where the browser has it. */
interface TrustedTypes {
  createPolicy(name: string, rules: { createScriptURL: (url: string) => string }): { createScriptURL(url: string): unknown };
}

let registration: ServiceWorkerRegistration | undefined;
let checked = 0;
/** Whether a newer purse has taken over this page, which runs the older one until it can load again. */
let behind = false;
let calmTimer: ReturnType<typeof setInterval> | undefined;
let offer: InstallOffer | undefined;

/**
 * Registers the service worker, in a build only (the dev server serves
 * modules no worker could keep), and lets a newer purse in whenever
 * `calm` says the page may load again.
 */
export function keepOffline(calm: () => boolean): void {
  const container = navigator.serviceWorker as ServiceWorkerContainer | undefined;
  if (!import.meta.env.PROD || container === undefined) {
    return;
  }
  // the first worker takes a page that had none: the page is that purse already
  let controlled = container.controller !== null;
  container.addEventListener('controllerchange', () => {
    if (!controlled) {
      controlled = true;
      return;
    }
    behind = true;
    settle(calm);
  });
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      settle(calm);
      return;
    }
    clearInterval(calmTimer);
    calmTimer = undefined;
    if (registration !== undefined && Date.now() - checked > CHECK_EVERY) {
      checked = Date.now();
      void registration.update().catch(() => undefined);
    }
  });
  const register = (): void => {
    let url: unknown;
    try {
      url = workerUrl();
    } catch {
      // a policy that will not pass the worker's URL: the purse works, but not offline
      return;
    }
    checked = Date.now();
    container
      .register(url as string, { scope: '/' })
      .then((each) => {
        registration = each;
        each.addEventListener('updatefound', () => {
          const installing = each.installing;
          installing?.addEventListener('statechange', () => {
            if (installing.state === 'installed') {
              settle(calm);
            }
          });
        });
      })
      .catch(() => {
        // no worker (a private window, storage refused): the purse works, but not offline
      });
  };
  // after the first paint's own requests: the worker fetches every file the purse has
  if (document.readyState === 'complete') {
    register();
  } else {
    window.addEventListener('load', register, { once: true });
  }
}

/**
 * While the page is out of sight and calm: loads it again if a newer purse
 * has taken it over, or lets a newer purse that waits take over. When it
 * is not calm, looks again every {@link CALM_EVERY} until it is, or until
 * the page is in sight again.
 */
function settle(calm: () => boolean): void {
  if (!document.hidden || (!behind && registration?.waiting == null)) {
    return;
  }
  if (!calm()) {
    calmTimer ??= setInterval(() => settle(calm), CALM_EVERY);
    return;
  }
  clearInterval(calmTimer);
  calmTimer = undefined;
  if (behind) {
    location.reload();
  } else {
    registration?.waiting?.postMessage('skip-waiting');
  }
}

/** {@link WORKER}, as Trusted Types requires a script's URL to be given (public/_headers). */
function workerUrl(): unknown {
  const types = (globalThis as { trustedTypes?: TrustedTypes }).trustedTypes;
  if (types === undefined) {
    return WORKER;
  }
  return types
    .createPolicy(POLICY, {
      createScriptURL: (url) => {
        if (url !== WORKER) {
          throw new TypeError(`the purse registers only ${WORKER}`);
        }
        return url;
      },
    })
    .createScriptURL(WORKER);
}

/**
 * Holds the browser's install offer, for {@link install}, and calls
 * `changed` whenever whether there is one changes; `installed` once the
 * purse is installed.
 */
export function holdOffer(changed: () => void, installed: () => void): void {
  window.addEventListener('beforeinstallprompt', (event) => {
    // the browser's own banner would come unasked; the purse offers it where its storage is spoken of
    event.preventDefault();
    offer = event as InstallOffer;
    changed();
  });
  window.addEventListener('appinstalled', () => {
    offer = undefined;
    changed();
    installed();
  });
}

/** Whether the browser has offered to install the purse, and the offer is not spent. */
export const canInstall = (): boolean => offer !== undefined;

/** Shows the browser's install offer: once, as the browser allows; it may make another later. */
export async function install(): Promise<void> {
  const held = offer;
  if (held === undefined) {
    return;
  }
  offer = undefined;
  try {
    await held.prompt();
    await held.userChoice;
  } catch {
    // the offer was spent or withdrawn: the purse simply stops offering it
  }
}
