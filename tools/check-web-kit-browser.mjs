#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Load a page packed by tools/publish-web in a real headless Chrome, start
// it, and prove it makes no request beyond itself.
//
//   node tools/check-web-kit-browser.mjs PAGE.html|URL [--seconds N]
//        [--min-colours N] [--screenshot OUT.png] [--chrome PATH]
//
// A file path is opened as a file:// URL; an http(s) URL is opened as
// given. The browser is driven over the DevTools protocol with nothing but
// Node's own WebSocket, so no test framework has to be installed: only a
// Chrome or Chromium (--chrome, $CHROME, or the usual install locations).
// It runs with a throwaway profile and is closed afterwards.
//
// The check: the page unpacks and enables its Start button, a real mouse
// click on it boots the machine, emulated time reaches N seconds (default
// 20), the canvas then holds at least --min-colours colours (default 2),
// no script error was thrown, and every request the page made is either
// the page itself or an in-memory blob:/data: URL.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const args = process.argv.slice(2);
const option = (name, fallback) => {
  const at = args.indexOf(name);
  if (at < 0) return fallback;
  const [, value] = args.splice(at, 2);
  assert.ok(value !== undefined, `${name} needs a value`);
  return value;
};
const seconds = Number(option('--seconds', '20'));
const minColours = Number(option('--min-colours', '2'));
const screenshot = option('--screenshot', null);
const chromeArg = option('--chrome', process.env.CHROME ?? null);
assert.ok(Number.isFinite(seconds) && Number.isFinite(minColours), 'numeric options need numbers');
assert.equal(args.length, 1, 'usage: check-web-kit-browser.mjs PAGE.html|URL [options]');
const pageUrl = /^https?:\/\//.test(args[0]) ? args[0] : pathToFileURL(resolve(args[0])).href;

const chrome = [
  chromeArg,
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
  '/usr/bin/google-chrome',
  '/usr/bin/google-chrome-stable',
  '/usr/bin/chromium',
  '/usr/bin/chromium-browser',
].find((path) => path && existsSync(path));
assert.ok(chrome, 'no Chrome or Chromium found; pass --chrome PATH or set $CHROME');

