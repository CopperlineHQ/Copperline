#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Run a page packed by tools/publish-web without a browser.
//
//   node tools/check-web-kit.mjs PAGE.html [--seconds N] [--min-colours N]
//
// The page's own scripts run against a stand-in for the page they sit in:
// the old-browser guard, then the inlined loader, which unpacks the page's
// own blocks, links its modules and calls the page shell's start(). The
// controls the shell then wires up are driven the way a visitor would
// drive them, on a synthetic clock:
//
//   - every About text opens,
//   - Start boots the machine, which after N emulated seconds (default 20)
//     must show at least --min-colours distinct colours (default 2),
//   - an imported disk becomes the stored copy,
//   - a loaded quick state takes its disks back with it and the stored
//     copies follow at once (pages packed with save states enabled),
//   - Erase saves leaves nothing for the next poll to store again, and
//   - a Restart after a failure runs exactly one frame loop.
//
// Nothing is fetched: the stand-in has no network, and a fetch() from the
// page fails the check. Two things differ from a browser and are not
// covered here. Node does not import blob: URLs, so module and worklet
// Blobs are given data: URLs (the linking itself is the loader's own);
// and there is no layout, canvas or audio device, so the picture is read
// from what the shell hands the canvas. tools/check-web-kit-browser.mjs
// covers those in a real browser.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

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

// --- the stand-in page -------------------------------------------------------
const attributes = (text) => {
  const attrs = new Map();
  for (const attr of text.matchAll(/([\w-]+)(?:="([^"]*)")?/g)) attrs.set(attr[1], attr[2] ?? '');
  return attrs;
};

let activeElement = null;
let lastPicture = null;
const context2d = {
  putImageData(image) {
    lastPicture = image;
  },
};

class FakeElement {
  constructor(tag, attrs = new Map()) {
    this.tagName = tag.toUpperCase();
    this.attrs = attrs;
    this.id = attrs.get('id') ?? '';
    this.hidden = attrs.has('hidden');
    this.disabled = attrs.has('disabled');
    this.value = attrs.get('value') ?? '';
    this.textContent = '';
    this.children = [];
    this.parentElement = null;
    this.style = {};
    this.listeners = new Map();
    const classes = new Set((attrs.get('class') ?? '').split(/\s+/).filter(Boolean));
    this.classList = {
      add: (name) => void classes.add(name),
      remove: (name) => void classes.delete(name),
      contains: (name) => classes.has(name),
      toggle: (name, on = !classes.has(name)) => {
        if (on) classes.add(name);
        else classes.delete(name);
        return on;
      },
    };
  }
  getAttribute(name) {
    return this.attrs.get(name) ?? null;
  }
  hasAttribute(name) {
    return this.attrs.has(name);
  }
  setAttribute(name, value) {
    this.attrs.set(name, String(value));
  }
  removeAttribute(name) {
    this.attrs.delete(name);
  }
  addEventListener(type, fn) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(fn);
  }
  removeEventListener(type, fn) {
    const list = this.listeners.get(type) ?? [];
    if (list.includes(fn)) list.splice(list.indexOf(fn), 1);
  }
  // Fire an event and wait for every handler, so a check that follows sees
  // what an async handler did.
  async dispatch(type, event = {}) {
    const e = { type, target: this, detail: 0, preventDefault() {}, ...event };
    await Promise.all((this.listeners.get(type) ?? []).slice().map((fn) => fn(e)));
  }
  click() {
    return this.dispatch('click');
  }
  append(...nodes) {
    for (const node of nodes) {
      node.parentElement = this;
      this.children.push(node);
    }
  }
  remove() {
    this.removed = true;
    const siblings = this.parentElement?.children;
    if (siblings?.includes(this)) siblings.splice(siblings.indexOf(this), 1);
  }
  focus() {
    activeElement = this;
  }
  closest() {
    return null;
  }
  getContext(kind) {
    assert.equal(kind, '2d');
    return context2d;
  }
  getBoundingClientRect() {
    return { left: 0, top: 0, width: 100, height: 100 };
  }
  showModal() {
    this.attrs.set('open', '');
  }
  close() {
    this.attrs.delete('open');
    return this.dispatch('close');
  }
  toBlob(done) {
    done(null);
  }
}

// The script elements, with their text, in page order.
const scripts = [];
for (const match of page.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/g)) {
  const script = new FakeElement('script', attributes(match[1]));
  script.textContent = match[2];
  scripts.push(script);
}

