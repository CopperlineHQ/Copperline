// SPDX-License-Identifier: GPL-3.0-or-later

// Web player kit shell: the page logic of a single-file player packed by
// tools/publish-web (docs/guide/publishing.md). It is the browser twin of
// the native Player kit's shell: one title, no launcher, no ROM or disk
// pickers, no netplay. kit-loader.js unpacks the page's embedded assets,
// links the modules and calls start() here with the result; the DOM this
// file drives is kit-shell.html.
//
// The parts a fix must reach on both pages are imported from the modules
// the copperline.dev/try glue (try.js) also uses: the audio pacing gate,
// the host-input mapping, the pointer scale and the render stride.

import init, { WebEmu } from './pkg/copperline_web.js';
import {
  AUDIO_GATE_MAX_FRAMES,
  audioGateMaxFrames,
  newAudioGate,
  rafStarved,
} from './audio-gate.js';
import { EMPTY_PAD, joyKeyMap, keyboardPortState, orPortState, readPad } from './input-map.js';
import { cssToEmuScale } from './present-2d.js';
import {
  newRenderStrideState,
  resetRenderStrideState,
  updateRenderStrideState,
} from './render-stride.js';
import {
  QUICK_STATE_KEY,
  bytesEqual,
  diskKey,
  exportFileName,
  makeRecord,
  openSaveStore,
  recordMatches,
} from './kit-saves.js';

const JOY_MODES = ['keys', 'cd32', 'off'];
// How often a running machine's writable disks are compared with what is
// stored, and stored again if the guest wrote to them.
const SAVE_POLL_MS = 3000;
// A touch on the screen shorter and stiller than this is a left click.
const TAP_MS = 300;
const TAP_SLOP_PX = 8;
const TAP_HOLD_MS = 90;

