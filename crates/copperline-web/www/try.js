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
let paused = false;
let framesThisSecond = 0;
let lastStatUpdate = 0;

// Transient caption over the screen, the page's version of the desktop's
// on-screen display. It exists because the shell's status line lives inside
// the boot overlay, which is hidden for the whole life of a running
// machine: without this, everything the page says after boot -- screenshot
// copied to the clipboard, state saved, disk inserted -- is written
// somewhere nobody can see, and a button that worked perfectly looks like
// it did nothing. Over the screen rather than below it so it reads in
// fullscreen too, where there is no page left to put a status line on.
let osd = null;
let osdHideTimer = 0;

function ensureOsd() {
  if (osd) return osd;
  osd = document.createElement('div');
  // Below the drop hint (z-index 4) and never in the way of the pointer.
  osd.style.cssText =
    'position:absolute;left:0;right:0;bottom:0;z-index:3;' +
    'padding:0.5rem 0.75rem;pointer-events:none;opacity:0;' +
    'transition:opacity 220ms ease;' +
    'background:linear-gradient(transparent,rgba(8,11,19,0.82));' +
    'color:rgba(255,255,255,0.92);text-align:center;' +
    'font:600 0.8rem "IBM Plex Mono",ui-monospace,monospace;';
  // Looked up here rather than through the module's `shell` binding, which
  // is declared further down the file: a status message can be raised
  // before that line has run.
  $('shell').appendChild(osd);
  return osd;
}

// Lift the caption clear of the on-screen keyboard. Only in fullscreen:
// there the shell is the whole viewport, so the shell's bottom really is
// behind the keyboard strip and a message would land under it -- the exact
// invisibility the caption exists to avoid. In the page the shell is just
// the picture and the offset would mean nothing.
function placeOsd() {
  if (!osd) return;
  osd.style.bottom = isFullscreen() ? 'var(--cl-kbd-h, 0px)' : '0';
}

function showOsd(text) {
  const el = ensureOsd();
  placeOsd();
  el.textContent = text;
  el.style.opacity = '1';
  clearTimeout(osdHideTimer);
  osdHideTimer = setTimeout(() => {
    el.style.opacity = '0';
  }, 3200);
}

