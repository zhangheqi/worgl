// The Rust core, compiled to WebAssembly. Importing this module loads it:
// from disk under Node, through fetch in a browser or bundler. A call that
// fails throws a WorglError (see error.ts).

import init, { initSync } from '../wasm/worgl_wasm.js';

const wasm = new URL('../wasm/worgl_wasm_bg.wasm', import.meta.url);
const fs = (
  globalThis as { process?: { getBuiltinModule?: (id: string) => unknown } }
).process?.getBuiltinModule?.('node:fs') as typeof import('node:fs') | undefined;

if (wasm.protocol === 'file:' && fs !== undefined) {
  initSync({ module: fs.readFileSync(wasm) });
} else {
  await init({ module_or_path: wasm });
}

export {
  Holder,
  breaksPolicy,
  checkPayment,
  mintSummary,
  noteIssuer,
  noteReport,
  paymentMint,
  paymentTransfers,
  requestAmount,
  requestMint,
  requestNotes,
  transferMint,
  verifyNotes,
} from '../wasm/worgl_wasm.js';
export type { MadePayment, Scheduled } from '../wasm/worgl_wasm.js';