// Everything else, as a tree of elements with the attributes and the text
// the shell starts from. An id the shell asks for and the page lacks comes
// back null, as it would in a browser.
const byId = new Map();
const body = new FakeElement('body');
{
  const markup = page
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/<(script|style)\b[^>]*>[\s\S]*?<\/\1>/g, '');
  const empty = new Set(['meta', 'link', 'input', 'br', 'img', 'hr']);
  const stack = [body];
  let textFrom = 0;
  for (const tag of markup.matchAll(/<(\/?)([a-zA-Z][\w-]*)([^>]*)>/g)) {
    const name = tag[2].toLowerCase();
    const top = stack.at(-1);
    if (tag[1]) {
      if (stack.length > 1) {
        if (top.children.length === 0) top.textContent = markup.slice(textFrom, tag.index).trim();
        stack.pop();
      }
    } else {
      const element = new FakeElement(name, attributes(tag[3]));
      top.append(element);
      if (element.id) byId.set(element.id, element);
      if (!empty.has(name)) stack.push(element);
    }
    textFrom = tag.index + tag[0].length;
  }
}

const documentEvents = new FakeElement('#document');
globalThis.document = {
  body,
  readyState: 'complete',
  hidden: false,
  title: '',
  pointerLockElement: null,
  fullscreenElement: null,
  get activeElement() {
    return activeElement;
  },
  getElementById: (id) => scripts.find((s) => s.id === id && !s.removed) ?? byId.get(id) ?? null,
  querySelector: (selector) => {
    assert.equal(selector, 'dialog[open]');
    const about = byId.get('cl-about');
    return about?.hasAttribute('open') ? about : null;
  },
  querySelectorAll: (selector) => {
    assert.equal(selector, 'script[data-cl-asset]');
    return scripts.filter((s) => s.hasAttribute('data-cl-asset') && !s.removed);
  },
  createElement: (tag) => new FakeElement(tag),
  addEventListener: (type, fn) => documentEvents.addEventListener(type, fn),
  removeEventListener: (type, fn) => documentEvents.removeEventListener(type, fn),
};

// In a browser the window is the global object; the page relies on that
// (the guard reads window.CopperlineKit, the loader sets it on globalThis).
const windowEvents = new FakeElement('#window');
globalThis.window = globalThis;
globalThis.addEventListener = (type, fn) => windowEvents.addEventListener(type, fn);
globalThis.removeEventListener = (type, fn) => windowEvents.removeEventListener(type, fn);
globalThis.confirm = () => true;
globalThis.devicePixelRatio = 1;
globalThis.ImageData = class {
  constructor(data, width, height) {
    Object.assign(this, { data, width, height });
  }
};

// Animation frames, run by hand on a synthetic clock.
const frames = new Map();
let frameSeq = 0;
globalThis.requestAnimationFrame = (fn) => {
  frames.set(++frameSeq, fn);
  return frameSeq;
};
globalThis.cancelAnimationFrame = (id) => void frames.delete(id);
let now = performance.now();
const settle = () => new Promise((resolve) => setImmediate(resolve));
async function runFrames(emulatedSeconds, emu) {
  const until = emu().emulated_seconds() + emulatedSeconds;
  for (let guard = 0; emu() && emu().emulated_seconds() < until; guard++) {
    assert.ok(guard < 1e6 && frames.size > 0, 'the frame loop stopped');
    now += 20;
    const due = Array.from(frames.values());
    frames.clear();
    for (const fn of due) fn(now);
    await settle();
  }
}

// The slice of IndexedDB the save store uses, in memory.
const databases = new Map();
globalThis.indexedDB = {
  open(name) {
    const request = {};
    queueMicrotask(() => {
      const fresh = !databases.has(name);
      request.result = {
        close() {},
        createObjectStore: () => void databases.set(name, new Map()),
        transaction() {
          const data = databases.get(name);
          const tx = {};
          const done = () => queueMicrotask(() => queueMicrotask(() => tx.oncomplete?.()));
          const later = (fn) => {
            const op = {};
            queueMicrotask(() => {
              op.result = fn();
              op.onsuccess?.();
            });
            done();
            return op;
          };
          tx.objectStore = () => ({
            get: (key) => later(() => data.get(key)),
            put: (value, key) => later(() => void data.set(key, value)),
            delete: (key) => later(() => void data.delete(key)),
          });
          return tx;
        },
      };
      if (fresh) request.onupgradeneeded?.();
      request.onsuccess?.();
    });
    return request;
  },
};