function setLoadStatus(text) {
  loadStatus.textContent = text;
  // Raise the caption only when the shell's own status line cannot be seen.
  // A display:none ancestor -- the hidden-overlay case -- generates no
  // layout boxes, so an empty getClientRects() is the test. (offsetParent
  // would look simpler, but it is also null for a position:fixed element,
  // so a shell with a pinned status line would get the message twice.)
  if (loadStatus.getClientRects().length === 0) showOsd(text);
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
// The page's copy of the last disk that went into DF0. The inserted bytes
// live inside the machine, so switching the machine model (which builds a
// new one) re-inserts from this stash; kept forever, like the ROM stash.
let lastDisk = null; // { bytes, name }

function refreshBootButton() {
  bootBtn.disabled = !(wasm && bootRom);
  bootBtn.textContent = bootRom && bootRom.label !== 'AROS' ? 'Boot Kickstart' : 'Boot AROS';
  // The save-state controls follow the same milestones (the module is
  // ready, a machine is running or has just died), so they refresh here.
  updateStateButtons();
}

// The AROS ROMs the page fetched, kept beyond the boot stash: forgetting a
// remembered Kickstart falls back to these without another download.
let arosRom = null; // { rom, ext, label: 'AROS' }
// Whether a Kickstart was explicitly chosen this session (picker, drop,
// URL, list). The remembered ROM only ever fills in when nothing was: an
// explicit pick always wins, whichever of the two resolves first.
let romChosenExplicitly = false;

// Route picked or dropped Kickstart bytes: live-swap a running machine, or
// stash them for the boot button. The stash is updated on a live swap too,
// so a reboot fits the ROM chosen last, not the one from the original boot;
// a rejected image throws before the stash is touched and changes nothing.
// A ROM that fits is remembered in the browser (IndexedDB), so the next
// visit boots it without the picker; see the saved-states panel to forget.
function fitRom(bytes, label) {
  if (emu) emu.load_rom(bytes, undefined);
  bootRom = { rom: bytes, ext: null, label };
  romChosenExplicitly = true;
  refreshBootButton();
  setLoadStatus(
    emu ? `Kickstart loaded: ${label} - machine power-cycled` : `will boot ${label}`,
  );
  persistRom(bytes, label);
}

// --- remembered Kickstart ----------------------------------------------
// The loaded ROM sticks in browser storage so a returning visitor's page
// boots their Kickstart with no picker round trip, the way the quick state
// does for a whole session. Only an explicitly chosen ROM is stored (AROS
// is bundled and never needs remembering), and only after the core
// accepted it. Everything stays in this browser; nothing is uploaded.

let storedRomInfo = null; // { label, size } of the remembered ROM, when any

async function persistRom(bytes, label) {
  let failure = null;
  try {
    await withDb(ROM_STORE, 'readwrite', (store) =>
      store.put({ rom: bytes, label, saved: new Date() }, ROM_SLOT),
    );
  } catch (e) {
    failure = e;
  }
  if (failure) {
    // The fit itself succeeded; only the remembering did not. Say so once
    // rather than failing silently - a visitor counting on it would
    // otherwise discover the gap a session too late.
    const hint =
      failure.name === 'QuotaExceededError' ? ' - browser storage is full' : '';
    showOsd(`could not remember the Kickstart: ${failure.message ?? failure}${hint}`);
    return;
  }
  storedRomInfo = { label, size: bytes.length };
  refreshStatesPanel();
}

// Startup probe: put the remembered ROM in the boot stash unless something
// explicit (picker, ?kick=, config) already claimed it. Racing the AROS
// fetch is fine either way: whoever lands second sees the other's choice
// (load() only installs AROS into an empty stash, and this only replaces
// nothing or AROS).
async function probeStoredRom() {
  let record;
  try {
    record = await withDb(ROM_STORE, 'readonly', (store) => store.get(ROM_SLOT));
  } catch {
    return;
  }
  if (!record?.rom) return;
  storedRomInfo = { label: record.label ?? 'Kickstart', size: record.rom.length };
  if (romChosenExplicitly || (bootRom && bootRom.label !== 'AROS')) return;
  bootRom = { rom: record.rom, ext: null, label: storedRomInfo.label, remembered: true };
  refreshBootButton();
  if (wasm) {
    setLoadStatus(`ready - boots ${storedRomInfo.label} (remembered in this browser)`);
  }
}

async function forgetStoredRom() {
  try {
    await withDb(ROM_STORE, 'readwrite', (store) => store.delete(ROM_SLOT));
  } catch (e) {
    setLoadStatus(`could not forget the Kickstart: ${e.message ?? e}`);
    return;
  }
  storedRomInfo = null;
  // The next boot goes back to AROS; a running machine keeps the ROM that
  // is physically fitted, exactly like ejecting the box the chip came in.
  if (bootRom?.remembered) {
    // The AROS stash can still be empty here: its download may be in
    // flight (load() adopts the empty boot stash when it lands) or may
    // have failed (the boot button goes back to disabled, exactly as
    // before anything was picked).
    bootRom = arosRom;
    refreshBootButton();
  }
  // Say what the boot button will actually build: the AROS fallback, a
  // Kickstart picked this session (forgetting the memory does not unfit
  // an explicit choice), or nothing yet while AROS is still downloading.
  setLoadStatus(
    bootRom
      ? `Kickstart forgotten - the boot button builds ${bootRom.label}`
      : 'Kickstart forgotten - waiting for the AROS ROM',
  );
  refreshStatesPanel();
}

// Route disk bytes from any source (picker, URL, drop): insert into a
// running machine, or stash them for the boot button to insert after boot.
function insertDisk(bytes, name) {
  lastDisk = { bytes, name };
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
    populateMachineSelect();
    populateVideoSelect();
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
    // Kept beyond the stash: forgetting a remembered Kickstart falls back
    // to AROS without re-downloading it.
    arosRom = { rom, ext, label: 'AROS' };
    // A Kickstart picked (or remembered) while the ROMs were downloading
    // wins; a ?kick= failure rides along either way.
    const problem = romUrlProblem ? ` (${romUrlProblem})` : '';
    if (!bootRom) {
      bootRom = arosRom;
      // A disk that landed first (file picker or ?df0= fetch) keeps its
      // place in the status line.
      setLoadStatus(
        (pendingDisk
          ? `ready - DF0: ${pendingDisk.name} inserts at boot`
          : 'ready - boots the open-source AROS ROM') + problem,
      );
    } else {
      setLoadStatus(
        `ready - boots ${bootRom.label}` +
          (bootRom.remembered ? ' (remembered in this browser)' : '') +
          (pendingDisk ? ` - DF0: ${pendingDisk.name} inserts at boot` : '') +
          problem,
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
    // With no ROM to fit at all, the machine keeps the placeholder WebEmu
    // builds itself: nothing the boot button can reach (it stays disabled
    // until a ROM exists), but a save state carries its own ROM and
    // replaces the whole machine, so a restore can start from one.
    // The model argument picks the machine profile; undefined (an older
    // shell, or the list not knowing better) builds the default A500. The
    // video argument picks PAL/NTSC the same way; a bundle older than both
    // ignores the extra arguments.
    const machine = new WebEmu(machineModel ?? undefined, videoStandard ?? undefined);
    if (bootRom) machine.load_rom(bootRom.rom, bootRom.ext ?? undefined);

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
    if (monoAudioToggle) machine.set_mono_audio(monoAudioToggle.checked);
    else if (configMonoAudio !== null) machine.set_mono_audio(configMonoAudio);
    if (floppySpeed !== null) machine.set_floppy_speed(floppySpeed);
    if (overscanMode !== null) machine.set_overscan?.(overscanMode);
    emu = machine;
    window.__emu = emu; // for debugging/automation
    lastFddTrack = null; // a new machine starts the track latch over
    // Nothing is held down in a machine that has just been built, so the
    // on-screen keyboard forgets rather than sending release codes into it.
    forgetVirtualKeys();
    updateStatusDisks();

    // Leave a fresh status behind: the old line ("inserts at boot", an
    // earlier failure) would otherwise go stale into any bug report filed
    // while the machine runs.
    setLoadStatus(
      // A ROM-less boot is only ever a landing place for a state load,
      // which overwrites this line the moment it lands.
      (bootRom
        ? `booted ${bootRom.label}${machineModel ? ` on the ${machineModel}` : ''}`
        : 'machine built, waiting for the state') +
        (df0Name ? ` - DF0: ${df0Name} (write-protected)` : ''),
    );
    overlay.style.display = 'none';
    showBugLink(false);
    running = true;
    // A reboot from a paused machine must not start the new one paused.
    paused = false;
    setPauseLabel();
    updateStateButtons();
    syncWakeLock();
    // Port fittings live on the machine, so a fresh one needs the pads
    // that are still plugged into the host put back.
    for (const port of padAssignments.values()) fitCd32Pad(port);
    if (joyMode === 'cd32') fitCd32Pad(2);
    // A visitor who left the keyboard up gets it back, but not until there
    // is a machine to type into: raised at page load it would cover half
    // the boot overlay with nothing behind it to receive the keys.
    if (HAS_KEY_RAW && storedPref(KB_OPEN_STORAGE_KEY) === 'on') openKeyboard();
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

// Blit whatever the core last rendered onto the canvas. Called once per
// tick, and again after a save state is loaded: that repaints the restored
// screen straight away, so a load into a paused machine shows where it
// resumes instead of the frame from before the load.
function presentFrame() {
  const rows = emu.present_rows();
  if (rows === 0) return;
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

function tick(nowMs) {
  if (!running) return;
  if (paused) return; // resumePause() restarts the loop
  // Polled, not event-driven: the Gamepad API reports button state only
  // when asked, so this is where a controller reaches the machine.
  pumpGamepads();
  syncCapsLed();
  pumpHostKeys();
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
    forgetVirtualKeys();
    refreshBootButton();
    showBugLink(true);
    syncWakeLock();
    console.error(e);
    return;
  }

  presentFrame();

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
  // The browser drops a screen wake lock when the page hides; re-request
  // it when a still-running machine comes back into view.
  syncWakeLock();
  if (!audioCtx) return;
  if (document.hidden) audioCtx.suspend();
  else audioCtx.resume();
});

// --- screen wake lock --------------------------------------------------
// A running machine keeps the screen awake, the way a video player does:
// demos and long loading sequences are exactly the hands-off viewing that
// trips a host's idle timeout. Released while paused, stopped, or hidden,
// so an idle page never pins the display; browsers without the API (or a
// battery saver refusing it) simply keep their usual timeout.

let wakeLock = null;
let wakeLockPending = false;

async function syncWakeLock() {
  const want = running && !paused && !document.hidden;
  if (want && !wakeLock && !wakeLockPending && navigator.wakeLock?.request) {
    wakeLockPending = true;
    try {
      const lock = await navigator.wakeLock.request('screen');
      // The browser can release it behind our back (tab hidden, battery
      // saver, OS policy). Re-sync right away: a hidden page fails the
      // want-check and is instead re-requested on visibilitychange, and
      // a policy that keeps refusing surfaces as a rejected request, so
      // this cannot ping-pong. Our own release path nulls the handle
      // before releasing, so it never re-enters here.
      lock.addEventListener('release', () => {
        if (wakeLock === lock) {
          wakeLock = null;
          syncWakeLock();
        }
      });
      wakeLock = lock;
    } catch {
      // Refused: nothing to hold, nothing to report.
    } finally {
      wakeLockPending = false;
    }
    // The machine can have paused or stopped while the request was in
    // flight; settle on the state that is true now.
    if (wakeLock && !(running && !paused && !document.hidden)) syncWakeLock();
  } else if (!want && wakeLock) {
    const lock = wakeLock;
    wakeLock = null;
    lock.release().catch(() => {});
  }
}

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

// Port state each input source contributes. The keyboard mapping and the
// touch pad always drive port 2 (the Amiga's joystick port); gamepads fill
// port 2 first and port 1 second (see the gamepad section). Sources on the
// same port are OR-ed rather than one silencing the other, so a pad and
// the keyboard can both be live without either going dead mid-game.
const EMPTY_PAD = {
  up: false,
  down: false,
  left: false,
  right: false,
  fire: false,
  button2: false,
  play: false,
  rwd: false,
  ffw: false,
  green: false,
  yellow: false,
};
const padPort = { 1: null, 2: null }; // gamepad-sourced state per Amiga port

// The touch pad's stick and fire button, when the canvas is in pad mode.
// Declared before the touch section that fills these in; hoisting makes
// that safe, and keeping every port source in one merge is worth it.
function touchPortState() {
  if (joyMode !== 'touch') return null;
  return {
    ...EMPTY_PAD,
    up: stickDirs.up,
    down: stickDirs.down,
    left: stickDirs.left,
    right: stickDirs.right,
    fire: fireTouchId !== null,
  };
}

function keyboardPortState() {
  const h = joyHeld;
  return {
    up: !!h.up,
    down: !!h.down,
    left: !!h.left,
    right: !!h.right,
    fire: !!(h.fireCtrl || h.fireAlt || h.fireLCtrl || h.red),
    button2: !!(h.blue || h.blueLAlt),
    play: !!h.play,
    rwd: !!h.rwd,
    ffw: !!h.ffw,
    green: !!h.green,
    yellow: !!h.yellow,
  };
}

function orPortState(a, b) {
  if (!a) return b ?? EMPTY_PAD;
  if (!b) return a;
  const out = {};
  for (const k of Object.keys(EMPTY_PAD)) out[k] = a[k] || b[k];
  return out;
}

// Push both ports' merged state into the machine. Port 1 is only touched
// while a gamepad holds it, so a mouse-only session never has its port 1
// switched away from the mouse.
function applyJoystick() {
  if (!emu) return;
  const port2 = orPortState(orPortState(keyboardPortState(), touchPortState()), padPort[2]);
  emu.set_joystick_port(2, port2.up, port2.down, port2.left, port2.right, port2.fire, port2.button2);
  emu.set_cd32_buttons_port(2, port2.play, port2.rwd, port2.ffw, port2.green, port2.yellow);
  const port1 = padPort[1];
  if (port1) {
    emu.set_joystick_port(
      1,
      port1.up,
      port1.down,
      port1.left,
      port1.right,
      port1.fire,
      port1.button2,
    );
    emu.set_cd32_buttons_port(1, port1.play, port1.rwd, port1.ffw, port1.green, port1.yellow);
  }
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
  // The cd32 mapping's extra buttons only reach a guest through a fitted
  // CD32 pad; the plain modes leave whatever is in the port alone.
  if (joyMode === 'cd32') fitCd32Pad(2);
  applyJoystick();
}

// Cycles the mode; wired to the control-bar button and to the fullscreen
// overlay's copy of it, which stay in step.
function cycleJoyMode() {
  setJoyMode(JOY_MODES[(JOY_MODES.indexOf(joyMode) + 1) % JOY_MODES.length]);
}

$('joy').addEventListener('click', cycleJoyMode);

// --- gamepads (USB / Bluetooth controllers) --------------------------------
// Real controllers need no toggle: the Gamepad API has no events for
// button state, so the frame loop polls it and whatever is plugged in
// drives a port. The first pad takes port 2 (where an Amiga game looks for
// its joystick), the second takes port 1 -- which is two-player, and is
// also literally what the hardware does: plugging a stick into port 1
// means the mouse is not there any more. When a pad on port 1 goes away
// the mouse is plugged back in, so the pointer never stays dead.
//
// Sticks and d-pads both steer, so the same pad works whichever a game
// expects. The face buttons follow the CD32 pad, which is a superset of a
// two-button stick: A fires (red), B is button 2 (blue), X/Y are
// green/yellow, the shoulders are rewind/forward, Start is play. A plain
// joystick guest only ever sees fire and button 2; the rest exist for
// CD32 titles and cost nothing when unused.

const PAD_AXIS_THRESHOLD = 0.5; // analogue stick deflection that counts
const padAssignments = new Map(); // gamepad index -> Amiga port (2 first)

// A port only reports the CD32 pad's extra buttons while a CD32 pad is
// what is plugged into it: the core runs the pad's shift register for
// PortDevice::Cd32Pad alone, so on a plain joystick those buttons exist
// but nothing can read them. Fitting the pad costs nothing elsewhere --
// outside the serial mode a CD32-aware game selects through POTGO, a pad
// reads exactly like a two-button stick -- so any source that can produce
// the extras (a gamepad, or the keyboard's cd32 mapping) fits one.
function fitCd32Pad(port) {
  emu?.set_port_device(port, 'cd32');
}

function padPressed(pad, index) {
  const b = pad.buttons[index];
  if (!b) return false;
  return typeof b === 'object' ? b.pressed || b.value > 0.5 : b > 0.5;
}

function readPad(pad) {
  const axis = (i) => (typeof pad.axes[i] === 'number' ? pad.axes[i] : 0);
  return {
    up: padPressed(pad, 12) || axis(1) <= -PAD_AXIS_THRESHOLD,
    down: padPressed(pad, 13) || axis(1) >= PAD_AXIS_THRESHOLD,
    left: padPressed(pad, 14) || axis(0) <= -PAD_AXIS_THRESHOLD,
    right: padPressed(pad, 15) || axis(0) >= PAD_AXIS_THRESHOLD,
    fire: padPressed(pad, 0),
    button2: padPressed(pad, 1),
    green: padPressed(pad, 2),
    yellow: padPressed(pad, 3),
    rwd: padPressed(pad, 4),
    ffw: padPressed(pad, 5),
    play: padPressed(pad, 9),
  };
}

// Assign connected pads to ports and drop assignments for pads that went
// away. Returns what changed, so the caller can report it accurately.
function refreshPadAssignments(pads) {
  let changed = false;
  let releasedPort1 = false;
  for (const index of [...padAssignments.keys()]) {
    if (!pads[index]) {
      const port = padAssignments.get(index);
      padAssignments.delete(index);
      padPort[port] = null;
      // Port 1 is the mouse socket on every machine this build boots, so
      // a pad leaving it puts the mouse back; port 2 keeps the pad fitting
      // (idle, and indistinguishable from a joystick to anything that is
      // not driving the CD32 serial protocol).
      if (port === 1 && emu) {
        emu.set_port_device(1, 'mouse');
        releasedPort1 = true;
      }
      changed = true;
    }
  }
  for (const pad of pads) {
    if (!pad || padAssignments.has(pad.index)) continue;
    const taken = new Set(padAssignments.values());
    const port = !taken.has(2) ? 2 : !taken.has(1) ? 1 : null;
    if (port === null) continue; // a third pad has nowhere to go
    padAssignments.set(pad.index, port);
    fitCd32Pad(port); // a real pad has the extra buttons; let them count
    changed = true;
  }
  return { changed, releasedPort1 };
}

function pumpGamepads() {
  if (!navigator.getGamepads) return;
  const pads = navigator.getGamepads();
  const anyConnected = [...pads].some((p) => p);
  if (!anyConnected && padAssignments.size === 0) return;
  const { changed, releasedPort1 } = refreshPadAssignments(pads);
  for (const [index, port] of padAssignments) {
    const pad = pads[index];
    if (pad) padPort[port] = readPad(pad);
  }
  applyJoystick();
  if (changed) updatePadStatus(releasedPort1);
}

// A pad is invisible until the browser reports it, and the assignment is
// not something the visitor chose, so say which port each one landed on.
// The mouse is only worth mentioning when a pad actually vacated port 1,
// which is the only case where the pointer was displaced.
function updatePadStatus(releasedPort1) {
  const where = [...padAssignments.values()]
    .sort()
    .map((port) => `port ${port}`)
    .join(' + ');
  const mouse = releasedPort1 ? ' - mouse restored on port 1' : '';
  if (padAssignments.size === 0) {
    setLoadStatus(`gamepad disconnected${mouse}`);
    return;
  }
  setLoadStatus(
    `gamepad ready: ${where}` +
      (padAssignments.size > 1 ? ' (two players)' : '') +
      mouse,
  );
}

// --- keyboard ------------------------------------------------------------

// Auto-repeat keydowns must not reach the emulator (the Amiga keyboard
// sends one down code; the guest OS does its own repeat), but the browser
// default still has to be suppressed on every repeat or holding a cursor
// key scrolls the page. Track which codes the first keydown consumed.
const consumedKeys = new Set();
window.addEventListener('keydown', (e) => {
  if (!emu || !running) return;
  if (e.repeat) {
    if (consumedKeys.has(e.code)) e.preventDefault();
    return;
  }
  if (joystickKey(e.code, true) || emu.key_event(e.code, true)) {
    consumedKeys.add(e.code);
    e.preventDefault();
  } else {
    consumedKeys.delete(e.code);
  }
});
window.addEventListener('keyup', (e) => {
  // Delete before the running check: a hold that spans an emulator stop
  // or a focus loss must not leave a stale entry behind.
  const consumed = consumedKeys.delete(e.code);
  if (!emu || !running) return;
  if (joystickKey(e.code, false) || emu.key_event(e.code, false) || consumed) {
    e.preventDefault();
  }
});

// --- mouse ---------------------------------------------------------------
// Unlocked: the cursor drives the Amiga pointer through position deltas
// (Workbench-friendly). Click to pointer-lock for relative motion (games);
// Esc releases the lock, as the browser enforces.

let lastPos = null;
// Emulator pixels per CSS pixel, one scale per axis. The bitmap fills the
// canvas element in every mode - fullscreen letterboxes the element
// itself, keeping the display's shape - but the bitmap is not the
// element's shape (the PAL capture is 668x540 shown as 4:3), so the two
// axis ratios differ and sharing one would skew vertical pointer speed.
const cssToEmu = () => ({
  x: canvas.width / canvas.clientWidth,
  y: canvas.height / canvas.clientHeight,
});

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
  applyJoystick(); // the touch pad is one more port-2 source; see the merge
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
        emu.mouse_delta(dx * scale.x, dy * scale.y);
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

// Rest positions while no finger is down, as fractions of the picture --
// not of the shell. In the page layout the two are the same box, but
// fullscreen letterboxes the canvas inside a shell that is the whole
// screen, and an open on-screen keyboard shortens it further; against the
// shell the stick would drift into the pillarbox and then behind the keys.
function placeTouchJoyAtRest(ui) {
  const s = $('shell').getBoundingClientRect();
  const c = canvas.getBoundingClientRect();
  const put = (el, fx, fy) => {
    el.style.left = `${c.left - s.left + c.width * fx}px`;
    el.style.top = `${c.top - s.top + c.height * fy}px`;
  };
  put(ui.base, 0.22, 0.72);
  put(ui.knob, 0.22, 0.72);
  put(ui.fire, 0.8, 0.72);
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
  // Four buttons do not fit a phone's width on one line, so the bar wraps
  // rather than running off the side of the screen.
  bar.style.cssText =
    'position:absolute;z-index:3;display:none;gap:0.5rem;' +
    'flex-wrap:wrap;justify-content:flex-end;' +
    'max-width:calc(100dvw - 1.2rem);' +
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
  const kbd = mk('Keys');
  kbd.addEventListener('click', toggleKeyboard);
  kbd.hidden = !HAS_KEY_RAW;
  // The device's own keyboard, the other half of the keyboard choice.
  // Built only where there is one to raise (see the control-bar button)
  // rather than built and hidden: this bar is glue, so there is no reason
  // for it to carry a button no tap on this device can ever want.
  const dev = HAS_KEY_RAW && hasTouch ? mk('Type') : null;
  dev?.addEventListener('click', toggleHostKeyboard);
  const pause = mk(paused ? 'Resume' : 'Pause');
  pause.addEventListener('click', togglePause);
  const exit = mk('Exit');
  exit.addEventListener('click', exitFullscreen);
  shell.appendChild(bar);
  fsUi = { bar, joy, kbd, dev, pause };
  return fsUi;
}

function updateFsUi() {
  placeOsd();
  if (!isFullscreen()) {
    if (fsUi) fsUi.bar.style.display = 'none';
    return;
  }
  const ui = ensureFsUi();
  ui.joy.textContent = `Joystick: ${joyMode}`;
  ui.kbd.textContent = kbdOpen ? 'Keys off' : 'Keys';
  if (ui.dev) ui.dev.textContent = hostKbdOpen ? 'Type off' : 'Type';
  ui.pause.textContent = paused ? 'Resume' : 'Pause';
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
// Fullscreen letterbox. The shell takes the monitor's shape, which has
// nothing to do with the display's, and a page shell's normal canvas rule
// (width: 100%) would stretch the picture to fill it - grotesquely so on
// an ultrawide monitor. The canvas instead becomes the largest 4:3 box
// that fits, the TV shape every shell gives it in the page layout, and
// the auto margins centre it. Inline styles rather than a page CSS rule
// so every embedding shell letterboxes, not just the hosted page, and
// applied to real fullscreen too. Dynamic viewport units: they measure
// the monitor exactly in real fullscreen, and in the pinned fallback
// (iPhone, where Safari's chrome stays) they track the visible area
// where plain vh would reach under the browser chrome.
// The bottom inset is the on-screen keyboard's strip: the picture is
// centred in what is left above it rather than standing behind it.
// --cl-kbd-h is 0px whenever the keyboard is closed, which computes to
// exactly the geometry this had before the keyboard existed.
const CSS_FS_CANVAS = {
  position: 'absolute',
  inset: '0 0 var(--cl-kbd-h, 0px) 0',
  margin: 'auto',
  width: 'min(100dvw, calc((100dvh - var(--cl-kbd-h, 0px)) * 4 / 3))',
  height: 'min(calc(100dvh - var(--cl-kbd-h, 0px)), calc(100dvw * 3 / 4))',
  // The bitmap fills the box, as in the page layout: the presentation
  // buffer is not itself 4:3 (the PAL capture is 668x540), and a shell's
  // own :fullscreen object-fit rule would re-letterbox it inside the box
  // at the buffer's ratio.
  objectFit: 'fill',
};

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

// Real fullscreen carries the same canvas letterbox as the CSS fallback,
// applied on the state change so it also covers Esc and any other
// browser-initiated exit.
document.addEventListener('fullscreenchange', () => {
  setStyles(canvas, CSS_FS_CANVAS, document.fullscreenElement !== null);
  updateFsUi();
});

// --- on-screen keyboard ----------------------------------------------------
// A phone cannot type into the Amiga. A mobile browser only raises its
// keyboard for a focused text field, and what that field then delivers is
// typed text (an `input` event carrying a character), never the key
// positions the emulator reads - so modifiers, the function keys, Help and
// both Amiga keys are unreachable, which is most of what a guest wants
// (Ctrl+C to a BBS, Ctrl+Amiga+Amiga to reboot). The page therefore draws
// the keyboard itself.
//
// It is an A600: the one Amiga keyboard with no numeric keypad, so the
// whole machine fits a phone's width instead of wasting a third of it on
// keys a touch screen has no room for. Geometry is off the A600 R1.5
// schematic on the Key Layout Editor's 1u grid; rawkeys are RKM Libraries
// table 34-6. Keys go straight to the keyboard MCU as rawkeys (key_raw),
// not as `KeyboardEvent.code` strings: $2B, the key beside Return, has no
// code a browser reports on every host layout, and routing synthetic events
// through the window listener above would hand the cursor keys and Return
// to joystickKey in keys/cd32 mode. An on-screen Amiga keyboard types.

// The on-screen keys ARE rawkeys, so there is no useful degraded mode on a
// bundle that predates key_raw; the button hides itself, exactly as the
// overscan control does. (Class methods exist as soon as the module is
// imported, so this needs no init().)
const HAS_KEY_RAW = typeof WebEmu.prototype?.key_raw === 'function';

const KB_U_WIDE = 16.5; // an A600 is 16.5 keys wide, every row
const KB_U_TALL = 6.5; // six rows plus the 0.5u float under the F row
const KB_U_MAX = 44; // px: the touch-target guideline, ~half a real 19mm cap
const KB_VH = 0.45; // most of the screen the keyboard may ever take
const KB_PAD_X = 10; // px each side; also clears a desktop scrollbar
const KB_PAD_Y = 6;

// Rows lay out left to right and x accumulates, so a key carries only what
// differs from the default 1u cap: `g` is a gap in u before it, `w` the cap
// width, `r` the Amiga rawkey, `t` the main legend, `s` the shifted legend
// printed above it, `m` a latching qualifier's id, `x2` the Return stem.
// Writing 78 x values by hand would be 78 chances to mistype one; the
// accumulation is checked against KB_U_WIDE when the keyboard is built.
//
// Rows 2, 3 and 4 stop at 15.5u rather than 16.5u. That is not a missing
// key: it is the notch the A600's inverted-T cursor cluster sits in.
const KB_ROWS = [
  {
    y: 0,
    k: [
      { r: 0x45, w: 1.25, t: 'Esc' },
      { g: 0.5, r: 0x50, w: 1.25, t: 'F1' },
      { r: 0x51, w: 1.25, t: 'F2' },
      { r: 0x52, w: 1.25, t: 'F3' },
      { r: 0x53, w: 1.25, t: 'F4' },
      { r: 0x54, w: 1.25, t: 'F5' },
      { g: 0.5, r: 0x55, w: 1.25, t: 'F6' },
      { r: 0x56, w: 1.25, t: 'F7' },
      { r: 0x57, w: 1.25, t: 'F8' },
      { r: 0x58, w: 1.25, t: 'F9' },
      { r: 0x59, w: 1.25, t: 'F10' },
      { g: 0.5, r: 0x5f, w: 1.25, t: 'Help' },
    ],
  },
  {
    y: 1.5,
    k: [
      { r: 0x00, w: 1.5, t: '`', s: '~' },
      { r: 0x01, t: '1', s: '!' },
      { r: 0x02, t: '2', s: '"' },
      { r: 0x03, t: '3', s: '\u00a3' }, // pound; the US cap prints 3 #
      { r: 0x04, t: '4', s: '$' },
      { r: 0x05, t: '5', s: '%' },
      { r: 0x06, t: '6', s: '^' },
      { r: 0x07, t: '7', s: '&' },
      { r: 0x08, t: '8', s: '*' },
      { r: 0x09, t: '9', s: '(' },
      { r: 0x0a, t: '0', s: ')' },
      { r: 0x0b, t: '-', s: '_' },
      { r: 0x0c, t: '=', s: '+' },
      { r: 0x0d, t: '\\', s: '|' },
      { r: 0x41, t: 'Bksp' },
      { r: 0x46, t: 'Del' },
    ],
  },
  {
    y: 2.5,
    k: [
      { r: 0x42, w: 2, t: 'Tab' },
      { r: 0x10, t: 'Q' },
      { r: 0x11, t: 'W' },
      { r: 0x12, t: 'E' },
      { r: 0x13, t: 'R' },
      { r: 0x14, t: 'T' },
      { r: 0x15, t: 'Y' },
      { r: 0x16, t: 'U' },
      { r: 0x17, t: 'I' },
      { r: 0x18, t: 'O' },
      { r: 0x19, t: 'P' },
      { r: 0x1a, t: '[', s: '{' },
      { r: 0x1b, t: ']', s: '}' },
      // The ISO reverse-L Return: this is the wide top arm, and `x2` hangs
      // the narrower stem off its bottom edge, inset from the left so the
      // two right edges line up and the notch is bottom-left. Both stop at
      // 15.5u, where the cursor cluster's notch begins.
      { r: 0x44, w: 1.5, t: 'Ret', x2: { dx: 0.25, w: 1.25 } },
    ],
  },
  {
    y: 3.5,
    k: [
      { r: 0x63, w: 1.25, t: 'Ctrl', m: 'ctrl' },
      { r: 0x62, t: 'Caps', caps: true },
      { r: 0x20, t: 'A' },
      { r: 0x21, t: 'S' },
      { r: 0x22, t: 'D' },
      { r: 0x23, t: 'F' },
      { r: 0x24, t: 'G' },
      { r: 0x25, t: 'H' },
      { r: 0x26, t: 'J' },
      { r: 0x27, t: 'K' },
      { r: 0x28, t: 'L' },
      { r: 0x29, t: ';', s: ':' },
      { r: 0x2a, t: "'", s: '@' },
      { r: 0x2b, t: '#', s: '~' },
    ],
  },
  {
    y: 4.5,
    k: [
      { r: 0x60, w: 1.75, t: 'Shift', m: 'lshift' },
      { r: 0x30, t: '\\', s: '|' },
      { r: 0x31, t: 'Z' },
      { r: 0x32, t: 'X' },
      { r: 0x33, t: 'C' },
      { r: 0x34, t: 'V' },
      { r: 0x35, t: 'B' },
      { r: 0x36, t: 'N' },
      { r: 0x37, t: 'M' },
      { r: 0x38, t: ',', s: '<' },
      { r: 0x39, t: '.', s: '>' },
      { r: 0x3a, t: '/', s: '?' },
      { r: 0x61, w: 1.75, t: 'Shift', m: 'rshift' },
      { r: 0x4c, t: '\u2191', aria: 'Cursor up' },
    ],
  },
  {
    y: 5.5,
    k: [
      { g: 0.5, r: 0x64, w: 1.25, t: 'Alt', m: 'lalt' },
      { r: 0x66, w: 1.25, t: 'A', m: 'lamiga', amiga: true, hollow: true, aria: 'Left Amiga' },
      { r: 0x40, w: 8, aria: 'Space' },
      { r: 0x67, w: 1.25, t: 'A', m: 'ramiga', amiga: true, aria: 'Right Amiga' },
      { r: 0x65, w: 1.25, t: 'Alt', m: 'ralt' },
      { r: 0x4f, t: '\u2190', aria: 'Cursor left' },
      { r: 0x4d, t: '\u2193', aria: 'Cursor down' },
      { r: 0x4e, t: '\u2192', aria: 'Cursor right' },
    ],
  },
];

// The only caps a US A600 prints differently; the shell is the same ISO
// 78-key case either way, which is why US machines ship blank keycaps in
// the $2B and $30 positions rather than omitting the switches.
const KB_US_LEGENDS = {
  0x02: ['2', '@'],
  0x03: ['3', '#'],
  0x2a: ["'", '"'],
  0x2b: ['', ''],
  0x30: ['', ''],
};

const KB_LEGENDS_STORAGE_KEY = 'copperline-keyboard-legends';
const KB_OPEN_STORAGE_KEY = 'copperline-keyboard';
// A qualifier tap under this counts as a tap rather than a hold, and two
// taps inside the double window lock it. Matched to the canvas trackpad's
// own tap window so the whole page agrees on what a tap is.
const KB_TAP_MS = 250;
const KB_DOUBLE_MS = 500;

let kbdRoot = null; // built lazily; a desktop session that never opens it
let kbdKeys = []; //   pays nothing but the table above
let kbdOpen = false;
let kbdLegends = 'uk';
let kbdCapsLit = false;
let kbdHintShown = false;
let kbdLegendChip = null;
// pointerId -> key, so a release always reaches the key the finger started
// on even after sliding off it, and several fingers can hold several keys.
const kbdPointers = new Map();
// The seven latching qualifiers. Caps Lock is deliberately not among them:
// the MCU owns that latch (chipset/keyboard.rs key_transition), where a
// press toggles the LED and sends the down code on lock or the up code on
// unlock, and the physical release sends nothing at all.
const kbdMods = new Map();

function kbdSend(rawkey, pressed) {
  if (!emu || !running || !HAS_KEY_RAW) return;
  emu.key_raw(rawkey, pressed);
}

// Key size is whichever of three limits bites first. The row has to fit the
// width; the six rows must not eat more than KB_VH of the height (a phone
// in landscape has almost none to give - a purely width-derived unit would
// put a 290px keyboard in a 390px viewport); and a desktop gets a keyboard,
// not a mural. The safe-area insets come out of the budget rather than
// being added on top of it, so a home indicator shrinks the keys instead of
// pushing the strip taller.
// The strip's padding, written once: the budget below subtracts exactly
// these, so the row can never be sized for more room than the padding
// leaves it. A safe-area inset replaces the default margin rather than
// adding to it, which is why these are max() and not sums.
const KB_PAD_L = `max(${KB_PAD_X}px, env(safe-area-inset-left, 0px))`;
const KB_PAD_R = `max(${KB_PAD_X}px, env(safe-area-inset-right, 0px))`;
const KB_PAD_B = `calc(${KB_PAD_Y}px + env(safe-area-inset-bottom, 0px))`;

function kbdUnitCss() {
  return (
    `min(` +
    `(100dvw - ${KB_PAD_L} - ${KB_PAD_R}) / ${KB_U_WIDE},` +
    `(100dvh * ${KB_VH} - ${KB_PAD_Y}px - ${KB_PAD_B}) / ${KB_U_TALL},` +
    `${KB_U_MAX}px)`
  );
}

function ensureKeyboard() {
  if (kbdRoot) return kbdRoot;
  const root = document.createElement('div');
  // Fixed, not absolute: the shell is only the picture, and the keyboard
  // wants the whole viewport width. `position: fixed` resolves against the
  // viewport in the page, against the fullscreen area in real fullscreen,
  // and against the visible area under the pinned CSS fallback - one
  // placement for all three - and it escapes the shell's overflow:hidden.
  // Above the sticky page furniture but below the site's cosmetic scanline
  // layer (9999, pointer-events:none), so the keys wear the same CRT
  // texture as everything else.
  root.style.cssText =
    'position:fixed;left:0;right:0;bottom:0;z-index:9998;display:none;' +
    'box-sizing:border-box;overflow:hidden;background:rgba(12,15,24,0.94);' +
    `padding:${KB_PAD_Y}px ${KB_PAD_R} ${KB_PAD_B} ${KB_PAD_L};` +
    // Every touch here is a keystroke, never a page gesture: no scrolling,
    // no double-tap zoom, no long-press callout, no selection, no flash.
    'touch-action:none;user-select:none;-webkit-user-select:none;' +
    '-webkit-touch-callout:none;-webkit-tap-highlight-color:transparent;';
  root.style.setProperty('--cl-u', kbdUnitCss());

  const grid = document.createElement('div');
  grid.setAttribute('role', 'group');
  grid.setAttribute('aria-label', 'Amiga 600 on-screen keyboard');
  grid.style.cssText =
    'position:relative;margin-inline:auto;' +
    `width:calc(${KB_U_WIDE} * var(--cl-u));` +
    `height:calc(${KB_U_TALL} * var(--cl-u));`;
  root.appendChild(grid);

  kbdKeys = [];
  for (const row of KB_ROWS) {
    let x = 0;
    for (const spec of row.k) {
      x += spec.g ?? 0;
      kbdKeys.push(buildKeyCap(grid, spec, x, row.y));
      x += spec.w ?? 1;
    }
    if (x > KB_U_WIDE + 1e-6) {
      console.warn(`keyboard row at y=${row.y} is ${x}u wide, over ${KB_U_WIDE}u`);
    }
  }

  grid.appendChild(buildLegendChip());
  wireKeyboardPointers(root);
  applyKbdLegends();
  // Measured rather than derived: the padding carries
  // env(safe-area-inset-bottom) and the height term is in dvh, both of
  // which only CSS can resolve, and a phone changes them on rotation and
  // whenever the browser chrome collapses. The fullscreen letterbox reads
  // the result, so it has to be the real height.
  // The observer is only the trigger; the height comes off the element,
  // because it has to be the border box. `borderBoxSize` is missing on
  // Safari before 15.4 -- the browser this feature exists for -- and
  // `contentRect` is the wrong box anyway: it excludes the padding that
  // carries env(safe-area-inset-bottom), so the letterbox would sit that
  // far into the keys.
  new ResizeObserver(() => {
    if (kbdOpen) publishKbdHeight(Math.round(root.getBoundingClientRect().height));
  }).observe(root);

  shell.appendChild(root);
  kbdRoot = root;
  return root;
}

// The letterbox and the OSD both need to know how much of the screen the
// keyboard is standing on. 0px is the closed state and computes to exactly
// the pre-keyboard geometry, so nothing has to branch on whether it is open.
function publishKbdHeight(px) {
  document.documentElement.style.setProperty('--cl-kbd-h', `${px}px`);
  placeOsd();
}

function buildKeyCap(grid, spec, x, y) {
  const w = spec.w ?? 1;
  // The hit area is the whole grid cell and the visible cap sits inside its
  // padding, so the gaps between keys are still live. On a phone at 22px
  // caps that is about 4px per key back, which is the difference between
  // fumbling and typing.
  const cell = document.createElement('div');
  cell.style.cssText =
    'position:absolute;box-sizing:border-box;padding:2px;' +
    `left:calc(${x} * var(--cl-u));top:calc(${y} * var(--cl-u));` +
    `width:calc(${w} * var(--cl-u));height:var(--cl-u);`;
  cell.setAttribute('role', 'button');
  cell.setAttribute('aria-label', spec.aria ?? spec.t ?? '');
  // Focusable, so the button role is not a promise the element breaks, but
  // deliberately not tabbable: 78 tab stops in front of the page's own
  // controls would be hostile to the very people who have a real keyboard,
  // and a tabbable button also activates on Space and Enter -- both Amiga
  // keys, which the window listener is already sending, so every press
  // would reach the guest twice.
  cell.tabIndex = -1;

  const cap = document.createElement('div');
  cap.style.cssText =
    'width:100%;height:100%;box-sizing:border-box;display:flex;' +
    'flex-direction:column;align-items:center;justify-content:center;' +
    'line-height:1.05;border-radius:4px;overflow:hidden;' +
    'font-family:"IBM Plex Mono",ui-monospace,monospace;font-weight:600;';
  cell.appendChild(cap);

  const key = {
    raw: spec.r,
    mod: spec.m ?? null,
    caps: spec.caps === true,
    cell,
    cap,
    stem: null,
    mainEl: null,
    shiftEl: null,
    down: false,
  };

  if (spec.s !== undefined) {
    // Two legends, printed the way the cap is: shifted glyph above the
    // unshifted one, smaller and dimmer.
    key.shiftEl = document.createElement('div');
    key.shiftEl.style.cssText = 'font-size:calc(0.34 * var(--cl-u));opacity:0.55;';
    key.shiftEl.textContent = spec.s;
    cap.appendChild(key.shiftEl);
    key.mainEl = document.createElement('div');
    key.mainEl.style.cssText = 'font-size:calc(0.42 * var(--cl-u));';
    key.mainEl.textContent = spec.t;
    cap.appendChild(key.mainEl);
  } else if (spec.t !== undefined) {
    key.mainEl = document.createElement('div');
    // Word legends shrink again on the 1u caps that carry them (Caps,
    // Bksp), or they spill out of a 22px key on a phone.
    const size = spec.t.length > 3 && w <= 1.25 ? 0.3 : 0.36;
    key.mainEl.style.cssText = `font-size:calc(${size} * var(--cl-u));`;
    key.mainEl.textContent = spec.t;
    if (spec.amiga) {
      // The Amiga key logo is a leaning A, and the left one is outlined
      // where the right one is filled, as the case prints them.
      key.mainEl.style.fontStyle = 'italic';
    }
    if (spec.hollow) {
      key.mainEl.style.webkitTextStroke = '1px #15171c';
      key.mainEl.style.color = 'transparent';
    }
    cap.appendChild(key.mainEl);
  }

  if (spec.caps) {
    // The keycap LED, driven from the MCU's own lamp rather than our taps.
    key.led = document.createElement('div');
    key.led.style.cssText =
      'position:absolute;top:15%;right:12%;border-radius:50%;' +
      'width:calc(0.16 * var(--cl-u));height:calc(0.16 * var(--cl-u));';
    cell.appendChild(key.led);
  }

  if (spec.x2) {
    // The stem of the ISO Return, a second box hanging off the bottom of
    // the arm. Drawn as a real L rather than clipped, because a clip-path
    // would cut the cap's own outline along exactly the two edges that make
    // the shape; the caps have no borders, so two touching fills read as
    // one key.
    //
    // Making them read as one key needs both halves of this: the fills are
    // fully opaque (two overlapping translucent fills composite to a denser
    // band, which is a seam drawn in the one place it must not be), and the
    // stem starts above the arm's bottom edge rather than at the cell's, so
    // the arm's own 2px padding cannot open a hairline gap. The overlap
    // absorbs the sub-pixel rounding a fractional unit produces.
    cap.style.borderRadius = '4px 4px 0 4px';
    const stem = document.createElement('div');
    stem.style.cssText =
      'position:absolute;box-sizing:border-box;padding:0 2px 2px 2px;' +
      `left:calc(${spec.x2.dx} * var(--cl-u));` +
      'top:calc(var(--cl-u) - 4px);' +
      `width:calc(${spec.x2.w} * var(--cl-u));` +
      'height:calc(var(--cl-u) + 4px);';
    const stemCap = document.createElement('div');
    stemCap.style.cssText = 'width:100%;height:100%;border-radius:0 0 4px 4px;';
    stem.appendChild(stemCap);
    // No data-k on the stem: the delegated handler resolves it by walking
    // up to the Return cell, so the whole L is one key.
    cell.appendChild(stem);
    key.stem = stemCap;
  }

  if (key.mod) {
    kbdMods.set(key.mod, {
      key,
      raw: spec.r,
      down: false,
      latch: 'none',
      pointer: null,
      downAt: 0,
      usedWhileDown: false,
      lastTapAt: 0,
    });
  }

  cell.dataset.k = String(kbdKeys.length);
  grid.appendChild(cell);
  paintKey(key);
  return key;
}

// The legend switch sits on the keyboard itself rather than in the page's
// settings row, which is off the screen in fullscreen - exactly where a
// phone visitor spends the session. It goes in the notch beside the cursor
// cluster, the one part of an A600's outline with no keys in it.
function buildLegendChip() {
  const chip = document.createElement('div');
  chip.style.cssText =
    'position:absolute;box-sizing:border-box;cursor:pointer;' +
    'left:calc(15.5 * var(--cl-u));top:calc(3.6 * var(--cl-u));' +
    'width:calc(var(--cl-u) - 4px);height:calc(0.8 * var(--cl-u));' +
    'display:flex;align-items:center;justify-content:center;' +
    'border-radius:4px;border:1px solid rgba(255,255,255,0.3);' +
    'color:rgba(255,255,255,0.75);' +
    'font:600 calc(0.3 * var(--cl-u)) "IBM Plex Mono",ui-monospace,monospace;';
  chip.dataset.legendChip = '1';
  chip.setAttribute('role', 'button');
  chip.tabIndex = -1;
  kbdLegendChip = chip;
  return chip;
}

// Only the handful of caps a US machine prints differently ever change;
// every other legend was set once when the cap was built.
function applyKbdLegends() {
  const us = kbdLegends === 'us';
  for (const key of kbdKeys) {
    const swap = KB_US_LEGENDS[key.raw];
    if (!swap) continue;
    const spec = kbdSpecFor(key.raw);
    const [main, shift] = us ? swap : [spec.t ?? '', spec.s ?? ''];
    if (key.mainEl) key.mainEl.textContent = main;
    if (key.shiftEl) key.shiftEl.textContent = shift;
  }
  if (kbdLegendChip) {
    kbdLegendChip.textContent = us ? 'US' : 'UK';
    kbdLegendChip.setAttribute('aria-label', `Keycap legends: ${us ? 'US' : 'UK'}`);
  }
}

function kbdSpecFor(raw) {
  for (const row of KB_ROWS) {
    for (const spec of row.k) if (spec.r === raw) return spec;
  }
  return {};
}

function setKbdLegends(next) {
  kbdLegends = next === 'us' ? 'us' : 'uk';
  storePref(KB_LEGENDS_STORAGE_KEY, kbdLegends);
  if (kbdRoot) applyKbdLegends();
  // The device keyboard reads the same legends to work out which key types
  // a character, so its table is rebuilt from the new ones on demand.
  hostCharMap = null;
}

// --- key painting ----------------------------------------------------------
// Flat fills only, and press feedback is a background swap rather than a
// transform: the canvas underneath is doing a full putImageData at 50Hz, and
// a blurred or shadowed layer over it costs a whole-screen readback every
// frame on a phone GPU.
// Opaque, not translucent: the ISO Return is two overlapping boxes, and
// two translucent fills composite to a denser band right along the join.
const KB_CAP_IDLE = 'rgb(226,223,214)';
const KB_CAP_MOD = 'rgb(196,193,184)';
const KB_CAP_DOWN = 'rgb(168,164,152)';
const KB_CAP_LOCKED = 'rgb(232,145,84)';
const KB_INK = '#15171c';

function paintKey(key) {
  const mod = key.mod ? kbdMods.get(key.mod) : null;
  let fill = key.mod ? KB_CAP_MOD : KB_CAP_IDLE;
  let ring = 'none';
  if (mod?.latch === 'locked') {
    fill = KB_CAP_LOCKED;
  } else if (mod?.latch === 'oneshot') {
    // A ring rather than a fill: armed for one keystroke, not held down.
    ring = `inset 0 0 0 2px ${KB_CAP_LOCKED}`;
  }
  if (key.down || (mod?.down && mod.latch === 'none')) fill = KB_CAP_DOWN;
  if (key.caps && kbdCapsLit) fill = KB_CAP_LOCKED;
  key.cap.style.background = fill;
  key.cap.style.boxShadow = ring;
  key.cap.style.color = KB_INK;
  if (key.mainEl?.style.webkitTextStroke) key.mainEl.style.color = 'transparent';
  if (key.stem) {
    key.stem.style.background = fill;
    key.stem.style.boxShadow = ring;
  }
  if (key.led) {
    key.led.style.background = kbdCapsLit ? 'rgb(44,200,80)' : 'rgba(0,0,0,0.25)';
  }
}

function repaintKeyboard() {
  for (const key of kbdKeys) paintKey(key);
}

// --- key press / release ---------------------------------------------------

function pressVirtualKey(key, pointerId) {
  if (key.caps) {
    // One send per tap, and only the press. The MCU owns this latch: a
    // press flips the lamp and emits the down code on lock or the up code
    // on unlock, and it discards the release entirely. Sending both would
    // still toggle once, but mirroring the toggle here as well would
    // double-toggle it.
    kbdSend(key.raw, true);
    key.down = true;
    paintKey(key);
    return;
  }
  const mod = key.mod ? kbdMods.get(key.mod) : null;
  if (mod) {
    mod.pointer = pointerId;
    mod.downAt = performance.now();
    mod.usedWhileDown = false;
    if (!mod.down) {
      kbdSend(mod.raw, true);
      mod.down = true;
    }
    paintKey(key);
    // Checked here as well as on an ordinary key: Ctrl+Amiga+Amiga is made
    // of nothing but qualifiers, so the press that completes it is always
    // this one.
    maybeResetChord();
    return;
  }
  key.down = true;
  kbdSend(key.raw, true);
  // Any qualifier now held is being used, which is what tells its own
  // release apart from a bare tap that should latch.
  for (const m of kbdMods.values()) if (m.down) m.usedWhileDown = true;
  paintKey(key);
  maybeResetChord();
}

function releaseVirtualKey(key) {
  if (key.caps) {
    key.down = false;
    paintKey(key);
    return;
  }
  const mod = key.mod ? kbdMods.get(key.mod) : null;
  if (mod) {
    releaseModifier(mod, key);
    return;
  }
  key.down = false;
  kbdSend(key.raw, false);
  // One-shots clear on the release, not the press, so the guest sees the
  // qualifier held across the whole keystroke.
  for (const m of kbdMods.values()) {
    if (m.latch === 'oneshot' && m.pointer === null) {
      kbdSend(m.raw, false);
      m.down = false;
      m.latch = 'none';
      paintKey(m.key);
    }
  }
  paintKey(key);
}

// A phone has one finger to spare, not two, so a qualifier tapped on its
// own stays down for the next keystroke; tapped twice it locks until tapped
// again; held down with a second finger it behaves like the real key, which
// is what a chord such as Ctrl+Amiga+Amiga wants.
function releaseModifier(mod, key) {
  const now = performance.now();
  mod.pointer = null;
  const tapped = now - mod.downAt < KB_TAP_MS && !mod.usedWhileDown;
  const clear = () => {
    kbdSend(mod.raw, false);
    mod.down = false;
    mod.latch = 'none';
  };
  if (!tapped) {
    clear(); // a real hold, released
  } else if (mod.latch === 'locked') {
    clear(); // tapping a locked qualifier unlocks it
  } else if (mod.latch === 'oneshot') {
    if (now - mod.lastTapAt < KB_DOUBLE_MS) mod.latch = 'locked';
    else clear(); // a second lone tap cancels the arm
  } else {
    mod.latch = 'oneshot'; // stays down for the next key
  }
  mod.lastTapAt = now;
  paintKey(key);
}

// The MCU has just latched Ctrl+Amiga+Amiga and is starting the reset flow;
// a human would now let go. Leaving the qualifiers latched would have
// begin_power_up() report them as still held (set_held runs before the
// in_reset_flow early return), and the next keystroke would reset the
// machine all over again.
function maybeResetChord() {
  const ctrl = kbdMods.get('ctrl');
  const la = kbdMods.get('lamiga');
  const ra = kbdMods.get('ramiga');
  if (ctrl?.down && la?.down && ra?.down) releaseAllVirtualKeys();
}

// Two clean-ups, because the situations differ: this one tells the machine
// the keys came up...
function releaseAllVirtualKeys() {
  for (const key of kbdKeys) {
    if (key.down && !key.mod && !key.caps) kbdSend(key.raw, false);
  }
  for (const mod of kbdMods.values()) if (mod.down) kbdSend(mod.raw, false);
  forgetVirtualKeys();
}

// ...and this one just forgets, for when the machine those keys were
// pressed on no longer exists. Clearing the pointer map too means a finger
// still down across a rebuild sends no phantom release to the new machine.
function forgetVirtualKeys() {
  kbdPointers.clear();
  // Keystrokes still queued were typed at a machine that is not there any
  // more; the new one must not inherit half a word.
  hostKeyQueue.length = 0;
  for (const key of kbdKeys) key.down = false;
  for (const mod of kbdMods.values()) {
    mod.down = false;
    mod.latch = 'none';
    mod.pointer = null;
    mod.usedWhileDown = false;
  }
  if (kbdRoot) repaintKeyboard();
}

function wireKeyboardPointers(root) {
  root.addEventListener('pointerdown', (e) => {
    if (e.target.closest('[data-legend-chip]')) {
      e.preventDefault();
      setKbdLegends(kbdLegends === 'uk' ? 'us' : 'uk');
      return;
    }
    const cell = e.target.closest('[data-k]');
    if (!cell) return;
    // Kills the focus steal, the text selection and the compatibility
    // mouse events in one call. The canvas handlers are on the canvas, and
    // the keyboard covers it, so a tap here never reaches the trackpad.
    e.preventDefault();
    // Captured on the root, not the key: one element can hold several
    // pointer ids at once (so multi-touch still works), and a finger
    // dragged off the strip still delivers its release here instead of
    // leaving the key stuck down in the guest. Capture is an optimisation,
    // not the mechanism -- the pointer map is -- so a pointer the browser
    // has already forgotten must not cost us the keystroke.
    try {
      root.setPointerCapture(e.pointerId);
    } catch {
      // No active pointer with that id; the release still finds its key.
    }
    const key = kbdKeys[Number(cell.dataset.k)];
    if (!key) return;
    kbdPointers.set(e.pointerId, key);
    pressVirtualKey(key, e.pointerId);
    navigator.vibrate?.(8);
  });
  const up = (e) => {
    const key = kbdPointers.get(e.pointerId);
    if (!key) return;
    kbdPointers.delete(e.pointerId);
    releaseVirtualKey(key);
  };
  root.addEventListener('pointerup', up);
  root.addEventListener('pointercancel', up);
  root.addEventListener('lostpointercapture', up);
}

// --- keyboard open / close -------------------------------------------------

function setKeyboardLabel() {
  if (keyboardBtn) keyboardBtn.textContent = kbdOpen ? 'Hide keys' : 'Keyboard';
  if (devKeyboardBtn) {
    devKeyboardBtn.textContent = hostKbdOpen ? 'Hide device keys' : 'Device keys';
  }
  if (fsUi) {
    fsUi.kbd.textContent = kbdOpen ? 'Keys off' : 'Keys';
    if (fsUi.dev) fsUi.dev.textContent = hostKbdOpen ? 'Type off' : 'Type';
  }
}

function openKeyboard() {
  if (kbdOpen) return;
  // One Amiga keyboard at a time: both write to the same machine, both
  // want the same strip of screen, and the device keyboard's field would
  // go on holding focus behind the drawn keys.
  closeHostKeyboard();
  const root = ensureKeyboard();
  // With the pointer locked the cursor is gone, and a mouse cannot reach
  // the keys at all.
  if (document.pointerLockElement) document.exitPointerLock?.();
  kbdOpen = true;
  root.style.display = 'block';
  publishKbdHeight(Math.round(root.getBoundingClientRect().height));
  updateTouchJoyUi();
  setKeyboardLabel();
  storePref(KB_OPEN_STORAGE_KEY, 'on');
  if (!kbdHintShown && innerHeight > innerWidth) {
    kbdHintShown = true;
    showOsd('turn the phone sideways for bigger keys');
  }
}

function closeKeyboard() {
  if (!kbdOpen) return;
  releaseAllVirtualKeys();
  kbdOpen = false;
  kbdRoot.style.display = 'none';
  // Set explicitly rather than waiting for a display:none resize
  // notification, which browsers have disagreed about firing.
  publishKbdHeight(0);
  updateTouchJoyUi();
  setKeyboardLabel();
  storePref(KB_OPEN_STORAGE_KEY, 'off');
}

function toggleKeyboard() {
  if (kbdOpen) closeKeyboard();
  else openKeyboard();
}

// The lamp belongs to the MCU, so it is polled from the frame loop rather
// than mirrored from the taps: a save-state load or a machine rebuild
// changes it without any key being pressed.
function syncCapsLed() {
  if (!kbdOpen || !emu) return;
  const lit = emu.caps_lock_led?.() ?? false;
  if (lit === kbdCapsLit) return;
  kbdCapsLit = lit;
  for (const key of kbdKeys) if (key.caps) paintKey(key);
}

// --- device keyboard -------------------------------------------------------
// The drawn A600 is the keyboard an Amiga guest needs: qualifiers, function
// keys, Help, both Amiga keys, all as key positions. What it is not is the
// keyboard the visitor can already type on without looking -- their own,
// with its swipe typing, its predictions, its languages and its muscle
// memory. So the page offers that one too, and the two are exclusive: a
// machine has one keyboard plugged into it.
//
// This path runs the opposite way round to the drawn one. A soft keyboard
// reports typed *text* rather than key positions (which is why the A600
// exists at all), so an invisible field is focused to raise it, its
// `beforeinput` events say what was typed, and each character is looked up
// in the keycap table above to find the key -- and the Shift -- that types
// it. Everything with no character to send is unreachable here by
// construction: Ctrl, Alt, both Amiga keys, the function keys and Help stay
// the drawn keyboard's job, which is why the button offers a choice rather
// than replacing it.
//
// Which cap prints which character is exactly what the drawn keyboard's
// UK/US legend switch says, so the two share it: that switch is a statement
// about the guest's keymap, and a keymap is what this translation needs.

const HOST_RAW_SPACE = 0x40;
const HOST_RAW_BACKSPACE = 0x41;
const HOST_RAW_TAB = 0x42;
const HOST_RAW_RETURN = 0x44;
const HOST_RAW_DELETE = 0x46;
const HOST_RAW_LSHIFT = 0x60;
// The field is never left empty. An Android keyboard asked to delete from
// an empty field has historically reported nothing at all -- no key event
// and no editing intent -- so it always holds something to aim a backspace
// at. Nothing ever reads the value back: every edit is cancelled before it
// lands, which is also why the padding stays put.
const HOST_PAD = '  ';
// A dismissal that has only just happened is what a tap on the toggle
// meant; see toggleHostKeyboard.
const HOST_DISMISS_GRACE_MS = 400;

let hostField = null;
let hostKbdOpen = false;
let hostHintShown = false;
let hostImeActive = false; // an IME composition is on screen
let hostComposed = ''; // how much of it has already gone to the guest
let hostDismissedAt = 0;
let hostViewportWatched = false;
let hostCharMap = null;

// Character -> the cap that types it, derived from the keycap table so the
// two can never drift apart. Only caps that print something take part:
// qualifiers type nothing, and the keys the table names with `aria` are
// legends rather than characters (space is added by hand, the cursor keys
// and the Amiga keys have no character at all).
function buildHostCharMap() {
  const map = new Map();
  // First cap wins, which is what a keyboard does: the A600 has two keys
  // printed \ and |, and either types the same character on the guest.
  // `caps` marks the keys the guest's Caps Lock applies to -- the letters,
  // and only the letters, as a keymap's capsable flag has it.
  const add = (ch, raw, shift, caps = false) => {
    if (ch.length === 1 && !map.has(ch)) map.set(ch, { raw, shift, caps });
  };
  const us = kbdLegends === 'us';
  for (const row of KB_ROWS) {
    for (const spec of row.k) {
      if (spec.m || spec.caps || spec.aria) continue;
      const swap = us ? KB_US_LEGENDS[spec.r] : null;
      const main = swap ? swap[0] : (spec.t ?? '');
      const shifted = swap ? swap[1] : (spec.s ?? '');
      // Letters are printed on the cap in upper case and typed in either,
      // so they are the one legend that maps to two characters. Word
      // legends (Esc, Bksp) are longer than a character and `add` drops
      // them; a US machine's two blank caps come through as empty and go
      // the same way.
      if (/^[A-Za-z]$/.test(main)) {
        add(main.toLowerCase(), spec.r, false, true);
        add(main.toUpperCase(), spec.r, true, true);
      } else {
        add(main, spec.r, false);
      }
      add(shifted, spec.r, true);
    }
  }
  map.set(' ', { raw: HOST_RAW_SPACE, shift: false });
  // Keys that can arrive as text inside an insertion rather than as an
  // editing intent of their own -- a line break in a swipe-typed phrase, a
  // tab in something pasted.
  map.set('\n', { raw: HOST_RAW_RETURN, shift: false });
  map.set('\t', { raw: HOST_RAW_TAB, shift: false });
  // Phone keyboards insert typographic punctuation where the Amiga has
  // only the typewriter forms, and a visitor who types an apostrophe means
  // the apostrophe key whatever their keyboard sent.
  for (const [fancy, plain] of [
    ['\u2018', "'"], // left single quote
    ['\u2019', "'"], // right single quote, an iPhone's apostrophe
    ['\u201c', '"'], // left double quote
    ['\u201d', '"'], // right double quote
    ['\u2013', '-'], // en dash
    ['\u2014', '-'], // em dash
  ]) {
    const key = map.get(plain);
    if (key) map.set(fancy, key);
  }
  return map;
}

function hostCharKey(ch) {
  if (!hostCharMap) hostCharMap = buildHostCharMap();
  return hostCharMap.get(ch) ?? null;
}

// The MCU's type-ahead buffer is ten events deep and drops whatever
// overflows, exactly as the 6500/1 does (chipset/keyboard.rs
// TYPEAHEAD_CAPACITY). A swipe-typed word arrives as one insertion of a
// dozen characters, which is fifty-odd key events: posted in one go, the
// guest would hear about a fifth of them. They queue here instead and the
// frame loop feeds them in at a rate a keyboard could plausibly send.
const hostKeyQueue = [];
const HOST_KEYS_PER_FRAME = 2;

function pumpHostKeys() {
  for (let i = 0; i < HOST_KEYS_PER_FRAME && hostKeyQueue.length; i++) {
    const [raw, pressed] = hostKeyQueue.shift();
    kbdSend(raw, pressed);
  }
}

// A whole keystroke, queued as one: nothing is ever left half-typed, so a
// Shift can never be stranded down over the rest of a session.
function hostTapKey(raw, shift = false) {
  // Only the frame loop drains this, and it runs for neither a machine
  // that does not exist nor one whose clock is stopped -- so a keyboard
  // raised over the boot overlay, or typed at while paused, would fill the
  // queue rather than move it, and a paragraph would land in one burst on
  // the resume. Nothing is typed into a paused machine, which is what
  // pausing means.
  if (!emu || !running || paused) return;
  if (shift) hostKeyQueue.push([HOST_RAW_LSHIFT, true]);
  hostKeyQueue.push([raw, true], [raw, false]);
  if (shift) hostKeyQueue.push([HOST_RAW_LSHIFT, false]);
}

function hostTypeText(text) {
  // Iterates code points, so an emoji is one unmappable character rather
  // than two halves of one. CRLF is folded first, or pasted Windows text
  // would type every line ending twice.
  //
  // The guest's Caps Lock is taken into account, which the drawn keyboard
  // deliberately does not do. That one sends key positions and the visitor
  // can see the lamp on the cap; this one is handed characters, by a
  // keyboard whose own shift state has nothing to do with the Amiga's, and
  // there is no lamp anywhere in sight -- so a locked guest would answer
  // every typed word in the case the visitor did not ask for. A keymap
  // treats Caps Lock as a shift its capsable keys exclusive-or with (only
  // the letters are capsable), so inverting the Shift for those while the
  // lamp is lit types what was actually typed. On the other reading, where
  // the lock forces upper case whatever Shift does, lower case is
  // unreachable while it is on and this changes nothing either way.
  const caps = emu?.caps_lock_led?.() ?? false;
  for (const ch of text.replace(/\r\n?/g, '\n')) {
    const key = hostCharKey(ch);
    if (key) hostTapKey(key.raw, key.shift !== (caps && key.caps));
    else showOsd(`no Amiga key types "${ch}"`);
  }
}

// A predictive keyboard rewrites its whole staged word on every keystroke,
// so what goes to the guest is the difference from what it staged last
// time: appending a letter types one letter, and accepting a suggestion
// backspaces over what was staged and types the replacement -- which is
// exactly what the visitor watches happen in their own candidate bar.
function hostComposeTo(next) {
  let same = 0;
  while (same < hostComposed.length && same < next.length && hostComposed[same] === next[same]) {
    same++;
  }
  for (let i = hostComposed.length; i > same; i--) hostTapKey(HOST_RAW_BACKSPACE);
  hostComposed = next;
  hostTypeText(next.slice(same));
}

function onHostBeforeInput(e) {
  // The field is a listening post, never a text box: the edit is cancelled
  // and only its description is used. (A composition is not always
  // cancellable, which is what resetHostField cleans up after.)
  e.preventDefault();
  if (!hostKbdOpen) return;
  const type = e.inputType ?? '';
  const data = e.data ?? e.dataTransfer?.getData('text') ?? '';
  if (type === 'insertCompositionText') {
    hostComposeTo(data);
    return;
  }
  if (type.startsWith('delete')) {
    // A word delete sends one backspace rather than guessing how much of
    // the guest's line the word was: the field cannot see what is on the
    // Amiga's screen, and erasing too much is not recoverable.
    if (hostComposed) hostComposeTo(hostComposed.slice(0, -1));
    else hostTapKey(type.endsWith('Forward') ? HOST_RAW_DELETE : HOST_RAW_BACKSPACE);
    return;
  }
  if (!type.startsWith('insert')) return;
  if (type === 'insertLineBreak' || type === 'insertParagraph') {
    hostComposed = '';
    hostTapKey(HOST_RAW_RETURN);
    return;
  }
  // Some browsers commit a finished composition a second time, as plain
  // text; it has already been typed, so only what is new is sent on.
  let text = data;
  if (hostComposed && text.startsWith(hostComposed)) text = text.slice(hostComposed.length);
  hostComposed = '';
  hostTypeText(text);
}

// Nothing may accumulate in the field. Every edit is cancelled, but an
// IME's composition can refuse to be, so whatever landed is wiped once it
// has been read and the caret goes back behind the padding.
function resetHostField() {
  if (!hostField) return;
  if (hostField.value !== HOST_PAD) hostField.value = HOST_PAD;
  const end = HOST_PAD.length;
  if (hostField.selectionStart !== end || hostField.selectionEnd !== end) {
    hostField.setSelectionRange(end, end);
  }
}

function ensureHostField() {
  if (hostField) return hostField;
  // A textarea rather than a text input, for one key: Return. A single-line
  // input has nowhere to put a line break, so its Return submits a form
  // that does not exist and reports no editing intent at all -- and the
  // keydown a soft keyboard sends alongside carries no `code` to fall back
  // on. In a textarea the same key is an insertLineBreak, which is a
  // Return the guest can hear.
  const field = document.createElement('textarea');
  field.rows = 1;
  // Every writing aid off: the field is a wire to a machine that has never
  // heard of autocorrect, and a browser rewriting what was typed on the way
  // through would be rewriting the guest's input.
  field.setAttribute('autocomplete', 'off');
  field.setAttribute('autocorrect', 'off');
  field.setAttribute('autocapitalize', 'off');
  field.setAttribute('spellcheck', 'false');
  field.setAttribute('inputmode', 'text');
  field.setAttribute('enterkeyhint', 'enter');
  field.setAttribute('aria-label', 'Type into the Amiga');
  // Out of the tab order for the same reason the drawn keys are: a page's
  // own controls come first for whoever is tabbing through them.
  field.tabIndex = -1;
  // Invisible, but genuinely rendered: neither display:none nor
  // visibility:hidden can hold focus, and focus is the entire mechanism.
  // Not fully transparent either -- iOS has refused the keyboard to a field
  // at opacity 0 -- so it is one 1px corner at 1% of a transparent colour.
  // It sits at the top of the viewport, the one part no soft keyboard ever
  // covers, so no browser scrolls the page to bring it into view; 16px is
  // the font size below which iOS zooms the page in on a focused field.
  field.style.cssText =
    'position:fixed;left:0;top:0;width:1px;height:1px;padding:0;border:0;' +
    'outline:none;resize:none;overflow:hidden;opacity:0.01;z-index:-1;' +
    'font-size:16px;background:transparent;color:transparent;' +
    'caret-color:transparent;';
  field.addEventListener('beforeinput', onHostBeforeInput);
  field.addEventListener('compositionstart', () => {
    hostImeActive = true;
    hostComposed = '';
  });
  field.addEventListener('compositionend', () => {
    hostImeActive = false;
    // What was staged is deliberately still remembered here. Browsers
    // disagree about whether the commit arrives as the last composition
    // update or as a plain insertion after this event, and in the second
    // ordering that insertion is the whole word again -- already typed.
    // The insert branch strips it and clears this, so the guard only ever
    // covers the one insertion that follows a composition.
    resetHostField();
  });
  field.addEventListener('input', () => {
    if (!hostImeActive) resetHostField();
  });
  // The visitor can put this keyboard away without touching the button --
  // the phone's own hide key, a tap on a page control -- and a button that
  // then still says the keyboard is up is lying about the screen.
  field.addEventListener('blur', () => {
    if (!hostKbdOpen) return;
    hostDismissedAt = performance.now();
    closeHostKeyboard();
  });
  // Inside the shell, which is what goes fullscreen: a focused element
  // outside the fullscreen subtree is not being displayed, and a browser
  // owes it no keyboard.
  shell.appendChild(field);
  hostField = field;
  return field;
}

// How much of the viewport the device keyboard is standing on. No browser
// reports that directly, and the two of them do opposite things: one
// shrinks the page under the keyboard (where the letterbox's dvh units have
// already accounted for it and this measures nothing), the other leaves the
// page alone and shrinks the visual viewport over it, which is the
// difference this reads. Either way the answer goes out as the --cl-kbd-h
// the drawn keyboard publishes, so the fullscreen letterbox reserves room
// for this keyboard without knowing which one is up.
function hostViewportOcclusion() {
  const vv = window.visualViewport;
  if (!vv) return 0;
  const hidden = window.innerHeight - (vv.height + vv.offsetTop);
  return hidden > 1 ? Math.round(hidden) : 0;
}

function onHostViewportChange() {
  if (!hostKbdOpen) return;
  publishKbdHeight(hostViewportOcclusion());
  updateTouchJoyUi();
}

function watchHostViewport(on) {
  const vv = window.visualViewport;
  if (!vv || on === hostViewportWatched) return;
  hostViewportWatched = on;
  const how = on ? 'addEventListener' : 'removeEventListener';
  // scroll as well as resize: iOS moves the visual viewport over a page it
  // did not resize, so the keyboard's arrival can be a shift rather than a
  // size change.
  vv[how]('resize', onHostViewportChange);
  vv[how]('scroll', onHostViewportChange);
  if (on) onHostViewportChange();
}

function openHostKeyboard() {
  if (hostKbdOpen) return;
  closeKeyboard();
  // With the pointer locked there is no cursor, and a locked page cannot
  // reach the field either.
  if (document.pointerLockElement) document.exitPointerLock?.();
  const field = ensureHostField();
  hostKbdOpen = true;
  hostComposed = '';
  resetHostField();
  // Raising a soft keyboard is focus, and a browser only grants it inside
  // the gesture that asked for it -- which is why this one, unlike the
  // drawn keyboard, is not put back up on its own at boot.
  field.focus({ preventScroll: true });
  resetHostField(); // focus puts the caret where the browser likes
  watchHostViewport(true);
  setKeyboardLabel();
  if (!hostHintShown) {
    hostHintShown = true;
    showOsd('your keyboard types text - the Amiga keyboard has Ctrl, Alt and the F keys');
  }
}

function closeHostKeyboard() {
  if (!hostKbdOpen) return;
  hostKbdOpen = false;
  hostComposed = '';
  hostField?.blur();
  // Keystrokes already queued are deliberately left to drain: they are
  // press/release pairs the guest is halfway through hearing, and cutting
  // one in half would leave a key held down in it.
  watchHostViewport(false);
  publishKbdHeight(0);
  updateTouchJoyUi();
  setKeyboardLabel();
}

function toggleHostKeyboard() {
  // A tap on the toggle blurs the field before the click arrives, and that
  // blur is what puts the keyboard away -- so by the time the click lands
  // the keyboard is already down and a plain toggle would raise it straight
  // back. A dismissal that has only just happened is what this tap meant.
  if (hostKbdOpen || performance.now() - hostDismissedAt < HOST_DISMISS_GRACE_MS) {
    hostDismissedAt = 0;
    closeHostKeyboard();
    return;
  }
  openHostKeyboard();
}

$('vol').addEventListener('input', (e) => {
  if (emu) emu.set_volume_percent(Number(e.target.value));
});

// --- pause / screenshot ----------------------------------------------------
// Two machine controls that belong on every shell, so they follow the
// floppy-speed pattern: a page can host its own #pause / #screenshot
// buttons wherever its control bar wants them, and without those elements
// the controls insert themselves below the canvas.
//
// Pause stops the emulated clock rather than the page: the frame loop
// stops stepping, audio is suspended so the last buffer does not loop,
// and resuming resyncs the pacer's wall-clock anchor (otherwise the first
// tick back would sprint through every frame the pause "owed").

function setPauseLabel() {
  const label = paused ? 'Resume' : 'Pause';
  if (pauseBtn) pauseBtn.textContent = label;
  if (fsUi) fsUi.pause.textContent = label;
}

function setPaused(next) {
  if (!emu || !running || next === paused) return;
  paused = next;
  setPauseLabel();
  syncWakeLock();
  if (paused) {
    audioCtx?.suspend().catch(() => {});
    setLoadStatus('paused');
  } else {
    audioCtx?.resume().catch(() => {});
    // Nothing elapsed for the guest while paused; start pacing from now.
    emu.resync_clock?.();
    setLoadStatus('running');
    requestAnimationFrame(tick);
  }
}

function togglePause() {
  setPaused(!paused);
}

// The canvas already holds exactly what the screen shows, so a screenshot
// is the canvas itself. Clipboard first (what was asked for), with a file
// download as the fallback: clipboard image writes need a secure context
// and browser support, and Firefox has neither for ClipboardItem in all
// versions. Both paths are driven from a click, which is the user gesture
// the clipboard API requires.
async function copyScreenshot() {
  if (!emu || !running) return;
  const blobOf = () =>
    new Promise((resolve, reject) => {
      // The screen tint is a CSS filter on the canvas element, which
      // toBlob never sees; bake it in through a filtered copy so the
      // screenshot shows what the visitor is looking at. Browsers without
      // canvas-context filters capture the untinted picture.
      let source = canvas;
      const filter = tintFilter();
      if (filter) {
        const copy = document.createElement('canvas');
        copy.width = canvas.width;
        copy.height = canvas.height;
        const copyCtx = copy.getContext('2d');
        if (copyCtx && typeof copyCtx.filter === 'string') {
          copyCtx.filter = filter;
          copyCtx.drawImage(canvas, 0, 0);
          source = copy;
        }
      }
      source.toBlob((b) => (b ? resolve(b) : reject(new Error('canvas capture failed'))), 'image/png');
    });
  try {
    if (!navigator.clipboard?.write || typeof ClipboardItem === 'undefined') {
      throw new Error('clipboard images unsupported');
    }
    // Safari requires the ClipboardItem to be constructed with the promise
    // inside the gesture; Chrome and Firefox accept that form too.
    await navigator.clipboard.write([new ClipboardItem({ 'image/png': blobOf() })]);
    setLoadStatus('screenshot copied to the clipboard');
  } catch (e) {
    try {
      const url = URL.createObjectURL(await blobOf());
      const a = document.createElement('a');
      a.href = url;
      a.download = `copperline-${new Date().toISOString().replace(/[:.]/g, '-')}.png`;
      a.click();
      // Revoking synchronously can cancel the download that click just
      // started; let the current task finish first.
      setTimeout(() => URL.revokeObjectURL(url), 60_000);
      setLoadStatus(`screenshot downloaded (clipboard unavailable: ${e.message ?? e})`);
    } catch (err) {
      setLoadStatus(`screenshot failed: ${err.message ?? err}`);
    }
  }
}

// --- save states -----------------------------------------------------------
// The desktop's save states, with the browser's storage instead of a
// filesystem. A state is the whole machine - RAM, ROM, chipset, CPU and the
// inserted floppy images themselves - in the same .clstate format the
// desktop writes, so one moves between the two in either direction.
//
// Two destinations, because they answer different questions. "Save state"
// downloads the blob as a file: it survives everything, and it can be
// shared or carried to a desktop build. Quick save keeps it in IndexedDB
// under a single slot, which is what a visitor resuming a game actually
// wants - one click out, one click back in, across page loads and browser
// restarts, with nothing in the downloads folder.
//
// No keyboard shortcuts: every key on this page belongs to the guest (the
// desktop's Cmd/Alt+Shift+S has no equivalent here that would not shadow an
// Amiga key), so these are buttons only.

const STATE_DB_NAME = 'copperline';
// Version 2 adds the 'roms' store (the remembered Kickstart); version 1
// databases upgrade in place, keeping their quick state.
const STATE_DB_VERSION = 2;
const STATE_STORE = 'states';
const ROM_STORE = 'roms';
// One quick slot, still what a visitor resuming a game wants first; named
// slots live in the same store under a prefix (see the saved-states panel).
const QUICK_SLOT = 'quick';
// Named slots are keyed 'named:<name>', so a state literally named "quick"
// can never shadow the quick slot.
const NAMED_SLOT_PREFIX = 'named:';
// The remembered Kickstart's key in the roms store.
const ROM_SLOT = 'kick';

let quickStateInfo = null; // metadata of the stored quick state, when there is one

function openStateDb() {
  return new Promise((resolve, reject) => {
    if (!window.indexedDB) {
      reject(new Error('this browser has no IndexedDB'));
      return;
    }
    const req = indexedDB.open(STATE_DB_NAME, STATE_DB_VERSION);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STATE_STORE)) db.createObjectStore(STATE_STORE);
      if (!db.objectStoreNames.contains(ROM_STORE)) db.createObjectStore(ROM_STORE);
    };
    req.onsuccess = () => resolve(req.result);
    // Private-browsing modes and blocked storage reject the open itself.
    req.onerror = () => reject(req.error ?? new Error('IndexedDB unavailable'));
    req.onblocked = () => reject(new Error('IndexedDB blocked by another tab'));
  });
}

// Resolve on commit, not on the request: a put that succeeds can still lose
// its transaction to the storage quota, and a quick save that quietly did
// not persist is exactly the failure a visitor would discover too late.
function dbTx(db, storeName, mode, run) {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(storeName, mode);
    const req = run(tx.objectStore(storeName));
    tx.oncomplete = () => resolve(req?.result);
    tx.onerror = () => reject(tx.error ?? new Error('IndexedDB transaction failed'));
    tx.onabort = () => reject(tx.error ?? new Error('IndexedDB transaction aborted'));
  });
}

