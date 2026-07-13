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

const FB_W = 716;

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

let romBytes = null;
let extBytes = null;

async function load() {
  try {
    setLoadStatus('loading emulator + AROS ROMs...');
    const [wasmExports, rom, ext] = await Promise.all([
      init(),
      fetchBytes('./aros/aros-amiga-m68k-rom.bin', 'AROS ROM'),
      fetchBytes('./aros/aros-amiga-m68k-ext.bin', 'AROS extended ROM'),
    ]);
    wasm = wasmExports;
    romBytes = rom;
    extBytes = ext;
    setLoadStatus('ready - boots the open-source AROS ROM');
    bootBtn.disabled = false;
    bootBtn.focus();
  } catch (e) {
    setLoadStatus(`failed to load: ${e.message ?? e}`);
    console.error(e);
  }
}

// --- boot ----------------------------------------------------------------

async function boot() {
  bootBtn.disabled = true;
  try {
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

    emu = new WebEmu();
    emu.load_rom(romBytes, extBytes);
    emu.set_volume_percent(Number($('vol').value));
    window.__emu = emu; // for debugging/automation

    overlay.style.display = 'none';
    running = true;
    requestAnimationFrame(tick);
  } catch (e) {
    setLoadStatus(`boot failed: ${e.message ?? e}`);
    bootBtn.disabled = false;
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
    console.error(e);
    return;
  }

  const rows = emu.present_rows();
  if (rows > 0) {
    if (canvas.height !== rows) {
      canvas.width = FB_W;
      canvas.height = rows;
    }
    // The view must be rebuilt every frame: wasm memory may grow and the
    // present buffer may reallocate.
    const view = new Uint8ClampedArray(
      wasm.memory.buffer,
      emu.present_ptr(),
      FB_W * rows * 4,
    );
    ctx2d.putImageData(new ImageData(view, FB_W, rows), 0, 0);
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

// --- keyboard ------------------------------------------------------------

window.addEventListener('keydown', (e) => {
  if (!emu || !running || e.repeat) return;
  if (emu.key_event(e.code, true)) e.preventDefault();
});
window.addEventListener('keyup', (e) => {
  if (!emu || !running) return;
  if (emu.key_event(e.code, false)) e.preventDefault();
});

// --- mouse ---------------------------------------------------------------
// Unlocked: the cursor drives the Amiga pointer through position deltas
// (Workbench-friendly). Click to pointer-lock for relative motion (games);
// Esc releases the lock, as the browser enforces.

let lastPos = null;
const cssToEmu = () => FB_W / canvas.clientWidth;

canvas.addEventListener('mousedown', (e) => {
  if (!emu || !running) return;
  e.preventDefault();
  if (document.pointerLockElement !== canvas && e.button === 0) {
    canvas.requestPointerLock();
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

// --- controls ------------------------------------------------------------

$('df0').addEventListener('change', async (e) => {
  const file = e.target.files[0];
  if (!file || !emu) return;
  try {
    const bytes = new Uint8Array(await file.arrayBuffer());
    emu.insert_floppy(0, bytes, file.name);
    setLoadStatus(`DF0: ${file.name} (write-protected)`);
  } catch (err) {
    setLoadStatus(`insert failed: ${err.message ?? err}`);
  }
  e.target.value = '';
});

$('kick').addEventListener('change', async (e) => {
  const file = e.target.files[0];
  if (!file || !emu) return;
  try {
    const bytes = new Uint8Array(await file.arrayBuffer());
    emu.load_rom(bytes, undefined);
    setLoadStatus(`Kickstart loaded: ${file.name} - machine power-cycled`);
  } catch (err) {
    setLoadStatus(`ROM load failed: ${err.message ?? err}`);
  }
  e.target.value = '';
});

$('eject').addEventListener('click', () => {
  if (!emu) return;
  try {
    emu.eject_floppy(0);
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

$('fullscreen').addEventListener('click', () => {
  $('shell').requestFullscreen?.();
});

$('vol').addEventListener('input', (e) => {
  if (emu) emu.set_volume_percent(Number(e.target.value));
});

bootBtn.addEventListener('click', boot);
load();
