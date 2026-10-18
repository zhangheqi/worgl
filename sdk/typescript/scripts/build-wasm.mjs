// Compiles the Rust core (crates/worgl-wasm) to WebAssembly and writes its
// JavaScript glue to wasm/. Needs the wasm32-unknown-unknown target and the
// wasm-bindgen CLI at the version Cargo.lock pins.

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const out = fileURLToPath(new URL('../wasm/', import.meta.url));

const lock = readFileSync(`${root}Cargo.lock`, 'utf8');
const pinned = /name = "wasm-bindgen"\nversion = "([^"]+)"/.exec(lock)?.[1];
let installed;
try {
  installed = execFileSync('wasm-bindgen', ['--version'], { encoding: 'utf8' }).trim().split(' ')[1];
} catch {
  installed = undefined;
}
if (installed !== pinned) {
  console.error(
    `wasm-bindgen ${pinned} is needed (found ${installed ?? 'none'}): ` +
      `cargo install wasm-bindgen-cli --version ${pinned} --locked`,
  );
  process.exit(1);
}

const run = (command, args) => execFileSync(command, args, { cwd: root, stdio: 'inherit' });
run('cargo', [
  'build',
  '--package', 'worgl-wasm',
  '--target', 'wasm32-unknown-unknown',
  '--release',
  '--locked',
]);
run('wasm-bindgen', [
  '--target', 'web',
  '--out-dir', out,
  `${root}target/wasm32-unknown-unknown/release/worgl_wasm.wasm`,
]);
