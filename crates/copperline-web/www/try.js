// Source of truth for the copperline.dev/try page glue: published to the
// website repository by .github/workflows/wasm-demo.yml alongside the wasm
// bundle, so this JS and the WebEmu API always change together.
// Copperline in the browser: page glue around the wasm build.
// Loads the emulator module and the AROS ROMs in parallel, boots on click
// (the click also unlocks the AudioContext), then runs one
// requestAnimationFrame loop: step the core to the wall clock, blit the
// presentation buffer to the canvas, and post the frame's audio to the
// worklet. Everything is served from this site - no external requests.

import init, { WebEmu } from './pkg/copperline_web.js';
import { TelnetSession } from './serial-telnet.js';

const $ = (id) => document.getElementById(id);
const canvas = $('screen');
const ctx2d = canvas.getContext('2d');
const overlay = $('overlay');
const bootBtn = $('boot');
const loadStatus = $('load-status');
const statLine = $('stat');

// iOS's document picker only offers files whose extensions map to a
// system-known type: .bin, .zip and .gz are fine, but .rom, .adf and
// friends grey out, locking iPhone/iPad users out of their own dumps.
// Drop the accept filters there so every file stays selectable; desktop
// pickers keep the extension filter. (iPadOS reports itself as MacIntel,
// hence the touch-points check.)
if (
  /iPad|iPhone|iPod/.test(navigator.userAgent) ||
  (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1)
) {
  $('df0').removeAttribute('accept');
  $('kick').removeAttribute('accept');
}

const hasTouch = navigator.maxTouchPoints > 0 || 'ontouchstart' in window;
// Touches on the canvas are emulator input, never page gestures: no
// scrolling, no double-tap zoom, no long-press callout.
canvas.style.touchAction = 'none';
canvas.style.webkitUserSelect = 'none';
canvas.style.userSelect = 'none';

let wasm = null;
let emu = null;
let audioCtx = null;
let audioNode = null;
let queuedMs = 0;
let running = false;
let framesThisSecond = 0;
let lastStatUpdate = 0;

function setLoadStatus(text) {
  loadStatus.textContent = text;
}

async function fetchBytes(url, label) {
  const resp = await fetch(url);
  if (!resp.ok) throw new Error(`${label}: HTTP ${resp.status}`);
  return new Uint8Array(await resp.arrayBuffer());
}

// --- loading -------------------------------------------------------------
// The ROM (and optionally a disk) can be chosen before booting: the file
// pickers stash their bytes here until the boot click, and swap live once
// the machine is running.

let bootRom = null; // { rom, ext, label } - what the boot button will fit
let pendingDisk = null; // { bytes, name } - inserted right after boot
let df0Name = null; // what the page believes is in DF0, for bug reports

function refreshBootButton() {
  bootBtn.disabled = !(wasm && bootRom);
  bootBtn.textContent = bootRom && bootRom.label !== 'AROS' ? 'Boot Kickstart' : 'Boot AROS';
}

// Route picked or dropped Kickstart bytes: live-swap a running machine, or
// stash them for the boot button. The stash is updated on a live swap too,
// so a reboot fits the ROM chosen last, not the one from the original boot;
// a rejected image throws before the stash is touched and changes nothing.
function fitRom(bytes, label) {
  if (emu) emu.load_rom(bytes, undefined);
  bootRom = { rom: bytes, ext: null, label };
  refreshBootButton();
  setLoadStatus(
    emu ? `Kickstart loaded: ${label} - machine power-cycled` : `will boot ${label}`,
  );
}

// Route disk bytes from any source (picker, URL, drop): insert into a
// running machine, or stash them for the boot button to insert after boot.
function insertDisk(bytes, name) {
  if (emu) {
    emu.insert_floppy(0, bytes, name);
    setLoadStatus(`DF0: ${name} (write-protected)`);
    lastFddTrack = null; // desktop clears its track latch on insert too
    updateStatusDisks();
  } else {
    pendingDisk = { bytes, name };
    setLoadStatus(`DF0: ${name} (inserts at boot)`);
  }
  df0Name = name;
}

// A disk image can also come from a link: /try/?df0=<url> fetches it and
// inserts it at boot, so a bootable demo is one shareable URL, and the
// "DF0 from URL" button does the same for a pasted address. The fetch
// happens in the visitor's browser and nothing is proxied, so the host
// must allow cross-origin GETs (same-origin always works, archive.org
// does too).
//
// A Kickstart can come from a link too, but only from the page's own
// origin: ?kick=<path> fetches the ROM and fits it like the picker. The
// same-origin restriction is the copyright gate: Kickstart images are
// copyrighted, and a cross-origin ?kick= would only exist to share them.
// A same-origin path can never load a ROM the serving site does not
// already host, so the public page stays exactly as ROM-free as its
// server, while a self-hosted copy (a Docker image with a mounted ROM
// volume, an intranet install) can serve its owner's ROMs next to the
// page and boot them by URL.

// Sanity cap on fetched disk images; SCP flux dumps run tens of MB.
const DISK_URL_MAX_BYTES = 64 << 20;

// Display name from a fetched URL's path. decodeURIComponent throws on a
// malformed percent-escape (a literal "%" survives URL parsing), and a
// throw here would escape the fetch functions' error handling as an
// unhandled rejection; keep such a name undecoded instead.
function nameFromUrlPath(pathname, fallback) {
  const last = pathname.split('/').pop() || '';
  try {
    return decodeURIComponent(last) || fallback;
  } catch {
    return last || fallback;
  }
}
// Kickstart images are 256 or 512 KiB (the core rejects anything else);
// the cap only keeps a mislinked file from buffering unbounded.
const ROM_URL_MAX_BYTES = 4 << 20;

async function insertDiskFromUrl(url) {
  let parsed;
  try {
    parsed = new URL(url, location.href);
  } catch {
    setLoadStatus('disk URL: not a valid URL');
    return;
  }
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
    setLoadStatus('disk URL: only http(s) is supported');
    return;
  }
  const name = nameFromUrlPath(parsed.pathname, 'disk.adf');
  setLoadStatus(`fetching ${name}...`);
  try {
    const resp = await fetch(parsed.href);
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    if (Number(resp.headers.get('content-length') ?? 0) > DISK_URL_MAX_BYTES) {
      throw new Error('file too large');
    }
    const bytes = new Uint8Array(await resp.arrayBuffer());
    if (bytes.length > DISK_URL_MAX_BYTES) throw new Error('file too large');
    insertDisk(bytes, name);
  } catch (e) {
    // A TypeError is the opaque network/CORS failure; HTTP and size errors
    // speak for themselves.
    const hint =
      e instanceof TypeError
        ? ' - the host must allow cross-origin requests (CORS)'
        : '';
    setLoadStatus(`disk fetch failed: ${e.message ?? e}${hint}`);
  }
}

// A failed ROM URL would flash past: load() overwrites the status line with
// its own progress, and the AROS "ready" line follows. Remembering the
// failure lets that ready line carry it, so the user learns both what will
// boot and why their ?kick= did not take.
let romUrlProblem = null;

function romUrlFailed(message) {
  romUrlProblem = message;
  setLoadStatus(message);
}

async function fitRomFromUrl(url) {
  romUrlProblem = null;
  let parsed;
  try {
    parsed = new URL(url, location.href);
  } catch {
    romUrlFailed('Kickstart URL: not a valid URL');
    return;
  }
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
    romUrlFailed('Kickstart URL: only http(s) is supported');
    return;
  }
  if (parsed.origin !== location.origin) {
    romUrlFailed(
      "Kickstart URL: ROMs only load from this page's own site (same-origin)",
    );
    return;
  }
  const name = nameFromUrlPath(parsed.pathname, 'kickstart.rom');
  setLoadStatus(`fetching ${name}...`);
  let bytes;
  try {
    const resp = await fetch(parsed.href);
    // fetch follows redirects, and a same-origin path can redirect to a
    // CORS-enabled foreign host; the origin gate holds only if the bytes'
    // final origin is checked, not just the requested URL's.
    if (!resp.url || new URL(resp.url).origin !== location.origin) {
      throw new Error('redirected off this site');
    }
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    if (Number(resp.headers.get('content-length') ?? 0) > ROM_URL_MAX_BYTES) {
      throw new Error('file too large');
    }
    bytes = new Uint8Array(await resp.arrayBuffer());
    if (bytes.length > ROM_URL_MAX_BYTES) throw new Error('file too large');
  } catch (e) {
    romUrlFailed(`Kickstart fetch failed: ${e.message ?? e}`);
    return;
  }
  // Same failure label as the picker: the fetch worked, the image did not.
  try {
    fitRom(bytes, name);
  } catch (err) {
    romUrlFailed(`ROM load failed: ${err.message ?? err}`);
  }
}

