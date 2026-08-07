// SPDX-License-Identifier: GPL-3.0-or-later

//! Browser frontend for Copperline: a thin wasm-bindgen wrapper around the
//! headless core. The page's JS drives everything: it fetches ROM bytes,
//! constructs a [`WebEmu`], calls [`WebEmu::run`] from requestAnimationFrame,
//! draws the presentation buffer to a canvas (a WebGL2 monitor pass with
//! the desktop's CRT shader and 1084 bezel, or a plain ImageData blit
//! without WebGL2), forwards keyboard/mouse events, and ships each frame's
//! mixed audio to an AudioWorklet. No winit, wgpu, or cpal: the canvas is
//! the display and the Web Audio API is the sound device, so the wasm
//! stays small and single-threaded (GitHub Pages cannot serve the
//! COOP/COEP headers that SharedArrayBuffer builds need).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use copperline::audio::AudioSink;
use copperline::bus::PortDevice;
use copperline::chipset::agnus::VideoStandard;
use copperline::config::{
    machine_profile_defaults, parse_machine_model, parse_video_standard, Config, Overscan,
};
use copperline::emulator::{build_machine, Emulator};
use copperline::serial::{ChannelSerialHandle, ChannelSerialSink};
use copperline::timebase::Instant;
use copperline::video::deinterlace::Deinterlacer;
use copperline::video::{bitplane, present_common, FB_WIDTH, MAX_CANVAS_PIXELS};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
}

/// Collects Paula's mixed 44.1 kHz stereo output as interleaved f32 frames;
/// the page drains it once per animation frame with [`WebEmu::take_audio`]
/// and posts the chunk to the AudioWorklet.
struct WebAudioSink {
    buf: Rc<RefCell<Vec<f32>>>,
}

impl AudioSink for WebAudioSink {
    fn push(&mut self, left: f32, right: f32) {
        let mut buf = self.buf.borrow_mut();
        buf.push(left);
        buf.push(right);
    }
    fn flush(&mut self) {}
}

fn js_err(e: anyhow::Error) -> JsValue {
    JsValue::from_str(&format!("{e:#}"))
}

/// Translate a W3C `KeyboardEvent.code` string to an Amiga raw scan code.
/// The table mirrors the desktop frontend's winit mapping
/// (`video/window/host_input.rs`); winit's `KeyCode` variant names are the
/// W3C code strings, so the two stay in lockstep by construction.
fn w3c_code_to_amiga_rawkey(code: &str) -> Option<u8> {
    Some(match code {
        // Letters (row-by-row, Amiga's funny layout)
        "KeyA" => 0x20,
        "KeyB" => 0x35,
        "KeyC" => 0x33,
        "KeyD" => 0x22,
        "KeyE" => 0x12,
        "KeyF" => 0x23,
        "KeyG" => 0x24,
        "KeyH" => 0x25,
        "KeyI" => 0x17,
        "KeyJ" => 0x26,
        "KeyK" => 0x27,
        "KeyL" => 0x28,
        "KeyM" => 0x37,
        "KeyN" => 0x36,
        "KeyO" => 0x18,
        "KeyP" => 0x19,
        "KeyQ" => 0x10,
        "KeyR" => 0x13,
        "KeyS" => 0x21,
        "KeyT" => 0x14,
        "KeyU" => 0x16,
        "KeyV" => 0x34,
        "KeyW" => 0x11,
        "KeyX" => 0x32,
        "KeyY" => 0x15,
        "KeyZ" => 0x31,
        // Top-row digits
        "Digit1" => 0x01,
        "Digit2" => 0x02,
        "Digit3" => 0x03,
        "Digit4" => 0x04,
        "Digit5" => 0x05,
        "Digit6" => 0x06,
        "Digit7" => 0x07,
        "Digit8" => 0x08,
        "Digit9" => 0x09,
        "Digit0" => 0x0A,
        // Punctuation
        "Backquote" => 0x00,
        "Minus" => 0x0B,
        "Equal" => 0x0C,
        "Backslash" => 0x0D,
        "BracketLeft" => 0x1A,
        "BracketRight" => 0x1B,
        "Semicolon" => 0x29,
        "Quote" => 0x2A,
        "Comma" => 0x38,
        "Period" => 0x39,
        "Slash" => 0x3A,
        // International keys: the ISO 102nd key between left Shift and Z is
        // Amiga rawkey $30; the Japanese Ro key sits in the same matrix
        // position on layouts that have it.
        "IntlBackslash" | "IntlRo" => 0x30,
        // Control
        "Space" => 0x40,
        "Enter" => 0x44,
        "Backspace" => 0x41,
        "Tab" => 0x42,
        "Escape" => 0x45,
        "Delete" => 0x46,
        // Amiga Help: F11 host-side (no dedicated host key exists).
        "F11" => 0x5F,
        "ShiftLeft" => 0x60,
        "ShiftRight" => 0x61,
        "CapsLock" => 0x62,
        // Single Ctrl key on the Amiga; right Ctrl doubles as Right Amiga
        // alongside the right Super/Meta key (see host_input.rs).
        "ControlLeft" => 0x63,
        "AltLeft" => 0x64,
        "AltRight" => 0x65,
        "MetaLeft" | "OSLeft" => 0x66,
        "MetaRight" | "OSRight" | "ControlRight" => 0x67,
        // Arrows
        "ArrowUp" => 0x4C,
        "ArrowDown" => 0x4D,
        "ArrowRight" => 0x4E,
        "ArrowLeft" => 0x4F,
        // Function keys
        "F1" => 0x50,
        "F2" => 0x51,
        "F3" => 0x52,
        "F4" => 0x53,
        "F5" => 0x54,
        "F6" => 0x55,
        "F7" => 0x56,
        "F8" => 0x57,
        "F9" => 0x58,
        "F10" => 0x59,
        // Numpad
        "Numpad0" => 0x0F,
        "Numpad1" => 0x1D,
        "Numpad2" => 0x1E,
        "Numpad3" => 0x1F,
        "Numpad4" => 0x2D,
        "Numpad5" => 0x2E,
        "Numpad6" => 0x2F,
        "Numpad7" => 0x3D,
        "Numpad8" => 0x3E,
        "Numpad9" => 0x3F,
        "NumpadDecimal" => 0x3C,
        "NumpadEnter" => 0x43,
        "NumpadSubtract" => 0x4A,
        "NumpadAdd" => 0x5E,
        "NumpadMultiply" => 0x5D,
        "NumpadDivide" => 0x5C,
        "NumpadParenLeft" => 0x5A,
        "NumpadParenRight" => 0x5B,
        _ => return None,
    })
}