async function withDb(storeName, mode, run) {
  const db = await openStateDb();
  try {
    return await dbTx(db, storeName, mode, run);
  } finally {
    db.close();
  }
}

async function withStateDb(mode, run) {
  return withDb(STATE_STORE, mode, run);
}

// Everything a state needs to describe itself in the UI. Uint8Array and Date
// are structured-cloneable, so the record stores as it stands.
function stateRecord(bytes) {
  return {
    bytes,
    saved: new Date(),
    emulated: emu.emulated_seconds(),
    rom: bootRom?.label ?? 'unknown',
    df0: df0Name,
    machine: emu.machine_model?.() ?? machineModel ?? null,
  };
}

function describeState(info) {
  if (!info) return '';
  const when = info.saved instanceof Date ? info.saved.toLocaleString() : 'unknown time';
  const machine = info.machine ? `${info.machine}, ` : '';
  return `${when} - ${info.df0 ?? 'no disk'} (${machine}${Math.round(info.emulated ?? 0)}s emulated)`;
}

// Enablement follows what each control can actually do right now: saving
// needs a running machine, loading only needs the wasm module (it boots one
// on demand), and quick load additionally needs something in the slot.
function updateStateButtons() {
  const live = Boolean(emu && running);
  if (saveStateBtn) saveStateBtn.disabled = !live;
  if (quickSaveBtn) quickSaveBtn.disabled = !live;
  if (loadStateBtn) loadStateBtn.disabled = !wasm;
  if (quickLoadBtn) {
    quickLoadBtn.disabled = !wasm || !quickStateInfo;
    quickLoadBtn.title = quickStateInfo
      ? `Saved in this browser: ${describeState(quickStateInfo)}`
      : 'No quick state saved in this browser yet';
  }
}