async function load() {
  try {
    setLoadStatus('loading emulator...');
    wasm = await init();
    buildInfo = WebEmu.build_info?.() ?? null;
  } catch (e) {
    setLoadStatus(`failed to load the emulator: ${e.message ?? e}`);
    console.error(e);
    return;
  }
  try {
    setLoadStatus('loading AROS ROMs...');
    const [rom, ext] = await Promise.all([
      fetchBytes('./aros/aros-amiga-m68k-rom.bin', 'AROS ROM'),
      fetchBytes('./aros/aros-amiga-m68k-ext.bin', 'AROS extended ROM'),
    ]);
    // A Kickstart picked while the ROMs were downloading wins.
    if (!bootRom) {
      bootRom = { rom, ext, label: 'AROS' };
      // A disk that landed first (file picker or ?df0= fetch) keeps its
      // place in the status line, and a ?kick= failure rides along.
      const problem = romUrlProblem ? ` (${romUrlProblem})` : '';
      setLoadStatus(
        (pendingDisk
          ? `ready - DF0: ${pendingDisk.name} inserts at boot`
          : 'ready - boots the open-source AROS ROM') + problem,
      );
    }
  } catch (e) {
    setLoadStatus(
      `AROS ROMs failed to load (${e.message ?? e}) - load your own Kickstart to boot`,
    );
    console.error(e);
  }
  refreshBootButton();
  if (!bootBtn.disabled) bootBtn.focus();
}

// --- boot ----------------------------------------------------------------

async function boot() {
  bootBtn.disabled = true;
  try {
    // Fit the ROM into a fresh machine before anything else: a bad image
    // must abort the boot with the page still in its pre-boot state (emu
    // stays null, so the pickers keep updating bootRom for the retry).
    const machine = new WebEmu();
    machine.load_rom(bootRom.rom, bootRom.ext ?? undefined);

    // A reboot after an emulator error builds a new audio stack; close the
    // previous one so it cannot keep playing alongside.
    if (audioCtx) {
      audioNode?.disconnect();
      audioCtx.close().catch(() => {});
      audioNode = null;
    }
    audioCtx = new AudioContext({ sampleRate: 44100 });
    await audioCtx.audioWorklet.addModule('./audio-worklet.js');
    audioNode = new AudioWorkletNode(audioCtx, 'copperline-audio', {
      outputChannelCount: [2],
    });
    audioNode.port.onmessage = (e) => {
      if (typeof e.data?.queuedMs === 'number') queuedMs = e.data.queuedMs;
    };
    audioNode.connect(audioCtx.destination);
    // Autoplay policies can leave the context suspended, and resume() may
    // not settle without a qualifying gesture; never let that block the
    // boot. Video runs regardless, and the next real interaction unlocks
    // the sound.
    audioCtx.resume().catch(() => {});
    if (audioCtx.state !== 'running') {
      const unlock = () => audioCtx.resume().catch(() => {});
      window.addEventListener('pointerdown', unlock, { once: true });
      window.addEventListener('keydown', unlock, { once: true });
    }

    // A fresh machine boots with an empty drive: DF0 holds the pending disk
    // or nothing, never a name left over from before the reboot (a crash
    // consumes the pending disk, and the bug report reads df0Name).
    if (pendingDisk) {
      machine.insert_floppy(0, pendingDisk.bytes, pendingDisk.name);
    }
    df0Name = pendingDisk?.name ?? null;
    pendingDisk = null;
    machine.set_volume_percent(Number($('vol').value));
    if (floppySoundsToggle) machine.set_floppy_sounds(floppySoundsToggle.checked);
    else if (configFloppySounds !== null) machine.set_floppy_sounds(configFloppySounds);
    if (floppySpeed !== null) machine.set_floppy_speed(floppySpeed);
    emu = machine;
    window.__emu = emu; // for debugging/automation
    lastFddTrack = null; // a new machine starts the track latch over
    updateStatusDisks();

    // Leave a fresh status behind: the old line ("inserts at boot", an
    // earlier failure) would otherwise go stale into any bug report filed
    // while the machine runs.
    setLoadStatus(
      `booted ${bootRom.label}` +
        (df0Name ? ` - DF0: ${df0Name} (write-protected)` : ''),
    );

    overlay.style.display = 'none';
    showBugLink(false);
    running = true;
    requestAnimationFrame(tick);
  } catch (e) {
    setLoadStatus(`boot failed: ${e.message ?? e}`);
    bootBtn.disabled = false;
    showBugLink(true);
    console.error(e);
  }
}

// --- main loop -----------------------------------------------------------

function maxFramesForQueue() {
  // The audio clock is the master: when the worklet has plenty queued, skip
  // stepping this tick (the pacer forgives deficits past 100 ms, so this
  // locks production to the audio device's consumption rate). Otherwise step
  // freely to the wall clock - the burst cap only bounds a single tick's
  // catch-up work after rAF throttling.
  return queuedMs > 150 ? 0 : 5;
}

function tick(nowMs) {
  if (!running) return;
  try {
    framesThisSecond += emu.run(nowMs, maxFramesForQueue());
  } catch (e) {
    running = false;
    setLoadStatus(`emulator error: ${e.message ?? e}`);
    overlay.style.display = '';
    // Drop the wedged machine and re-arm the boot button: the pickers go
    // back to stashing (never a live swap into a crashed instance, which
    // may have panicked) and a fresh boot rebuilds from the stash.
    emu = null;
    refreshBootButton();
    showBugLink(true);
    console.error(e);
    return;
  }

  const rows = emu.present_rows();
  if (rows > 0) {
    // The presentation size follows the emulated display (the cropped TV
    // aperture for a standard PAL screen, the full overscan framebuffer
    // otherwise), so track both dimensions every frame.
    const width = emu.present_width();
    if (canvas.width !== width || canvas.height !== rows) {
      canvas.width = width;
      canvas.height = rows;
    }
    // The view must be rebuilt every frame: wasm memory may grow and the
    // present buffer may reallocate.
    const view = new Uint8ClampedArray(
      wasm.memory.buffer,
      emu.present_ptr(),
      width * rows * 4,
    );
    ctx2d.putImageData(new ImageData(view, width, rows), 0, 0);
  }

  const audio = emu.take_audio();
  if (audio.length > 0 && audioNode) {
    audioNode.port.postMessage(audio, [audio.buffer]);
  }

  pumpSerial();
  updateStatusLeds();

  if (nowMs - lastStatUpdate >= 1000) {
    statLine.textContent =
      `${framesThisSecond} fps | ` +
      `${emu.emulated_seconds().toFixed(1)}s emulated | ` +
      `audio ${queuedMs.toFixed(0)} ms`;
    framesThisSecond = 0;
    lastStatUpdate = nowMs;
    updateStatusDisks();
  }
  requestAnimationFrame(tick);
}

document.addEventListener('visibilitychange', () => {
  if (!audioCtx) return;
  if (document.hidden) audioCtx.suspend();
  else audioCtx.resume();
});

// --- serial / BBS bridge ---------------------------------------------------
// Optional page feature: a shell that provides #serial-url (text input) and
// #serial-connect (button) gets the Amiga serial port bridged to a WebSocket
// (a websockify-style gateway in front of a telnet BBS or any TCP service).
// #serial-status (a status span) and #serial-raw (a checkbox that bypasses
// the telnet layer, for gateways to non-telnet services) are optional too.
// Pages without the elements are untouched - the pump still drains the
// guest's bounded serial buffer every frame, it just goes nowhere.
//
// Telnet-mode connections follow the guest's DTR line the way a modem
// follows its terminal. Dialling before the terminal is up would scroll the
// BBS greeting into a UART nobody is reading and leak boot-ROM chatter to
// the BBS as phantom keypresses (a stray newline at a login prompt starts
// the new-user flow), so Connect defers the dial until the terminal has
// opened the serial port, and a live session hangs up when the guest drops
// the line (terminal exit, reboot, power cycle) - then re-arms, so the
// next boot of the terminal reconnects by itself. Raw mode is ungated, for
// byte services and guests that never drive CIA-B DTR.
//
// The dial waits for the line to be READY (DTR asserted) and QUIET (no
// guest transmit) continuously for a guard period, not for a mere DTR
// edge: AROS raises DTR for a couple of seconds during early boot while
// its kernel debug output streams to the serial port, and dialling into
// that window is exactly the reported failure. The debug burst fails both
// conditions; a terminal holds DTR silently and passes.