/// Map a page-facing port number to the core's port index: `1` selects the
/// mouse/port-1 socket (index 0) and any other value port 2 (index 1).
fn port_index(port: u8) -> usize {
    usize::from(port != 1)
}

/// Mirrors the desktop frontend's fractional mouse-delta accumulator
/// (`take_integral_mouse_delta` in window/present.rs): whole pixels go to the
/// emulated mouse, the fraction carries to the next event.
fn take_integral_delta(value: &mut f64) -> i32 {
    let whole = value.trunc();
    if whole > i32::MAX as f64 {
        *value = 0.0;
        i32::MAX
    } else if whole < i32::MIN as f64 {
        *value = 0.0;
        i32::MIN
    } else {
        *value -= whole;
        whole as i32
    }
}

/// How far the emulated clock may fall behind the wall clock before `run`
/// gives up catching up and re-anchors instead (tab was backgrounded, a GC
/// pause, ...). Mirrors the native pacer's `MAX_REALTIME_CATCHUP`.
const MAX_CATCHUP_SECONDS: f64 = 0.1;

#[wasm_bindgen]
pub struct WebEmu {
    emu: Emulator,
    audio: Rc<RefCell<Vec<f32>>>,
    fb: Vec<u32>,
    deinterlacer: Deinterlacer,
    present: Vec<u32>,
    present_width: usize,
    present_rows: usize,
    /// Monotonic identity of the bytes and geometry in `present`. The page
    /// compares this with the last revision it uploaded, so an emulated frame
    /// that the exact-reuse detector matched does not cross the JS/WebGL
    /// presentation path again.
    presentation_revision: u32,
    /// Emulated field lines the presentation buffer shows, for the page's
    /// CRT shader pass; 0 while no frame has rendered or the scan carries
    /// no 15 kHz line structure (a programmable scan). See
    /// [`Self::present_crt_lines`].
    present_crt_lines: f32,
    last_rendered_frame: Option<u64>,
    /// Frames were stepped by `run_hidden` (which renders nothing) since
    /// the last render, so the presentation buffer is behind the machine.
    /// The next rendering `run` repaints even if it steps no frames.
    presentation_stale: bool,
    /// Wall-clock/emulated-time pair the pacer chases from; None until the
    /// first `run` call after (re)boot.
    anchor: Option<(f64, f64)>,
    mouse_remainder: (f64, f64),
    /// Whole-pixel mouse motion not yet applied to the hardware counters.
    /// The JOYxDAT counters are 8 bits and input.device samples them once
    /// per vblank, so any burst past +/-127 counts in a frame reads back
    /// as motion in the opposite direction. Browsers coalesce pointer
    /// events (a fast flick can arrive as one huge delta), so the pool
    /// re-spreads host input at a rate a physical mouse could produce.
    mouse_pending: (i32, i32),
    /// Host side of Paula's serial port; the page bridges it to whatever
    /// byte stream it likes (typically a WebSocket to a telnet gateway).
    serial: ChannelSerialHandle,
    /// Presentation overscan, the desktop's `[display] overscan` knob: `Tv`
    /// masks the deep horizontal overscan like a CRT bezel (the default),
    /// `Full` presents everything Denise produced.
    overscan: Overscan,
    /// Aperture/recentring decisions latched across border-only frames,
    /// as on the desktop: the blank frames a screen change emits keep the
    /// previous presentation geometry instead of snapping to the full
    /// framebuffer, so the canvas does not jump at every mode change.
    presentation_latch: present_common::PresentationLatch,
    /// Exact previous-frame reuse, the desktop render cache's detector via
    /// the same path the benchmark uses: a frame whose render input matches
    /// the previous one skips the whole render/present pipeline, since the
    /// present buffer already shows it. A static screen then costs no
    /// render at all. Replaced whenever the machine or the presentation
    /// settings change under it.
    repeated_frame_detector: bitplane::RepeatedFrameDetector,
    /// Host timings for the most recent `run`/`run_hidden` call. They split
    /// the page's existing whole-call timer without requiring a second core
    /// traversal or a profiler-only build.
    last_run_core_ms: f64,
    last_run_render_ms: f64,
}

