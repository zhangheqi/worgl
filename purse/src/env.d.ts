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
