// Raw deflate for the purse's links (links.ts), by the browser's own
// CompressionStream where it offers `deflate-raw`, else by fflate, loaded
// only then. Only fflate's `deflateSync` and its streaming `Inflate` are
// used: its async API makes Workers from Blob URLs, which the purse's
// Trusted Types policy forbids. Inflating stops as soon as the output runs
// past its cap, so a small packed part that inflates to a great deal is never
// held whole.
//
// Only Blob, ReadableStream, CompressionStream and DecompressionStream, so
// this compiles for the browser and for Node's tests alike.

/** Whether this browser deflates `deflate-raw` itself (iOS 15.4 to 16.3 does not). */
let native: boolean | undefined;

function hasNative(): boolean {
  if (native === undefined) {
    try {
      new CompressionStream('deflate-raw');
      new DecompressionStream('deflate-raw');
      native = true;
    } catch {
      native = false;
    }
  }
  return native;
}

/** Joins chunks of `size` bytes in all. */
function join(chunks: readonly Uint8Array[], size: number): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(size);
  let at = 0;
  for (const chunk of chunks) {
    out.set(chunk, at);
    at += chunk.length;
  }
  return out;
}

/** Reads a stream to its end, refusing past `cap` bytes; `seen` hears the running size. */
async function drain(
  stream: ReadableStream<Uint8Array>,
  cap: number,
  seen?: (size: number) => void,
): Promise<Uint8Array<ArrayBuffer>> {
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) {
      return join(chunks, size);
    }
    size += value.length;
    seen?.(size);
    if (size > cap) {
      await reader.cancel().catch(() => undefined);
      throw new RangeError(`more than ${cap} bytes`);
    }
    chunks.push(value);
  }
}

/** `bytes` as a stream, from a copy, so the caller's buffer is never handed on. */
const streamOf = (bytes: Uint8Array): ReadableStream<Uint8Array<ArrayBuffer>> => new Blob([new Uint8Array(bytes)]).stream();

/** Deflates `bytes` by the browser's CompressionStream. */
export async function nativeDeflate(bytes: Uint8Array): Promise<Uint8Array<ArrayBuffer>> {
  return drain(streamOf(bytes).pipeThrough(new CompressionStream('deflate-raw')), Number.POSITIVE_INFINITY);
}

/** Inflates `bytes` by the browser's DecompressionStream, refusing past `cap` bytes with a RangeError. */
export async function nativeInflate(
  bytes: Uint8Array,
  cap: number,
  seen?: (size: number) => void,
): Promise<Uint8Array<ArrayBuffer>> {
  return drain(streamOf(bytes).pipeThrough(new DecompressionStream('deflate-raw')), cap, seen);
}

/** Deflates `bytes` by fflate, at its best compression. */
export async function fallbackDeflate(bytes: Uint8Array): Promise<Uint8Array<ArrayBuffer>> {
  const { deflateSync } = await import('fflate');
  return new Uint8Array(deflateSync(bytes, { level: 9 }));
}

/** How much of the input fflate is given at once: small, so the output of one push stays near the cap. */
const PUSH = 64;

/** Inflates `bytes` by fflate's streaming Inflate, refusing past `cap` bytes with a RangeError. */
export async function fallbackInflate(
  bytes: Uint8Array,
  cap: number,
  seen?: (size: number) => void,
): Promise<Uint8Array<ArrayBuffer>> {
  const { Inflate } = await import('fflate');
  const chunks: Uint8Array[] = [];
  let size = 0;
  let over = false;
  const inflater = new Inflate((chunk) => {
    if (over) {
      return;
    }
    size += chunk.length;
    seen?.(size);
    if (size > cap) {
      over = true;
      return;
    }
    chunks.push(chunk);
  });
  for (let at = 0; at < bytes.length && !over; at += PUSH) {
    inflater.push(bytes.subarray(at, at + PUSH), at + PUSH >= bytes.length);
  }
  if (bytes.length === 0) {
    inflater.push(bytes, true);
  }
  if (over) {
    throw new RangeError(`more than ${cap} bytes`);
  }
  return join(chunks, size);
}

/** Deflates `bytes` (raw deflate, no header): the browser's own where it can, else fflate. */
export function deflate(bytes: Uint8Array): Promise<Uint8Array<ArrayBuffer>> {
  return hasNative() ? nativeDeflate(bytes) : fallbackDeflate(bytes);
}

/** Inflates raw deflate, refusing past `cap` bytes with a RangeError; damaged input rejects too. */
export function inflate(bytes: Uint8Array, cap: number, seen?: (size: number) => void): Promise<Uint8Array<ArrayBuffer>> {
  return hasNative() ? nativeInflate(bytes, cap, seen) : fallbackInflate(bytes, cap, seen);
}