#[wasm_bindgen]
impl WebEmu {
    /// Build a machine with a placeholder ROM; `load_rom` supplies the real
    /// one. `model` picks the machine profile by name ("A500", "A1200", ...)
    /// exactly as the desktop's `--model` flag does; omitted or empty, the
    /// default machine is the A500 of the desktop launcher, so pages built
    /// against the model-less constructor keep booting what they always did.
    /// `models` lists the profiles the hosted page offers; any name the
    /// desktop flag takes is accepted here, but the ones outside that list
    /// may need pieces a browser page cannot supply (CDTV/CD32 want an
    /// extended ROM and a CD). An unknown name throws.
    ///
    /// `video` picks the video standard ("PAL" or "NTSC", the desktop's
    /// `[chipset] video` key) on top of whatever the profile chose; omitted
    /// or empty keeps the profile's own standard (PAL for every offered
    /// profile). An unknown name throws, like an unknown model.
    #[wasm_bindgen(constructor)]
    pub fn new(model: Option<String>, video: Option<String>) -> Result<WebEmu, JsValue> {
        let mut cfg = match model.as_deref().map(str::trim) {
            None | Some("") => Config::default(),
            Some(name) => machine_profile_defaults(parse_machine_model(name).map_err(js_err)?),
        };
        match video.as_deref().map(str::trim) {
            None | Some("") => {}
            Some(name) => cfg.video_standard = parse_video_standard(name).map_err(js_err)?,
        }
        let audio = Rc::new(RefCell::new(Vec::new()));
        let sink = WebAudioSink { buf: audio.clone() };
        // rom_optional: the default rom_path names the bundled AROS file,
        // which does not exist in the browser; build with a placeholder.
        let mut emu = build_machine(&cfg, Box::new(sink), false, true).map_err(js_err)?;
        // Replace the default stdout serial sink (useless in a browser) with
        // the channel pair the serial_* methods drive. Paula keeps host sinks
        // across resets and ROM swaps, so installing it once here holds for
        // the machine's whole life.
        let (serial_sink, serial) = ChannelSerialSink::pair();
        emu.bus_mut().paula.serial = Box::new(serial_sink);
        Ok(WebEmu {
            emu,
            audio,
            fb: vec![0u32; MAX_CANVAS_PIXELS],
            deinterlacer: Deinterlacer::new(),
            present: Vec::new(),
            present_width: FB_WIDTH,
            present_rows: 0,
            presentation_revision: 0,
            present_crt_lines: 0.0,
            last_rendered_frame: None,
            presentation_stale: false,
            anchor: None,
            mouse_remainder: (0.0, 0.0),
            mouse_pending: (0, 0),
            serial,
            overscan: Overscan::Tv,
            presentation_latch: present_common::PresentationLatch::default(),
            repeated_frame_detector: bitplane::RepeatedFrameDetector::default(),
            last_run_core_ms: 0.0,
            last_run_render_ms: 0.0,
        })
    }

    /// Identify this build for bug reports: the tag or branch and commit the
    /// wasm was compiled from. GitHub Actions exports GITHUB_REF_NAME and
    /// GITHUB_SHA to every step, so the publish workflow bakes them in for
    /// free; anything built outside CI reports itself as a dev build.
    pub fn build_info() -> String {
        match (option_env!("GITHUB_REF_NAME"), option_env!("GITHUB_SHA")) {
            (Some(ref_name), Some(sha)) => {
                format!("{ref_name} ({})", sha.get(..9).unwrap_or(sha))
            }
            _ => "dev build".to_string(),
        }
    }

    /// The machine profiles the page's machine select offers, in menu
    /// order. A vetted subset of what the constructor accepts: every model
    /// here boots the bundled AROS ROM (or a plain Kickstart) with nothing
    /// but a floppy, so the page can offer it unconditionally. The page
    /// builds its select from this list, which is also its feature test --
    /// older bundles have no `models` and the select stays hidden.
    pub fn models() -> Vec<String> {
        vec!["A500".to_string(), "A1200".to_string()]
    }

    /// The video standards the constructor's `video` argument accepts, in
    /// menu order. Like `models`, the page builds its select from this list
    /// and its presence doubles as the feature test: older bundles have no
    /// `video_standards` and the control stays hidden.
    pub fn video_standards() -> Vec<String> {
        vec!["PAL".to_string(), "NTSC".to_string()]
    }

    /// The running machine's profile name ("A500", "A1200", ...), or
    /// undefined for a machine no profile describes -- the model-less
    /// default constructor's machine, or a custom-shaped machine restored
    /// from a save state. Follows `load_state`, so a page can re-point its
    /// machine select at what a state brought back.
    pub fn machine_model(&self) -> Option<String> {
        self.emu
            .machine_descriptor()
            .machine
            .map(|model| format!("{model:?}"))
    }

    /// The running machine's video standard ("PAL" or "NTSC"). Follows
    /// `load_state` like `machine_model`, so a page can re-point its video
    /// select at what a state brought back. This is the machine's fitted
    /// standard (the Agnus crystal), not the live BEAMCON0 PAL bit ECS
    /// software can flip at runtime.
    pub fn video_standard(&self) -> String {
        match self.emu.machine_descriptor().video_standard {
            VideoStandard::Pal => "PAL".to_string(),
            VideoStandard::Ntsc => "NTSC".to_string(),
        }
    }

    /// One-line description of the running machine for bug reports and
    /// diagnostics: profile, CPU, chipset, RAM sizes, and the fitted ROM's
    /// fingerprint. Tracks ROM swaps and state loads.
    pub fn machine_summary(&self) -> String {
        self.emu.machine_descriptor().summary()
    }