const profile = await mkdtemp(join(tmpdir(), 'copperline-kit-check-'));
const browser = spawn(
  chrome,
  [
    '--headless=new',
    '--remote-debugging-port=0',
    `--user-data-dir=${profile}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-background-networking',
    '--disable-component-update',
    '--disable-sync',
    '--window-size=1024,768',
    'about:blank',
  ],
  { stdio: ['ignore', 'ignore', 'pipe'] },
);
let failure = null;
try {
  const endpoint = await new Promise((resolveEndpoint, reject) => {
    let log = '';
    const timer = setTimeout(() => reject(new Error(`Chrome did not start:\n${log}`)), 30000);
    browser.stderr.on('data', (chunk) => {
      log += chunk;
      const match = log.match(/DevTools listening on (ws:\/\/\S+)/);
      if (match) {
        clearTimeout(timer);
        resolveEndpoint(match[1]);
      }
    });
    browser.on('exit', (code) => reject(new Error(`Chrome exited with ${code}:\n${log}`)));
  });

  const socket = new WebSocket(endpoint);
  await new Promise((opened, reject) => {
    socket.addEventListener('open', opened, { once: true });
    socket.addEventListener('error', () => reject(new Error('cannot reach DevTools')), {
      once: true,
    });
  });
  let nextId = 1;
  const pending = new Map();
  const requests = [];
  const errors = [];
  socket.addEventListener('message', (event) => {
    const message = JSON.parse(event.data);
    if (message.id && pending.has(message.id)) {
      const { resolve: done, reject } = pending.get(message.id);
      pending.delete(message.id);
      if (message.error) reject(new Error(message.error.message));
      else done(message.result);
    } else if (message.method === 'Network.requestWillBeSent') {
      requests.push(message.params.request.url);
    } else if (message.method === 'Runtime.exceptionThrown') {
      const details = message.params.exceptionDetails;
      errors.push(details.exception?.description ?? details.text);
    } else if (message.method === 'Runtime.consoleAPICalled' && message.params.type === 'error') {
      errors.push(message.params.args.map((a) => a.value ?? a.description).join(' '));
    }
  });
  const send = (method, params = {}, sessionId) =>
    new Promise((done, reject) => {
      const id = nextId++;
      pending.set(id, { resolve: done, reject });
      socket.send(JSON.stringify({ id, method, params, sessionId }));
    });

  const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
  const page = (method, params) => send(method, params, sessionId);
  await page('Page.enable');
  await page('Network.enable');
  await page('Runtime.enable');
  const evaluate = async (expression) => {
    const { result, exceptionDetails } = await page('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (exceptionDetails) throw new Error(exceptionDetails.exception?.description ?? 'evaluate');
    return result.value;
  };
  const waitFor = async (what, expression, timeoutMs) => {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const value = await evaluate(expression);
      if (value) return value;
      if (Date.now() > deadline) {
        const status = await evaluate("document.getElementById('cl-status')?.textContent ?? ''");
        throw new Error(`timed out waiting for ${what} (page status: "${status}")`);
      }
      await new Promise((tick) => setTimeout(tick, 250));
    }
  };

  await page('Page.navigate', { url: pageUrl });
  const box = await waitFor(
    'the Start button',
    `(() => {
      const b = document.getElementById('cl-boot');
      if (!b || b.disabled) return null;
      const r = b.getBoundingClientRect();
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
    })()`,
    60000,
  );
  // A trusted click: the gesture that unlocks audio, as a visitor's would.
  for (const type of ['mousePressed', 'mouseReleased']) {
    await page('Input.dispatchMouseEvent', {
      type,
      x: box.x,
      y: box.y,
      button: 'left',
      clickCount: 1,
    });
  }
  await waitFor('the machine to start', 'globalThis.CopperlineKit?.session?.running === true', 30000);
  await waitFor(
    `${seconds}s of emulated time`,
    `(globalThis.CopperlineKit.session.emu?.emulated_seconds() ?? 0) >= ${seconds}`,
    seconds * 4000 + 60000,
  );
  const report = await evaluate(`(() => {
    const session = globalThis.CopperlineKit.session;
    const canvas = document.getElementById('cl-screen');
    const data = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
    const colours = new Set(new Uint32Array(data.buffer));
    return {
      title: document.title,
      seconds: session.emu.emulated_seconds(),
      summary: session.emu.machine_summary(),
      audio: session.audioState,
      width: canvas.width,
      height: canvas.height,
      colours: colours.size,
      overlayHidden: document.getElementById('cl-overlay').hidden,
      blocksLeft: document.querySelectorAll('script[data-cl-asset]:not([data-cl-lazy])').length,
      status: document.getElementById('cl-status').textContent,
    };
  })()`);
  if (screenshot) {
    const shot = await page('Page.captureScreenshot', { format: 'png' });
    await writeFile(screenshot, Buffer.from(shot.data, 'base64'));
  }
  await send('Browser.close').catch(() => {});

  const outside = requests.filter(
    (url) => url !== pageUrl && !url.startsWith('blob:') && !url.startsWith('data:'),
  );
  assert.deepEqual(outside, [], 'the page requested something other than itself');
  assert.ok(requests.includes(pageUrl), 'the page itself was never requested');
  assert.deepEqual(errors, [], 'the page reported script errors');
  assert.equal(report.overlayHidden, true, 'the start overlay is still showing');
  assert.equal(report.blocksLeft, 0, 'unpacked asset blocks were left in the DOM');
  assert.ok(
    report.colours >= minColours,
    `the canvas holds ${report.colours} colour(s); expected at least ${minColours}`,
  );
  const blobs = requests.filter((url) => url.startsWith('blob:')).length;
  console.log(
    `web kit page runs in Chrome: "${report.title}" from ${new URL(pageUrl).protocol}// ` +
      `on ${report.summary}; ${report.seconds.toFixed(1)}s emulated, ` +
      `${report.width}x${report.height}, ${report.colours} colours, audio ${report.audio}; ` +
      `requests: 1 page + ${blobs} in-memory blob, 0 other`,
  );
} catch (e) {
  failure = e;
} finally {
  browser.kill();
  await new Promise((exited) => {
    if (browser.exitCode !== null) exited();
    else browser.once('exit', exited);
  });
  await rm(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
if (failure) {
  console.error(failure.message ?? failure);
  process.exit(1);
}
