import { mintSummary, noteReport } from './core.ts';

/** How a {@link Client} sends its requests. */
export interface ClientOptions {
  /** The fetch to use; the global one by default. */
  fetch?: typeof globalThis.fetch;
  /** How long a request may take, in milliseconds; 30 seconds by default. */
  timeoutMs?: number;
}

/** A mint's info, in numbers that keep their precision. */
export interface MintInfo {
  /** The mint's issuer key, in hex. */
  issuer: string;
  /** Events the mint has applied since it was created. */
  sequence: bigint;
  /** Number of layers. */
  layers: number;
  /** Generation of the first layer: rotations so far. */
  oldest: bigint;
  /** Whether the mint is paused: it then refuses transfers and rotations. */
  paused: boolean;
  /** When it rotates on its own; `undefined` when only on request. */
  rotation: { periodSecs: bigint; nextAt: bigint } | undefined;
  /** Value minted, destroyed by rotations, burned and outstanding. */
  supply: { minted: bigint; destroyed: bigint; burned: bigint; outstanding: bigint };
}

/** What a mint says about one note. */
export interface NoteState {
  /** `live`, `spent`, `expired` or `unissued`. */
  state: string;
  /** The note's value. */
  amount: bigint;
  /** Its serial. */
  serial: { generation: bigint; index: number };
  /** For a live note: its layer, hops left before the next rotation, and
   * rotations until it expires untouched. */
  live: { position: number; hopsLeft: number; expiresAfterRotations: number } | undefined;
}

/** What a playground faucet hands out. */
export interface FaucetInfo {
  /** The mint its notes come from, in hex. */
  mint: string;
  /** The value of each note. */
  amount: bigint;
  /** Notes one client address may get in any hour. */
  perAddressPerHour: number;
  /** Notes the faucet hands out in a day, UTC. */
  perDay: number;
  /** Notes left to hand out today. */
  leftToday: number;
}

/** An error answer from a mint. */
export class MintError extends Error {
  /** The HTTP status. */
  readonly status: number;
  /**
   * A stable code: `spent`, `expired`, `frozen`, `paused`, `policy`,
   * `unknown_mint` and so on, as the server documents them. `http` when the
   * answer carried none, such as from a proxy.
   */
  readonly code: string;
  /**
   * How long to wait before sending the request again, in milliseconds,
   * from the answer's `Retry-After`. A `busy` mint sends one; it took
   * nothing, and has room again after that.
   */
  readonly retryAfterMs: number | undefined;

  constructor(status: number, code: string, message: string, retryAfterMs?: number) {
    super(`${message} (${code}, HTTP ${status})`);
    this.name = 'MintError';
    this.status = status;
    this.code = code;
    this.retryAfterMs = retryAfterMs;
  }
}

/**
 * Whether trying again later may succeed: the request did not complete, or
 * the mint was paused, busy or failed to store the change. Transfers are
 * safe to resend.
 */
export function isTransient(error: unknown): boolean {
  if (error instanceof MintError) {
    return (
      ['paused', 'busy', 'storage', 'internal', 'layer_full'].includes(error.code) ||
      error.status >= 500
    );
  }
  // fetch rejects with a TypeError when the request never completed, and
  // with a DOMException when it timed out.
  return error instanceof TypeError || error instanceof DOMException;
}

/**
 * A connection to one Worgl server, which may host many mints.
 *
 * Every call is a single request with no retry. Protocol objects travel as
 * JSON text, as the Rust core makes them, so no amount loses precision.
 */
export class Client {
  /** The server URL requests go to. */
  readonly baseUrl: string;
  readonly #fetch: typeof globalThis.fetch;
  readonly #timeoutMs: number;

  /** A client for the server at `baseUrl`, such as `https://mint.example.com`. */
  constructor(baseUrl: string, options: ClientOptions = {}) {
    const url = new URL(baseUrl);
    if (url.protocol !== 'http:' && url.protocol !== 'https:') {
      throw new TypeError(`${baseUrl} is not an http or https URL`);
    }
    this.baseUrl = url.href.replace(/\/+$/, '');
    const fetch = options.fetch ?? globalThis.fetch;
    this.#fetch = (input, init) => fetch(input, init);
    this.#timeoutMs = options.timeoutMs ?? 30_000;
  }

  /** `GET /v1/mints/{mint}/info`, as the server wrote it. */
  infoText(mint: string): Promise<string> {
    return this.#send('GET', `/v1/mints/${mint}/info`);
  }

  /**
   * {@link infoText}, with the time the server answered: Unix seconds by its
   * clock, from the answer's `Date` header, or by this machine's if the
   * answer has no `Date` it can read. A browser reads the `Date` of an
   * answer from another origin only if the answer carries
   * `Access-Control-Expose-Headers: Date`. Plan the transfers due before the
   * mint's next rotation from it, as `Purse.planRefresh` does: the mint
   * rotates by its server's clock, and planning by this machine's, if it
   * runs slow, can put them past the rotation.
   */
  async infoTextAndTime(mint: string): Promise<{ info: string; time: bigint }> {
    const { text, date } = await this.#answer('GET', `/v1/mints/${mint}/info`);
    const answered = date === null ? Number.NaN : Date.parse(date);
    const ms = Number.isNaN(answered) ? Date.now() : answered;
    return { info: text, time: BigInt(Math.floor(ms / 1000)) };
  }