    /// Fit a Kickstart/AROS ROM (and optional extended ROM) from bytes and
    /// cold-reset, as if the chips had been swapped and the machine power
    /// cycled. 256 KiB Kickstart 1.x images are mirrored up automatically.
    pub fn load_rom(&mut self, rom: Vec<u8>, ext: Option<Vec<u8>>) -> Result<(), JsValue> {
        self.emu.reload_rom(rom, ext).map_err(js_err)?;
        self.anchor = None;
        Ok(())
    }

    /// Step emulated time up to the wall clock (`now_ms` is
    /// `performance.now()`), at most `max_frames` PAL frames per call, then
    /// render the latest completed frame into the presentation buffer.
    /// Returns the number of frames stepped. Deficits past 100 ms are
    /// forgiven by re-anchoring, so a backgrounded tab resumes at real time
    /// instead of fast-forwarding.
    pub fn run(&mut self, now_ms: f64, max_frames: u32) -> Result<u32, JsValue> {
        self.run_paced(now_ms, max_frames, true)
    }

    /// `run` for a hidden page: step the machine and queue its audio, but
    /// skip rendering the presentation buffer nobody can see. The first
    /// rendering `run` after hidden stepping repaints even if it stepped
    /// no frames itself, so a tab that kept running in the background
    /// shows the current picture the moment it is visible again.
    pub fn run_hidden(&mut self, now_ms: f64, max_frames: u32) -> Result<u32, JsValue> {
        self.run_paced(now_ms, max_frames, false)
    }

    fn run_paced(&mut self, now_ms: f64, max_frames: u32, render: bool) -> Result<u32, JsValue> {
        self.last_run_core_ms = 0.0;
        self.last_run_render_ms = 0.0;
        let (anchor_wall, anchor_emu) = *self
            .anchor
            .get_or_insert((now_ms, self.emu.bus().emulated_seconds()));
        let target = anchor_emu + (now_ms - anchor_wall) / 1000.0;
        let mut stepped = 0u32;
        let core_started = Instant::now();
        while self.emu.bus().emulated_seconds() < target && stepped < max_frames {
            self.drain_pending_mouse();
            self.emu.step_frame().map_err(js_err)?;
            stepped += 1;
        }
        // Audio-saturated or already-on-target ticks step no frames; keep
        // the pool draining so buffered motion cannot stall.
        if stepped == 0 {
            self.drain_pending_mouse();
        }
        self.last_run_core_ms = core_started.elapsed().as_secs_f64() * 1000.0;
        if target - self.emu.bus().emulated_seconds() > MAX_CATCHUP_SECONDS {
            self.anchor = Some((now_ms, self.emu.bus().emulated_seconds()));
        }
        if render {
            if stepped > 0 || self.presentation_stale {
                let render_started = Instant::now();
                self.render_completed_frame();
                self.last_run_render_ms = render_started.elapsed().as_secs_f64() * 1000.0;
                self.presentation_stale = false;
            }
        } else if stepped > 0 {
            self.presentation_stale = true;
        }
        Ok(stepped)
    }