// The machine a state loads into: states carry their own ROM and disks, so
// booting first and restoring over it is enough, and a visitor can land
// straight back in a game from a cold page load. No boot ROM is needed for
// that - not even AROS, whose download may have failed, or a self-hosted
// shell that serves none - because the restore replaces the whole machine
// including its ROM. Reports whether it had to boot, so a restore that
// then fails can put the page back rather than strand the visitor on a
// machine they never asked to start.
async function machineForStateLoad() {
  if (emu && running) return { ready: true, booted: false };
  if (!wasm) {
    setLoadStatus('the emulator is still loading');
    return { ready: false, booted: false };
  }
  await boot();
  return { ready: Boolean(emu && running), booted: true };
}

// Undo a boot that only happened to receive a state which then would not
// load. Without the state there is nothing to run - a ROM-less machine
// does nothing at all - so the page returns to its pre-boot screen with
// the failure still on the status line.
function unbootAfterFailedStateLoad() {
  const failure = loadStatus.textContent;
  emu = null;
  window.__emu = null;
  forgetVirtualKeys();
  running = false;
  paused = false;
  setPauseLabel();
  syncWakeLock();
  overlay.style.display = '';
  refreshBootButton();
  setLoadStatus(failure);
}

// Restore from a blob, whatever produced it. The core leaves the running
// machine untouched when a blob does not parse, so a bad file costs the
// visitor nothing but the message.
function restoreState(bytes, source) {
  try {
    emu.load_state(bytes);
  } catch (e) {
    setLoadStatus(`${source} failed to load: ${e.message ?? e}`);
    return false;
  }
  // Host-side settings are not part of the machine, so the page's own
  // choices are re-applied over the restored one; the state's idea of them
  // came from whatever session saved it.
  emu.set_volume_percent(Number($('vol').value));
  if (floppySoundsToggle) emu.set_floppy_sounds(floppySoundsToggle.checked);
  else if (configFloppySounds !== null) emu.set_floppy_sounds(configFloppySounds);
  if (monoAudioToggle) emu.set_mono_audio(monoAudioToggle.checked);
  else if (configMonoAudio !== null) emu.set_mono_audio(configMonoAudio);
  if (floppySpeed !== null) emu.set_floppy_speed(floppySpeed);
  if (overscanMode !== null) emu.set_overscan?.(overscanMode);
  // Port fittings live on the machine, so the pads plugged into the host go
  // back into the restored one, exactly as after a boot. Port 1 is the
  // mouse socket first: a state saved while a pad occupied it would
  // otherwise restore a stick nothing drives, and the pointer would be
  // dead until the visitor plugged that pad back in.
  if (!new Set(padAssignments.values()).has(1)) emu.set_port_device(1, 'mouse');
  for (const port of padAssignments.values()) fitCd32Pad(port);
  if (joyMode === 'cd32') fitCd32Pad(2);
  // A restored state carries its own idea of which keys are held, so the
  // on-screen keyboard's latches are stale: believe the machine.
  forgetVirtualKeys();
  // The disk came back inside the state; believe the machine, not the page.
  df0Name = emu.disk_name(0) ?? null;
  lastFddTrack = null;
  updateStatusDisks();
  // So did the machine itself, model, video standard and all.
  syncMachineSelect();
  syncVideoSelect();
  // Paint the restored screen now: a load into a paused machine steps no
  // frames, so nothing else would.
  presentFrame();
  setLoadStatus(
    `state loaded from ${source}` + (df0Name ? ` - DF0: ${df0Name}` : ''),
  );
  return true;
}