const serialUrlInput = $('serial-url');
const serialConnectBtn = $('serial-connect');
const serialStatus = $('serial-status');
const serialRawToggle = $('serial-raw');

let serialWs = null;
let serialTelnet = null;
// Connect clicked before the guest's line was ready: dial once it is.
let serialWaitingDtr = false;
// The open session is DTR-gated (telnet mode): drop of the line hangs up.
let serialDtrGated = false;
// Emulated-time instant the guest's line last became ready-and-quiet;
// pushed forward by every disqualifier (DTR down, guest transmit) the
// pump sees. Emulated seconds, not wall time: in a throttled background
// tab the machine runs far slower than the wall clock, and a wall-time
// guard would fire inside a stretched boot transient.
let serialLineReadySince = 0;
// The line must hold ready-and-quiet this long (emulated seconds) before
// a deferred dial fires. The AROS boot-debug burst holds DTR for ~1.75s
// while transmitting; 3s of held silence clears it with margin and still
// feels immediate once a terminal is really up.
const SERIAL_DIAL_GUARD_EMU_S = 3.0;
// Inbound chunks the guest's UART has not had room for yet. The UART
// consumes at the emulated baud rate, so a fast sender (a file download)
// backlogs here rather than ballooning inside the wasm heap.
let serialRxQueue = [];
// Stop feeding the guest while its input backlog exceeds this many bytes;
// the queue above absorbs the difference, a frame at a time.
const SERIAL_BACKLOG_LIMIT = 32768;

// The guest's view of the serial DTR line. A powered-off machine has the
// line down; a wasm bundle older than serial_dtr() reports it up, which
// disengages the gate (see serialLineSettled) rather than waiting forever.
function guestDtr() {
  if (!emu) return false;
  if (typeof emu.serial_dtr !== 'function') return true;
  return emu.serial_dtr();
}

function emuSeconds() {
  return emu && typeof emu.emulated_seconds === 'function' ? emu.emulated_seconds() : 0;
}

// Ready-and-quiet for the full guard period, judged from what the pump
// has observed. Only meaningful while the machine is emulating, which is
// when the pump keeps serialLineReadySince honest.
function serialLineSettled() {
  if (!emu) return false;
  if (typeof emu.serial_dtr !== 'function') return true; // pre-gate wasm
  return emu.serial_dtr() && emuSeconds() - serialLineReadySince >= SERIAL_DIAL_GUARD_EMU_S;
}

function setSerialStatus(text) {
  if (serialStatus) serialStatus.textContent = text;
}

function serialTeardown() {
  if (serialWs) {
    // Neuter the handlers first: close() fires onclose asynchronously, and
    // a stale handler would clobber the status of a connection made later.
    serialWs.onopen = serialWs.onclose = serialWs.onerror = serialWs.onmessage = null;
    serialWs.close();
    serialWs = null;
  }
  serialTelnet = null;
  serialDtrGated = false;
  serialRxQueue = [];
}

function serialDisconnect(status) {
  serialTeardown();
  serialWaitingDtr = false;
  if (serialConnectBtn) serialConnectBtn.textContent = 'Connect';
  setSerialStatus(status);
}

// DTR dropped mid-session: hang up like a modem losing its terminal, but
// keep the visitor's intent armed - when the line settles again (the
// terminal is back after a reboot) the dial repeats by itself.
function serialHangup() {
  serialTeardown();
  serialWaitingDtr = true;
  if (serialConnectBtn) serialConnectBtn.textContent = 'Cancel';
  setSerialStatus('terminal closed the serial port - reconnects when it is back...');
}

// Open the socket now, with whatever is in the URL box. Reached directly
// from a Connect click when the guest is ready (or in raw mode), or from
// the pump when a deferred connect sees DTR rise.
function serialOpen() {
  const url = serialUrlInput?.value?.trim();
  if (!url) {
    serialDisconnect('enter a ws:// or wss:// gateway URL');
    return;
  }
  let ws;
  try {
    ws = new WebSocket(url);
  } catch (e) {
    serialDisconnect(`bad URL: ${e.message ?? e}`);
    return;
  }
  ws.binaryType = 'arraybuffer';
  serialWs = ws;
  serialTelnet = serialRawToggle?.checked ? null : new TelnetSession();
  serialDtrGated = serialTelnet !== null;
  serialRxQueue = [];
  if (serialConnectBtn) serialConnectBtn.textContent = 'Disconnect';
  setSerialStatus('connecting...');
  ws.onopen = () => setSerialStatus(`connected (${serialTelnet ? 'telnet' : 'raw'})`);
  ws.onclose = () => serialDisconnect('disconnected');
  ws.onerror = () => setSerialStatus('connection failed');
  ws.onmessage = (e) => {
    let bytes = new Uint8Array(e.data);
    if (serialTelnet) {
      const { data, reply } = serialTelnet.receive(bytes);
      if (reply.length && ws.readyState === WebSocket.OPEN) ws.send(reply);
      bytes = data;
    }
    if (bytes.length) serialRxQueue.push(bytes);
  };
}

function serialConnect() {
  const url = serialUrlInput?.value?.trim();
  if (!url) {
    setSerialStatus('enter a ws:// or wss:// gateway URL');
    return;
  }
  if (!serialRawToggle?.checked && !serialLineSettled()) {
    // Telnet mode with no settled terminal yet: arm the deferred dial
    // instead of connecting into the void. The pump completes it once
    // the guest's line has been ready and quiet for the guard period.
    serialWaitingDtr = true;
    if (serialConnectBtn) serialConnectBtn.textContent = 'Cancel';
    setSerialStatus('waiting for the terminal - boot the terminal disk, connects when it is ready...');
    return;
  }
  serialOpen();
}

if (serialConnectBtn) {
  serialConnectBtn.addEventListener('click', () => {
    if (serialWaitingDtr) serialDisconnect('cancelled');
    else if (serialWs) serialDisconnect('disconnected');
    else serialConnect();
  });
}

function pumpSerial() {
  if (!emu) return;
  // Guest -> socket. Drained every frame even with no socket connected, so
  // the guest's bounded output buffer (which also carries boot-ROM debug
  // chatter) never overflows into dropped bytes mid-session.
  const out = emu.serial_take();
  // Any disqualifier - line down, guest transmit, or the emulated clock
  // rewinding (a power cycle) - restarts the ready-and-quiet guard clock.
  // Checked here rather than on a timer because the line can only change
  // while the machine is emulating, and emulation is what drives this
  // pump.
  const nowEmu = emuSeconds();
  if (!guestDtr() || out.length || nowEmu < serialLineReadySince) {
    serialLineReadySince = nowEmu;
  }
  // Deferred dial: the guest's line has settled, so connect now.
  if (serialWaitingDtr && serialLineSettled()) {
    serialWaitingDtr = false;
    serialOpen();
  }
  // Modem-style hangup (and automatic re-arm): the guest dropped DTR, so
  // the session ends before boot chatter can reach the far end as
  // phantom input.
  if (serialDtrGated && serialWs && !guestDtr()) {
    serialHangup();
  }
  if (out.length && serialWs?.readyState === WebSocket.OPEN) {
    serialWs.send(serialTelnet ? serialTelnet.send(out) : out);
  }
  // Socket -> guest, paced by the UART's own consumption.
  while (serialRxQueue.length && emu.serial_input_backlog() < SERIAL_BACKLOG_LIMIT) {
    emu.serial_send(serialRxQueue.shift());
  }
}

// --- joystick (port 2) -----------------------------------------------------
// The toggle cycles off -> keys -> cd32 (-> touch on touch screens). Keys
// is a two-button stick, the desktop frontend's FS-UAE-compatible mapping
// plus left-hand fire keys: cursor keys for directions, Right Ctrl /
// Right Alt or Left Ctrl for fire, Left Alt for the second button
// (left-hand fire pairs with the right-hand arrows, and compact keyboards
// often lack the right-side modifiers). Cd32 adds the pad extras on
// C/X/D/S/Enter/Z/A. The split matters for typing-heavy guests (a BBS
// terminal): keys mode leaves Enter and the letters on the Amiga
// keyboard, so only a CD32 title needs the full capture. While a mode is
// on, its mapped keys drive the port-2 joystick instead of reaching the
// Amiga keyboard. Touch turns the canvas into a pad (see the touch
// section). The page shell can preset the mode (data-default on the
// toggle, or the config file's "joy") and ?joy=off|keys|cd32|touch
// overrides per link.