    /// The desktop sync render path (`render_emulated_frame_sync`) against
    /// the shared present_common helpers: render the completed hardware
    /// frame, post-process, deinterlace, and copy out the woven rows.
    fn render_completed_frame(&mut self) {
        if !self.emu.bus().frame_render_available() {
            return;
        }
        let emulated_frame = self.emu.bus().emulated_frames();
        if !present_common::should_render_emulated_frame(self.last_rendered_frame, emulated_frame) {
            return;
        }
        let visible_start_vpos = self.emu.bus().frame_visible_start_vpos();
        // A frame identical to the previous render needs no pipeline at
        // all: the present buffer already shows it, and the detector
        // carries the frame's CLXDAT so collisions still accumulate.
        if bitplane::render_reusing_previous(
            self.emu.bus_mut(),
            &mut self.fb,
            &mut self.repeated_frame_detector,
        ) {
            self.last_rendered_frame = Some(emulated_frame);
            return;
        }
        let geometry = self.emu.bus().frame_geometry();
        let canvas_scale = self.emu.bus().frame_canvas_scale();
        let base = self.emu.bus().frame_render_base();
        // The desktop's recentring shift (window.rs render jobs): full
        // overscan recentres a standard display whose deep left overscan
        // would push the picture right of centre; TV mode is a fixed
        // aperture and shifts nothing. Latched across border-only frames
        // like the desktop, so screen changes do not jump.
        let h_shift = self
            .presentation_latch
            .presentation_h_shift(&base, self.overscan);
        let field_rows = present_common::post_process_rendered_field(
            &mut self.fb,
            geometry,
            canvas_scale,
            self.emu.bus().frame_presentation_h_window(),
            self.emu.bus().frame_presentation_v_window(),
            visible_start_vpos,
            h_shift,
            self.overscan,
        );
        self.deinterlacer.push_field(
            &self.fb,
            field_rows,
            FB_WIDTH * canvas_scale,
            base.bplcon0 & 0x0004 != 0,
            base.long_field,
            !geometry.programmable,
        );
        let woven_rows = self.deinterlacer.output_rows();
        let woven = self.deinterlacer.output();
        let tv_aperture_rows = if self.overscan == Overscan::Tv {
            self.presentation_latch
                .resolve_tv_aperture(present_common::standard_tv_aperture_frame(
                    geometry, woven_rows, &base,
                ))
        } else {
            None
        };
        if let Some(aperture_rows) = tv_aperture_rows {
            // Standard 15 kHz display: present the captured TV aperture, the
            // browser counterpart of the desktop's TV-aperture crop. Clipped
            // to real framebuffer columns so the canvas never shows the
            // bezel-mask black stripe on the left or bezel padding on the
            // right; the standard window sits exactly centred. The canvas
            // keeps one shape for both video standards -- their apertures
            // fill the same 4:3 glass -- so a 60 Hz crop's rows scale onto
            // the 50 Hz aperture's native row count (whole-row selection,
            // like the desktop present copy).
            self.present_width = present_common::TV_CAPTURED_WIDTH;
            self.present_rows = present_common::TV_GLASS_PRESENT_ROWS;
            // Two woven rows per emulated field line, and the aperture's
            // rows fill the glass exactly, so the aperture is the line
            // count whatever row count it was scaled onto (the desktop's
            // crt_scanline_count, without its bezel-padding rescale).
            self.present_crt_lines = (aperture_rows / 2).max(1) as f32;
            self.present
                .resize(self.present_width * self.present_rows, 0);
            for (y, dst) in self
                .present
                .chunks_exact_mut(present_common::TV_CAPTURED_WIDTH)
                .enumerate()
            {
                let crop_y = copperline::screenshot::scaled_source_row(
                    y,
                    aperture_rows,
                    present_common::TV_GLASS_PRESENT_ROWS,
                );
                let src = (present_common::TV_PRESENT_SOURCE_Y + crop_y) * FB_WIDTH
                    + present_common::TV_CAPTURED_SOURCE_X;
                dst.copy_from_slice(&woven[src..src + present_common::TV_CAPTURED_WIDTH]);
            }
        } else {
            self.present_width = self.deinterlacer.output_width();
            self.present_rows = woven_rows;
            let active = woven_rows * self.present_width;
            self.present.resize(active, 0);
            self.present.copy_from_slice(&woven[..active]);
            // A programmable scan has no 15 kHz line structure for a CRT
            // pass to draw (the desktop suspends its pass there too);
            // standard scans count two woven rows per emulated field line.
            self.present_crt_lines = if geometry.programmable {
                0.0
            } else {
                (woven_rows / 2).max(1) as f32
            };
        }
        self.last_rendered_frame = Some(emulated_frame);
        self.presentation_revision = self.presentation_revision.wrapping_add(1);
    }

    /// Presentation buffer: RGBA bytes in memory order, `present_width() x
    /// present_rows()` pixels, directly viewable as canvas ImageData. The
    /// pointer is only valid until the next `run` call (the buffer may
    /// reallocate and wasm memory may grow), so JS must re-create its view
    /// every frame.
    pub fn present_ptr(&self) -> *const u32 {
        self.present.as_ptr()
    }

    /// Identity of the current presentation bytes and geometry. It advances
    /// only when the renderer writes a new presentation, not merely because
    /// the emulated machine stepped, so a browser can skip redundant canvas
    /// uploads and draws without comparing the framebuffer itself.
    pub fn presentation_revision(&self) -> u32 {
        self.presentation_revision
    }

    pub fn present_rows(&self) -> u32 {
        self.present_rows as u32
    }

    /// Width of the presentation buffer in pixels. The captured TV aperture
    /// for standard 15 kHz displays (PAL and NTSC alike), the full
    /// framebuffer width otherwise; it can change between frames, so JS must
    /// size the canvas from it each frame alongside `present_rows`.
    pub fn present_width(&self) -> u32 {
        self.present_width as u32
    }

    /// Emulated field lines the presentation buffer shows, for a page-side
    /// CRT shader pass to key its scanline pitch: 270 on the standard 50 Hz
    /// TV aperture, 214 on a 60 Hz scan, half the presented rows in full
    /// overscan. 0 means the pass has nothing to draw -- no frame yet, or a
    /// programmable scan, whose lines are not a 15 kHz raster (the desktop
    /// suspends its CRT preset there too). Tracks the presentation, so it
    /// can change between frames like `present_width`.
    pub fn present_crt_lines(&self) -> f32 {
        self.present_crt_lines
    }

    /// Host milliseconds spent advancing the emulated machine in the most
    /// recent `run`/`run_hidden` call, excluding the Rust presentation
    /// renderer.
    pub fn last_run_core_ms(&self) -> f64 {
        self.last_run_core_ms
    }

    /// Host milliseconds spent in the Rust presentation renderer in the most
    /// recent `run` call. `run_hidden`, an idle run, and a call that needed no
    /// repaint report zero.
    pub fn last_run_render_ms(&self) -> f64 {
        self.last_run_render_ms
    }

    /// Drain the mixed audio: interleaved stereo f32 at 44.1 kHz, one PAL
    /// frame is 882 stereo frames. The page transfers the returned buffer to
    /// the AudioWorklet.
    pub fn take_audio(&mut self) -> Vec<f32> {
        std::mem::take(&mut *self.audio.borrow_mut())
    }

    /// Queued audio frames not yet drained (diagnostics).
    pub fn audio_pending(&self) -> u32 {
        (self.audio.borrow().len() / 2) as u32
    }

