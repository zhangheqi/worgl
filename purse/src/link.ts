// Mint links: `#mint=<issuer key hex>&at=<server URL>`, as an operator
// publishes them. The part after `#` never reaches whoever hosts the purse.

/** A mint, as a link names it: its issuer key and the server that hosts it. */
export interface MintLink {
  /** The issuer key, 64 lowercase hex digits: the mint's real name. */
  mint: string;
  /** The server's URL, as the SDK's `Client` writes it: no trailing slash. */
  server: string;
}

/**
 * Why a text is not a mint link the purse can use:
 *
 * - `not_a_link`: it names no mint and server.
 * - `key`: the key is not 64 hex digits.
 * - `server`: the server is not an http or https URL, or carries a user,
 *   a query or a fragment, which a server URL never needs. An empty `?` or
 *   `#` at its end is dropped.
 * - `insecure`: plain http to another machine, which a page served over
 *   https may not call; only this machine (`localhost`, `127.0.0.1`,
 *   `[::1]`) is reached without https.
 */
export type LinkProblem = 'not_a_link' | 'key' | 'server' | 'insecure';

/** What {@link parseMintLink} makes of a text. */
export type ParsedLink = { ok: true; link: MintLink } | { ok: false; problem: LinkProblem };

/**
 * Reads a mint link from what a person pasted or opened: a whole URL with
 * the link after `#`, or the part after `#` alone. Spaces around it are
 * ignored, and the key is taken in either case.
 */
export function parseMintLink(text: string): ParsedLink {
  const trimmed = text.trim();
  const hash = trimmed.indexOf('#');
  const params = new URLSearchParams(hash >= 0 ? trimmed.slice(hash + 1) : trimmed);
  const mint = params.get('mint')?.trim();
  const at = params.get('at')?.trim();
  if (mint === undefined || at === undefined || mint === '' || at === '') {
    return { ok: false, problem: 'not_a_link' };
  }
  if (!/^[0-9a-fA-F]{64}$/.test(mint)) {
    return { ok: false, problem: 'key' };
  }
  const server = parseServer(at);
  return server.ok ? { ok: true, link: { mint: mint.toLowerCase(), server: server.server } } : server;
}

/** Why a text is not a server URL the purse can use: see {@link LinkProblem}'s `server` and `insecure`. */
export type ServerProblem = 'server' | 'insecure';

/**
 * Reads a server's URL, as a mint link or a request link names it: http or
 * https, with no user, query or fragment, and plain http only to this
 * machine. Its trailing slash is dropped, as the SDK's `Client` writes it.
 */
export function parseServer(text: string): { ok: true; server: string } | { ok: false; problem: ServerProblem } {
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return { ok: false, problem: 'server' };
  }
  if (
    (url.protocol !== 'https:' && url.protocol !== 'http:') ||
    url.username !== '' ||
    url.password !== '' ||
    url.search !== '' ||
    url.hash !== ''
  ) {
    return { ok: false, problem: 'server' };
  }
  if (url.protocol === 'http:' && !isLoopback(url.hostname)) {
    return { ok: false, problem: 'insecure' };
  }
  // from its parts, so that an empty `?` or `#`, which `search` and `hash` do not show, is left out too
  return { ok: true, server: `${url.origin}${url.pathname}`.replace(/\/+$/, '') };
}

/** Whether a location's fragment is meant as a mint link, whether or not it is a good one. */
export function isMintHash(hash: string): boolean {
  return new URLSearchParams(hash.replace(/^#/, '')).has('mint');
}

/** The fragment that names `link`: `#mint=…&at=…`, the server's URL escaped. */
export function mintHash(link: MintLink): string {
  return `#mint=${link.mint}&at=${encodeURIComponent(link.server)}`;
}

/** The host a person recognizes a server by: `mint.example.org`, with a port if it has one. */
export function hostOf(server: string): string {
  try {
    return new URL(server).host;
  } catch {
    return server;
  }
}

/** The key's first eight digits, which name a mint wherever the purse shows it. */
export const shortKey = (mint: string): string => mint.slice(0, 8);

/** Whether a host name is this machine, which a browser trusts without https. */
function isLoopback(hostname: string): boolean {
  return (
    hostname === 'localhost' ||
    hostname.endsWith('.localhost') ||
    hostname === '[::1]' ||
    /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(hostname)
  );
}