const JOY_KEYS_TWO_BUTTON = {
  ArrowUp: 'up',
  ArrowDown: 'down',
  ArrowLeft: 'left',
  ArrowRight: 'right',
  ControlRight: 'fireCtrl',
  AltRight: 'fireAlt',
  ControlLeft: 'fireLCtrl',
  AltLeft: 'blueLAlt',
};
const JOY_KEYS_CD32 = {
  ...JOY_KEYS_TWO_BUTTON,
  KeyC: 'red',
  KeyX: 'blue',
  KeyD: 'green',
  KeyS: 'yellow',
  Enter: 'play',
  NumpadEnter: 'play',
  KeyZ: 'rwd',
  KeyA: 'ffw',
};
const JOY_MODES = hasTouch ? ['off', 'keys', 'cd32', 'touch'] : ['off', 'keys', 'cd32'];
let joyMode = 'off';
const joyHeld = {};

function applyJoystick() {
  const h = joyHeld;
  emu.set_joystick_port2(
    !!h.up,
    !!h.down,
    !!h.left,
    !!h.right,
    !!(h.fireCtrl || h.fireAlt || h.fireLCtrl || h.red),
    !!(h.blue || h.blueLAlt),
  );
  emu.set_cd32_buttons_port2(!!h.play, !!h.rwd, !!h.ffw, !!h.green, !!h.yellow);
}

// Returns true when the key was captured for the joystick.
function joystickKey(code, pressed) {
  const map =
    joyMode === 'keys'
      ? JOY_KEYS_TWO_BUTTON
      : joyMode === 'cd32'
        ? JOY_KEYS_CD32
        : null;
  const control = map?.[code];
  if (!control) return false;
  joyHeld[control] = pressed;
  applyJoystick();
  return true;
}

function setJoyMode(mode) {
  joyMode = mode;
  $('joy').textContent = `Joystick: ${joyMode}`;
  if (fsUi) fsUi.joy.textContent = `Joystick: ${joyMode}`;
  for (const k of Object.keys(joyHeld)) joyHeld[k] = false;
  resetTouchState();
  if (emu) {
    applyJoystick();
    emu.set_cd32_buttons_port2(false, false, false, false, false);
  }
}

// Cycles the mode; wired to the control-bar button and to the fullscreen
// overlay's copy of it, which stay in step.
function cycleJoyMode() {
  setJoyMode(JOY_MODES[(JOY_MODES.indexOf(joyMode) + 1) % JOY_MODES.length]);
}

$('joy').addEventListener('click', cycleJoyMode);

// --- keyboard ------------------------------------------------------------

window.addEventListener('keydown', (e) => {
  if (!emu || !running || e.repeat) return;
  if (joystickKey(e.code, true) || emu.key_event(e.code, true)) e.preventDefault();
});
window.addEventListener('keyup', (e) => {
  if (!emu || !running) return;
  if (joystickKey(e.code, false) || emu.key_event(e.code, false)) e.preventDefault();
});

// --- mouse ---------------------------------------------------------------
// Unlocked: the cursor drives the Amiga pointer through position deltas
// (Workbench-friendly). Click to pointer-lock for relative motion (games);
// Esc releases the lock, as the browser enforces.

let lastPos = null;
// Emulator pixels per CSS pixel. Fullscreen letterboxes the canvas
// (object-fit: contain), so there the displayed scale is the larger of the
// two axis ratios; in the normal layout the bitmap fills the element.
const cssToEmu = () =>
  isFullscreen()
    ? Math.max(canvas.width / canvas.clientWidth, canvas.height / canvas.clientHeight)
    : canvas.width / canvas.clientWidth;

canvas.addEventListener('mousedown', (e) => {
  if (!emu || !running) return;
  e.preventDefault();
  if (document.pointerLockElement !== canvas && e.button === 0) {
    canvas.requestPointerLock?.();
  }
  emu.mouse_button(e.button, true);
});
window.addEventListener('mouseup', (e) => {
  if (!emu || !running) return;
  emu.mouse_button(e.button, false);
});
canvas.addEventListener('contextmenu', (e) => e.preventDefault());
window.addEventListener('mousemove', (e) => {
  if (!emu || !running) return;
  const scale = cssToEmu();
  if (document.pointerLockElement === canvas) {
    emu.mouse_delta(e.movementX * scale, e.movementY * scale);
    lastPos = null;
  } else if (e.target === canvas) {
    if (lastPos) {
      emu.mouse_delta((e.clientX - lastPos.x) * scale, (e.clientY - lastPos.y) * scale);
    }
    lastPos = { x: e.clientX, y: e.clientY };
  } else {
    lastPos = null;
  }
});
document.addEventListener('pointerlockchange', () => {
  lastPos = null;
});

// --- touch ---------------------------------------------------------------
// The canvas is a trackpad on touch screens: the Amiga pointer only takes
// relative motion, so absolute finger positions cannot map to it. One
// finger drags the pointer, a quick tap left-clicks, holding still for a
// moment picks the button up for a drag (icons, windows), and a second
// finger holds the right button for Intuition menus. With the joystick
// toggle in touch mode the canvas is a pad instead: the left half is a
// floating eight-way stick, the right half is fire.

const TAP_MAX_MS = 250;
const TAP_SLOP_CSS_PX = 12;
const CLICK_HOLD_MS = 90;
const LONG_PRESS_MS = 400;
const STICK_DEADZONE_CSS_PX = 14;
const STICK_RANGE_CSS_PX = 40;
const STICK_DIAGONAL = 0.383; // sin(22.5 deg): eight-way sectors

let padTouch = null; // primary trackpad finger: {id, x, y, start, moved}
let padDragging = false; // long-press engaged, LMB held until the finger lifts
let padRmbTouchId = null; // second finger, RMB held while it is down
let longPressTimer = 0;
let stickTouch = null; // stick finger: {id, ox, oy}
let stickDirs = { up: false, down: false, left: false, right: false };
let fireTouchId = null;

function resetTouchState() {
  clearTimeout(longPressTimer);
  if (emu) {
    if (padDragging) emu.mouse_button(0, false);
    if (padRmbTouchId !== null) emu.mouse_button(2, false);
  }
  padTouch = null;
  padDragging = false;
  padRmbTouchId = null;
  stickTouch = null;
  stickDirs = { up: false, down: false, left: false, right: false };
  fireTouchId = null;
  updateTouchJoyUi();
}

function applyTouchJoystick() {
  emu.set_joystick_port2(
    stickDirs.up,
    stickDirs.down,
    stickDirs.left,
    stickDirs.right,
    fireTouchId !== null,
    false,
  );
}

canvas.addEventListener(
  'touchstart',
  (e) => {
    if (!emu || !running) return;
    e.preventDefault();
    if (joyMode === 'touch') return touchJoyStart(e);
    const now = performance.now();
    for (const t of e.changedTouches) {
      if (padTouch === null) {
        padTouch = { id: t.identifier, x: t.clientX, y: t.clientY, start: now, moved: 0 };
        clearTimeout(longPressTimer);
        longPressTimer = setTimeout(() => {
          // emu can be gone by now: an emulator error drops the machine.
          if (emu && padTouch && padTouch.moved < TAP_SLOP_CSS_PX && padRmbTouchId === null) {
            padDragging = true;
            emu.mouse_button(0, true);
            navigator.vibrate?.(15);
          }
        }, LONG_PRESS_MS);
      } else if (padRmbTouchId === null) {
        padRmbTouchId = t.identifier;
        clearTimeout(longPressTimer);
        emu.mouse_button(2, true);
      }
    }
  },
  { passive: false },
);

canvas.addEventListener(
  'touchmove',
  (e) => {
    if (!emu || !running) return;
    e.preventDefault();
    if (joyMode === 'touch') return touchJoyMove(e);
    for (const t of e.changedTouches) {
      if (padTouch && t.identifier === padTouch.id) {
        const scale = cssToEmu();
        const dx = t.clientX - padTouch.x;
        const dy = t.clientY - padTouch.y;
        padTouch.moved += Math.abs(dx) + Math.abs(dy);
        padTouch.x = t.clientX;
        padTouch.y = t.clientY;
        emu.mouse_delta(dx * scale, dy * scale);
      }
    }
  },
  { passive: false },
);