  /** `GET /v1/mints/{mint}/info`, read. */
  async info(mint: string): Promise<MintInfo> {
    const summary = mintSummary(await this.infoText(mint));
    try {
      const periodSecs = summary.periodSecs;
      const nextAt = summary.nextAt;
      return {
        issuer: summary.issuer,
        sequence: summary.sequence,
        layers: summary.layers,
        oldest: summary.oldest,
        paused: summary.paused,
        rotation:
          periodSecs === undefined || nextAt === undefined ? undefined : { periodSecs, nextAt },
        supply: {
          minted: summary.minted,
          destroyed: summary.destroyed,
          burned: summary.burned,
          outstanding: summary.outstanding,
        },
      };
    } finally {
      summary.free();
    }
  }

  /** `GET /v1/mints/{mint}/keys`: the keys that sign its notes, as JSON. */
  keysText(mint: string): Promise<string> {
    return this.#send('GET', `/v1/mints/${mint}/keys`);
  }

  /** `POST /v1/verify`: what the note's mint says about it. */
  async verify(note: string): Promise<NoteState> {
    const report = noteReport(await this.#send('POST', '/v1/verify', JSON.stringify({ note })));
    try {
      const { position, hopsLeft, expiresAfterRotations } = report;
      return {
        state: report.state,
        amount: report.amount,
        serial: { generation: report.generation, index: report.index },
        live:
          position === undefined || hopsLeft === undefined || expiresAfterRotations === undefined
            ? undefined
            : { position, hopsLeft, expiresAfterRotations },
      };
    } finally {
      report.free();
    }
  }

  /** `POST /v1/transfer`: the new notes, in the order of the outputs. */
  async transfer(transfer: string): Promise<string[]> {
    const answer = JSON.parse(await this.#send('POST', '/v1/transfer', transfer)) as {
      notes: string[];
    };
    return answer.notes;
  }

  /** `POST /v1/mints/{mint}/mint`, for a request signed by a mint authority. */
  async mint(mint: string, request: string): Promise<string> {
    const answer = JSON.parse(await this.#send('POST', `/v1/mints/${mint}/mint`, request)) as {
      note: string;
    };
    return answer.note;
  }

  /**
   * `GET /v1/faucet`, from a playground faucet: its mint and limits. The
   * faucet is `worgl-faucet`, at this client's URL, which may be its mint's
   * server with the faucet behind the same proxy.
   */
  async faucet(): Promise<FaucetInfo> {
    const text = await this.#send('GET', '/v1/faucet');
    const about = JSON.parse(text) as {
      mint: string;
      per_address_per_hour: number;
      per_day: number;
      left_today: number;
    };
    // JSON.parse would round an amount above 2^53; read its digits.
    const amount = /"amount"\s*:\s*(\d+)/.exec(text)?.[1];
    if (amount === undefined) {
      throw new MintError(200, 'http', `the faucet's answer has no amount: ${text.slice(0, 512)}`);
    }
    return {
      mint: about.mint,
      amount: BigInt(amount),
      perAddressPerHour: about.per_address_per_hour,
      perDay: about.per_day,
      leftToday: about.left_today,
    };
  }

  /**
   * `POST /v1/faucet`: a note of the faucet's mint, bound to `holder`. Past
   * the faucet's limits it rejects with a {@link MintError} of status 429 and
   * code `rate_limited` or `exhausted`.
   */
  async askFaucet(holder: string): Promise<string> {
    const answer = JSON.parse(
      await this.#send('POST', '/v1/faucet', JSON.stringify({ holder })),
    ) as { note: string };
    return answer.note;
  }

  /** `POST /v1/mints/{mint}/rotate`, with the mint's admin token. */
  async rotate(mint: string, token: string): Promise<void> {
    await this.#send('POST', `/v1/mints/${mint}/rotate`, undefined, token);
  }

  /** `POST /v1/mints/{mint}/pause`, with the mint's admin token. */
  async pause(mint: string, token: string): Promise<void> {
    await this.#send('POST', `/v1/mints/${mint}/pause`, undefined, token);
  }

  /** `POST /v1/mints/{mint}/resume`, with the mint's admin token. */
  async resume(mint: string, token: string): Promise<void> {
    await this.#send('POST', `/v1/mints/${mint}/resume`, undefined, token);
  }

  async #send(method: string, path: string, body?: string, token?: string): Promise<string> {
    return (await this.#answer(method, path, body, token)).text;
  }

  /** Sends a request and returns the answer and its `Date` header, or rejects with the refusal. */
  async #answer(
    method: string,
    path: string,
    body?: string,
    token?: string,
  ): Promise<{ text: string; date: string | null }> {
    const headers: Record<string, string> = {};
    if (body !== undefined) {
      headers['content-type'] = 'application/json';
    }
    if (token !== undefined) {
      headers.authorization = `Bearer ${token}`;
    }
    const init: RequestInit = {
      method,
      headers,
      signal: AbortSignal.timeout(this.#timeoutMs),
    };
    if (body !== undefined) {
      init.body = body;
    }
    const response = await this.#fetch(`${this.baseUrl}${path}`, init);
    const text = await response.text();
    if (response.ok) {
      return { text, date: response.headers.get('date') };
    }
    let code = 'http';
    let message = text.trim().slice(0, 512);
    try {
      const parsed = JSON.parse(text) as { error?: unknown; message?: unknown };
      if (typeof parsed.error === 'string') {
        code = parsed.error;
        message = String(parsed.message ?? '');
      }
    } catch {
      // Not JSON: keep the text.
    }
    // In seconds; the server never sends the date form.
    const retryAfter = response.headers.get('retry-after')?.trim();
    const retryAfterMs =
      retryAfter !== undefined && /^\d+$/.test(retryAfter) ? Number(retryAfter) * 1000 : undefined;
    throw new MintError(response.status, code, message, retryAfterMs);
  }
}
