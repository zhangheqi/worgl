/// <reference types="vite/client" />

interface ImportMetaEnv {
  /**
   * A mint link (`#mint=<issuer key hex>&at=<server URL>`) for the mint the
   * first run offers as the Playground. Empty by default: the purse names no
   * playground until one is hosted.
   */
  readonly VITE_PLAYGROUND_LINK?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

/** A code found by the browser's own reader. */
interface DetectedBarcode {
  readonly rawValue: string;
}

/**
 * The browser's own code reader (Shape Detection API), where it has one:
 * not in TypeScript's lib.dom yet, so the little the purse uses is here.
 */
interface BarcodeDetector {
  detect(source: ImageBitmapSource): Promise<DetectedBarcode[]>;
}

interface BarcodeDetectorConstructor {
  new (options?: { formats?: string[] }): BarcodeDetector;
  getSupportedFormats(): Promise<string[]>;
}

// eslint-disable-next-line no-var
declare var BarcodeDetector: BarcodeDetectorConstructor | undefined;