function onTouchEnd(e) {
  if (!emu || !running) return;
  e.preventDefault();
  if (joyMode === 'touch') return touchJoyEnd(e);
  const now = performance.now();
  for (const t of e.changedTouches) {
    if (padTouch && t.identifier === padTouch.id) {
      clearTimeout(longPressTimer);
      if (padDragging) {
        emu.mouse_button(0, false);
        padDragging = false;
      } else if (
        e.type === 'touchend' &&
        now - padTouch.start < TAP_MAX_MS &&
        padTouch.moved < TAP_SLOP_CSS_PX
      ) {
        emu.mouse_button(0, true);
        setTimeout(() => emu?.mouse_button(0, false), CLICK_HOLD_MS);
      }
      padTouch = null;
    } else if (t.identifier === padRmbTouchId) {
      emu.mouse_button(2, false);
      padRmbTouchId = null;
    }
  }
}
canvas.addEventListener('touchend', onTouchEnd, { passive: false });
canvas.addEventListener('touchcancel', onTouchEnd, { passive: false });

// The touch-joystick overlay: stick base and knob on the left, fire pad on
// the right. Built lazily so desktop sessions never touch the DOM; inline
// styles keep the page shell independent of the glue.
let touchJoyUi = null;

function ensureTouchJoyUi() {
  if (touchJoyUi) return touchJoyUi;
  const shell = $('shell');
  const mk = (size) => {
    const el = document.createElement('div');
    el.style.cssText =
      'position:absolute;pointer-events:none;border-radius:50%;z-index:2;' +
      'border:1px solid rgba(255,255,255,0.4);background:rgba(255,255,255,0.08);' +
      'transform:translate(-50%,-50%);visibility:hidden;' +
      'display:flex;align-items:center;justify-content:center;';
    el.style.width = `${size}px`;
    el.style.height = `${size}px`;
    shell.appendChild(el);
    return el;
  };
  const base = mk(96);
  const knob = mk(44);
  knob.style.background = 'rgba(255,255,255,0.25)';
  const fire = mk(72);
  fire.textContent = 'FIRE';
  fire.style.font = '600 12px "IBM Plex Mono", ui-monospace, monospace';
  fire.style.color = 'rgba(255,255,255,0.6)';
  fire.style.letterSpacing = '0.1em';
  touchJoyUi = { base, knob, fire };
  return touchJoyUi;
}

// Rest positions while no finger is down, as fractions of the shell.
function placeTouchJoyAtRest(ui) {
  ui.base.style.left = '22%';
  ui.base.style.top = '72%';
  ui.knob.style.left = '22%';
  ui.knob.style.top = '72%';
  ui.fire.style.left = '80%';
  ui.fire.style.top = '72%';
}

function updateTouchJoyUi() {
  if (!touchJoyUi && joyMode !== 'touch') return;
  const ui = ensureTouchJoyUi();
  const on = joyMode === 'touch';
  if (on) placeTouchJoyAtRest(ui);
  ui.base.style.visibility = on ? 'visible' : 'hidden';
  ui.knob.style.visibility = on ? 'visible' : 'hidden';
  ui.fire.style.visibility = on ? 'visible' : 'hidden';
  ui.fire.style.background = 'rgba(255,255,255,0.08)';
}

function shellPos(clientX, clientY) {
  const r = $('shell').getBoundingClientRect();
  return { x: clientX - r.left, y: clientY - r.top };
}

function touchJoyStart(e) {
  const rect = canvas.getBoundingClientRect();
  const ui = ensureTouchJoyUi();
  for (const t of e.changedTouches) {
    const leftHalf = t.clientX < rect.left + rect.width / 2;
    if (leftHalf && stickTouch === null) {
      stickTouch = { id: t.identifier, ox: t.clientX, oy: t.clientY };
      const p = shellPos(t.clientX, t.clientY);
      ui.base.style.left = `${p.x}px`;
      ui.base.style.top = `${p.y}px`;
      ui.knob.style.left = `${p.x}px`;
      ui.knob.style.top = `${p.y}px`;
    } else if (!leftHalf && fireTouchId === null) {
      fireTouchId = t.identifier;
      ui.fire.style.background = 'rgba(255,255,255,0.3)';
      applyTouchJoystick();
    }
  }
}

function touchJoyMove(e) {
  if (stickTouch === null) return;
  const ui = ensureTouchJoyUi();
  for (const t of e.changedTouches) {
    if (t.identifier !== stickTouch.id) continue;
    const dx = t.clientX - stickTouch.ox;
    const dy = t.clientY - stickTouch.oy;
    const dist = Math.hypot(dx, dy);
    const clamp = dist > STICK_RANGE_CSS_PX ? STICK_RANGE_CSS_PX / dist : 1;
    const origin = shellPos(stickTouch.ox, stickTouch.oy);
    ui.knob.style.left = `${origin.x + dx * clamp}px`;
    ui.knob.style.top = `${origin.y + dy * clamp}px`;
    const dirs = { up: false, down: false, left: false, right: false };
    if (dist >= STICK_DEADZONE_CSS_PX) {
      const ux = dx / dist;
      const uy = dy / dist;
      dirs.right = ux > STICK_DIAGONAL;
      dirs.left = ux < -STICK_DIAGONAL;
      dirs.down = uy > STICK_DIAGONAL;
      dirs.up = uy < -STICK_DIAGONAL;
    }
    if (
      dirs.up !== stickDirs.up ||
      dirs.down !== stickDirs.down ||
      dirs.left !== stickDirs.left ||
      dirs.right !== stickDirs.right
    ) {
      stickDirs = dirs;
      applyTouchJoystick();
    }
  }
}

function touchJoyEnd(e) {
  for (const t of e.changedTouches) {
    if (stickTouch && t.identifier === stickTouch.id) {
      stickTouch = null;
      stickDirs = { up: false, down: false, left: false, right: false };
      applyTouchJoystick();
      updateTouchJoyUi();
    } else if (t.identifier === fireTouchId) {
      fireTouchId = null;
      if (touchJoyUi) touchJoyUi.fire.style.background = 'rgba(255,255,255,0.08)';
      applyTouchJoystick();
    }
  }
}

// --- controls ------------------------------------------------------------

$('df0').addEventListener('change', async (e) => {
  const file = e.target.files[0];
  e.target.value = '';
  if (!file) return;
  try {
    insertDisk(new Uint8Array(await file.arrayBuffer()), file.name);
  } catch (err) {
    setLoadStatus(`insert failed: ${err.message ?? err}`);
  }
});

$('kick').addEventListener('change', async (e) => {
  const file = e.target.files[0];
  e.target.value = '';
  if (!file) return;
  try {
    fitRom(new Uint8Array(await file.arrayBuffer()), file.name);
  } catch (err) {
    setLoadStatus(`ROM load failed: ${err.message ?? err}`);
  }
});

$('eject').addEventListener('click', () => {
  if (!emu) return;
  try {
    emu.eject_floppy(0);
    df0Name = null;
    setLoadStatus('DF0 ejected');
    updateStatusDisks();
  } catch (err) {
    setLoadStatus(`${err.message ?? err}`);
  }
});

$('reset').addEventListener('click', () => {
  if (!emu) return;
  try {
    emu.reset();
    lastFddTrack = null; // desktop clears its track latch on reset too
    setLoadStatus('machine reset');
  } catch (err) {
    setLoadStatus(`reset failed: ${err.message ?? err}`);
  }
});

// --- fullscreen ------------------------------------------------------------
// iPhone Safari has no element fullscreen (only <video> goes fullscreen
// there), so the button falls back to pinning the shell over the page:
// Safari's own chrome stays, but the page furniture goes. Either way the
// control bar ends up off screen, so while fullscreen the shell carries a
// small overlay with the two controls that matter mid-session: the joystick
// toggle and Exit.

const shell = $('shell');
let cssFullscreen = false;
let fsUi = null; // { bar, joy } - built lazily, like the touch-joystick UI

function isFullscreen() {
  return document.fullscreenElement !== null || cssFullscreen;
}

