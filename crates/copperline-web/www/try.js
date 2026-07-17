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
    emu = machine;
    window.__emu = emu; // for debugging/automation

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

  if (nowMs - lastStatUpdate >= 1000) {
    statLine.textContent =
      `${framesThisSecond} fps | ` +
      `${emu.emulated_seconds().toFixed(1)}s emulated | ` +
      `audio ${queuedMs.toFixed(0)} ms`;
    framesThisSecond = 0;
    lastStatUpdate = nowMs;
  }
  requestAnimationFrame(tick);
}

document.addEventListener('visibilitychange', () => {
  if (!audioCtx) return;
  if (document.hidden) audioCtx.suspend();
  else audioCtx.resume();
});

// --- joystick (port 2) -----------------------------------------------------
// The toggle cycles off -> keys (-> touch on touch screens). Keys is the
// desktop frontend's FS-UAE-compatible mapping: cursor keys for directions,
// Right Ctrl / Right Alt for fire, CD32 extras on C/X/D/S/Enter/Z/A; while
// on, these keys drive the port-2 joystick instead of reaching the Amiga
// keyboard. Touch turns the canvas into a pad (see the touch section).

const JOY_KEYS = {
  ArrowUp: 'up',
  ArrowDown: 'down',
  ArrowLeft: 'left',
  ArrowRight: 'right',
  ControlRight: 'fireCtrl',
  AltRight: 'fireAlt',
  KeyC: 'red',
  KeyX: 'blue',
  KeyD: 'green',
  KeyS: 'yellow',
  Enter: 'play',
  NumpadEnter: 'play',
  KeyZ: 'rwd',
  KeyA: 'ffw',
};
const JOY_MODES = hasTouch ? ['off', 'keys', 'touch'] : ['off', 'keys'];
let joyMode = 'off';
const joyHeld = {};

function applyJoystick() {
  const h = joyHeld;
  emu.set_joystick_port2(
    !!h.up,
    !!h.down,
    !!h.left,
    !!h.right,
    !!(h.fireCtrl || h.fireAlt || h.red),
    !!h.blue,
  );
  emu.set_cd32_buttons_port2(!!h.play, !!h.rwd, !!h.ffw, !!h.green, !!h.yellow);
}

// Returns true when the key was captured for the joystick.
function joystickKey(code, pressed) {
  if (joyMode !== 'keys') return false;
  const control = JOY_KEYS[code];
  if (!control) return false;
  joyHeld[control] = pressed;
  applyJoystick();
  return true;
}

// Cycles the mode; wired to the control-bar button and to the fullscreen
// overlay's copy of it, which stay in step.
function cycleJoyMode() {
  joyMode = JOY_MODES[(JOY_MODES.indexOf(joyMode) + 1) % JOY_MODES.length];
  $('joy').textContent = `Joystick: ${joyMode}`;
  if (fsUi) fsUi.joy.textContent = `Joystick: ${joyMode}`;
  for (const k of Object.keys(joyHeld)) joyHeld[k] = false;
  resetTouchState();
  if (emu) {
    applyJoystick();
    emu.set_cd32_buttons_port2(false, false, false, false, false);
  }
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
  } catch (err) {
    setLoadStatus(`${err.message ?? err}`);
  }
});

$('reset').addEventListener('click', () => {
  if (!emu) return;
  try {
    emu.reset();
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
const linkedDisk = pageParams.get('df0');
if (linkedDisk) insertDiskFromUrl(linkedDisk);
const linkedKick = pageParams.get('kick');
if (linkedKick) fitRomFromUrl(linkedKick);
load();
