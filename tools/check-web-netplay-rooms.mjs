#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Test the site against a local room service. Set NETPLAY_SERVICE for a deployed
// service and NETPLAY_RELAY_ONLY=1 to require a verified TURN route.
import assert from 'node:assert/strict';
import { mkdir } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const url = new URL(process.argv[2] ?? 'http://127.0.0.1:8765/try/');
const service = new URL(process.env.NETPLAY_SERVICE ?? 'http://127.0.0.1:8787');
const relayOnly = process.env.NETPLAY_RELAY_ONLY === '1';
const output = resolve(process.argv[3] ?? '/tmp/copperline-netplay-rooms-browser');
await mkdir(output, { recursive: true });
const module = process.env.PLAYWRIGHT_MODULE;
const playwright = await import(module ? pathToFileURL(resolve(module)).href : 'playwright');
const engine = process.env.NETPLAY_BROWSER ?? 'chromium';
const browser = await playwright[engine].launch({
  ...(engine === 'chromium' ? { executablePath: process.env.CHROME_PATH,
    args: ['--autoplay-policy=no-user-gesture-required', '--disable-background-timer-throttling',
      '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows'] } : {}),
});
const errors = [];
const shutdownNotices = [];
let phase = 'setup';
try {
  const pages = [];
  for (let player = 0; player < 2; player++) {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block' });
    await context.route('**/*', route => {
      const target = new URL(route.request().url());
      return [url.origin, service.origin].includes(target.origin) ? route.continue() : route.abort();
    });
    await context.addInitScript(base => {
      // Use a test endpoint without changing the deployable page configuration.
      new MutationObserver(() => {
        const meta = document.querySelector('meta[name="copperline-netplay-service"]');
        if (meta && meta.content !== base) meta.content = base;
      }).observe(document, { childList: true, subtree: true });
      Object.defineProperty(navigator, 'clipboard', { value: { writeText: async text => { window.__copied = text; } } });
    }, service.origin);
    const page = await context.newPage();
    page.on('pageerror', error => {
      // WebKit logs a native console error when its send queue meets a remote
      // shutdown before the closing event arrives (RTCDataChannel.cpp). Keep
      // it visible and permit it only during deliberately ended sessions.
      if (engine === 'webkit' && ['disconnect', 'mismatch'].includes(phase)
          && error.message === 'Error sending binary data through RTCDataChannel.') {
        shutdownNotices.push({ phase, message: error.message });
      } else errors.push({ phase, message: error.message });
    });
    await page.goto(url.href);
    await page.locator('#boot:enabled').waitFor({ timeout: 30000 });
    if (await page.locator('#netplay-open').count()) await page.locator('#netplay-open').click();
    else await page.locator('#netplay-panel > summary').click();
    if (relayOnly) {
      await page.locator('#netplay-advanced > summary').click();
      await page.locator('#netplay-relay-only').check();
      await page.locator('#netplay-advanced > summary').click();
    }
    pages.push(page);
  }
  const [host, guest] = pages;
  const invitation = async () => {
    await host.locator('#netplay-room-host').click();
    await host.waitForFunction(() => document.querySelector('#netplay-invite').value.includes('#room='), null, { timeout: 30000 });
    return host.locator('#netplay-invite').inputValue();
  };
  const cancelled = await invitation();
  phase = 'disconnect';
  await host.locator('#netplay-disconnect').click();
  await guest.locator('#netplay-room-code').fill(cancelled);
  await guest.locator('#netplay-room-join').click();
  await guest.waitForFunction(() => /expired|ended/.test(document.querySelector('#netplay-status').textContent));
  assert.equal(await guest.locator('#boot').isEnabled(), true);
  async function connect() {
    phase = 'connecting';
    const link = await invitation();
    assert.equal(new URL(link).search, '');
    await host.locator('#netplay-qr svg').waitFor();
    await host.locator('#netplay-copy-invite').click();
    assert.equal(await host.evaluate(() => window.__copied), link);
    // Fragment navigation must reveal the panel and populate Join on an already loaded page.
    await guest.evaluate(link => { location.hash = new URL(link).hash; }, link);
    await guest.waitForFunction(() => document.querySelector('#netplay-room-code').value === new URLSearchParams(location.hash.slice(1)).get('room'));
    await host.locator('#netplay-panel').screenshot({ path: `${output}/invitation.png` });
    await guest.locator('#netplay-room-join').click();
    await Promise.all(pages.map(page => page.waitForFunction(() => window.__emu?.netplay_status()[6] >= 120,
      null, { timeout: 90000 })));
    phase = 'playing';
    return link;
  }
  const link = await connect();
  for (const [i, page] of pages.entries()) {
    for (const id of ['boot', 'machine', 'video', 'reset', 'pause', 'df0', 'df1']) {
      assert.equal(await page.locator(`#${id}`).isDisabled(), true, `${id} must stay locked`);
    }
    await page.evaluate(() => { window.__copied = ''; });
    await page.locator('#netplay-diagnostics').click();
    await page.waitForFunction(() => window.__copied?.startsWith('{'));
    const report = JSON.parse(await page.evaluate(() => window.__copied));
    assert.equal(report.connection.peer, 'connected');
    assert.equal(report.stats.dtls, 'connected');
    if (relayOnly) assert.equal(report.stats.selectedPair.local, 'relay');
    assert.ok(!JSON.stringify(report).includes(new URL(link).hash.slice(6)));
    console.log(`Player ${i + 1}: ${JSON.stringify({ status: await page.evaluate(() => [...window.__emu.netplay_status()]), route: report.stats.selectedPair })}`);
  }
  phase = 'disconnect';
  await host.locator('#netplay-disconnect').click();
  await Promise.all(pages.map(page => page.locator('#netplay-room-host:enabled').waitFor()));
  await guest.evaluate(() => { window.__copied = ''; });
  await guest.locator('#netplay-diagnostics').click();
  await guest.waitForFunction(() => window.__copied?.startsWith('{'));
  assert.ok(JSON.parse(await guest.evaluate(() => window.__copied)).stats);
  await connect();
  phase = 'disconnect';
  await guest.locator('#netplay-disconnect').click();
  await Promise.all(pages.map(page => page.locator('#netplay-room-host:enabled').waitFor()));
  await guest.locator('#video').selectOption('NTSC');
  phase = 'mismatch';
  const mismatch = await invitation();
  await guest.locator('#netplay-room-code').fill(mismatch);
  await guest.locator('#netplay-room-join').click();
  await Promise.all(pages.map(page => page.waitForFunction(() => /mismatch|different/i.test(document.querySelector('#netplay-status').textContent), null, { timeout: 30000 })));
  await host.setViewportSize({ width: 390, height: 844 });
  await host.locator('#netplay-open').click();
  await host.locator('#netplay-panel').screenshot({ path: `${output}/mobile.png` });
  assert.deepEqual(errors, []);
  if (shutdownNotices.length) console.log('WebKit native shutdown notices:', JSON.stringify(shutdownNotices));
  console.log('Room invitations, cancellation, reconnect, machine mismatch, diagnostics and responsive rendering passed');
} finally { await browser.close(); }