function ensureFsUi() {
  if (fsUi) return fsUi;
  const bar = document.createElement('div');
  // The top-right corner sits in the letterbox in any orientation; the
  // safe-area offsets keep the buttons clear of notches and rounded corners.
  bar.style.cssText =
    'position:absolute;z-index:3;display:none;gap:0.5rem;' +
    'top:calc(0.6rem + env(safe-area-inset-top));' +
    'right:calc(0.6rem + env(safe-area-inset-right));';
  const mk = (label) => {
    const b = document.createElement('button');
    b.textContent = label;
    b.style.cssText =
      'padding:0.5rem 0.9rem;border-radius:8px;cursor:pointer;' +
      'border:1px solid rgba(255,255,255,0.35);' +
      'background:rgba(10,13,22,0.6);color:rgba(255,255,255,0.85);' +
      'font:600 0.85rem "IBM Plex Mono",ui-monospace,monospace;' +
      'touch-action:manipulation;-webkit-tap-highlight-color:transparent;';
    bar.appendChild(b);
    return b;
  };
  const joy = mk(`Joystick: ${joyMode}`);
  joy.addEventListener('click', cycleJoyMode);
  const exit = mk('Exit');
  exit.addEventListener('click', exitFullscreen);
  shell.appendChild(bar);
  fsUi = { bar, joy };
  return fsUi;
}

function updateFsUi() {
  if (!isFullscreen()) {
    if (fsUi) fsUi.bar.style.display = 'none';
    return;
  }
  const ui = ensureFsUi();
  ui.joy.textContent = `Joystick: ${joyMode}`;
  ui.bar.style.display = 'flex';
}

// The pinned fallback is plain inline styles so it works with any page
// shell. The z-index clears the page's fixed overlays (the scanline layer
// sits at 9999); real fullscreen renders above them via the top layer.
const CSS_FS_SHELL = {
  position: 'fixed',
  inset: '0',
  zIndex: '10000',
  border: 'none',
  borderRadius: '0',
};
const CSS_FS_CANVAS = { width: '100%', height: '100%', objectFit: 'contain' };

function setStyles(el, styles, on) {
  for (const k of Object.keys(styles)) el.style[k] = on ? styles[k] : '';
}

function enterCssFullscreen() {
  cssFullscreen = true;
  setStyles(shell, CSS_FS_SHELL, true);
  setStyles(canvas, CSS_FS_CANVAS, true);
  document.documentElement.style.overflow = 'hidden';
  updateFsUi();
}

function exitCssFullscreen() {
  cssFullscreen = false;
  setStyles(shell, CSS_FS_SHELL, false);
  setStyles(canvas, CSS_FS_CANVAS, false);
  document.documentElement.style.overflow = '';
  updateFsUi();
}

function exitFullscreen() {
  if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
  else exitCssFullscreen();
}

$('fullscreen').addEventListener('click', () => {
  if (document.fullscreenEnabled && shell.requestFullscreen) {
    shell.requestFullscreen().catch(enterCssFullscreen);
  } else {
    enterCssFullscreen();
  }
});

// Covers Esc and any other browser-initiated exit from real fullscreen.
document.addEventListener('fullscreenchange', updateFsUi);

$('vol').addEventListener('input', (e) => {
  if (emu) emu.set_volume_percent(Number(e.target.value));
});

// Optional in the page shell: a checkbox #floppy-sounds toggles the
// synthesized drive sounds (motor hum, head-step clicks, read hiss).
// Without the element the sounds stay on, as before; the checkbox's
// initial state is applied at boot, so a shell can default them off.
const floppySoundsToggle = $('floppy-sounds');
// The config file's floppy_sounds on a shell without the checkbox: stashed
// here and applied at boot.
let configFloppySounds = null;
floppySoundsToggle?.addEventListener('change', () => {
  if (emu) emu.set_floppy_sounds(floppySoundsToggle.checked);
});

// The floppy drive speed control, always visible: a page shell can host
// its own <select id="floppy-speed"> (option values 100/200/400/800 for
// percent, 0 for turbo) wherever its control bar wants it; without one
// the control builds itself directly below the canvas shell with its own
// styling, like the status strip. Applied at boot and live on change; a
// ?fdspeed= link overrides the initial choice.
const FLOPPY_SPEEDS = [100, 200, 400, 800, 0];
const FLOPPY_SPEED_LABELS = { 100: '100%', 200: '200%', 400: '400%', 800: '800%', 0: 'Turbo' };
function buildFloppySpeedControl() {
  const row = document.createElement('label');
  row.style.cssText =
    'display:inline-flex;align-items:center;gap:0.45rem;margin:0.4rem 0;' +
    'font:600 0.8rem "IBM Plex Mono",ui-monospace,monospace;' +
    'color:rgba(255,255,255,0.75);';
  row.appendChild(document.createTextNode('Floppy speed'));
  const sel = document.createElement('select');
  sel.style.cssText =
    'padding:0.15rem 0.4rem;border-radius:6px;cursor:pointer;' +
    'border:1px solid rgba(255,255,255,0.35);' +
    'background:rgba(10,13,22,0.6);color:rgba(255,255,255,0.85);' +
    'font:inherit;';
  for (const value of FLOPPY_SPEEDS) {
    const option = document.createElement('option');
    option.value = String(value);
    option.textContent = FLOPPY_SPEED_LABELS[value];
    sel.appendChild(option);
  }
  row.appendChild(sel);
  shell.insertAdjacentElement('afterend', row);
  return sel;
}
const floppySpeedSel = $('floppy-speed') ?? buildFloppySpeedControl();
let floppySpeed = null; // null = leave the emulator at its default (100%)
function setFloppySpeed(value) {
  if (!FLOPPY_SPEEDS.includes(value)) return;
  floppySpeed = value;
  floppySpeedSel.value = String(value);
  if (emu) emu.set_floppy_speed(value);
}
floppySpeedSel.addEventListener('change', () => {
  setFloppySpeed(Number(floppySpeedSel.value));
});

// --- status bar --------------------------------------------------------
// Front-panel status strip mirroring the desktop status bar's LED block:
// PWR/FDD LEDs (HDD/CD only on machines fitted with the drive), the
// floppy track counter, and the inserted disk name per connected drive.
// Built lazily at first boot, like the fullscreen UI, so it never sits on
// an idle page. Optional in the page shell: a #ledbar element hosts the
// strip and the page owns its layout; without one the strip drops in
// directly below the canvas shell with its own styling.

// The desktop status bar's LED and track-counter palette (window.rs).
const LED_COLORS = {
  pwr: ['rgb(232,31,24)', 'rgb(66,12,10)'],
  fdd: ['rgb(236,142,28)', 'rgb(72,38,10)'],
  hdd: ['rgb(44,200,80)', 'rgb(14,56,24)'],
  cd: ['rgb(64,170,234)', 'rgb(16,46,70)'],
};

let statusBar = null;
// Latched like the desktop bar: the counter keeps showing the last track
// between accesses instead of flickering back to "---".
let lastFddTrack = null;

function ensureStatusBar() {
  if (statusBar) return statusBar;
  const host = $('ledbar');
  const bar = host ?? document.createElement('div');
  if (!host) {
    bar.style.cssText =
      'display:flex;align-items:center;gap:0.9rem;flex-wrap:wrap;' +
      'margin:0.4rem 0;' +
      'font:600 0.8rem "IBM Plex Mono",ui-monospace,monospace;' +
      'color:rgba(255,255,255,0.75);';
  }
  const mkLed = (label, [onColor, offColor]) => {
    const row = document.createElement('span');
    row.style.cssText = 'display:inline-flex;align-items:center;gap:0.35rem;';
    const dot = document.createElement('span');
    dot.style.cssText =
      'width:10px;height:10px;border-radius:50%;' +
      `border:1px solid rgba(0,0,0,0.6);background:${offColor};`;
    row.appendChild(dot);
    row.appendChild(document.createTextNode(label));
    bar.appendChild(row);
    return { row, dot, onColor, offColor, state: null };
  };
  const pwr = mkLed('PWR', LED_COLORS.pwr);
  const fdd = mkLed('FDD', LED_COLORS.fdd);
  const hdd = mkLed('HDD', LED_COLORS.hdd);
  const cd = mkLed('CD', LED_COLORS.cd);
  const track = document.createElement('span');
  // The desktop's seven-segment track counter, as text.
  track.style.cssText =
    'background:rgb(6,8,6);color:rgb(27,220,71);padding:0.1rem 0.4rem;' +
    'border-radius:3px;letter-spacing:0.1em;';
  track.textContent = '---';
  bar.appendChild(track);
  const disks = document.createElement('span');
  disks.style.cssText =
    'overflow:hidden;text-overflow:ellipsis;white-space:nowrap;' +
    'max-width:24rem;color:rgba(255,255,255,0.55);';
  bar.appendChild(disks);
  if (!host) shell.insertAdjacentElement('afterend', bar);
  statusBar = {
    bar,
    pwr,
    fdd,
    hdd,
    cd,
    track,
    disks,
    trackText: '---',
    disksText: null,
  };
  return statusBar;
}

