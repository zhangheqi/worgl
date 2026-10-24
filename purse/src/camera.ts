// The camera, for scanning a request on Send or a hand-back on Receive.
// It starts only when the person asks, reads a frame every 150 ms (the
// browser's own BarcodeDetector where it reads QR codes, else jsQR, loaded
// then), and stops the moment it is not needed: the light goes off.
//
// One <video> is made once and moved into each new drawing of the
// scanner, so that a render neither stops the stream nor asks again. The
// reader runs on the main thread: Trusted Types forbids Workers.

import { h } from './dom.ts';
import { type Camera, cameraWords } from './flow-words.ts';

export { type Camera, cameraWords };

/** How often a frame is read. */
const EVERY_MS = 150;

/** How long the same text is ignored, so a code held still does not flash its words again. */
const SAME_MS = 2000;

/** The longer side of the frame jsQR reads. */
const READ_SIDE = 720;

type Reader = (video: HTMLVideoElement) => Promise<string | undefined>;

let video: HTMLVideoElement | undefined;
let stream: MediaStream | undefined;
let timer: ReturnType<typeof setTimeout> | undefined;
/** Counts starts and stops, so a start that resolves after a stop lets its stream go. */
let run = 0;
let last: { text: string; at: number } | undefined;

/** Whether this browser offers a camera here: a secure page with getUserMedia. */
export function cameraSupported(): boolean {
  return globalThis.isSecureContext === true && typeof navigator.mediaDevices?.getUserMedia === 'function';
}

/** Whether the camera runs now. */
export const cameraRunning = (): boolean => stream !== undefined;

/** The browser's own reader, when it reads QR codes. */
async function nativeReader(): Promise<Reader | undefined> {
  const Detector = globalThis.BarcodeDetector;
  if (Detector === undefined) {
    return undefined;
  }
  try {
    if (!(await Detector.getSupportedFormats()).includes('qr_code')) {
      return undefined;
    }
    const detector = new Detector({ formats: ['qr_code'] });
    return async (source) => (await detector.detect(source))[0]?.rawValue;
  } catch {
    return undefined;
  }
}

/** jsQR, on a frame drawn to a canvas off the page, light codes on dark only (the stamps print so in both themes). */
async function jsqrReader(): Promise<Reader> {
  const loaded = (await import('jsqr')) as unknown as { default: typeof import('jsqr').default | { default: typeof import('jsqr').default } };
  const outer = loaded.default;
  const jsQR = typeof outer === 'function' ? outer : outer.default;
  const canvas = document.createElement('canvas');
  const context = canvas.getContext('2d', { willReadFrequently: true });
  if (context === null) {
    throw new Error('no 2d canvas');
  }
  return async (source) => {
    const vw = source.videoWidth;
    const vh = source.videoHeight;
    if (vw === 0 || vh === 0) {
      return undefined;
    }
    const scale = Math.min(1, READ_SIDE / Math.max(vw, vh));
    const w = Math.round(vw * scale);
    const hh = Math.round(vh * scale);
    if (canvas.width !== w || canvas.height !== hh) {
      canvas.width = w;
      canvas.height = hh;
    }
    context.drawImage(source, 0, 0, w, hh);
    const image = context.getImageData(0, 0, w, hh);
    return jsQR(image.data, w, hh, { inversionAttempts: 'dontInvert' })?.data;
  };
}

/** The camera's name for a refusal: the DOMException's, or the error's. */
const refusal = (error: unknown): string =>
  error instanceof DOMException || error instanceof Error ? error.name : 'Error';

/**
 * Starts the camera and reads codes from it: `onText` hears each text read
 * (the same one again only after two seconds), `onState` where the camera
 * is. Asks for the camera, so call it only on the person's tap.
 */
export async function startCamera(onText: (text: string) => void, onState: (state: Camera) => void): Promise<void> {
  stopCamera();
  if (!cameraSupported()) {
    onState({ kind: 'unsupported' });
    return;
  }
  const mine = ++run;
  onState({ kind: 'starting' });
  let got: MediaStream;
  try {
    got = await navigator.mediaDevices.getUserMedia({
      video: { facingMode: { ideal: 'environment' }, width: { ideal: 1280 }, height: { ideal: 720 } },
      audio: false,
    });
  } catch (error) {
    if (mine === run) {
      onState({ kind: 'refused', name: refusal(error) });
    }
    return;
  }
  if (mine !== run) {
    got.getTracks().forEach((track) => track.stop());
    return;
  }
  let reader: Reader;
  try {
    reader = (await nativeReader()) ?? (await jsqrReader());
  } catch {
    got.getTracks().forEach((track) => track.stop());
    if (mine === run) {
      onState({ kind: 'no-reader' });
    }
    return;
  }
  if (mine !== run) {
    got.getTracks().forEach((track) => track.stop());
    return;
  }
  stream = got;
  video ??= h('video', { class: 'cam', 'aria-hidden': 'true', playsinline: '', muted: '' });
  video.muted = true;
  video.playsInline = true;
  video.srcObject = got;
  try {
    await video.play();
  } catch {
    // shown in the scanner once it is in the page (attachCamera plays it again)
  }
  if (mine !== run) {
    return;
  }
  onState({ kind: 'running' });
  last = undefined;
  const tick = async (): Promise<void> => {
    if (mine !== run || video === undefined) {
      return;
    }
    if (!document.hidden && video.readyState >= 2) {
      let text: string | undefined;
      try {
        text = await reader(video);
      } catch {
        text = undefined;
      }
      if (mine !== run) {
        return;
      }
      const now = Date.now();
      if (text !== undefined && text !== '' && (last === undefined || last.text !== text || now - last.at > SAME_MS)) {
        last = { text, at: now };
        onText(text);
        if (mine !== run) {
          return;
        }
      }
    }
    timer = setTimeout(() => void tick(), EVERY_MS);
  };
  timer = setTimeout(() => void tick(), EVERY_MS);
}

/** Stops the camera: its tracks, the stream on the video, and the reads. The light goes off. */
export function stopCamera(): void {
  run++;
  clearTimeout(timer);
  timer = undefined;
  stream?.getTracks().forEach((track) => track.stop());
  stream = undefined;
  if (video !== undefined) {
    video.pause();
    video.srcObject = null;
    video.remove();
  }
  last = undefined;
}

/** Puts the camera's one video into a scanner just drawn, and plays it again. */
export function attachCamera(box: HTMLElement): void {
  if (video === undefined || stream === undefined) {
    return;
  }
  if (video.parentElement !== box) {
    box.prepend(video);
  }
  void video.play().catch(() => undefined);
}
