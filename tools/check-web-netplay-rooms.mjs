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
const exerciseGatherDeadline = process.env.NETPLAY_GATHER_DEADLINE_TEST === '1';
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
      if (target.origin === url.origin && target.pathname.endsWith('/copperline.json')) {
        return route.fulfill({ json: { mono_audio: player === 0 } });
      }
      return [url.origin, service.origin].includes(target.origin) ? route.continue() : route.abort();
    });
    await context.addInitScript(({ base, exerciseGatherDeadline }) => {
      // Use a test endpoint without changing the deployable page configuration.
      new MutationObserver(() => {
        const meta = document.querySelector('meta[name="copperline-netplay-service"]');
        if (meta && meta.content !== base) meta.content = base;
      }).observe(document, { childList: true, subtree: true });
      Object.defineProperty(navigator, 'clipboard', { value: { writeText: async text => { window.__copied = text; } } });
      if (exerciseGatherDeadline) {
        // Keep real ICE candidates and connectivity, but make the wrapper use
        // its deadline path even when the browser finishes gathering quickly.
        const Peer = RTCPeerConnection, schedule = setTimeout;
        window.RTCPeerConnection = class extends Peer { get iceGatheringState() { return 'gathering'; } };
        window.setTimeout = (callback, delay, ...args) => schedule(callback, delay === 15000 ? 2000 : delay, ...args);
      }
    }, { base: service.origin, exerciseGatherDeadline });
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
  // Only the host has the game files. Deliberately disagree on every copied
  // setting and the guest ROM so checked frames prove the transfer took effect.
  await host.locator('#machine').selectOption('A1200');
  await host.locator('#video').selectOption('NTSC');
  await host.locator('#floppy-speed').selectOption('400');
  await host.locator('#floppy-sounds').uncheck();
  if (await host.locator('#mono-audio').count()) await host.locator('#mono-audio').check();
  await host.locator('#writable-floppies').check();
  const disk = Buffer.alloc(901120);
  await host.locator('#df0').setInputFiles({ name: 'host-df0.adf', mimeType: 'application/octet-stream', buffer: disk });
  await host.locator('#df1').setInputFiles({ name: 'host-df1.adf', mimeType: 'application/octet-stream', buffer: disk });
  await guest.locator('#kick').setInputFiles({ name: 'guest-original.rom', mimeType: 'application/octet-stream', buffer: Buffer.alloc(256 * 1024) });
  await guest.waitForFunction(() => document.querySelector('#load-status').textContent.includes('guest-original.rom'));
  const restored = async () => {
    assert.equal(await guest.locator('#machine').inputValue(), 'A500');
    assert.equal(await guest.locator('#video').inputValue(), 'PAL');
    assert.equal(await guest.locator('#floppy-speed').inputValue(), '100');
    assert.equal(await guest.locator('#floppy-sounds').isChecked(), true);
    if (await guest.locator('#mono-audio').count()) assert.equal(await guest.locator('#mono-audio').isChecked(), false);
    assert.equal(await guest.locator('#boot').textContent(), 'Boot Kickstart');
  };
  const invitation = async () => {
    await host.locator('#netplay-room-host').click();
    await host.waitForFunction(() => document.querySelector('#netplay-invite').value.includes('#room='), null, { timeout: 30000 })
      .catch(async error => {
        console.error('Host setup:', await host.locator('#netplay-status').textContent());
        await host.locator('#netplay-diagnostics').click();
        await host.waitForFunction(() => window.__copied?.startsWith('{'));
        console.error('Host diagnostics:', await host.evaluate(() => window.__copied));
        throw error;
      });
    return host.locator('#netplay-invite').inputValue();
  };
  const cancelled = await invitation();
  phase = 'disconnect';
  await host.locator('#netplay-disconnect').click();
  await guest.locator('#netplay-room-code').fill(cancelled);
  await guest.locator('#netplay-room-join').click();
  await guest.waitForFunction(() => /expired|ended/.test(document.querySelector('#netplay-status').textContent));
  assert.equal(await guest.locator('#boot').isEnabled(), true);
  // Hold the media bytes after the manifest, then cancel from the receiver.
  await host.evaluate(() => {
    const send = RTCDataChannel.prototype.send;
    window.__restoreSetupSend = () => { RTCDataChannel.prototype.send = send; };
    RTCDataChannel.prototype.send = function (data) {
      if (this.label === 'copperline-setup-v1' && typeof data !== 'string') return;
      return send.call(this, data);
    };
  });
  const interrupted = await invitation();
  await guest.locator('#netplay-room-code').fill(interrupted);
  await guest.locator('#netplay-room-join').click();
  await guest.waitForFunction(() => /Receiving/.test(document.querySelector('#netplay-status').textContent));
  await guest.locator('#netplay-disconnect').click();
  await Promise.all(pages.map(page => page.locator('#netplay-room-host:enabled').waitFor()));
  assert.ok(await guest.evaluate(() => !window.__emu));
  await restored();
  await host.evaluate(() => window.__restoreSetupSend());
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
      null, { timeout: 90000 }))).catch(async error => {
      for (const [i, page] of pages.entries()) {
        console.error(`Player ${i + 1} setup:`, await page.locator('#netplay-status').textContent());
        await page.evaluate(() => { window.__copied = ''; });
        if (await page.locator('#netplay-diagnostics').isEnabled()) {
          await page.locator('#netplay-diagnostics').click();
          await page.waitForFunction(() => window.__copied?.startsWith('{'));
          console.error(`Player ${i + 1} diagnostics:`, await page.evaluate(() => window.__copied));
        }
      }
      throw error;
    });
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
    if (exerciseGatherDeadline) assert.ok(report.events.some(event => event.event === 'gathering-deadline'));
    assert.deepEqual(await page.evaluate(() => ({ model: window.__emu.machine_model(), video: window.__emu.video_standard(), speed: window.__emu.floppy_speed() })),
      { model: 'A1200', video: 'NTSC', speed: 400 });
    assert.deepEqual(await page.evaluate(() => [0, 1].map(drive => ({ name: window.__emu.disk_name(drive),
      protected: window.__emu.floppy_write_protected(drive) }))),
    [{ name: 'netplay-df0', protected: false }, { name: 'netplay-df1', protected: false }]);
    assert.equal(await page.locator('#floppy-sounds').isChecked(), false);
    if (await page.locator('#mono-audio').count()) assert.equal(await page.locator('#mono-audio').isChecked(), true);
    if (relayOnly) assert.equal(report.stats.selectedPair.local, 'relay');
    assert.ok(!JSON.stringify(report).includes(new URL(link).hash.slice(6)));
    console.log(`Player ${i + 1}: ${JSON.stringify({ status: await page.evaluate(() => [...window.__emu.netplay_status()]), route: report.stats.selectedPair })}`);
  }
  // Record the real WASM commit boundary; each peer must apply once at the
  // same frame, produce the same state digest, and keep checking future frames.
  await Promise.all(pages.map(page => page.evaluate(() => {
    window.__diskCommits = [];
    const emu = window.__emu, apply = emu.netplay_apply_disk.bind(emu);
    emu.netplay_apply_disk = () => {
      apply();
      window.__diskCommits.push({ frame: emu.netplay_status()[1], hash: [...emu.netplay_swap_digest()] });
    };
  })));
  assert.equal(await guest.locator('#netplay-disks').isVisible(), false);
  let commits = 0;
  for (const [drive, value] of [[0, 3], [0, 4], [0, null], [0, 3], [1, 5]]) {
    await host.locator('#netplay-disk-file:enabled').waitFor();
    await host.locator('#netplay-disk-drive').selectOption(String(drive));
    if (value === null) await host.locator('#netplay-disk-eject').click();
    else await host.locator('#netplay-disk-file').setInputFiles({ name: `replacement-${value}.adf`,
      mimeType: 'application/octet-stream', buffer: Buffer.alloc(901120, value) });
    commits++;
    await Promise.all(pages.map(page => page.waitForFunction(count => window.__diskCommits.length === count,
      commits, { timeout: 30000 })));
    const boundaries = await Promise.all(pages.map(page => page.evaluate(() => window.__diskCommits.at(-1))));
    assert.deepEqual(boundaries[0], boundaries[1], 'swap must commit identical states at one frame');
    const checked = (Math.ceil(boundaries[0].frame / 60) + 2) * 60;
    await Promise.all(pages.map(page => page.waitForFunction(frame => window.__emu?.netplay_status()[6] >= frame,
      checked, { timeout: 90000 })));
    for (const page of pages) assert.equal(await page.evaluate(drive => window.__emu.disk_name(drive), drive),
      value === null ? undefined : `netplay-df${drive}`);
    console.log(`DF${drive} ${value === null ? 'ejection' : 'swap'} at frame ${boundaries[0].frame}; both checked ${checked}`);
  }
  await host.locator('#netplay-panel').screenshot({ path: `${output}/disk-swaps.png` });
  phase = 'disconnect';
  await host.locator('#netplay-disconnect').click();
  await Promise.all(pages.map(page => page.locator('#netplay-room-host:enabled').waitFor()));
  await restored();
  await guest.evaluate(() => { window.__copied = ''; });
  await guest.locator('#netplay-diagnostics').click();
  await guest.waitForFunction(() => window.__copied?.startsWith('{'));
  assert.ok(JSON.parse(await guest.evaluate(() => window.__copied)).stats);
  await connect();
  // Interrupt an actual replacement transfer while both cores are held.
  await host.evaluate(() => {
    const send = RTCDataChannel.prototype.send;
    RTCDataChannel.prototype.send = function(data) {
      if (this.label === 'copperline-disks-v1') {
        if (typeof data !== 'string') return;
        if (JSON.parse(data).type === 'end') { window.__heldDiskTransfer = true; return; }
      }
      return send.call(this, data);
    };
    window.__restoreDiskSend = () => { RTCDataChannel.prototype.send = send; };
  });
  await host.locator('#netplay-disk-file:enabled').waitFor();
  await host.locator('#netplay-disk-file').setInputFiles({ name: 'interrupted.adf',
    mimeType: 'application/octet-stream', buffer: Buffer.alloc(901120, 6) });
  await host.waitForFunction(() => window.__heldDiskTransfer === true);
  await Promise.all(pages.map(page => page.waitForFunction(() => window.__emu?.netplay_swap_ready())));
  const heldFrames = await Promise.all(pages.map(page => page.evaluate(() => window.__emu.netplay_status()[1])));
  assert.equal(heldFrames[0], heldFrames[1]);
  phase = 'disconnect';
  await guest.locator('#netplay-disconnect').click();
  await Promise.all(pages.map(page => page.locator('#netplay-room-host:enabled').waitFor()));
  await restored();
  await host.evaluate(() => window.__restoreDiskSend());
  const remembered = await guest.evaluate(() => new Promise((resolve, reject) => {
    const request = indexedDB.open('copperline');
    request.onerror = () => reject(request.error);
    request.onsuccess = () => {
      const db = request.result;
      const record = db.transaction('roms').objectStore('roms').get('kick');
      record.onerror = () => { db.close(); reject(record.error); };
      record.onsuccess = () => { resolve({ label: record.result?.label, length: record.result?.rom.length,
        unchanged: record.result?.rom.every(byte => byte === 0) }); db.close(); };
    };
  }));
  assert.deepEqual(remembered, { label: 'guest-original.rom', length: 256 * 1024, unchanged: true });
  await host.setViewportSize({ width: 390, height: 844 });
  await host.locator('#netplay-open').click();
  await host.locator('#netplay-panel').screenshot({ path: `${output}/mobile.png` });
  assert.deepEqual(errors, []);
  if (shutdownNotices.length) console.log('WebKit native shutdown notices:', JSON.stringify(shutdownNotices));
  console.log('Invitations, verified host setup, repeated synchronized swaps/ejections, transfer cancellation, reconnect, guest restoration, diagnostics and responsive rendering passed');
} finally { await browser.close(); }