// state: true/false lights or dims the LED; undefined hides the whole row
// (the machine has no such drive). Early-returns on no change so steady
// frames do no DOM writes.
function setLed(led, state) {
  if (led.state === state) return;
  led.state = state;
  if (state === undefined) {
    led.row.style.display = 'none';
    return;
  }
  led.row.style.display = 'inline-flex';
  led.dot.style.background = state ? led.onColor : led.offColor;
}

// Called every animation frame: LEDs and the track counter.
function updateStatusLeds() {
  if (!emu) return;
  const sb = ensureStatusBar();
  setLed(sb.pwr, emu.power_led());
  setLed(sb.fdd, emu.fdd_led());
  setLed(sb.hdd, emu.hdd_led());
  setLed(sb.cd, emu.cd_led());
  const track = emu.fdd_track();
  if (track !== undefined) lastFddTrack = track;
  const text =
    lastFddTrack === null ? '---' : String(lastFddTrack).padStart(3, '0');
  if (text !== sb.trackText) {
    sb.trackText = text;
    sb.track.textContent = text;
  }
}

// Called at the 1 Hz stat refresh and right after insert/eject/boot, so a
// disk change shows immediately.
function updateStatusDisks() {
  if (!emu) return;
  const sb = ensureStatusBar();
  const parts = [];
  for (let drive = 0; drive < 4; drive++) {
    if (!emu.drive_connected(drive)) continue;
    parts.push(`DF${drive}: ${emu.disk_name(drive) ?? '-'}`);
  }
  const text = parts.join('  ');
  if (text !== sb.disksText) {
    sb.disksText = text;
    sb.disks.textContent = text;
    sb.disks.title = text;
  }
}

// --- disk and Kickstart lists ------------------------------------------
// Optional in the page shell: a <select id="df0list"> fills itself with
// the disk images the site serves next to the page and inserts the picked
// one into DF0 (before boot it queues, like the picker), and a
// <select id="kicklist"> does the same for Kickstart ROMs, fitting the
// picked one like the ROM picker. Each folder comes from the select's
// data-src attribute (defaults "adf/" and "kick/"), and the list from
// <folder>/index.json - a JSON array of file names, or of {name, url}
// objects with URLs relative to the folder. Without a manifest, a server
// directory listing of the folder (nginx autoindex, Apache, python -m
// http.server) is scraped for links with a matching extension instead.
// An empty or unreachable folder hides the select.

const DISK_LIST_EXT = /\.(adf|adz|dms|ipf|scp|zip|gz)$/i;
// Raw ROM images only: a list pick feeds load_rom directly, which takes
// uncompressed 256/512 KiB images.
const KICK_LIST_EXT = /\.(rom|bin)$/i;

async function folderListEntries(folder, extensions) {
  // A manifest wins when the site ships one; a missing or invalid one
  // (fetch error, unparsable JSON, not an array) falls through to the
  // directory listing.
  try {
    const resp = await fetch(new URL('index.json', folder).href);
    if (resp.ok) {
      const manifest = await resp.json();
      if (Array.isArray(manifest)) {
        return manifest
          .map((entry) => {
            const rel = typeof entry === 'string' ? entry : entry?.url;
            if (typeof rel !== 'string') return null;
            const url = new URL(rel, folder);
            // A non-string name is ignored rather than trusted: the
            // sort and the option label both expect a string.
            const name =
              (typeof entry?.name === 'string' && entry.name) ||
              nameFromUrlPath(url.pathname, rel);
            return { name, url: url.href };
          })
          .filter(Boolean);
      }
    }
  } catch {
    // fall through to the directory listing
  }
  try {
    const resp = await fetch(folder.href);
    if (!resp.ok) return [];
    const doc = new DOMParser().parseFromString(await resp.text(), 'text/html');
    const entries = [];
    for (const a of doc.querySelectorAll('a[href]')) {
      let url;
      try {
        url = new URL(a.getAttribute('href'), folder);
      } catch {
        continue;
      }
      // Only files inside the folder itself; autoindex pages also carry
      // parent-directory and sort links.
      if (url.origin !== folder.origin || !url.pathname.startsWith(folder.pathname)) continue;
      if (!extensions.test(url.pathname)) continue;
      entries.push({ name: nameFromUrlPath(url.pathname, url.pathname), url: url.href });
    }
    return entries;
  } catch {
    return [];
  }
}

// sameOriginOnly enforces the Kickstart copyright gate at the list level:
// the folder must be on the page's own site and cross-origin manifest
// entries are dropped, so the select never offers a ROM that
// fitRomFromUrl's own gate would refuse pick by pick.
async function loadFolderList(select, defaultSrc, extensions, placeholder, sameOriginOnly, pick) {
  let folder;
  try {
    folder = new URL(select.dataset.src || defaultSrc, location.href);
  } catch {
    select.hidden = true;
    return;
  }
  if (sameOriginOnly && folder.origin !== location.origin) {
    select.hidden = true;
    return;
  }
  if (!folder.pathname.endsWith('/')) folder.pathname += '/';
  let entries = await folderListEntries(folder, extensions);
  if (sameOriginOnly) {
    entries = entries.filter((entry) => new URL(entry.url).origin === location.origin);
  }
  if (!entries.length) {
    select.hidden = true;
    return;
  }
  entries.sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
  if (!select.options.length) {
    const option = document.createElement('option');
    option.value = '';
    option.textContent = placeholder;
    select.appendChild(option);
  }
  for (const { name, url } of entries) {
    const option = document.createElement('option');
    option.value = url;
    option.textContent = name;
    select.appendChild(option);
  }
  select.addEventListener('change', () => {
    if (select.value) pick(select.value);
  });
}

const diskListSelect = $('df0list');
if (diskListSelect) {
  loadFolderList(diskListSelect, 'adf/', DISK_LIST_EXT, 'DF0 from list...', false, insertDiskFromUrl);
}

// The hosted page's server carries no ROMs, so its kick/ folder lists
// nothing and the select hides; a self-hosted shell that serves its
// owner's ROMs next to the page gets a one-click ROM chooser.
const kickListSelect = $('kicklist');
if (kickListSelect) {
  loadFolderList(kickListSelect, 'kick/', KICK_LIST_EXT, 'Kickstart from list...', true, fitRomFromUrl);
}

// Optional in the page shell: older shells have no URL button.
$('df0url')?.addEventListener('click', () => {
  const url = window.prompt(
    'Disk image URL (ADF/ADZ/DMS/IPF/SCP, gzip or zip packed):',
  );
  if (url && url.trim()) insertDiskFromUrl(url.trim());
});

// Optional too, for self-hosted shells that serve ROMs alongside the page;
// the same-origin rule in fitRomFromUrl applies.
$('kickurl')?.addEventListener('click', () => {
  const url = window.prompt('Kickstart ROM URL (on this site only):');
  if (url && url.trim()) fitRomFromUrl(url.trim());
});

// --- bug reports -----------------------------------------------------------
// Two Report-a-bug links live in the page shell: one in the notes below the
// emulator, one in the overlay that only shows once something has failed.
// Both open the repository's bug-report issue form, which accepts its field
// ids as query parameters, so everything this page can know arrives
// prefilled: the wasm build, the browser, the machine state, and the status
// line. The href is rebuilt on interaction to reflect that moment; nothing
// is sent anywhere by the click itself - it all lands in an editable form
// on GitHub. Older page shells have neither link and nothing here runs.

const BUG_REPORT_URL = 'https://github.com/LinuxJedi/Copperline/issues/new';
let buildInfo = null; // the wasm build's tag and commit, known once init resolves