// No network: the page must not ask for anything.
globalThis.fetch = (resource) => {
  throw new assert.AssertionError({ message: `the page fetched ${resource}` });
};

// Node's loader does not import blob: URLs. A JavaScript Blob remembers a
// data: URL of the same source, and that is what createObjectURL hands the
// loader for it; the specifier rewriting stays the loader's own.
const NodeBlob = globalThis.Blob;
globalThis.Blob = class extends NodeBlob {
  constructor(parts = [], options = {}) {
    super(parts, options);
    if (options.type === 'text/javascript') {
      const source = Buffer.concat(parts.map((part) => Buffer.from(part)));
      this.dataUrl = `data:text/javascript;base64,${source.toString('base64')}`;
    }
  }
};
const createObjectURL = URL.createObjectURL.bind(URL);
URL.createObjectURL = (blob) => blob.dataUrl ?? createObjectURL(blob);

// --- the page's own scripts --------------------------------------------------
// A classic script, run in the global scope the way a script element is.
// Indirect eval rather than node:vm, whose scripts cannot import() without
// an experimental loader option.
const runScript = (text, name) => (0, eval)(`${text}\n//# sourceURL=${name}`);
const $ = (id) => {
  const element = byId.get(id);
  assert.ok(element, `the page has no #${id}`);
  return element;
};
const inline = scripts.filter((s) => !s.hasAttribute('type') && !s.hasAttribute('src'));
assert.deepEqual(
  inline.map((s) => s.id),
  ['cl-compat', ''],
  'the page must carry two inline scripts: the old-browser guard, then the loader',
);
const config = JSON.parse(document.getElementById('cl-config').textContent);
const status = $('cl-status');

