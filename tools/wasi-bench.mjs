// SPDX-License-Identifier: GPL-3.0-or-later
// Run the wasm32-wasip1 copperline-bench build under Node's WASI:
//   node tools/wasi-bench.mjs target/wasm32-wasip1/release/copperline-bench.wasm \
//     --rom /work/assets/aros/aros-amiga-m68k-rom.bin --seconds 30
// The current directory is preopened as /work, so pass /work-prefixed paths.
// Node's V8 wasm tiers are what Chrome uses, so these numbers are the closest
// host-side proxy for browser performance.
import { readFile } from 'node:fs/promises';
import { WASI } from 'node:wasi';

const [wasmPath, ...rest] = process.argv.slice(2);
if (!wasmPath) {
  console.error('usage: node tools/wasi-bench.mjs <bench.wasm> [bench args...]');
  process.exit(2);
}
const wasi = new WASI({
  version: 'preview1',
  args: [wasmPath, ...rest],
  env: {},
  preopens: { '/work': process.cwd() },
  returnOnExit: true,
});
const mod = await WebAssembly.compile(await readFile(wasmPath));
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
process.exitCode = wasi.start(inst);
