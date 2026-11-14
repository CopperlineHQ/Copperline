// SPDX-License-Identifier: GPL-3.0-or-later

// Web player kit loader: the one classic script tools/publish-web inlines
// verbatim into a single-file player page. Everything else the page needs
// (the wasm binary, its wasm-bindgen glue, the page modules, the audio
// worklet, the boot ROMs and the disk images) sits in the same HTML file as
// <script type="text/plain" data-cl-asset="..."> blocks holding base64 of a
// zlib stream, so the page makes no request beyond itself. This script
//
//   1. base64-decodes and inflates each block (DecompressionStream) and
//      drops the block from the DOM so its text is not retained,
//   2. links the ES modules: each becomes a Blob URL, with the relative
//      import specifiers of later modules rewritten to the Blob URLs of
//      the modules they name (tools/publish-web stores them dependencies
//      first), and
//   3. imports the entry module and hands it everything it unpacked.
//
// The functions are also reachable as globalThis.CopperlineKit so
// tools/check-web-kit.mjs can drive the same code against a packed file
// without a browser. This file must stay a classic script with no imports,
// and must never contain the closing script tag sequence.

(() => {
  'use strict';

  const FORMAT = 1;
  const B64_CHUNK = 1 << 20; // decoded in slices so no huge binary string exists

  function base64ToBytes(text) {
    if (typeof Uint8Array.fromBase64 === 'function') return Uint8Array.fromBase64(text);
    const clean = text.replace(/\s+/g, '');
    if (clean.length % 4 !== 0) throw new Error('truncated base64 block');
    let pad = 0;
    if (clean.endsWith('==')) pad = 2;
    else if (clean.endsWith('=')) pad = 1;
    const out = new Uint8Array((clean.length / 4) * 3 - pad);
    let at = 0;
    for (let i = 0; i < clean.length; i += B64_CHUNK) {
      const bin = atob(clean.slice(i, i + B64_CHUNK));
      for (let j = 0; j < bin.length; j++) out[at++] = bin.charCodeAt(j);
    }
    return out;
  }

  function bytesToBase64(bytes) {
    let bin = '';
    for (let i = 0; i < bytes.length; i += 0x8000) {
      bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    }
    return btoa(bin);
  }

  // Inflate one zlib stream. The stream's own Adler-32 makes a damaged
  // block fail here instead of reaching the emulator.
  async function inflate(bytes) {
    const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream('deflate'));
    return new Uint8Array(await new Response(stream).arrayBuffer());
  }

  async function readBlock(el) {
    const name = el.getAttribute('data-cl-asset');
    let bytes;
    try {
      bytes = await inflate(base64ToBytes(el.textContent));
    } catch (e) {
      throw new Error(`"${name}" is damaged: ${e.message ?? e}`);
    }
    const size = Number(el.getAttribute('data-cl-size'));
    if (bytes.length !== size) {
      throw new Error(`"${name}" unpacked to ${bytes.length} bytes, expected ${size}`);
    }
    return bytes;
  }

  // A relative specifier in a static import, a re-export, or a dynamic
  // import of a string literal. tools/publish-web scans with the same
  // pattern and refuses to pack a module whose specifiers it cannot place.
  const SPECIFIER = /(\bfrom\s*|\bimport\s*\(?\s*)(['"])(\.\.?\/[^'"\n]+)\2/g;

  // Turn module sources into Blob URLs, in the given dependencies-first
  // order. Returns { specifier: url }.
  function linkModules(sources, order) {
    const urls = {};
    for (const name of order) {
      const source = sources[name];
      if (typeof source !== 'string') throw new Error(`module "${name}" is missing`);
      const linked = source.replace(SPECIFIER, (match, lead, quote, spec) => {
        const url = urls[spec];
        if (!url) throw new Error(`module "${name}" imports "${spec}", which is not packed before it`);
        return `${lead}${quote}${url}${quote}`;
      });
      urls[name] = URL.createObjectURL(new Blob([linked], { type: 'text/javascript' }));
    }
    return urls;
  }

  // Unpack every block of `doc`. `progress(text)` is told what is being
  // unpacked. Blocks marked data-cl-lazy stay in the document until
  // kit.readLazy(name) asks for them (license texts nobody may ever open).
  async function unpack(doc, progress = () => {}) {
    const configEl = doc.getElementById('cl-config');
    if (!configEl) throw new Error('the page has no player configuration');
    const config = JSON.parse(configEl.textContent);
    if (config.format !== FORMAT) {
      throw new Error(`unsupported player kit format ${config.format}`);
    }
    if (typeof DecompressionStream !== 'function') {
      throw new Error('this browser is too old to unpack the player (no DecompressionStream)');
    }
    const decoder = new TextDecoder('utf-8', { fatal: true });
    const assets = new Map();
    const sources = {};
    const lazy = new Map();
    // The audio worklet module, as the URLs to try in order: a Blob URL,
    // then the same source as a data: URL. A page opened from disk has an
    // opaque origin, where Chrome refuses a Blob URL worklet module but
    // accepts the data: one.
    const workletUrls = [];
    for (const el of Array.from(doc.querySelectorAll('script[data-cl-asset]'))) {
      const name = el.getAttribute('data-cl-asset');
      const kind = el.getAttribute('data-cl-kind');
      if (el.hasAttribute('data-cl-lazy')) {
        lazy.set(name, el);
        continue;
      }
      progress(`unpacking ${name}...`);
      const bytes = await readBlock(el);
      el.remove();
      if (kind === 'module') sources[name] = decoder.decode(bytes);
      else if (kind === 'worklet') {
        workletUrls.push(
          URL.createObjectURL(new Blob([bytes], { type: 'text/javascript' })),
          `data:text/javascript;base64,${bytesToBase64(bytes)}`,
        );
      } else assets.set(name, bytes);
    }
    const moduleUrls = linkModules(sources, config.modules.order);
    return {
      config,
      assets,
      moduleUrls,
      workletUrls,
      async readLazy(name) {
        const el = lazy.get(name);
        if (!el) throw new Error(`"${name}" is not in this page`);
        return readBlock(el);
      },
    };
  }

  async function start() {
    const status = (text) => {
      const el = document.getElementById('cl-status');
      if (el) el.textContent = text;
    };
    try {
      const kit = await unpack(document, status);
      status('starting...');
      const entry = await import(kit.moduleUrls[kit.config.modules.entry]);
      await entry.start(kit);
    } catch (e) {
      status(`cannot start: ${e.message ?? e}`);
      console.error(e);
    }
  }

  globalThis.CopperlineKit = { FORMAT, base64ToBytes, inflate, linkModules, unpack, start };
  if (typeof document !== 'undefined' && !globalThis.COPPERLINE_KIT_MANUAL_START) {
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start);
    else start();
  }
})();
