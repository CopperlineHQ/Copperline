#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Boot a page packed by tools/publish-web without a browser.
//
//   node tools/check-web-kit.mjs PAGE.html [--seconds N] [--min-colours N]
//
// The page's own inlined loader unpacks the page's own blocks, so this
// exercises exactly what a visitor's browser would: the config, the base64
// + zlib blocks, the module linking, the wasm-bindgen glue and binary pair,
// the ROM and the disk. The machine is then built the way kit-player.js
// builds it and run on a synthetic clock for N emulated seconds (default
// 20), after which the picture must hold at least --min-colours distinct
// colours (default 2). No display, audio device or network is involved.
// What it cannot cover is the page shell itself (DOM, canvas, audio
// worklet, Blob URL imports); those need a browser.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

const args = process.argv.slice(2);
const option = (name, fallback) => {
  const at = args.indexOf(name);
  if (at < 0) return fallback;
  const value = Number(args[at + 1]);
  assert.ok(Number.isFinite(value) && value >= 0, `${name} needs a number`);
  args.splice(at, 2);
  return value;
};
const seconds = option('--seconds', 20);
const minColours = option('--min-colours', 2);
assert.equal(args.length, 1, 'usage: check-web-kit.mjs PAGE.html [--seconds N] [--min-colours N]');
const page = await readFile(args[0], 'utf8');

// A document just deep enough for the loader: the script elements, with
// the attribute and text access it uses.
const scripts = [];
for (const match of page.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/g)) {
  const attrs = new Map();
  for (const attr of match[1].matchAll(/([\w-]+)(?:="([^"]*)")?/g)) attrs.set(attr[1], attr[2] ?? '');
  scripts.push({
    textContent: match[2],
    removed: false,
    getAttribute: (name) => attrs.get(name) ?? null,
    hasAttribute: (name) => attrs.has(name),
    remove() {
      this.removed = true;
    },
  });
}
const doc = {
  getElementById: (id) => scripts.find((s) => s.getAttribute('id') === id) ?? null,
  querySelectorAll: (selector) => {
    assert.equal(selector, 'script[data-cl-asset]');
    return scripts.filter((s) => s.hasAttribute('data-cl-asset'));
  },
};

const inline = scripts.filter((s) => !s.hasAttribute('type') && !s.hasAttribute('src'));
assert.equal(inline.length, 1, 'the page must carry exactly one inline script: the loader');
vm.runInThisContext(inline[0].textContent, { filename: 'kit-loader.js (inlined)' });
const kit = await globalThis.CopperlineKit.unpack(doc);
const { config } = kit;

// Everything the shell will ask for is there.
const blobText = async (url) => (await fetch(url)).text();
assert.equal(config.modules.order.at(-1), config.modules.entry);
for (const name of config.modules.order) {
  assert.match(kit.moduleUrls[name] ?? '', /^blob:/, `module ${name} was not linked`);
}
const entry = await blobText(kit.moduleUrls[config.modules.entry]);
assert.match(entry, /export async function start\(/, 'the entry module has no start()');
assert.equal(/from\s*['"]\.\.?\//.test(entry), false, 'the entry module kept a relative import');
assert.match(kit.workletUrls[0] ?? '', /^blob:/, 'the audio worklet is missing');
assert.match(await blobText(kit.workletUrls[0]), /registerProcessor\(/);
for (const item of config.notices) {
  assert.ok((await kit.readLazy(item.asset)).length > 0, `${item.title} is empty`);
}
assert.ok(config.disks.length > 0, 'the page carries no disk');

// Node's loader does not import blob: URLs, so the (import-free) glue is
// read back and imported as a data: URL instead.
const glue = await blobText(kit.moduleUrls['./pkg/copperline_web.js']);
const { default: init, WebEmu } = await import(
  `data:text/javascript;base64,${Buffer.from(glue).toString('base64')}`
);
const wasm = await init({ module_or_path: kit.assets.get(config.wasm) });

const drives = config.machine.drives;
const emu = new WebEmu(config.machine.model, config.machine.video ?? undefined, drives);
emu.load_rom(
  kit.assets.get(config.rom.main),
  config.rom.ext ? kit.assets.get(config.rom.ext) : undefined,
);
for (let drive = 0; drive < Math.min(drives, config.disks.length); drive++) {
  const disk = config.disks[drive];
  const bytes = kit.assets.get(disk.asset);
  try {
    emu.insert_floppy_writable(drive, bytes, disk.name);
    assert.deepEqual(emu.export_floppy(drive), bytes, `${disk.name} did not export as inserted`);
  } catch (e) {
    if (e instanceof assert.AssertionError) throw e;
    emu.insert_floppy(drive, bytes, disk.name); // read-only image format
  }
  assert.equal(emu.disk_name(drive), disk.name);
}

let now = 0;
while (emu.emulated_seconds() < seconds) {
  now += 20;
  emu.run(now, 8);
  emu.take_audio();
}
const rows = emu.present_rows();
const width = emu.present_width();
assert.ok(rows > 0 && width > 0, 'the machine presented no picture');
const pixels = new Uint32Array(wasm.memory.buffer, emu.present_ptr(), width * rows);
const colours = new Set(pixels);
assert.ok(
  colours.size >= minColours,
  `the picture holds ${colours.size} colour(s) after ${seconds}s; expected at least ${minColours}`,
);
console.log(
  `web kit page boots: ${config.game.title} on ${emu.machine_summary()}; ` +
    `${emu.emulated_seconds().toFixed(1)}s emulated, ${width}x${rows}, ${colours.size} colours`,
);
