// SPDX-License-Identifier: GPL-3.0-or-later

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import zlib from 'node:zlib';

// The loader is a classic script; with no `document` it only publishes its
// functions, which is how tools/check-web-kit.mjs drives it too.
vm.runInThisContext(readFileSync(new URL('./kit-loader.js', import.meta.url), 'utf8'), {
  filename: 'kit-loader.js',
});
const kitLoader = globalThis.CopperlineKit;

// Node's module loader does not import blob: URLs (browsers do), so the
// tests read a linked module back as text, and run one by re-encoding it
// as a data: URL when it has no imports of its own.
const blobText = async (url) => (await fetch(url)).text();
const importText = (text) =>
  import(`data:text/javascript;base64,${Buffer.from(text).toString('base64')}`);

const pack = (bytes) => zlib.deflateSync(Buffer.from(bytes), { level: 9 }).toString('base64');

function block(name, kind, bytes, { lazy = false, size = bytes.length, text } = {}) {
  const attrs = { 'data-cl-asset': name, 'data-cl-kind': kind, 'data-cl-size': String(size) };
  if (lazy) attrs['data-cl-lazy'] = '';
  return {
    removed: false,
    textContent: text ?? pack(bytes),
    getAttribute: (key) => attrs[key] ?? null,
    hasAttribute: (key) => key in attrs,
    remove() {
      this.removed = true;
    },
  };
}

function fakeDoc(config, blocks) {
  return {
    getElementById: (id) => (id === 'cl-config' ? { textContent: JSON.stringify(config) } : null),
    querySelectorAll: () => blocks,
  };
}

const enc = (text) => new TextEncoder().encode(text);

test('base64 decodes with or without the native decoder, ignoring line breaks', () => {
  const bytes = Uint8Array.from({ length: 5000 }, (_, i) => (i * 37) & 0xff);
  const text = Buffer.from(bytes).toString('base64').replace(/(.{76})/g, '$1\n');
  assert.deepEqual(kitLoader.base64ToBytes(text), bytes);
  const native = Uint8Array.fromBase64;
  try {
    Uint8Array.fromBase64 = undefined;
    assert.deepEqual(kitLoader.base64ToBytes(text), bytes);
    for (const length of [0, 1, 2, 3, 4]) {
      const short = bytes.slice(0, length);
      assert.deepEqual(kitLoader.base64ToBytes(Buffer.from(short).toString('base64')), short);
    }
    assert.throws(() => kitLoader.base64ToBytes('abc'), /truncated/);
  } finally {
    Uint8Array.fromBase64 = native;
  }
});

test('modules link through Blob URLs in dependency order', async () => {
  const sources = {
    './pkg/glue.js': 'export default function init() { return 40; }',
    './dep.js': "import init from './pkg/glue.js';\nexport const answer = init() + 2;",
    './entry.js':
      "import { answer } from \"./dep.js\";\nexport { answer };\n" +
      "export const lazy = () => import('./dep.js');",
  };
  const urls = kitLoader.linkModules(sources, ['./pkg/glue.js', './dep.js', './entry.js']);
  assert.deepEqual(Object.keys(urls), ['./pkg/glue.js', './dep.js', './entry.js']);
  for (const url of Object.values(urls)) assert.match(url, /^blob:/);
  // A module with no imports is stored untouched.
  assert.equal(await blobText(urls['./pkg/glue.js']), sources['./pkg/glue.js']);
  assert.equal((await importText(await blobText(urls['./pkg/glue.js']))).default(), 40);
  // Every form of relative specifier now names the Blob URL of its target,
  // keeping its own quote style, and nothing else changed.
  assert.equal(
    await blobText(urls['./dep.js']),
    `import init from '${urls['./pkg/glue.js']}';\nexport const answer = init() + 2;`,
  );
  assert.equal(
    await blobText(urls['./entry.js']),
    `import { answer } from "${urls['./dep.js']}";\nexport { answer };\n` +
      `export const lazy = () => import('${urls['./dep.js']}');`,
  );
});

test('only relative specifiers are rewritten', async () => {
  const source =
    "import fs from 'node:fs';\n" +
    "const wasm = new URL('copperline_web_bg.wasm', import.meta.url);\n" +
    "const text = 'from here on';\n";
  const urls = kitLoader.linkModules({ './glue.js': source }, ['./glue.js']);
  assert.equal(await blobText(urls['./glue.js']), source);
});