    /// Forward a keyboard event; `code` is `KeyboardEvent.code`. Returns
    /// true when the key maps to an Amiga key (the page then calls
    /// preventDefault).
    pub fn key_event(&mut self, code: &str, pressed: bool) -> bool {
        match w3c_code_to_amiga_rawkey(code) {
            Some(rawkey) => {
                self.emu.bus_mut().enqueue_key_event(rawkey, pressed);
                true
            }
            None => false,
        }
    }

    /// Forward an Amiga raw key transition straight to the keyboard MCU.
    /// The page's on-screen keyboard draws Amiga keys, so its keys already
    /// are rawkeys and a `KeyboardEvent.code` round trip would be a lossy
    /// detour: $2B, the key beside Return on an ISO Amiga keyboard, has no
    /// positional code a browser reports on every host layout, and the
    /// reverse table would have to be duplicated in the page glue.
    pub fn key_raw(&mut self, rawkey: u8, pressed: bool) {
        self.emu.bus_mut().enqueue_key_event(rawkey & 0x7F, pressed);
    }

    /// The Caps Lock LED, owned by the keyboard MCU: pressing the key
    /// toggles it, and the up code is what unlocking sends. A page lighting
    /// a virtual Caps Lock key must read this rather than mirror its own
    /// taps, or a save-state load leaves the two disagreeing.
    pub fn caps_lock_led(&self) -> bool {
        self.emu.bus().keyboard.caps_lock_led()
    }