export async function start(kit) {
  const { config } = kit;
  const $ = (id) => document.getElementById(id);
  const app = $('cl-app');
  const canvas = $('cl-screen');
  const ctx = canvas.getContext('2d');
  const overlay = $('cl-overlay');
  const bootBtn = $('cl-boot');
  const statusEl = $('cl-status');
  const noteEl = $('cl-note');
  const setStatus = (text) => {
    statusEl.textContent = text;
  };
  let noteTimer = 0;
  const note = (text, sticky = false) => {
    noteEl.textContent = text;
    clearTimeout(noteTimer);
    if (!sticky) noteTimer = setTimeout(() => (noteEl.textContent = ''), 4000);
  };

  setStatus('loading emulator...');
  const wasm = await init({ module_or_path: kit.assets.get(config.wasm) });
  kit.assets.delete(config.wasm); // compiled; the bytes are no longer needed

  // --- disks and saves -----------------------------------------------------
  // Each embedded disk runs from the visitor's own copy, as the native
  // player's does: the stored copy when one matches this build's image,
  // the pristine image otherwise.
  const store = openSaveStore(config.game.id, globalThis.indexedDB);
  let storageWarned = false;
  const disks = [];
  for (const [index, disk] of config.disks.entries()) {
    const pristine = kit.assets.get(disk.asset);
    const record = await store.get(diskKey(index));
    const saved = recordMatches(record, disk.sha256) ? record.bytes : null;
    disks.push({
      index,
      name: disk.name,
      sha256: disk.sha256,
      pristine,
      bytes: saved ?? pristine, // what the next insert puts in a drive
      stored: saved ?? pristine, // what the save store currently holds
      writable: true, // until the core refuses a writable insert
    });
  }
  const drives = config.machine.drives;
  // Which disk each drive holds (an index into `disks`), or null.
  const driveDisk = Array.from({ length: drives }, (_, d) => (d < disks.length ? d : null));

  let emu = null;
  let running = false;
  let paused = false;
  let rafId = 0;
  let lastRevision = null;
  let presentSize = { width: 0, rows: 0 };
  let lastSavePollMs = 0;
  let flushing = false;

  function insertDisk(machine, drive, disk) {
    if (disk.writable) {
      try {
        machine.insert_floppy_writable(drive, disk.bytes, disk.name);
        return;
      } catch (e) {
        // Not an image the core can write back (a flux format, say): it
        // still plays, read-only, and is left out of the save store.
        disk.writable = false;
        console.warn(`${disk.name} is read-only here: ${e.message ?? e}`);
      }
    }
    machine.insert_floppy(drive, disk.bytes, disk.name);
  }

  // Read one drive's disk back out of the machine, so `disk.bytes` is what
  // the guest has written so far. Synchronous on purpose: a caller about
  // to change the drive's disk captures it with no gap for a write to
  // fall into. Returns the disk, or null when there is nothing to keep.
  function captureDrive(drive) {
    const disk = disks[driveDisk[drive]];
    if (!emu || !disk || !disk.writable) return null;
    try {
      disk.bytes = emu.export_floppy(drive);
    } catch (e) {
      disk.writable = false;
      console.warn(`${disk.name} cannot be exported: ${e.message ?? e}`);
      return null;
    }
    return disk;
  }

  // Store a disk's captured bytes if they differ from the stored copy.
  async function persistDisk(disk) {
    const bytes = disk.bytes;
    if (bytesEqual(bytes, disk.stored)) return;
    if (await store.put(diskKey(disk.index), makeRecord(disk.sha256, bytes, Date.now()))) {
      disk.stored = bytes;
    } else if (!storageWarned) {
      storageWarned = true;
      note('This browser cannot store saves for this page; use Export disk to keep them.', true);
    }
  }

  // Store one drive's disk if the guest has written to it.
  async function flushDrive(drive) {
    const disk = captureDrive(drive);
    if (disk) await persistDisk(disk);
  }

  async function flushAll() {
    if (flushing || !emu) return;
    flushing = true;
    try {
      for (let drive = 0; drive < drives; drive++) await flushDrive(drive);
    } finally {
      flushing = false;
    }
  }

  function buildMachine() {
    const machine = new WebEmu(
      config.machine.model,
      config.machine.video ?? undefined,
      drives,
    );
    try {
      const ext = config.rom.ext ? kit.assets.get(config.rom.ext) : undefined;
      machine.load_rom(kit.assets.get(config.rom.main), ext);
      for (let drive = 0; drive < drives; drive++) {
        const disk = disks[driveDisk[drive]];
        if (disk) insertDisk(machine, drive, disk);
      }
      fitHostSettings(machine);
    } catch (e) {
      machine.free();
      throw e;
    }
    return machine;
  }

  // What the page, not the machine, owns: the volume and what is plugged
  // into the controller ports. Applied to a fresh machine and again after
  // a state load, whose machine comes back with the saving session's idea
  // of them. Port 1 is the mouse socket unless a gamepad holds it: a state
  // saved while one did would otherwise restore a stick nothing drives.
  function fitHostSettings(machine) {
    machine.set_volume_percent(Number(volume.value));
    const padPorts = new Set(padAssignments.values());
    if (!padPorts.has(1)) machine.set_port_device(1, 'mouse');
    // A CD32 pad's extra buttons only reach a guest through a fitted pad.
    for (const port of padPorts) machine.set_port_device(port, 'cd32');
    if (joyMode === 'cd32') machine.set_port_device(2, 'cd32');
  }

  // --- audio -----------------------------------------------------------------
  // The worklet module arrives as a Blob URL from the loader. Any failure
  // here leaves the title playing silently rather than not at all.
  let audioCtx = null;
  let audioNode = null;
  let queuedMs = 0;
  const gate = newAudioGate();

  const audioUnlock = () => {
    if (paused || !audioCtx) return;
    audioCtx.resume().then(
      () => {
        if (audioCtx?.state === 'running') disarmAudioUnlock();
      },
      () => {},
    );
  };
  const armAudioUnlock = () => {
    window.addEventListener('pointerup', audioUnlock);
    window.addEventListener('keydown', audioUnlock);
  };
  const disarmAudioUnlock = () => {
    window.removeEventListener('pointerup', audioUnlock);
    window.removeEventListener('keydown', audioUnlock);
  };

  async function buildAudio() {
    const AudioCtor = globalThis.AudioContext ?? globalThis.webkitAudioContext;
    if (audioCtx || kit.workletUrls.length === 0 || !AudioCtor) return;
    let context;
    try {
      context = new AudioCtor({ sampleRate: 44100 });
      // The loader offers the module under more than one URL scheme; which
      // one a browser accepts depends on the page's origin (see there).
      let refusal = null;
      for (const url of kit.workletUrls) {
        try {
          await context.audioWorklet.addModule(url);
          refusal = null;
          break;
        } catch (e) {
          refusal = e;
        }
      }
      if (refusal) throw refusal;
      const node = new AudioWorkletNode(context, 'copperline-audio', {
        outputChannelCount: [2],
      });
      node.port.onmessage = (e) => {
        if (typeof e.data?.queuedMs === 'number') queuedMs = e.data.queuedMs;
        // The worklet's reports are a clock the browser never throttles:
        // when the page is visible but starved of animation frames they
        // keep the machine and its audio real time, deferring the render
        // to the next frame that does arrive.
        if (
          running &&
          !paused &&
          emu &&
          !document.hidden &&
          rafStarved(performance.now(), lastRafMs, stride.avgStepMs)
        ) {
          step(performance.now(), true);
        }
      };
      node.connect(context.destination);
      audioCtx = context;
      audioNode = node;
    } catch (e) {
      context?.close().catch(() => {});
      console.warn(`audio is unavailable: ${e.message ?? e}`);
      note('Sound is unavailable in this browser.');
      return;
    }
    // An autoplay policy can leave the context suspended, and resume() may
    // not settle without a gesture: never let that hold up the boot.
    context.resume().catch(() => {});
    if (context.state !== 'running') armAudioUnlock();
  }

  const audioRunning = () => audioCtx?.state === 'running';

  // --- main loop -------------------------------------------------------------
  const stride = newRenderStrideState();
  let deferredLastTick = false;
  let lastRafMs = 0;

  function present(force) {
    const rows = emu.present_rows();
    if (rows === 0) return;
    const revision = emu.presentation_revision();
    if (revision === lastRevision && !force) return;
    lastRevision = revision;
    const width = emu.present_width();
    presentSize = { width, rows };
    if (canvas.width !== width || canvas.height !== rows) {
      canvas.width = width;
      canvas.height = rows;
    }
    // Rebuilt on every draw: wasm memory may grow and the buffer move.
    const view = new Uint8ClampedArray(wasm.memory.buffer, emu.present_ptr(), width * rows * 4);
    ctx.putImageData(new ImageData(view, width, rows), 0, 0);
  }

  function fail(e) {
    running = false;
    // A failure outside the frame loop (a control, a starved-frame step)
    // leaves a frame requested; dropped here, so a Restart before it
    // fires cannot end up with two loops running.
    cancelAnimationFrame(rafId);
    rafId = 0;
    console.error(e);
    emu = null; // the instance may have panicked; never reuse it
    releaseSession();
    setStatus(`emulator error: ${e.message ?? e}`);
    bootBtn.textContent = 'Restart';
    bootBtn.disabled = false;
    overlay.hidden = false;
  }

  function step(nowMs, deferRender) {
    try {
      // The audio clock paces the machine only while it is actually
      // running; a suspended context reports nothing, and a stale depth
      // must not hold the gate shut.
      let max = AUDIO_GATE_MAX_FRAMES;
      if (audioRunning()) {
        max = audioGateMaxFrames(gate, queuedMs);
        if (max === 0) emu.resync_clock();
      } else {
        gate.closed = false;
      }
      const began = performance.now();
      const stepped = deferRender ? emu.run_hidden(nowMs, max) : emu.run(nowMs, max);
      updateRenderStrideState(stride, nowMs, performance.now() - began, stepped, !deferRender);
    } catch (e) {
      fail(e);
      return false;
    }
    const audio = emu.take_audio();
    if (audio.length > 0 && audioNode && audioRunning()) {
      audioNode.port.postMessage(audio, [audio.buffer]);
    }
    return true;
  }

  function tick(nowMs) {
    rafId = 0;
    if (!running || paused) return;
    lastRafMs = nowMs;
    pumpGamepads();
    // Under sustained overload defer the frame render on alternate ticks:
    // the machine keeps real time and only the shown rate degrades.
    const deferRender = stride.active && !deferredLastTick;
    if (!step(nowMs, deferRender)) return;
    deferredLastTick = deferRender;
    present(false);
    led.classList.toggle('cl-on', emu.fdd_led());
    if (nowMs - lastSavePollMs >= SAVE_POLL_MS && !emu.fdd_led()) {
      lastSavePollMs = nowMs;
      flushAll();
    }
    rafId = requestAnimationFrame(tick);
  }

  function resumeLoop() {
    if (!running || paused || rafId) return;
    emu.resync_clock();
    lastRafMs = performance.now();
    rafId = requestAnimationFrame(tick);
  }

  // --- joystick, gamepads, touch ---------------------------------------------
  // The keyboard mapping and the touch pad drive port 2 (the Amiga's
  // joystick port); gamepads fill port 2 first and port 1 second, where a
  // pad displaces the mouse exactly as plugging a stick in does.
  let joyMode = JOY_MODES.includes(config.input.joystick) ? config.input.joystick : 'keys';
  const joyHeld = {};
  const padPort = { 1: null, 2: null };
  const padAssignments = new Map(); // gamepad index -> Amiga port
  let touchPad = null; // the on-screen pad's port state while it is touched

  function applyJoystick() {
    if (!emu) return;
    const p2 = orPortState(orPortState(keyboardPortState(joyHeld), touchPad), padPort[2]);
    emu.set_joystick_port(2, p2.up, p2.down, p2.left, p2.right, p2.fire, p2.button2);
    emu.set_cd32_buttons_port(2, p2.play, p2.rwd, p2.ffw, p2.green, p2.yellow);
    const p1 = padPort[1];
    if (p1) {
      emu.set_joystick_port(1, p1.up, p1.down, p1.left, p1.right, p1.fire, p1.button2);
      emu.set_cd32_buttons_port(1, p1.play, p1.rwd, p1.ffw, p1.green, p1.yellow);
    }
  }

  function joystickKey(code, pressed) {
    const control = joyKeyMap(joyMode)?.[code];
    if (!control) return false;
    joyHeld[control] = pressed;
    applyJoystick();
    return true;
  }

  const joyBtn = $('cl-joy');
  function setJoyMode(mode) {
    joyMode = mode;
    joyBtn.textContent = `Joystick: ${mode}`;
    // A key held across the change would be released into the other
    // mapping; let go of everything under the old one first.
    releaseHeldKeys();
    // The cd32 mapping's extra buttons only reach a guest through a fitted
    // CD32 pad; the plain modes leave whatever is in the port alone.
    if (mode === 'cd32') emu?.set_port_device(2, 'cd32');
    applyJoystick();
  }
  joyBtn.addEventListener('click', () => {
    setJoyMode(JOY_MODES[(JOY_MODES.indexOf(joyMode) + 1) % JOY_MODES.length]);
  });

  function pumpGamepads() {
    if (!navigator.getGamepads) return;
    const pads = navigator.getGamepads();
    if (padAssignments.size === 0 && !Array.from(pads).some((pad) => pad)) return;
    for (const index of Array.from(padAssignments.keys())) {
      if (pads[index]) continue;
      const port = padAssignments.get(index);
      padAssignments.delete(index);
      padPort[port] = null;
      // Port 1 is the mouse socket; a pad leaving it puts the mouse back.
      if (port === 1) emu.set_port_device(1, 'mouse');
    }
    for (const pad of pads) {
      if (!pad || padAssignments.has(pad.index)) continue;
      const taken = new Set(padAssignments.values());
      const port = !taken.has(2) ? 2 : !taken.has(1) ? 1 : null;
      if (port === null) continue; // a third pad has nowhere to go
      padAssignments.set(pad.index, port);
      emu.set_port_device(port, 'cd32'); // a real pad has the extra buttons
      note(`Gamepad connected on port ${port}`);
    }
    for (const [index, port] of padAssignments) {
      if (pads[index]) padPort[port] = readPad(pads[index]);
    }
    applyJoystick();
  }

  // The on-screen pad: a stick zone read as eight directions around its
  // centre, and two fire buttons. Shown on touch screens only.
  const touchUi = $('cl-touch');
  const stickEl = $('cl-stick');
  const touchState = { ...EMPTY_PAD };
  const syncTouch = () => {
    touchPad = Object.values(touchState).some(Boolean) ? { ...touchState } : null;
    applyJoystick();
  };
  function stickFrom(e) {
    const box = stickEl.getBoundingClientRect();
    const dx = (e.clientX - (box.left + box.width / 2)) / (box.width / 2);
    const dy = (e.clientY - (box.top + box.height / 2)) / (box.height / 2);
    const dead = 0.25;
    touchState.left = dx < -dead;
    touchState.right = dx > dead;
    touchState.up = dy < -dead;
    touchState.down = dy > dead;
    syncTouch();
  }
  function stickRelease() {
    touchState.left = touchState.right = touchState.up = touchState.down = false;
    syncTouch();
  }
  stickEl.addEventListener('pointerdown', (e) => {
    e.preventDefault();
    try {
      stickEl.setPointerCapture?.(e.pointerId);
    } catch {
      // Capture only keeps a drag that leaves the zone; steer without it.
    }
    stickFrom(e);
  });
  stickEl.addEventListener('pointermove', (e) => {
    if (e.buttons || e.pointerType === 'touch') stickFrom(e);
  });
  for (const type of ['pointerup', 'pointercancel', 'lostpointercapture']) {
    stickEl.addEventListener(type, stickRelease);
  }
  for (const [id, control] of [
    ['cl-fire1', 'fire'],
    ['cl-fire2', 'button2'],
  ]) {
    const el = $(id);
    const set = (pressed) => (e) => {
      e.preventDefault();
      touchState[control] = pressed;
      el.classList.toggle('cl-on', pressed);
      syncTouch();
    };
    el.addEventListener('pointerdown', set(true));
    for (const type of ['pointerup', 'pointercancel', 'pointerleave']) {
      el.addEventListener(type, set(false));
    }
  }
  const hasTouch = (navigator.maxTouchPoints ?? 0) > 0;
  const touchBtn = $('cl-touchpad');
  touchBtn.hidden = !hasTouch;
  const setTouchUi = (on) => {
    touchUi.hidden = !on;
    touchBtn.textContent = on ? 'Hide pad' : 'Show pad';
    if (!on) {
      for (const key of Object.keys(touchState)) touchState[key] = false;
      syncTouch();
    }
  };
  touchBtn.addEventListener('click', () => setTouchUi(touchUi.hidden));

  // --- keyboard --------------------------------------------------------------
  // Auto-repeat keydowns must not reach the emulator (the Amiga keyboard
  // sends one down code; the guest does its own repeat), but the browser
  // default still has to be suppressed on every repeat or holding a cursor
  // key scrolls the page.
  const consumedKeys = new Set();
  const pageSpentKeys = new Set();
  const guestKeys = new Set(); // codes the Amiga keyboard currently holds
  const dialogOpen = () => !!document.querySelector('dialog[open]');
  // Keys belong to the Amiga while the picture (or nothing) has focus, and
  // to the page's own controls while one of them does: Tab, the arrows on
  // the volume slider and the disk list keep working for keyboard users.
  // Shift+Esc is the way from the picture to the controls.
  const amigaHasKeys = () => {
    const active = document.activeElement;
    return !active || active === document.body || active === canvas;
  };
  // Let go of everything held: a key whose release the page will never
  // see (focus went elsewhere mid-press) must not stay down in the guest.
  function releaseHeldKeys() {
    if (emu) for (const code of guestKeys) emu.key_event(code, false);
    guestKeys.clear();
    consumedKeys.clear();
    pageSpentKeys.clear();
    for (const key of Object.keys(joyHeld)) joyHeld[key] = false;
    applyJoystick();
  }
  window.addEventListener('blur', releaseHeldKeys);
  window.addEventListener('keydown', (e) => {
    if (!emu || !running || dialogOpen() || !amigaHasKeys()) return;
    if (e.code === 'Escape' && e.shiftKey && !document.pointerLockElement) {
      releaseHeldKeys();
      pauseBtn.focus();
      e.preventDefault();
      return;
    }
    if (e.repeat) {
      if (consumedKeys.has(e.code)) e.preventDefault();
      return;
    }
    pageSpentKeys.delete(e.code);
    // Esc with the mouse captured releases it and goes no further; the
    // guest only sees Esc while the pointer is free.
    if (e.code === 'Escape' && document.pointerLockElement) {
      document.exitPointerLock?.();
      pageSpentKeys.add(e.code);
      consumedKeys.add(e.code);
      e.preventDefault();
      return;
    }
    if (joystickKey(e.code, true)) {
      consumedKeys.add(e.code);
      e.preventDefault();
    } else if (emu.key_event(e.code, true)) {
      guestKeys.add(e.code);
      consumedKeys.add(e.code);
      e.preventDefault();
    } else {
      consumedKeys.delete(e.code);
    }
  });
  window.addEventListener('keyup', (e) => {
    const consumed = consumedKeys.delete(e.code);
    const pageSpent = pageSpentKeys.delete(e.code);
    guestKeys.delete(e.code);
    if (!emu || !running) return;
    if (pageSpent) {
      e.preventDefault();
      return;
    }
    // A release the guest saw the press of always reaches it; any other
    // key let go over one of the page's controls is that control's.
    if (!consumed && !amigaHasKeys()) return;
    if (joystickKey(e.code, false) || emu.key_event(e.code, false) || consumed) {
      e.preventDefault();
    }
  });

  // --- mouse and touch pointer -------------------------------------------------
  // Unlocked, the cursor drives the Amiga pointer through position deltas;
  // a click captures the pointer for relative motion and Esc releases it.
  // A finger on the screen is a trackpad: drag to move, tap to left-click.
  const pointerScale = () =>
    cssToEmuScale(null, presentSize, canvas, window.devicePixelRatio);
  let lastPos = null;
  canvas.addEventListener('mousedown', (e) => {
    if (!emu || !running) return;
    e.preventDefault();
    canvas.focus({ preventScroll: true });
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
    const scale = pointerScale();
    if (document.pointerLockElement === canvas) {
      emu.mouse_delta(e.movementX * scale.x, e.movementY * scale.y);
      lastPos = null;
    } else if (e.target === canvas) {
      if (lastPos) {
        emu.mouse_delta((e.clientX - lastPos.x) * scale.x, (e.clientY - lastPos.y) * scale.y);
      }
      lastPos = { x: e.clientX, y: e.clientY };
    } else {
      lastPos = null;
    }
  });
  document.addEventListener('pointerlockchange', () => {
    lastPos = null;
  });

  let finger = null; // { id, x, y, startX, startY, startMs }
  canvas.addEventListener(
    'touchstart',
    (e) => {
      if (!emu || !running) return;
      e.preventDefault();
      if (finger) return;
      const t = e.changedTouches[0];
      finger = {
        id: t.identifier,
        x: t.clientX,
        y: t.clientY,
        startX: t.clientX,
        startY: t.clientY,
        startMs: performance.now(),
      };
    },
    { passive: false },
  );
  canvas.addEventListener(
    'touchmove',
    (e) => {
      if (!emu || !running || !finger) return;
      e.preventDefault();
      for (const t of e.changedTouches) {
        if (t.identifier !== finger.id) continue;
        const scale = pointerScale();
        emu.mouse_delta((t.clientX - finger.x) * scale.x, (t.clientY - finger.y) * scale.y);
        finger.x = t.clientX;
        finger.y = t.clientY;
      }
    },
    { passive: false },
  );
  const touchEnd = (e) => {
    if (!finger) return;
    for (const t of e.changedTouches) {
      if (t.identifier !== finger.id) continue;
      const still =
        Math.hypot(t.clientX - finger.startX, t.clientY - finger.startY) <= TAP_SLOP_PX;
      const quick = performance.now() - finger.startMs <= TAP_MS;
      finger = null;
      if (emu && running && still && quick && e.type === 'touchend') {
        emu.mouse_button(0, true);
        setTimeout(() => emu?.mouse_button(0, false), TAP_HOLD_MS);
      }
    }
  };
  canvas.addEventListener('touchend', touchEnd);
  canvas.addEventListener('touchcancel', touchEnd);

  // --- controls ----------------------------------------------------------------
  const pauseBtn = $('cl-pause');
  const volume = $('cl-volume');
  const led = $('cl-led');

  function setPaused(next) {
    if (!running || paused === next) return;
    paused = next;
    pauseBtn.textContent = paused ? 'Resume' : 'Pause';
    if (paused) {
      audioCtx?.suspend().catch(() => {});
      flushAll();
    } else {
      audioCtx?.resume().catch(() => {});
      resumeLoop();
    }
  }
  pauseBtn.addEventListener('click', () => setPaused(!paused));

  $('cl-reset').addEventListener('click', () => {
    if (!emu || !running) return;
    try {
      emu.reset();
      resetRenderStrideState(stride);
      note('Machine reset');
    } catch (e) {
      fail(e);
    }
  });

  volume.addEventListener('input', () => emu?.set_volume_percent(Number(volume.value)));

  // A control used with the mouse hands the keyboard straight back to the
  // Amiga; one reached with the keyboard keeps focus (a click from the
  // keyboard has no detail count), so Tab can carry on from it.
  const focusScreen = () => canvas.focus({ preventScroll: true });
  $('cl-bar').addEventListener('click', (e) => {
    if (e.detail > 0 && e.target.closest?.('button')) focusScreen();
  });
  volume.addEventListener('pointerup', focusScreen);

  function download(blob, name) {
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = name;
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 10000);
  }

  $('cl-shot').addEventListener('click', () => {
    if (!emu) return;
    canvas.toBlob((blob) => {
      if (blob) download(blob, exportFileName(config.game.title, '.png'));
    }, 'image/png');
  });

  // The disk in DF0 can be changed between the embedded images that are
  // not sitting in another drive: the swap list of a multi-disk title.
  const diskSelect = $('cl-disk');
  const swappable = disks.filter((disk) => disk.index === 0 || disk.index >= drives);
  if (drives > 0 && swappable.length > 1) {
    for (const disk of swappable) {
      const option = document.createElement('option');
      option.value = String(disk.index);
      option.textContent = disk.name;
      diskSelect.append(option);
    }
    diskSelect.value = '0';
    diskSelect.parentElement.hidden = false;
    diskSelect.addEventListener('change', async () => {
      const next = disks[Number(diskSelect.value)];
      if (!emu) {
        driveDisk[0] = next.index;
        return;
      }
      try {
        // Capture and swap in one go, then store what was captured: no
        // guest write can land on the outgoing disk in between.
        const previous = captureDrive(0);
        driveDisk[0] = next.index;
        insertDisk(emu, 0, next);
        note(`DF0: ${next.name}`);
        if (previous) await persistDisk(previous);
      } catch (e) {
        note(`Disk change failed: ${e.message ?? e}`, true);
      }
      canvas.focus({ preventScroll: true });
    });
  }

  const exportBtn = $('cl-export');
  exportBtn.hidden = drives === 0 || disks.length === 0;
  exportBtn.addEventListener('click', async () => {
    const disk = disks[driveDisk[0]];
    if (!disk) return;
    await flushDrive(0);
    download(
      new Blob([disk.bytes], { type: 'application/octet-stream' }),
      exportFileName(`${config.game.title}-${disk.name}`, ''),
    );
  });

  // The way back in: an image exported earlier (on another device, say)
  // replaces the disk in DF0 and becomes the stored copy. The core decides
  // whether the file is a disk image at all, so this needs a running
  // machine.
  const importBtn = $('cl-import');
  const importFile = $('cl-import-file');
  importBtn.hidden = exportBtn.hidden;
  importBtn.addEventListener('click', () => {
    if (!emu || !running) {
      note('Start the title before importing a disk');
      return;
    }
    importFile.click();
  });
  importFile.addEventListener('change', async () => {
    const file = importFile.files?.[0];
    importFile.value = '';
    const disk = disks[driveDisk[0]];
    if (!file || !disk || !emu) return;
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      if (!emu) return;
      // The import replaces the visitor's copy of this disk for good.
      emu.insert_floppy_writable(0, bytes, disk.name);
      disk.writable = true;
      await flushDrive(0);
      note(`Imported ${file.name} as ${disk.name}; reset to boot from it`, true);
    } catch (e) {
      // The core refuses a file that is not a disk image before touching
      // the drive, so the disk that was in it is still there.
      note(`Import failed: ${e.message ?? e}`, true);
    }
    canvas.focus({ preventScroll: true });
  });

  // Quick save state, when the manifest enables it. A state is only
  // offered back to the build that made it: it embeds the machine and its
  // ROM, and one written by a differently packed page may not fit.
  const stateSave = $('cl-state-save');
  const stateLoad = $('cl-state-load');
  if (config.features.save_states) {
    stateSave.hidden = false;
    stateLoad.hidden = false;
    stateSave.addEventListener('click', async () => {
      if (!emu) return;
      try {
        const record = makeRecord(config.build, emu.save_state(), Date.now());
        record.driveDisk = driveDisk.slice();
        note((await store.put(QUICK_STATE_KEY, record)) ? 'State saved' : 'State could not be stored');
      } catch (e) {
        note(`Save failed: ${e.message ?? e}`, true);
      }
    });
    stateLoad.addEventListener('click', async () => {
      if (!emu) return;
      const record = await store.get(QUICK_STATE_KEY);
      if (!emu) return;
      if (!recordMatches(record, config.build)) {
        note('No saved state for this version');
        return;
      }
      // A state is the whole machine, disks included: a disk that was in
      // a drive when it was saved comes back as it was at that moment,
      // and the stored copy follows it (the libretro core's rule too). A
      // disk that kept later writes under a machine restored to before
      // them would be at the mercy of whatever the guest still holds in
      // memory. Only a disk the load takes out of the drives keeps what
      // was written since, so that is captured first.
      for (let d = 0; d < drives; d++) captureDrive(d);
      const before = new Map(disks.map((disk) => [disk, disk.bytes]));
      try {
        emu.load_state(record.bytes);
      } catch (e) {
        // A state the core cannot read leaves the running machine as it was.
        note(`Load failed: ${e.message ?? e}`, true);
        return;
      }
      // The state carries its own disks; follow what it had in each drive.
      if (Array.isArray(record.driveDisk)) {
        for (let d = 0; d < drives; d++) driveDisk[d] = record.driveDisk[d] ?? null;
        if (!diskSelect.parentElement.hidden && driveDisk[0] !== null) {
          diskSelect.value = String(driveDisk[0]);
        }
      }
      // Host-side settings are not part of the machine, so the page's own
      // are put back over the restored one, as after a boot.
      fitHostSettings(emu);
      releaseHeldKeys();
      resetRenderStrideState(stride);
      emu.resync_clock();
      present(true);
      // Take the restored disks as the visitor's copies now, not at the
      // next poll, and say so when that undid something written since.
      const restored = [];
      for (let d = 0; d < drives; d++) {
        const disk = captureDrive(d);
        if (disk && !bytesEqual(before.get(disk), disk.bytes)) restored.push(disk.name);
      }
      if (restored.length > 0) {
        note(`State loaded; ${restored.join(', ')} went back to how the state saved it`, true);
      } else {
        note('State loaded');
      }
      for (const disk of disks) if (disk.writable) await persistDisk(disk);
    });
  }

  // Fullscreen: the element API where it exists, a pinned full-viewport
  // layout where it does not (iPhone Safari) or when it is refused.
  const fsBtn = $('cl-fs');
  const fsElement = () => document.fullscreenElement ?? document.webkitFullscreenElement ?? null;
  const syncFs = () => {
    fsBtn.textContent =
      fsElement() || app.classList.contains('cl-pinned') ? 'Exit fullscreen' : 'Fullscreen';
  };
  function enterFullscreen() {
    const request = app.requestFullscreen ?? app.webkitRequestFullscreen;
    const pin = () => {
      app.classList.add('cl-pinned');
      syncFs();
    };
    if (!request) {
      pin();
      return;
    }
    try {
      Promise.resolve(request.call(app)).catch(pin);
    } catch {
      pin();
    }
  }
  function exitFullscreen() {
    app.classList.remove('cl-pinned');
    if (fsElement()) (document.exitFullscreen ?? document.webkitExitFullscreen)?.call(document);
    syncFs();
  }
  fsBtn.addEventListener('click', () => {
    if (fsElement() || app.classList.contains('cl-pinned')) exitFullscreen();
    else enterFullscreen();
    canvas.focus({ preventScroll: true });
  });
  document.addEventListener('fullscreenchange', syncFs);
  document.addEventListener('webkitfullscreenchange', syncFs);

  // About: what this page is, and the license texts it is obliged to carry.
  const about = $('cl-about');
  const licenseText = $('cl-license-text');
  $('cl-about-title').textContent =
    config.game.title + (config.game.version ? ` ${config.game.version}` : '');
  $('cl-about-engine').textContent =
    `Copperline ${config.copperline.version}` +
    (config.copperline.ref ? ` (${config.copperline.ref})` : '');
  $('cl-about-source').textContent =
    config.copperline.source +
    (config.copperline.commit ? ` (commit ${config.copperline.commit.slice(0, 12)})` : '');
  $('cl-about-rom').textContent = config.rom.label;
  const noticeList = $('cl-notices');
  for (const item of config.notices) {
    const button = document.createElement('button');
    button.type = 'button';
    button.textContent = item.title;
    button.addEventListener('click', async () => {
      try {
        licenseText.textContent = new TextDecoder().decode(await kit.readLazy(item.asset));
      } catch (e) {
        licenseText.textContent = `cannot show this text: ${e.message ?? e}`;
      }
      licenseText.hidden = false;
    });
    noticeList.append(button);
  }
  $('cl-about-open').addEventListener('click', () => {
    licenseText.hidden = true;
    if (about.showModal) about.showModal();
    else about.setAttribute('open', '');
  });
  $('cl-about-close').addEventListener('click', () => {
    if (about.close) about.close();
    else about.removeAttribute('open');
  });
  about.addEventListener('close', () => canvas.focus({ preventScroll: true }));
  $('cl-erase').addEventListener('click', async () => {
    const ask = 'Erase the saves this browser holds for this title and restart it?';
    if (!window.confirm(ask)) return;
    for (const disk of disks) {
      await store.remove(diskKey(disk.index));
      disk.bytes = disk.pristine;
      disk.stored = disk.pristine;
    }
    await store.remove(QUICK_STATE_KEY);
    // A running machine still holds the written disks, and would store
    // them again at the next poll: put the pristine ones in and power
    // cycle, so what plays is what is stored.
    if (emu && running) {
      try {
        for (let drive = 0; drive < drives; drive++) {
          const disk = disks[driveDisk[drive]];
          if (disk) insertDisk(emu, drive, disk);
        }
        emu.reset();
        resetRenderStrideState(stride);
      } catch (e) {
        fail(e);
        return;
      }
    }
    note('Saves erased', true);
  });

  // --- page lifecycle ----------------------------------------------------------
  // A hidden page gets no animation frames, so the machine sleeps with it;
  // the pacer is re-anchored on return instead of fast-forwarding.
  document.addEventListener('visibilitychange', () => {
    if (!running) return;
    if (document.hidden) {
      // Drop the pending frame so the return re-anchors the pacer instead
      // of the first frame back sprinting to catch up.
      cancelAnimationFrame(rafId);
      rafId = 0;
      releaseHeldKeys();
      flushAll();
      audioCtx?.suspend().catch(() => {});
    } else if (!paused) {
      audioCtx?.resume().catch(() => {});
      resumeLoop();
    }
  });
  window.addEventListener('pagehide', () => {
    flushAll();
  });

  // A handle for automation and bug reports, like try.js's __emu.
  const session = {
    config,
    get emu() {
      return emu;
    },
    get running() {
      return running;
    },
    get audioState() {
      return audioCtx?.state ?? 'none';
    },
    disks,
    flush: flushAll,
  };
  const releaseSession = () => {
    led.classList.remove('cl-on');
  };
  globalThis.CopperlineKit.session = session;

  // --- boot --------------------------------------------------------------------
  // The click is the gesture that lets audio (and fullscreen) start.
  async function boot() {
    bootBtn.disabled = true;
    if (config.display.fullscreen) enterFullscreen();
    // Port fittings live on the machine: forget the pads so the new one
    // has them fitted afresh as the frame loop finds them again.
    padAssignments.clear();
    padPort[1] = padPort[2] = null;
    try {
      emu = buildMachine();
    } catch (e) {
      console.error(e);
      setStatus(`cannot start: ${e.message ?? e}`);
      bootBtn.disabled = false;
      return;
    }
    await buildAudio();
    // A restart after a failure reuses the audio stack, which a pause may
    // have left suspended.
    if (audioCtx && audioCtx.state !== 'running') {
      audioCtx.resume().catch(() => {});
      armAudioUnlock();
    }
    resetRenderStrideState(stride);
    deferredLastTick = false;
    lastRevision = null;
    paused = false;
    pauseBtn.textContent = 'Pause';
    overlay.hidden = true;
    // Primed before running flips on, so no queue report reads the epoch
    // as an already-starved animation frame.
    lastRafMs = performance.now();
    running = true;
    lastSavePollMs = lastRafMs;
    if (hasTouch) setTouchUi(true);
    canvas.focus({ preventScroll: true });
    rafId = requestAnimationFrame(tick);
  }
  bootBtn.addEventListener('click', boot);

  setJoyMode(joyMode);
  syncFs();
  document.title = config.game.title;
  $('cl-title').textContent = config.game.title;
  setStatus('');
  bootBtn.disabled = false;
  bootBtn.focus({ preventScroll: true });
}