test('a module that imports something not yet linked is refused', () => {
  assert.throws(
    () => kitLoader.linkModules({ './a.js': "import './b.js';", './b.js': '' }, ['./a.js', './b.js']),
    /"\.\/a\.js" imports "\.\/b\.js", which is not packed before it/,
  );
  assert.throws(() => kitLoader.linkModules({}, ['./gone.js']), /"\.\/gone\.js" is missing/);
});

test('a page unpacks into config, assets, linked modules and a worklet URL', async () => {
  const disk = Uint8Array.from({ length: 70000 }, (_, i) => (i * 7 + (i >> 8)) & 0xff);
  const config = { format: kitLoader.FORMAT, modules: { order: ['./m.js'], entry: './m.js' } };
  const blocks = [
    block('./m.js', 'module', enc('export const start = (kit) => kit.assets.size;')),
    block('audio-worklet.js', 'worklet', enc('/* worklet */')),
    block('disk/0', 'binary', disk),
    block('notice/about', 'text', enc('hello'), { lazy: true }),
  ];
  const seen = [];
  const kit = await kitLoader.unpack(fakeDoc(config, blocks), (text) => seen.push(text));
  assert.deepEqual(kit.config, config);
  assert.deepEqual([...kit.assets.keys()], ['disk/0']);
  assert.deepEqual(kit.assets.get('disk/0'), disk);
  // The worklet is offered as a Blob URL and, for origins that refuse
  // that, as a data: URL of the same source.
  assert.equal(kit.workletUrls.length, 2);
  assert.match(kit.workletUrls[0], /^blob:/);
  assert.equal(await blobText(kit.workletUrls[0]), '/* worklet */');
  assert.equal(
    kit.workletUrls[1],
    `data:text/javascript;base64,${Buffer.from('/* worklet */').toString('base64')}`,
  );
  const entry = await importText(await blobText(kit.moduleUrls[config.modules.entry]));
  assert.equal(entry.start(kit), 1);
  assert.deepEqual(seen, [
    'unpacking ./m.js...',
    'unpacking audio-worklet.js...',
    'unpacking disk/0...',
  ]);
  // Unpacked blocks leave the DOM; a lazy one stays until it is asked for.
  assert.deepEqual(blocks.map((b) => b.removed), [true, true, true, false]);
  assert.equal(new TextDecoder().decode(await kit.readLazy('notice/about')), 'hello');
  await assert.rejects(kit.readLazy('notice/none'), /is not in this page/);
});

test('a page without a worklet block offers no worklet URL', async () => {
  const config = { format: kitLoader.FORMAT, modules: { order: [], entry: '' } };
  assert.deepEqual((await kitLoader.unpack(fakeDoc(config, []))).workletUrls, []);
});

test('damage and mismatches stop the page with the block named', async () => {
  const config = { format: kitLoader.FORMAT, modules: { order: [], entry: '' } };
  const good = Uint8Array.from({ length: 4000 }, (_, i) => i & 0xff);
  await assert.rejects(
    kitLoader.unpack(fakeDoc(config, [block('disk/0', 'binary', good, { size: 4001 })])),
    /"disk\/0" unpacked to 4000 bytes, expected 4001/,
  );
  // Flip one byte of the compressed stream: zlib's own checksum catches it.
  const packed = zlib.deflateSync(Buffer.from(good), { level: 9 });
  packed[packed.length - 1] ^= 0xff;
  await assert.rejects(
    kitLoader.unpack(
      fakeDoc(config, [block('disk/0', 'binary', good, { text: packed.toString('base64') })]),
    ),
    /"disk\/0" is damaged/,
  );
  await assert.rejects(
    kitLoader.unpack(fakeDoc({ ...config, format: 99 }, [])),
    /unsupported player kit format 99/,
  );
  await assert.rejects(
    kitLoader.unpack({ getElementById: () => null, querySelectorAll: () => [] }),
    /no player configuration/,
  );
});

test('a browser without DecompressionStream is told so', async () => {
  const config = { format: kitLoader.FORMAT, modules: { order: [], entry: '' } };
  const native = globalThis.DecompressionStream;
  try {
    globalThis.DecompressionStream = undefined;
    await assert.rejects(kitLoader.unpack(fakeDoc(config, [])), /too old to unpack/);
  } finally {
    globalThis.DecompressionStream = native;
  }
});

test('the loader can be inlined in a script element', () => {
  const source = readFileSync(new URL('./kit-loader.js', import.meta.url), 'utf8');
  assert.equal(/<\/script/i.test(source), false);
  assert.equal(/^\s*(import|export)\s/m.test(source), false);
});