    /// Relative mouse motion in emulated hi-res pixels (pointer-lock
    /// movementX/Y, or scaled cursor deltas when unlocked).
    pub fn mouse_delta(&mut self, dx: f64, dy: f64) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        self.mouse_remainder.0 += dx;
        self.mouse_remainder.1 += dy;
        let ix = take_integral_delta(&mut self.mouse_remainder.0);
        let iy = take_integral_delta(&mut self.mouse_remainder.1);
        // Into the pending pool, not the counters: `run` drains it a
        // bounded amount per emulated frame (see `mouse_pending`).
        self.mouse_pending.0 = self.mouse_pending.0.saturating_add(ix);
        self.mouse_pending.1 = self.mouse_pending.1.saturating_add(iy);
    }

    /// Move at most one frame's worth of physically plausible mouse motion
    /// from the pending pool into the hardware counters. 100 counts per
    /// vblank sits under the 127-count wrap limit of the 8-bit JOYxDAT
    /// counters with margin, and is still ~5000 counts/second - faster
    /// than a hand can sweep a real mouse.
    fn drain_pending_mouse(&mut self) {
        const MAX_COUNTS_PER_FRAME: i32 = 100;
        let dx = self
            .mouse_pending
            .0
            .clamp(-MAX_COUNTS_PER_FRAME, MAX_COUNTS_PER_FRAME);
        let dy = self
            .mouse_pending
            .1
            .clamp(-MAX_COUNTS_PER_FRAME, MAX_COUNTS_PER_FRAME);
        if dx != 0 || dy != 0 {
            self.mouse_pending.0 -= dx;
            self.mouse_pending.1 -= dy;
            self.emu.bus_mut().input.add_mouse_delta(0, dx, dy);
        }
    }

    /// Mouse buttons: 0 = left, 1 = middle, 2 = right (MouseEvent.button).
    pub fn mouse_button(&mut self, button: u8, pressed: bool) {
        let input = &mut self.emu.bus_mut().input;
        match button {
            0 => input.set_mouse_button(0, 0, pressed),
            1 => input.set_mouse_button(0, 2, pressed),
            2 => input.set_mouse_button(0, 1, pressed),
            _ => {}
        }
    }

    /// Digital joystick state for either port (1 or 2): the page's
    /// keyboard-joystick mapping, or a Gamepad API bridge. Marks the port as
    /// a joystick, which is what makes two-player work -- a second pad takes
    /// port 1, exactly like unplugging the mouse to plug a stick in. `fire`
    /// is the red/primary button, `button2` the blue/second button. Any port
    /// number other than 1 means port 2, matching the core's convention.
    #[allow(clippy::too_many_arguments)]
    pub fn set_joystick_port(
        &mut self,
        port: u8,
        up: bool,
        down: bool,
        left: bool,
        right: bool,
        fire: bool,
        button2: bool,
    ) {
        self.emu.bus_mut().input.set_joystick(
            port_index(port),
            up,
            down,
            left,
            right,
            fire,
            button2,
        );
    }

    /// The CD32 pad's extra buttons on either port (red/blue arrive through
    /// `set_joystick_port` as fire/button2).
    pub fn set_cd32_buttons_port(
        &mut self,
        port: u8,
        play: bool,
        rwd: bool,
        ffw: bool,
        green: bool,
        yellow: bool,
    ) {
        self.emu
            .bus_mut()
            .input
            .set_cd32_buttons(port_index(port), play, rwd, ffw, green, yellow);
    }

    /// Plug a device into a port: "mouse", "joystick", "cd32", "analogue",
    /// or "none". Unplugging releases every line the old device drove, so a
    /// page whose gamepad goes away restores the mouse on port 1 with
    /// `set_port_device(1, "mouse")` rather than leaving a stuck stick.
    /// Unknown names are ignored.
    pub fn set_port_device(&mut self, port: u8, device: &str) {
        if let Some(device) = PortDevice::parse(device) {
            self.emu
                .bus_mut()
                .input
                .set_port_device(port_index(port), device);
        }
    }

    /// Port-2 joystick state. Superseded by `set_joystick_port`, kept
    /// because it is the published page-glue API.
    #[allow(clippy::too_many_arguments)]
    pub fn set_joystick_port2(
        &mut self,
        up: bool,
        down: bool,
        left: bool,
        right: bool,
        fire: bool,
        button2: bool,
    ) {
        self.set_joystick_port(2, up, down, left, right, fire, button2);
    }

    /// Port-2 CD32 buttons. Superseded by `set_cd32_buttons_port`.
    pub fn set_cd32_buttons_port2(
        &mut self,
        play: bool,
        rwd: bool,
        ffw: bool,
        green: bool,
        yellow: bool,
    ) {
        self.set_cd32_buttons_port(2, play, rwd, ffw, green, yellow);
    }

    /// Insert a floppy image (ADF/ADZ/DMS/extended ADF, optionally
    /// gzip/zip-packed) from bytes. Always write-protected: the browser has
    /// nowhere to write changes back to.
    pub fn insert_floppy(&mut self, drive: u8, bytes: Vec<u8>, name: &str) -> Result<(), JsValue> {
        self.emu
            .bus_mut()
            .floppy
            .insert_disk_image_bytes(drive as usize, bytes, PathBuf::from(name), true)
            .map_err(js_err)
    }

    pub fn eject_floppy(&mut self, drive: u8) -> Result<(), JsValue> {
        self.emu
            .bus_mut()
            .floppy
            .eject_disk_image(drive as usize)
            .map_err(js_err)
    }

    /// Power LED brightness: true while the guest holds CIA-A's /LED line
    /// engaged (full brightness, Paula's filter on), false once it releases
    /// it -- the page then shows the dimmed A500 rev 6+ level, never an
    /// unlit LED, as a running machine is always powered. The front-panel
    /// getters below are cheap enough to poll once per animation frame.
    pub fn power_led(&self) -> bool {
        self.emu.bus().front_panel_status().power_led_bright
    }

    /// Floppy activity LED: lit while any drive's motor runs.
    pub fn fdd_led(&self) -> bool {
        self.emu.bus().front_panel_status().fdd_led_on
    }

    /// Cylinder under the selected floppy drive's head, or undefined when
    /// no drive is selected. The page latches the last value so a track
    /// counter does not flicker between accesses, like the desktop bar.
    pub fn fdd_track(&self) -> Option<u8> {
        self.emu.bus().front_panel_status().fdd_track
    }

    /// Hard-disk activity LED, or undefined on machines without a disk
    /// controller (the page hides the LED).
    pub fn hdd_led(&self) -> Option<bool> {
        self.emu.bus().front_panel_status().hdd_led
    }

    /// CD activity LED, or undefined on machines without a CD drive.
    pub fn cd_led(&self) -> Option<bool> {
        self.emu.bus().front_panel_status().cd_led
    }

    /// Whether DFn is wired up: DF0 always, DF1-DF3 when configured.
    pub fn drive_connected(&self, drive: u8) -> bool {
        self.emu.bus().floppy.drive_connected(drive as usize)
    }

    /// File name of the image in DFn, or undefined when the drive is
    /// empty (so this doubles as the inserted check).
    pub fn disk_name(&self, drive: u8) -> Option<String> {
        self.emu.bus().floppy.inserted_disk_name(drive as usize)
    }

    /// Queue received bytes for Paula's serial receiver (the page's
    /// socket -> the guest). The queue is unbounded and the UART consumes it
    /// at the emulated baud rate, so pace large transfers with
    /// `serial_input_backlog` instead of pushing megabytes at once.
    pub fn serial_send(&mut self, bytes: Vec<u8>) {
        self.serial.push_input(&bytes);
    }

    /// Drain everything the guest transmitted on the serial port since the
    /// last call (the guest -> the page's socket). Call once per animation
    /// frame, like `take_audio`; output is bounded, and anything a
    /// non-draining page lets pile up past that bound is dropped oldest
    /// first. This also carries boot-ROM/OS debug output, so a page may log
    /// it even with no socket connected.
    pub fn serial_take(&mut self) -> Vec<u8> {
        self.serial.take_output()
    }

    /// Bytes queued by `serial_send` that the guest's UART has not yet
    /// consumed. Flow control: stop reading the socket while this is large.
    pub fn serial_input_backlog(&self) -> u32 {
        self.serial.input_backlog().min(u32::MAX as usize) as u32
    }

    /// Whether the guest is asserting the serial port's DTR line (CIA-B PA7
    /// driven low). A terminal raises DTR when it opens the port --
    /// serial.device does it on OpenDevice, hardware-level terminals set the
    /// CIA bit themselves -- and drops it on close and at reset, so this is
    /// the "guest terminal is ready" signal a modem would key off. The page
    /// bridge uses it to defer dialling until the terminal can actually
    /// display the far end's greeting.
    pub fn serial_dtr(&self) -> bool {
        self.emu.bus().cia_b.port_a_pins() & 0x80 == 0
    }

    /// Snapshot the whole emulated machine (RAM, ROM, chipset, CPU, the
    /// floppy images themselves) into a `.clstate` blob, the same format the
    /// desktop builds write, so a state saved here loads there and back. The
    /// page decides where it goes: a download, IndexedDB, anywhere it can
    /// keep bytes. Call between frames -- outside `run`, which every
    /// JS-facing method is by construction.
    pub fn save_state(&self) -> Result<Vec<u8>, JsValue> {
        self.emu.save_state_bytes().map_err(js_err)
    }

    /// Restore a state produced by `save_state` (or by a desktop build).
    /// The machine rebuilds from the blob, so the fitted ROM and inserted
    /// disks come back with it. A blob that is not a readable state of this
    /// build's format version throws and leaves the running machine
    /// untouched, so a page can offer a load without risking the session.
    ///
    /// Host-side settings do not travel with the state (they are not part of
    /// the machine): a page that keeps its own volume, drive-sound or floppy
    /// speed choices should re-apply them after a load.
    pub fn load_state(&mut self, blob: &[u8]) -> Result<(), JsValue> {
        self.emu.load_state_bytes(blob).map_err(js_err)?;
        // Emulated time jumps to the state's timeline, so the pacer must
        // start over from now rather than chase the gap, and motion buffered
        // against the pre-load machine must not replay into it.
        self.anchor = None;
        self.mouse_remainder = (0.0, 0.0);
        self.mouse_pending = (0, 0);
        // The restored frame counter may match or precede the last one
        // presented; forget it so the next render is unconditional, and
        // paint the restored screen now so a paused page shows it without
        // stepping the machine. The presentation latch belongs to the old
        // timeline, so it starts over too.
        self.last_rendered_frame = None;
        self.presentation_latch.reset();
        // The reuse detector's snapshot belongs to the replaced machine;
        // the repaint below must render, not match against it.
        self.repeated_frame_detector = bitplane::RepeatedFrameDetector::default();
        self.render_completed_frame();
        self.presentation_stale = false;
        Ok(())
    }

    /// Cold reset (power cycle), keeping the fitted ROM and inserted disks.
    pub fn reset(&mut self) -> Result<(), JsValue> {
        self.emu.power_on_reset().map_err(js_err)?;
        self.anchor = None;
        // Motion buffered against the old machine must not replay into the
        // fresh one, and the presentation latch starts over with it.
        self.mouse_remainder = (0.0, 0.0);
        self.mouse_pending = (0, 0);
        self.presentation_latch.reset();
        self.repeated_frame_detector = bitplane::RepeatedFrameDetector::default();
        Ok(())
    }

    /// Forget the wall-clock/emulated-time pairing, so the next `run` starts
    /// pacing from now instead of trying to make up the gap. A page calls
    /// this when resuming from a pause: without it the first tick after the
    /// pause sees a wall clock that ran on while the guest did not, and
    /// sprints through frames until the catch-up clamp trips.
    pub fn resync_clock(&mut self) {
        self.anchor = None;
    }

    /// Presentation overscan, the desktop's `[display] overscan` knob:
    /// "tv" (the default) masks the deep horizontal overscan margins like a
    /// CRT bezel and presents standard screens as the captured TV
    /// aperture; "full" presents the whole overscan field the renderer
    /// produces. Unknown names are ignored, like `set_port_device`. The
    /// last completed frame is re-presented under the new aperture, so a
    /// paused page repaints without stepping the machine.
    pub fn set_overscan(&mut self, mode: &str) {
        let overscan = match mode.trim().to_ascii_lowercase().as_str() {
            "tv" => Overscan::Tv,
            "full" => Overscan::Full,
            _ => return,
        };
        if overscan == self.overscan {
            return;
        }
        self.overscan = overscan;
        // Judge the new mode's geometry fresh rather than from decisions
        // latched under the old one. The reuse detector resets with it:
        // the frame's content has not changed, but its presentation has,
        // so the repaint below must run the pipeline, not match.
        self.presentation_latch.reset();
        self.last_rendered_frame = None;
        self.repeated_frame_detector = bitplane::RepeatedFrameDetector::default();
        self.render_completed_frame();
    }

    pub fn set_volume_percent(&mut self, percent: u8) {
        self.emu.bus_mut().set_output_volume_percent(percent);
    }

    /// Average the left and right channels into both outputs (the desktop's
    /// `[audio] channel_mode = "mono"`). Off by default: Paula's hardware
    /// panning, with two channels on each side.
    pub fn set_mono_audio(&mut self, enabled: bool) {
        self.emu.bus_mut().paula.set_mono_output(enabled);
    }

    /// Enable or mute the synthesized floppy drive sounds (motor hum,
    /// head-step clicks, read hiss). On by default, like the desktop's
    /// `[audio] floppy_sounds` knob.
    pub fn set_floppy_sounds(&mut self, enabled: bool) {
        self.emu
            .bus_mut()
            .paula
            .drive_sounds_mut()
            .set_enabled(enabled);
    }

    /// Drive-sound level, 0-100, relative to Paula's output (the desktop's
    /// `[audio] floppy_sounds_volume`).
    pub fn set_floppy_sounds_volume(&mut self, percent: u8) {
        self.emu
            .bus_mut()
            .paula
            .drive_sounds_mut()
            .set_volume_percent(percent);
    }

    /// Emulated floppy drive speed (the desktop's `[floppy] speed`): a
    /// data-rate percentage of 100/200/400/800, or 0 for turbo, where disk
    /// DMA transfers complete almost instantly. Other values fall back to
    /// 100. Applies immediately; drive mechanics stay at real speed.
    pub fn set_floppy_speed(&mut self, percent: u16) {
        self.emu.bus_mut().floppy.set_speed_percent(percent);
    }

    /// Current floppy drive speed value (percentage, or 0 for turbo).
    pub fn floppy_speed(&self) -> u16 {
        self.emu.bus().floppy.speed_percent()
    }

    pub fn emulated_seconds(&self) -> f64 {
        self.emu.bus().emulated_seconds()
    }
}