function bugReportHref() {
  const toml = (v) =>
    `"${String(v).replaceAll('\\', '\\\\').replaceAll('"', '\\"')}"`;
  const params = new URLSearchParams({
    template: 'bug_report.yml',
    version: `copperline.dev/try web build: ${buildInfo ?? 'unknown'}`,
    host: navigator.userAgent,
    config: [
      'frontend = "copperline.dev/try (WebAssembly)"',
      'machine = "A500, 512K chip RAM + 512K trapdoor"',
      `kickstart = ${toml(bootRom?.label ?? 'none')}`,
      `df0 = ${toml(df0Name ?? 'empty')}`,
      `joystick = ${toml(joyMode)}`,
      `canvas = "${canvas.width}x${canvas.height}"`,
      `running = ${running}`,
    ].join('\n'),
    logs: `status: ${loadStatus.textContent}\nstats: ${statLine.textContent || '-'}`,
  });
  return `${BUG_REPORT_URL}?${params}`;
}

// The overlay link only appears once something has gone wrong.
function showBugLink(on) {
  $('bug-report-err')?.toggleAttribute('hidden', !on);
}

for (const id of ['bug-report', 'bug-report-err']) {
  // pointerdown catches middle clicks and context menus before the browser
  // reads the href; click covers keyboard activation. When this module
  // never ran, the shell's static href (the bare issue form) still works.
  const refresh = (e) => {
    e.currentTarget.href = bugReportHref();
  };
  $(id)?.addEventListener('pointerdown', refresh);
  $(id)?.addEventListener('click', refresh);
}

// --- drag and drop ---------------------------------------------------------
// Files dropped anywhere on the page route like the pickers: a .rom loads
// (or queues) a Kickstart, anything else inserts into DF0. The hint overlay
// is built here rather than in the page shell (index.html lives in the
// website repository and is left alone).

let dropHint = null; // built lazily, like the fullscreen UI
let dragDepth = 0; // dragenter/dragleave fire per element crossed

function ensureDropHint() {
  if (dropHint) return dropHint;
  dropHint = document.createElement('div');
  dropHint.style.cssText =
    'position:absolute;inset:0;z-index:4;display:none;' +
    'align-items:center;justify-content:center;text-align:center;' +
    'pointer-events:none;background:rgba(10,13,22,0.7);' +
    'border:2px dashed rgba(255,255,255,0.5);' +
    'color:rgba(255,255,255,0.9);padding:1rem;' +
    'font:600 1rem "IBM Plex Mono",ui-monospace,monospace;';
  dropHint.textContent = 'Drop: disk image -> DF0, .rom -> Kickstart';
  shell.appendChild(dropHint);
  return dropHint;
}

function showDropHint(on) {
  ensureDropHint().style.display = on ? 'flex' : 'none';
}

async function handleDroppedFiles(files) {
  const list = Array.from(files ?? []);
  if (!list.length) return;
  const oversize = list.find((f) => f.size > DISK_URL_MAX_BYTES);
  if (oversize) {
    setLoadStatus(`${oversize.name}: file too large`);
    return;
  }
  // One drive and one ROM socket: the first of each kind wins, extras
  // are ignored.
  const rom = list.find((f) => /\.rom$/i.test(f.name));
  const disk = list.find((f) => !/\.rom$/i.test(f.name));
  try {
    if (rom) {
      fitRom(new Uint8Array(await rom.arrayBuffer()), rom.name);
    }
    if (disk) {
      insertDisk(new Uint8Array(await disk.arrayBuffer()), disk.name);
    }
  } catch (err) {
    setLoadStatus(`drop failed: ${err.message ?? err}`);
  }
}

// Document-level handlers so a missed drop never navigates the page away
// to the dropped file.
document.addEventListener('dragenter', (e) => {
  if (!e.dataTransfer?.types?.includes('Files')) return;
  e.preventDefault();
  dragDepth += 1;
  showDropHint(true);
});
document.addEventListener('dragover', (e) => {
  if (!e.dataTransfer?.types?.includes('Files')) return;
  e.preventDefault();
  e.dataTransfer.dropEffect = 'copy';
});
document.addEventListener('dragleave', () => {
  dragDepth = Math.max(0, dragDepth - 1);
  if (dragDepth === 0) showDropHint(false);
});
document.addEventListener('drop', (e) => {
  dragDepth = 0;
  showDropHint(false);
  if (!e.dataTransfer) return;
  e.preventDefault();
  handleDroppedFiles(e.dataTransfer.files);
});

bootBtn.addEventListener('click', boot);
const pageParams = new URLSearchParams(location.search);

// --- page configuration file ---------------------------------------------
// Optional copperline.json next to the page: a site sets its defaults in
// one hand-editable file instead of touching the shell or this glue. All
// keys are optional; a missing or invalid file is simply no defaults.
// Link parameters (?df0=, ?kick=, ?joy=, ?fdspeed=) override the file,
// and anything the visitor changes by hand wins as usual.
//
//   {
//     "kick": "roms/kick31.rom",     same-origin path, like ?kick=
//     "df0": "adf/demo.adf",         URL, like ?df0=
//     "floppy_sounds": false,        preset the drive-sounds toggle
//     "floppy_speed": 800,           100|200|400|800|0 (0 = turbo)
//     "joy": "keys",                 off|keys|cd32|touch
//     "serial_url": "wss://...",     preset the BBS gateway input
//     "serial_raw": true,            preset the raw checkbox
//     "autoboot": true               power on once everything is loaded
//   }
async function fetchPageConfig() {
  try {
    const resp = await fetch('./copperline.json');
    if (!resp.ok) return {};
    const cfg = await resp.json();
    return cfg && typeof cfg === 'object' && !Array.isArray(cfg) ? cfg : {};
  } catch {
    return {};
  }
}

async function startup() {
  // The wasm + AROS download starts immediately; the config fetch rides
  // alongside and its choices land before anything needs them (a config
  // Kickstart simply replaces the stashed boot ROM when it arrives).
  const loaded = load();
  const cfg = await fetchPageConfig();

  if (serialUrlInput && typeof cfg.serial_url === 'string') {
    serialUrlInput.value = cfg.serial_url;
  }
  if (serialRawToggle && typeof cfg.serial_raw === 'boolean') {
    serialRawToggle.checked = cfg.serial_raw;
  }
  if (typeof cfg.floppy_sounds === 'boolean') {
    if (floppySoundsToggle) floppySoundsToggle.checked = cfg.floppy_sounds;
    else configFloppySounds = cfg.floppy_sounds;
  }

  const fetches = [];
  const linkedDisk =
    pageParams.get('df0') ?? (typeof cfg.df0 === 'string' ? cfg.df0 : null);
  if (linkedDisk) fetches.push(insertDiskFromUrl(linkedDisk));
  const linkedKick =
    pageParams.get('kick') ?? (typeof cfg.kick === 'string' ? cfg.kick : null);
  if (linkedKick) fetches.push(fitRomFromUrl(linkedKick));

  // Starting joystick mode: the page shell's default (data-default on the
  // toggle or the config file), overridden per link by
  // ?joy=off|keys|cd32|touch. A touch request on a screen without touch
  // falls back to keys, so a game link written for tablets still gets a
  // joystick on a desktop.
  const requestedJoy = (
    pageParams.get('joy') ??
    (typeof cfg.joy === 'string' ? cfg.joy : null) ??
    $('joy').dataset.default ??
    ''
  ).trim();
  if (requestedJoy && requestedJoy !== joyMode) {
    if (JOY_MODES.includes(requestedJoy)) setJoyMode(requestedJoy);
    else if (requestedJoy === 'touch') setJoyMode('keys');
  }

  // Starting floppy speed: the speed select's initial value, overridden by
  // the config file and per link by ?fdspeed=100|200|400|800|0|turbo.
  // Applied to the machine at boot.
  const requestedSpeed = (
    pageParams.get('fdspeed') ??
    (typeof cfg.floppy_speed === 'number' ? String(cfg.floppy_speed) : '')
  ).trim();
  if (requestedSpeed) {
    setFloppySpeed(requestedSpeed === 'turbo' ? 0 : Number(requestedSpeed));
  } else {
    setFloppySpeed(Number(floppySpeedSel.value));
  }

  // Autoboot: a page dedicated to one demo or the BBS can land straight in
  // the machine. Waits for the ROM/disk choices above so the boot never
  // races its own media; the boot button staying disabled (ROMs failed)
  // vetoes it. Browsers keep audio locked until a real gesture - the
  // existing unlock listeners pick that up.
  if (cfg.autoboot === true) {
    await Promise.all([loaded, ...fetches]);
    if (!bootBtn.disabled && !emu) boot();
  }
}
startup();