// Download the state as a file, the shareable, permanent form.
function downloadState() {
  if (!emu || !running) return;
  try {
    const blob = new Blob([emu.save_state()], { type: 'application/octet-stream' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `copperline-${new Date().toISOString().replace(/[:.]/g, '-')}.clstate`;
    a.click();
    // Revoking synchronously can cancel the download that click just
    // started; let the current task finish first (as for screenshots).
    setTimeout(() => URL.revokeObjectURL(url), 60_000);
    setLoadStatus('save state downloaded');
  } catch (e) {
    setLoadStatus(`save state failed: ${e.message ?? e}`);
  }
}

async function loadStateFromFile(file) {
  if (!file) return;
  let bytes;
  try {
    bytes = new Uint8Array(await file.arrayBuffer());
  } catch (e) {
    setLoadStatus(`${file.name}: could not be read (${e.message ?? e})`);
    return;
  }
  const machine = await machineForStateLoad();
  if (!machine.ready) return;
  if (!restoreState(bytes, file.name) && machine.booted) unbootAfterFailedStateLoad();
}

async function quickSaveState() {
  if (!emu || !running) return;
  let record;
  try {
    record = stateRecord(emu.save_state());
  } catch (e) {
    setLoadStatus(`quick save failed: ${e.message ?? e}`);
    return;
  }
  try {
    await withStateDb('readwrite', (store) => store.put(record, QUICK_SLOT));
  } catch (e) {
    // Quota is the failure worth naming: states are around a megabyte and a
    // browser low on storage refuses the write rather than evicting.
    const hint = e.name === 'QuotaExceededError' ? ' - browser storage is full' : '';
    setLoadStatus(`quick save failed: ${e.message ?? e}${hint}`);
    return;
  }
  const { bytes, ...info } = record;
  quickStateInfo = info;
  updateStateButtons();
  setLoadStatus(`quick state saved in this browser (${Math.round(bytes.length / 1024)} KB)`);
  refreshStatesPanel();
}

async function quickLoadState() {
  let record;
  try {
    record = await withStateDb('readonly', (store) => store.get(QUICK_SLOT));
  } catch (e) {
    setLoadStatus(`quick load failed: ${e.message ?? e}`);
    return;
  }
  if (!record?.bytes) {
    setLoadStatus('no quick state saved in this browser');
    quickStateInfo = null;
    updateStateButtons();
    return;
  }
  const machine = await machineForStateLoad();
  if (!machine.ready) return;
  if (!restoreState(record.bytes, 'this browser') && machine.booted) {
    unbootAfterFailedStateLoad();
  }
}

// What the quick slot holds, for the button's enabled state and tooltip.
// A browser that refuses storage simply leaves quick load disabled.
async function probeQuickState() {
  try {
    const record = await withStateDb('readonly', (store) => store.get(QUICK_SLOT));
    if (record?.bytes) {
      const { bytes, ...info } = record;
      quickStateInfo = info;
    }
  } catch {
    quickStateInfo = null;
  }
  updateStateButtons();
}

// Build whichever of the controls the shell did not provide, in one row that
// matches the self-inserted floppy-speed control's styling. Listeners are
// attached afterwards, once, so a shell-provided and a self-built button
// take exactly the same path.
function buildMachineControls() {
  const missing = [
    ['pause', 'Pause'],
    ['keyboard', 'Keyboard'],
    // Only where a device keyboard exists to raise. Left out of the row
    // rather than inserted and hidden, so no shell's own `button` rule can
    // out-rank the `hidden` attribute and put it back on a desktop.
    ...(hasTouch && HAS_KEY_RAW ? [['devkeyboard', 'Device keys']] : []),
    ['screenshot', 'Screenshot'],
    ['savestate', 'Save state'],
    ['loadstate', 'Load state...'],
    ['quicksave', 'Quick save'],
    ['quickload', 'Quick load'],
    ['savedstates', 'Saved states...'],
  ].filter(([id]) => !$(id));
  if (missing.length === 0) return;
  const row = document.createElement('div');
  row.style.cssText = 'display:inline-flex;align-items:center;gap:0.45rem;margin:0.4rem 0.6rem 0.4rem 0;';
  for (const [id, label] of missing) {
    const b = document.createElement('button');
    b.id = id;
    b.textContent = label;
    b.style.cssText =
      'padding:0.25rem 0.7rem;border-radius:6px;cursor:pointer;' +
      'border:1px solid rgba(255,255,255,0.35);' +
      'background:rgba(10,13,22,0.6);color:rgba(255,255,255,0.85);' +
      'font:600 0.8rem "IBM Plex Mono",ui-monospace,monospace;';
    row.appendChild(b);
  }
  shell.insertAdjacentElement('afterend', row);
}
buildMachineControls();
const pauseBtn = $('pause');
const screenshotBtn = $('screenshot');
const keyboardBtn = $('keyboard');
const devKeyboardBtn = $('devkeyboard');
pauseBtn?.addEventListener('click', togglePause);
screenshotBtn?.addEventListener('click', copyScreenshot);
// Hidden rather than degraded on a bundle that predates key_raw: the keys
// are rawkeys, and there is no half of this that still works.
if (HAS_KEY_RAW) keyboardBtn?.addEventListener('click', toggleKeyboard);
else if (keyboardBtn) keyboardBtn.hidden = true;
// The device keyboard is offered where there is one to offer. A screen
// without touch already has the real thing plugged in, and routing it
// through a text field would only lose every key the field cannot
// describe -- which is most of an Amiga keyboard.
if (HAS_KEY_RAW && hasTouch) devKeyboardBtn?.addEventListener('click', toggleHostKeyboard);
else if (devKeyboardBtn) {
  // Only a shell-provided button can reach here (the self-inserted one is
  // not built at all off a touch screen), and it goes away inline as well
  // as by the attribute: `[hidden]` is a user-agent rule, so a shell whose
  // stylesheet gives its buttons a `display` would out-rank it and leave a
  // dead control on the page.
  devKeyboardBtn.hidden = true;
  devKeyboardBtn.style.display = 'none';
}

const saveStateBtn = $('savestate');
const loadStateBtn = $('loadstate');
const quickSaveBtn = $('quicksave');
const quickLoadBtn = $('quickload');
saveStateBtn?.addEventListener('click', downloadState);
quickSaveBtn?.addEventListener('click', quickSaveState);
quickLoadBtn?.addEventListener('click', quickLoadState);
// The file picker is built here rather than expected from the shell, so
// #loadstate is a plain button like the rest of the row wherever it lives.
if (loadStateBtn) {
  const picker = document.createElement('input');
  picker.type = 'file';
  picker.accept = '.clstate';
  picker.hidden = true;
  document.body.appendChild(picker);
  picker.addEventListener('change', () => {
    const file = picker.files?.[0];
    // Clear the selection so picking the same file twice fires again.
    picker.value = '';
    loadStateFromFile(file);
  });
  loadStateBtn.addEventListener('click', () => picker.click());
}
updateStateButtons();
probeQuickState();

// --- saved-states panel ------------------------------------------------
// One place to see everything this browser remembers: the Kickstart the
// pickers stored, the quick slot, and named save states - the browser's
// version of a desktop save-state folder. The panel is glue-built (a
// shell only needs the #savedstates button, self-inserted like the other
// state controls) and lists each state with Load / Export / Delete, plus
// a name box that saves the running machine under a named slot. Export
// downloads the stored blob as the same .clstate file "Save state"
// writes, so a browser-kept state can still move to a desktop build.

let statesPanel = null; // { root, list, name, save } - built lazily
let statesPanelOpen = false;
// Refresh generation: an await inside a stale refresh must not rebuild
// the list a newer refresh already built.
let statesPanelRefresh = 0;

const PANEL_BTN_CSS =
  'padding:0.15rem 0.55rem;border-radius:6px;cursor:pointer;' +
  'border:1px solid rgba(255,255,255,0.35);' +
  'background:rgba(10,13,22,0.6);color:rgba(255,255,255,0.85);' +
  'font:600 0.75rem "IBM Plex Mono",ui-monospace,monospace;';

function ensureStatesPanel() {
  if (statesPanel) return statesPanel;
  const root = document.createElement('div');
  root.id = 'savedstates-panel';
  root.style.cssText =
    'display:none;margin:0.6rem 0;padding:0.55rem 0.75rem;' +
    'border:1px solid rgba(255,255,255,0.25);border-radius:8px;' +
    'background:rgba(10,13,22,0.55);color:rgba(255,255,255,0.85);' +
    'font:600 0.8rem "IBM Plex Mono",ui-monospace,monospace;';
  const list = document.createElement('div');
  const saveRow = document.createElement('div');
  saveRow.style.cssText =
    'display:flex;flex-wrap:wrap;gap:0.45rem;align-items:center;margin-top:0.55rem;';
  const name = document.createElement('input');
  name.type = 'text';
  name.placeholder = 'name this state';
  name.maxLength = 60;
  name.style.cssText =
    'flex:1;min-width:10rem;padding:0.25rem 0.5rem;border-radius:6px;' +
    'border:1px solid rgba(255,255,255,0.35);background:rgba(10,13,22,0.6);' +
    'color:rgba(255,255,255,0.85);font:inherit;';
  const save = document.createElement('button');
  save.textContent = 'Save new';
  save.style.cssText = PANEL_BTN_CSS;
  save.addEventListener('click', () => saveNamedState(name.value));
  // Typing a name must reach neither the guest's keyboard nor the page's
  // joystick mapping; Enter saves, like the button.
  const fence = (e) => {
    e.stopPropagation();
    if (e.type === 'keydown' && e.key === 'Enter') saveNamedState(name.value);
  };
  name.addEventListener('keydown', fence);
  name.addEventListener('keyup', fence);
  root.appendChild(list);
  saveRow.appendChild(name);
  saveRow.appendChild(save);
  root.appendChild(saveRow);
  shell.insertAdjacentElement('afterend', root);
  statesPanel = { root, list, name, save };
  return statesPanel;
}

function toggleStatesPanel() {
  statesPanelOpen = !statesPanelOpen;
  const panel = ensureStatesPanel();
  panel.root.style.display = statesPanelOpen ? '' : 'none';
  if (statesPanelOpen) refreshStatesPanel();
}

function stateKeyName(key) {
  return key === QUICK_SLOT ? 'Quick slot' : key.slice(NAMED_SLOT_PREFIX.length);
}

// Everything the states store holds, bytes left behind: the panel only
// needs each state's metadata and size, not megabytes of machine.
async function listStoredStates() {
  const rows = [];
  await withStateDb('readonly', (store) => {
    const req = store.openCursor();
    req.onsuccess = () => {
      const cursor = req.result;
      if (!cursor) return;
      const key = String(cursor.key);
      if (key === QUICK_SLOT || key.startsWith(NAMED_SLOT_PREFIX)) {
        const { bytes, ...info } = cursor.value ?? {};
        rows.push({ key, size: bytes?.length ?? 0, info });
      }
      cursor.continue();
    };
    return req;
  });
  rows.sort((a, b) => {
    // The quick slot first (it has its own buttons and its own meaning),
    // then named states newest first.
    if (a.key === QUICK_SLOT) return -1;
    if (b.key === QUICK_SLOT) return 1;
    return (b.info.saved?.getTime?.() ?? 0) - (a.info.saved?.getTime?.() ?? 0);
  });
  return rows;
}

// Rebuild the panel from storage. Called whenever stored content changes;
// a closed (or never-built) panel skips the storage round trip.
async function refreshStatesPanel() {
  if (!statesPanel || !statesPanelOpen) return;
  const token = ++statesPanelRefresh;
  let rows = null;
  let storageError = null;
  try {
    rows = await listStoredStates();
  } catch (e) {
    storageError = e;
  }
  if (token !== statesPanelRefresh || !statesPanelOpen) return;
  const { list } = statesPanel;
  statesPanel.save.disabled = !(emu && running);
  statesPanel.save.title = emu && running ? '' : 'boot a machine first';
  list.textContent = '';
  const mkRow = () => {
    const row = document.createElement('div');
    row.style.cssText =
      'display:flex;flex-wrap:wrap;align-items:center;gap:0.45rem;padding:0.18rem 0;';
    list.appendChild(row);
    return row;
  };
  const mkText = (row, text, muted) => {
    const s = document.createElement('span');
    s.textContent = text;
    s.style.cssText = `flex:1;min-width:12rem;${muted ? 'color:rgba(255,255,255,0.55);' : ''}`;
    row.appendChild(s);
    return s;
  };
  const mkBtn = (row, label, fn) => {
    const b = document.createElement('button');
    b.textContent = label;
    b.style.cssText = PANEL_BTN_CSS;
    b.addEventListener('click', fn);
    row.appendChild(b);
    return b;
  };
  const romRow = mkRow();
  if (storedRomInfo) {
    mkText(
      romRow,
      `Kickstart: ${storedRomInfo.label} (${Math.round(storedRomInfo.size / 1024)} KB) - remembered for your next visit`,
    );
    mkBtn(romRow, 'Forget', forgetStoredRom);
  } else {
    mkText(romRow, 'Kickstart: none remembered - load one and it sticks', true);
  }
  if (storageError) {
    mkText(mkRow(), `browser storage unavailable: ${storageError.message ?? storageError}`, true);
    return;
  }
  if (!rows.length) {
    mkText(mkRow(), 'no states saved in this browser yet', true);
    return;
  }
  for (const { key, size, info } of rows) {
    const row = mkRow();
    mkText(row, `${stateKeyName(key)} - ${describeState(info)}, ${Math.round(size / 1024)} KB`);
    mkBtn(row, 'Load', () => loadStoredState(key));
    mkBtn(row, 'Export', () => exportStoredState(key));
    mkBtn(row, 'Delete', () => deleteStoredState(key));
  }
}

async function saveNamedState(rawName) {
  if (!emu || !running) return;
  const name = String(rawName ?? '').trim();
  if (!name) {
    setLoadStatus('give the state a name first');
    return;
  }
  let record;
  try {
    record = stateRecord(emu.save_state());
  } catch (e) {
    setLoadStatus(`save failed: ${e.message ?? e}`);
    return;
  }
  try {
    // put() replaces an existing state of the same name, which is what
    // re-saving under a name means.
    await withStateDb('readwrite', (store) => store.put(record, NAMED_SLOT_PREFIX + name));
  } catch (e) {
    const hint = e.name === 'QuotaExceededError' ? ' - browser storage is full' : '';
    setLoadStatus(`save failed: ${e.message ?? e}${hint}`);
    return;
  }
  if (statesPanel) statesPanel.name.value = '';
  setLoadStatus(`state "${name}" saved in this browser (${Math.round(record.bytes.length / 1024)} KB)`);
  refreshStatesPanel();
}

async function getStoredState(key) {
  return withStateDb('readonly', (store) => store.get(key));
}

async function loadStoredState(key) {
  let record;
  try {
    record = await getStoredState(key);
  } catch (e) {
    setLoadStatus(`load failed: ${e.message ?? e}`);
    return;
  }
  if (!record?.bytes) {
    setLoadStatus('that state is no longer in browser storage');
    refreshStatesPanel();
    return;
  }
  const machine = await machineForStateLoad();
  if (!machine.ready) return;
  if (!restoreState(record.bytes, `"${stateKeyName(key)}"`) && machine.booted) {
    unbootAfterFailedStateLoad();
  }
}

async function exportStoredState(key) {
  let record;
  try {
    record = await getStoredState(key);
  } catch (e) {
    setLoadStatus(`export failed: ${e.message ?? e}`);
    return;
  }
  if (!record?.bytes) {
    setLoadStatus('that state is no longer in browser storage');
    refreshStatesPanel();
    return;
  }
  const blob = new Blob([record.bytes], { type: 'application/octet-stream' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = `${stateKeyName(key).replace(/[^\w.-]+/g, '_')}.clstate`;
  a.click();
  // Revoking synchronously can cancel the download that click just
  // started; let the current task finish first (as for screenshots).
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
  setLoadStatus(`state "${stateKeyName(key)}" downloaded`);
}

async function deleteStoredState(key) {
  try {
    await withStateDb('readwrite', (store) => store.delete(key));
  } catch (e) {
    setLoadStatus(`delete failed: ${e.message ?? e}`);
    return;
  }
  if (key === QUICK_SLOT) {
    quickStateInfo = null;
    updateStateButtons();
  }
  setLoadStatus(`state "${stateKeyName(key)}" deleted`);
  refreshStatesPanel();
}

$('savedstates')?.addEventListener('click', toggleStatesPanel);

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

// Optional in the page shell: a checkbox #mono-audio mixes the left and
// right channels into both speakers (the desktop's [audio]
// channel_mode = "mono"). Without the element (and no mono_audio key in
// copperline.json) the output stays stereo; the checkbox's initial
// state is applied at boot, so a shell can default it on.
const monoAudioToggle = $('mono-audio');
// The config file's mono_audio on a shell without the checkbox: stashed
// here and applied at boot.
let configMonoAudio = null;
monoAudioToggle?.addEventListener('change', () => {
  if (emu) emu.set_mono_audio(monoAudioToggle.checked);
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

// --- machine model ---------------------------------------------------------
// Which Amiga the boot button builds: the A500 the page has always booted,
// or an AGA A1200. Always visible like the floppy speed select: a page
// shell can host its own <select id="machine"> wherever its control bar
// wants it (option values are model names; data-default presets one), and
// without the element the control inserts itself below the canvas. The
// option list comes from WebEmu.models() once the wasm module is ready,
// which doubles as the feature test: an older wasm bundle has no models()
// and the control hides rather than promising a switch it cannot make.
// The config file's "machine" and ?machine= in the URL preset the choice.
//
// Changing the model while a machine runs rebuilds it: the model is the
// board itself, not a knob on it. The chosen ROM (the boot stash) and the
// page's copy of the inserted disk carry over, and the new machine powers
// up - the browser version of picking another profile in the launcher.

const MACHINE_LABELS = { A500: 'A500', A1200: 'A1200 (AGA)' };

// A labelled select below the canvas shell, the machine control's pattern,
// shared by every self-inserted setting. Carries the hook id even when
// self-built (like the self-inserted buttons), so page-side scripts can
// drive the control either way.
function buildSettingControl(id, labelText) {
  const row = document.createElement('label');
  row.style.cssText =
    'display:inline-flex;align-items:center;gap:0.45rem;margin:0.4rem 0.6rem 0.4rem 0;' +
    'font:600 0.8rem "IBM Plex Mono",ui-monospace,monospace;' +
    'color:rgba(255,255,255,0.75);';
  row.appendChild(document.createTextNode(labelText));
  const sel = document.createElement('select');
  sel.id = id;
  sel.style.cssText =
    'padding:0.15rem 0.4rem;border-radius:6px;cursor:pointer;' +
    'border:1px solid rgba(255,255,255,0.35);' +
    'background:rgba(10,13,22,0.6);color:rgba(255,255,255,0.85);' +
    'font:inherit;';
  row.appendChild(sel);
  shell.insertAdjacentElement('afterend', row);
  return sel;
}
const machineShellSel = $('machine');
const machineSel = machineShellSel ?? buildSettingControl('machine', 'Machine');
// null = the wasm default machine (the A500); boot() passes it through.
let machineModel = null;
// A ?machine=/config/data-default choice that arrived before the model
// list did; applied once both exist.
let requestedMachine = null;

// Model names compare like the core parses them: case-insensitive, with
// separator characters ignored, so ?machine=a1200 matches "A1200".
function matchModelOption(name) {
  const norm = (s) => String(s).replace(/[-_ ]/g, '').toUpperCase();
  return [...machineSel.options].map((o) => o.value).find((v) => v && norm(v) === norm(name));
}

function tryApplyRequestedMachine() {
  if (requestedMachine === null || !machineSel.options.length) return;
  const name = String(requestedMachine).trim();
  requestedMachine = null;
  // A blank request (?machine= with no value, "machine": "" in the config)
  // is no request, like the constructor's empty model and the joy param.
  if (!name) return;
  const model = matchModelOption(name);
  if (model) {
    machineModel = model;
    machineSel.value = model;
  } else {
    console.warn(`unknown machine ${name}; keeping ${machineSel.value}`);
  }
}

// Called once the wasm module is ready (load()): fill the select from the
// build's own list - unless the shell shipped its own options - and hide
// the control on a bundle too old to take a model.
function populateMachineSelect() {
  let models = null;
  try {
    models = WebEmu.models?.();
  } catch {
    models = null;
  }
  if (!models?.length) {
    (machineShellSel ?? machineSel.parentElement).hidden = true;
    return;
  }
  if (!machineSel.options.length) {
    for (const name of models) {
      const option = document.createElement('option');
      option.value = name;
      option.textContent = MACHINE_LABELS[name] ?? name;
      machineSel.appendChild(option);
    }
  }
  // From here on every boot names its model explicitly, so the machine is
  // properly labelled in save states and bug reports.
  if (machineModel === null) machineModel = machineSel.value || null;
  tryApplyRequestedMachine();
}

// A restored state carries its machine, model and all; point the select at
// what is actually running. A shape no offered profile describes (a state
// from a custom desktop config) leaves the select alone.
function syncMachineSelect() {
  const model = emu?.machine_model?.();
  if (!model) return;
  const match = matchModelOption(model);
  if (match) {
    machineModel = match;
    machineSel.value = match;
  }
}

machineSel.addEventListener('change', () => {
  const model = machineSel.value;
  if (!model || model === machineModel) return;
  machineModel = model;
  if (emu && running) {
    // Carry the page's copy of the inserted disk into the new machine; a
    // disk that only exists inside the old one (restored from a state)
    // cannot come along.
    if (df0Name && lastDisk?.name === df0Name) pendingDisk = lastDisk;
    boot();
  } else if (!emu) {
    setLoadStatus(`machine: ${model} - applies at boot`);
  }
});

// --- video standard ----------------------------------------------------
// PAL or NTSC, the desktop's `[chipset] video` key. Like the machine
// model this is the board itself - the Agnus crystal - not a knob on it,
// so it follows the machine select's pattern exactly: always visible
// (self-inserting without a shell `#video` select), rebuilding a running
// machine on change with the ROM and disk carried over, preset by the
// config file's "video" or `?video=NTSC` in the URL, synced to whatever a
// save state brings back, and hidden on a wasm bundle too old to take a
// standard (`WebEmu.video_standards` is the feature test).

const videoShellSel = $('video');
const videoSel = videoShellSel ?? buildSettingControl('video', 'Video');
// null = the profile's own standard (PAL); boot() passes it through.
let videoStandard = null;
// A ?video=/config/data-default choice that arrived before the standards
// list did; applied once both exist.
let requestedVideo = null;

function matchVideoOption(name) {
  const norm = (s) => String(s).trim().toUpperCase();
  return [...videoSel.options].map((o) => o.value).find((v) => v && norm(v) === norm(name));
}

function tryApplyRequestedVideo() {
  if (requestedVideo === null || !videoSel.options.length) return;
  const name = String(requestedVideo).trim();
  requestedVideo = null;
  if (!name) return;
  const std = matchVideoOption(name);
  if (std) {
    videoStandard = std;
    videoSel.value = std;
  } else {
    console.warn(`unknown video standard ${name}; keeping ${videoSel.value}`);
  }
}

// Called once the wasm module is ready (load()), like the machine select.
function populateVideoSelect() {
  let standards = null;
  try {
    standards = WebEmu.video_standards?.();
  } catch {
    standards = null;
  }
  if (!standards?.length) {
    (videoShellSel ?? videoSel.parentElement).hidden = true;
    return;
  }
  if (!videoSel.options.length) {
    for (const name of standards) {
      const option = document.createElement('option');
      option.value = name;
      option.textContent = name;
      videoSel.appendChild(option);
    }
  }
  // From here on every boot names its standard explicitly, like the model.
  if (videoStandard === null) videoStandard = videoSel.value || null;
  tryApplyRequestedVideo();
}

// A restored state carries its machine's standard; point the select at
// what is actually running (the getter follows load_state).
function syncVideoSelect() {
  const std = emu?.video_standard?.();
  if (!std) return;
  const match = matchVideoOption(std);
  if (match) {
    videoStandard = match;
    videoSel.value = match;
  }
}

videoSel.addEventListener('change', () => {
  const std = videoSel.value;
  if (!std || std === videoStandard) return;
  videoStandard = std;
  if (emu && running) {
    // The machine select's rebuild, for the same reason: the standard is
    // soldered in. ROM and disk carry over the same way.
    if (df0Name && lastDisk?.name === df0Name) pendingDisk = lastDisk;
    boot();
  } else if (!emu) {
    setLoadStatus(`video: ${std} - applies at boot`);
  }
});

// --- display: overscan and screen tint -----------------------------------
// Presentation-only choices, so unlike the machine and video standard they
// are remembered per browser (localStorage) and re-applied on the next
// visit: nothing the guest can observe changes, only what the glass shows.
//
// Overscan is the desktop's `[display] overscan` knob: "tv" (default)
// masks the deep horizontal overscan like a CRT bezel and crops standard
// PAL screens to the TV aperture; "full" shows everything Denise produced.
// The tint is a monitor phosphor choice rendered as a CSS filter on the
// canvas - pure presentation, zero per-frame cost, and baked into
// screenshots via a filtered copy (see copyScreenshot).

function storedPref(key) {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function storePref(key, value) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Private browsing or blocked storage: the choice just does not stick.
  }
}

const OVERSCAN_STORAGE_KEY = 'copperline-overscan';
const OVERSCAN_MODES = ['tv', 'full'];
const OVERSCAN_LABELS = { tv: 'TV', full: 'Full overscan' };

const overscanShellSel = $('overscan');
const overscanSel = overscanShellSel ?? buildSettingControl('overscan', 'View');
if (!overscanSel.options.length) {
  for (const mode of OVERSCAN_MODES) {
    const option = document.createElement('option');
    option.value = mode;
    option.textContent = OVERSCAN_LABELS[mode];
    overscanSel.appendChild(option);
  }
}
// Hidden on a bundle that cannot switch (the class methods exist as soon
// as the module is imported, so no need to wait for init()).
if (typeof WebEmu.prototype?.set_overscan !== 'function') {
  (overscanShellSel ?? overscanSel.parentElement).hidden = true;
}
let overscanMode = null; // null = leave the emulator at its default (tv)

function setOverscanMode(mode, remember) {
  if (!OVERSCAN_MODES.includes(mode)) return;
  overscanMode = mode;
  overscanSel.value = mode;
  if (remember) storePref(OVERSCAN_STORAGE_KEY, mode);
  if (emu) {
    emu.set_overscan?.(mode);
    // The wasm re-presents the last frame under the new aperture, but a
    // paused page has no ticking loop to blit it.
    if (running && paused) presentFrame();
  }
}
overscanSel.addEventListener('change', () => setOverscanMode(overscanSel.value, true));

const TINT_STORAGE_KEY = 'copperline-tint';
// Phosphor approximations: grayscale first so the sepia+hue chain works
// from luminance, saturate to pull the single-hue look together.
const TINTS = {
  none: { label: 'Colour', filter: '' },
  bw: { label: 'Black & white', filter: 'grayscale(1)' },
  green: {
    label: 'Green phosphor',
    filter: 'grayscale(1) sepia(1) saturate(4) hue-rotate(80deg) brightness(0.92)',
  },
  amber: {
    label: 'Amber phosphor',
    filter: 'grayscale(1) sepia(1) saturate(4) hue-rotate(-8deg)',
  },
  sepia: { label: 'Sepia', filter: 'sepia(1)' },
};

const tintShellSel = $('tint');
const tintSel = tintShellSel ?? buildSettingControl('tint', 'Screen');
if (!tintSel.options.length) {
  for (const [value, { label }] of Object.entries(TINTS)) {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = label;
    tintSel.appendChild(option);
  }
}
let tintMode = 'none';

function tintFilter() {
  return TINTS[tintMode]?.filter ?? '';
}

function setTintMode(mode, remember) {
  if (!TINTS[mode]) return;
  tintMode = mode;
  tintSel.value = mode;
  if (remember) storePref(TINT_STORAGE_KEY, mode);
  canvas.style.filter = tintFilter();
}
tintSel.addEventListener('change', () => setTintMode(tintSel.value, true));

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
  // PWR is never dark on a running machine: the pair is the bright
  // (/LED engaged) and dimmed (released) levels of an A500 rev 6+
  // board, which dims the LED rather than switching it off.
  pwr: ['rgb(255,38,28)', 'rgb(150,24,18)'],
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

const DISK_LIST_EXT = /\.(adf|adz|dms|scp|zip|gz)$/i;
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
    'Disk image URL (ADF/ADZ/DMS/SCP, gzip or zip packed):',
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

const BUG_REPORT_URL = 'https://github.com/CopperlineHQ/Copperline/issues/new';
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
      // The running machine's own description when there is one (it also
      // tracks state loads); otherwise what the next boot would build.
      `machine = ${toml(emu?.machine_summary?.() ?? machineModel ?? 'A500 (default)')}`,
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
  dropHint.textContent = 'Drop: disk image -> DF0, .rom -> Kickstart, .clstate -> restore';
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
  // A dropped save state replaces the whole machine, so it takes the drop
  // on its own: a ROM or disk alongside it would be overwritten by the
  // state's own ROM and disks anyway.
  const state = list.find((f) => /\.clstate$/i.test(f.name));
  if (state) {
    await loadStateFromFile(state);
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
// Link parameters (?df0=, ?kick=, ?machine=, ?joy=, ?fdspeed=) override
// the file, and anything the visitor changes by hand wins as usual.
//
//   {
//     "machine": "A1200",            machine model (WebEmu.models() lists them)
//     "video": "NTSC",               video standard, like ?video= (PAL|NTSC)
//     "kick": "roms/kick31.rom",     same-origin path, like ?kick=
//     "df0": "adf/demo.adf",         URL, like ?df0=
//     "floppy_sounds": false,        preset the drive-sounds toggle
//     "mono_audio": true,            preset the mono-audio toggle
//     "floppy_speed": 800,           100|200|400|800|0 (0 = turbo)
//     "overscan": "full",            starting view (tv|full); a visitor's
//                                    own remembered choice wins
//     "tint": "green",               starting screen tint (none|bw|green|
//                                    amber|sepia); same visitor rule
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
  // Kickstart simply replaces the stashed boot ROM when it arrives). The
  // remembered-Kickstart probe rides along too: it only ever fills an
  // empty or AROS boot stash, so it cannot beat an explicit choice.
  const loaded = load();
  probeStoredRom();
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
  if (typeof cfg.mono_audio === 'boolean') {
    if (monoAudioToggle) monoAudioToggle.checked = cfg.mono_audio;
    else configMonoAudio = cfg.mono_audio;
  }

  const fetches = [];
  const linkedDisk =
    pageParams.get('df0') ?? (typeof cfg.df0 === 'string' ? cfg.df0 : null);
  if (linkedDisk) fetches.push(insertDiskFromUrl(linkedDisk));
  const linkedKick =
    pageParams.get('kick') ?? (typeof cfg.kick === 'string' ? cfg.kick : null);
  if (linkedKick) fetches.push(fitRomFromUrl(linkedKick));

  // Starting machine model: the shell's data-default on the #machine
  // select or the config file's "machine", overridden per link by
  // ?machine=A1200 (names compare like the core parses them, so a1200
  // works too). Applied once the wasm module has supplied the model list;
  // whichever of the two arrives second completes it.
  requestedMachine =
    pageParams.get('machine') ??
    (typeof cfg.machine === 'string' ? cfg.machine : null) ??
    machineSel.dataset.default ??
    null;
  tryApplyRequestedMachine();

  // Starting video standard, the machine pattern: shell data-default or
  // config "video", overridden per link by ?video=NTSC (names compare
  // case-insensitively). Applied once the wasm module has supplied the
  // standards list.
  requestedVideo =
    pageParams.get('video') ??
    (typeof cfg.video === 'string' ? cfg.video : null) ??
    videoSel.dataset.default ??
    null;
  tryApplyRequestedVideo();

  // Starting view and tint: the visitor's own remembered choice first
  // (these are per-browser viewing preferences), then the config file's
  // starting point for first-time visitors.
  const overscanPref =
    storedPref(OVERSCAN_STORAGE_KEY) ??
    (typeof cfg.overscan === 'string' ? cfg.overscan.trim() : null);
  if (overscanPref) setOverscanMode(overscanPref, false);
  const tintPref =
    storedPref(TINT_STORAGE_KEY) ?? (typeof cfg.tint === 'string' ? cfg.tint.trim() : null);
  if (tintPref) setTintMode(tintPref, false);
  // Which A600 the visitor owns, for the keycap legends. Nothing the guest
  // can observe changes - the rawkeys are the same either way, only what is
  // printed on the caps.
  kbdLegends = storedPref(KB_LEGENDS_STORAGE_KEY) === 'us' ? 'us' : 'uk';

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