// The guard: a script error before the loader has run means the browser
// could not parse it, and the visitor is told so.
const loading = status.textContent;
const guardCode = inline[0].textContent.replace(/^\s*\/\/.*$/gm, '');
assert.equal(
  /=>|`|\b(?:let|const|class|async|await)\b|\?\?|\?\./.test(guardCode),
  false,
  'the old-browser guard uses syntax an old browser cannot parse',
);
runScript(inline[0].textContent, 'kit-shell.html (guard)');
await windowEvents.dispatch('error');
assert.match(status.textContent, /too old to run this page/);
status.textContent = loading;

globalThis.COPPERLINE_KIT_MANUAL_START = true;
runScript(inline[1].textContent, 'kit-loader.js (inlined)');
await windowEvents.dispatch('error');
assert.equal(status.textContent, loading, 'the guard spoke after the loader had run');

// The loader's own start(): unpack, link, import the entry, start the shell.
await globalThis.CopperlineKit.start();
const boot = $('cl-boot');
assert.equal(status.textContent, '', `the page did not start: ${status.textContent}`);
assert.equal(boot.disabled, false, 'the Start button stayed disabled');
assert.equal($('cl-title').textContent, config.game.title);
assert.equal(document.querySelectorAll('script[data-cl-asset]').length, config.notices.length,
  'only the lazy notice blocks should be left in the page');

// Every About text opens.
const note = $('cl-note');
const licenseText = $('cl-license-text');
const noticeButtons = $('cl-notices').children;
assert.deepEqual(
  noticeButtons.map((button) => button.textContent),
  config.notices.map((item) => item.title),
);
for (const button of noticeButtons) {
  licenseText.textContent = '';
  await button.click();
  assert.ok(licenseText.textContent.length > 0, `${button.textContent} is empty`);
  assert.doesNotMatch(licenseText.textContent, /^cannot show this text/);
}
assert.match($('cl-about-engine').textContent, /^Copperline /);
assert.ok($('cl-about-source').textContent.startsWith(config.copperline.source));

// Start, and run the machine.
await boot.click();
const session = globalThis.CopperlineKit.session;
const emu = () => session.emu;
assert.ok(session.running && emu(), `the machine did not start: ${status.textContent}`);
assert.equal($('cl-overlay').hidden, true);
assert.equal(frames.size, 1, 'Start must leave exactly one frame requested');
assert.ok(config.disks.length > 0, 'the page carries no disk');
for (let drive = 0; drive < Math.min(config.machine.drives, config.disks.length); drive++) {
  assert.equal(emu().disk_name(drive), config.disks[drive].name);
}
await runFrames(seconds, emu);
assert.ok(session.running, `the machine stopped: ${status.textContent}`);
// The shell hands the canvas a view of wasm memory, which a later growth
// detaches; run on until a live one is in hand.
for (let tries = 0; !lastPicture?.data.byteLength; tries++) {
  assert.ok(tries < 500, 'the machine presented no picture');
  await runFrames(0.02, emu);
}
const { data, width, height } = lastPicture;
const colours = new Set(new Uint32Array(data.buffer, data.byteOffset, width * height));
assert.ok(
  colours.size >= minColours,
  `the picture holds ${colours.size} colour(s) after ${seconds}s; expected at least ${minColours}`,
);
const summary =
  `${config.game.title} on ${emu().machine_summary()}; ` +
  `${emu().emulated_seconds().toFixed(1)}s emulated, ${width}x${height}, ${colours.size} colours`;

// --- saves -------------------------------------------------------------------
const same = (a, b) => a.length === b.length && Buffer.compare(Buffer.from(a), Buffer.from(b)) === 0;
const stored = (index = 0) =>
  databases.get(`copperline-kit-${config.game.id}`)?.get(`disk:${index}`)?.bytes ?? null;
const checked = [];
const disk = session.disks[0];
if (disk.writable) {
  // Past a poll first: an untouched disk is not stored at all.
  await runFrames(4, emu);
  const saveState = $('cl-state-save');
  const withStates = config.features.save_states;
  assert.equal(saveState.hidden, !withStates);
  let atSave = null;
  if (withStates) {
    await saveState.click();
    assert.equal(note.textContent, 'State saved');
    atSave = emu().export_floppy(0);
  }

  // A disk the visitor imports (standing in for a guest write: the drive's
  // contents change under the page) becomes the stored copy.
  const written = emu().export_floppy(0);
  written.set(Buffer.from('copperline web kit check'), written.length - 512);
  const importFile = $('cl-import-file');
  importFile.files = [{ name: 'written.adf', arrayBuffer: async () => written.slice().buffer }];
  await importFile.dispatch('change');
  assert.match(note.textContent, /^Imported written\.adf/);
  assert.ok(same(emu().export_floppy(0), written), 'the imported disk is not in the drive');
  assert.ok(stored() && same(stored(), written), 'the imported disk was not stored');
  await runFrames(4, emu);
  assert.ok(same(stored(), written), 'a poll replaced the stored disk');
  checked.push('disk saves');

  if (withStates) {
    // A state is the whole machine: its disks come back with it, the
    // stored copy follows at once (not at some later poll), and the
    // visitor is told a disk went back.
    await $('cl-state-load').click();
    assert.ok(same(emu().export_floppy(0), atSave), 'the state did not bring its disk back');
    assert.ok(
      same(stored(), atSave),
      'after a state load the stored disk is not the one in the drive',
    );
    assert.match(note.textContent, new RegExp(`^State loaded; ${disk.name} went back`));
    await runFrames(4, emu);
    assert.ok(same(stored(), atSave), 'a poll changed the stored disk after a state load');
    checked.push('state load');
  }

  // Erase saves: nothing stored, and the running machine holds nothing
  // the next poll would store again.
  await $('cl-erase').click();
  assert.equal(stored(), null, 'Erase saves left a stored disk');
  await runFrames(4, emu);
  assert.equal(stored(), null, 'a poll stored a disk again after Erase saves');
  assert.ok(same(emu().export_floppy(0), disk.pristine), 'Erase saves left a written disk in');
  checked.push('erase');
} else {
  console.log(`${disk.name} is read-only in the page: the save checks are skipped`);
}

// --- restart after a failure ---------------------------------------------------
// A failure outside the frame loop arrives with a frame still requested.
// It must not survive into the restarted session as a second loop.
assert.equal(frames.size, 1);
emu().reset = () => {
  throw new Error('forced failure');
};
const consoleError = console.error;
console.error = () => {};
await $('cl-reset').click();
console.error = consoleError;
assert.equal(session.running, false);
assert.equal($('cl-overlay').hidden, false);
assert.equal(boot.textContent, 'Restart');
assert.match(status.textContent, /forced failure/);
assert.equal(frames.size, 0, 'a failed session left a frame requested');
await boot.click();
assert.ok(session.running && emu(), `the machine did not restart: ${status.textContent}`);
assert.equal(frames.size, 1, 'Restart must leave exactly one frame requested');
await runFrames(1, emu);
assert.equal(frames.size, 1, 'the restarted session runs more than one frame loop');
checked.push('restart');

console.log(`web kit page runs: ${summary}; checked ${checked.join(', ')}`);
// Pending note timers would keep the process alive for a few seconds.
process.exit(0);
