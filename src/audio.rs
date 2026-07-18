// SPDX-License-Identifier: GPL-3.0-or-later

//! Audio output sink. Paula's mixed stereo output is funneled through
//! here. Mirrors the shape of [`crate::serial::SerialSink`]: a trait
//! plus several concrete implementations chosen at startup time based
//! on CLI flags.

#[cfg(feature = "frontend")]
use crate::timebase::Instant;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
#[cfg(feature = "frontend")]
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[cfg(feature = "frontend")]
use std::sync::Arc;

use anyhow::{anyhow, Result};
#[cfg(feature = "frontend")]
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(feature = "frontend")]
use ringbuf::traits::{Consumer, Observer, Producer, Split};
#[cfg(feature = "frontend")]
use ringbuf::HeapRb;

/// Sample rate the mixer feeds the sink at. This is the rate the
/// emulator-side stereo mixer (Paula::tick_audio) runs at; live CPAL
/// output resamples these frames to the selected device rate.
pub const MIX_SAMPLE_RATE: u32 = 44_100;
#[cfg(feature = "frontend")]
const PAL_PAULA_CLOCK_HZ: u32 = 3_546_895;
const AUDIO_PROFILE_ENV: &str = "COPPERLINE_AUDIO_PROFILE";
#[cfg(feature = "frontend")]
const CPAL_BUFFER_FRAMES: usize = 131072;
// Live-output latency budget. The steady-state target is deliberately fixed:
// the emulator's real-time pacer runs the core ahead of the wall clock by
// `CPAL_TARGET_BUFFER_FRAMES` worth of audio (it subtracts
// `live_output_lead_seconds` from its device-time target). If a host hitch
// drains an already-started queue below target, the sink reports the shortfall
// as extra temporary lead so the pacer refills the fixed cushion instead of
// settling into a fragile low-latency state.
#[cfg(feature = "frontend")]
const CPAL_TARGET_BUFFER_FRAMES: usize = 6615; // ~150 ms steady lead
#[cfg(feature = "frontend")]
const CPAL_PREBUFFER_FRAMES: usize = CPAL_TARGET_BUFFER_FRAMES;
#[cfg(feature = "frontend")]
const CPAL_STALE_DROP_THRESHOLD_FRAMES: usize = 13230; // trim only past ~300 ms

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioRuntimeStatus {
    pub queue_depth_frames: usize,
    pub output_lead_seconds: f64,
    pub callback_underrun_frames: u64,
    pub dropped_overrun_frames: u64,
    pub skipped_stale_frames: u64,
    pub prebuffering: bool,
}

// Note: this trait is intentionally *not* `Send`. CpalSink owns a
// `cpal::Stream` which is `!Send` on macOS (the CoreAudio backend
// uses non-thread-safe Objective-C types internally). The whole
// emulator runs on the winit main thread, so no thread crossing
// happens. The cpal callback thread receives samples via the
// internal ring buffer, whose producer half *is* Send.
pub trait AudioSink {
    /// Push one stereo frame of normalised samples. Both inputs are
    /// expected to be in roughly [-1.0, 1.0] though we don't clip.
    fn push(&mut self, left: f32, right: f32);
    fn flush(&mut self);
    /// Suspend only the host live-output stream while emulation is
    /// intentionally not producing samples. Offline sinks can ignore this.
    fn set_live_output_suspended(&mut self, _suspended: bool) {}
    /// Discard host-side live-output frames that belong to an abandoned
    /// emulated timeline. The emulated Paula/CD/floppy audio state is not
    /// touched; only queued cpal presentation samples are reset.
    fn reset_live_output_after_timeline_jump(&mut self) {}
    fn live_output_lead_seconds(&self) -> f64 {
        0.0
    }
    fn runtime_status(&self) -> AudioRuntimeStatus {
        AudioRuntimeStatus::default()
    }
    /// True only for the no-op sink that discards every sample (`NullSink`).
    /// The configuration screen runs on a silent placeholder machine using this
    /// sink; loading a state over that machine detects it here so the restored
    /// machine can be given a real host output instead of staying silent.
    fn is_null_sink(&self) -> bool {
        false
    }
    /// True once the live output device has gone away (e.g. unplugged) and the
    /// stream can no longer play. The host polls this to rebuild the sink on the
    /// current default device. Sinks without a host device never report it.
    fn device_lost(&self) -> bool {
        false
    }
}

pub fn audio_profile_enabled() -> bool {
    crate::envcfg::flag(AUDIO_PROFILE_ENV)
}

// -----------------------------------------------------------------
// NullSink: drops everything. Used when --noaudio is passed without
// --audio-wav.
// -----------------------------------------------------------------

pub struct NullSink;

impl AudioSink for NullSink {
    fn push(&mut self, _left: f32, _right: f32) {}
    fn flush(&mut self) {}
    fn is_null_sink(&self) -> bool {
        true
    }
}

// -----------------------------------------------------------------
// CpalSink: writes mixer frames into a single-producer/single-consumer
// ring buffer; a cpal output stream pulls from the consumer half on
// its own callback thread. Underruns are silently filled with zeros
// and counted. When the producer overruns, the callback is asked to
// drop old queued frames so live output stays near the emulator's
// current Paula state instead of playing a stale backlog.
// -----------------------------------------------------------------

#[cfg(feature = "frontend")]
pub struct CpalSink {
    producer: ringbuf::HeapProd<(f32, f32)>,
    // Keep the stream alive for the lifetime of the sink.
    _stream: cpal::Stream,
    playback_started: Arc<AtomicBool>,
    clear_buffer: Arc<AtomicBool>,
    drop_old_frames: Arc<AtomicUsize>,
    dropped_old_frames: Arc<AtomicU64>,
    total_dropped_old_frames: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
    total_underruns: Arc<AtomicU64>,
    live_output_suspended: Arc<AtomicBool>,
    // Set by the stream error callback when the output device disappears
    // (unplugged); polled by the host so it can rebuild on the default device.
    device_lost: Arc<AtomicBool>,
    profile_callbacks: Arc<AtomicU64>,
    profile_callback_frames: Arc<AtomicU64>,
    profile_callback_device_cck: Arc<AtomicU64>,
    profile_enabled: bool,
    generated_frames: u64,
    overruns: u64,
    total_overruns: u64,
    last_log: Instant,
    prebuffer_frames: usize,
}

// libasound's `snd_lib_error_handler_t` is variadic (`..., const char *fmt,
// ...`). A handler that ignores the trailing varargs is safe to install under
// the C calling convention (the caller cleans the stack), so it is declared
// without them.
#[cfg(all(feature = "frontend", target_os = "linux"))]
type AlsaErrorHandler = extern "C" fn(
    *const std::ffi::c_char,
    std::ffi::c_int,
    *const std::ffi::c_char,
    std::ffi::c_int,
    *const std::ffi::c_char,
);

#[cfg(all(feature = "frontend", target_os = "linux"))]
#[link(name = "asound")]
extern "C" {
    fn snd_lib_error_set_handler(handler: Option<AlsaErrorHandler>) -> std::ffi::c_int;
}

#[cfg(all(feature = "frontend", target_os = "linux"))]
extern "C" fn alsa_ignore_error(
    _file: *const std::ffi::c_char,
    _line: std::ffi::c_int,
    _function: *const std::ffi::c_char,
    _err: std::ffi::c_int,
    _fmt: *const std::ffi::c_char,
) {
}

/// Silence libasound's stderr chatter (the `ALSA lib pcm_...` lines) that it
/// prints while cpal probes devices for enumeration. Those are informational
/// plugin messages, not failures we can act on -- cpal still reports real
/// errors through its `Result` API, so nothing user-facing is hidden. Installs
/// a no-op error handler once, process-wide, keeping `--list-audio-devices` and
/// the picker readable. No-op off Linux, where the handler does not exist.
#[cfg(all(feature = "frontend", target_os = "linux"))]
pub(crate) fn quiet_alsa_probe_logging() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        snd_lib_error_set_handler(Some(alsa_ignore_error));
    });
}

#[cfg(all(feature = "frontend", not(target_os = "linux")))]
pub(crate) fn quiet_alsa_probe_logging() {}

/// Whether an ALSA device name is a low-level *plugin* handle rather than a
/// device a user would pick: the channel-layout plugins (`front`, `surround51`,
/// ...), the software mix/snoop plugins (`dmix`, `dsnoop`), and the raw/wrapped
/// per-card hardware handles (`hw`, `plughw`, `sysdefault`) -- ALSA advertises
/// all of these for every card, so one physical output shows up many times over.
/// The clean routes (`default`, `pipewire`, `pulse`, `jack`) and any
/// friendly-named devices pass through. This keys off ALSA's fixed plugin-type
/// vocabulary (the token before the first `:`), identical on every distro, not
/// off any card or device name -- and hidden handles are still selectable by
/// name in the config/CLI, since only the displayed list is filtered. A no-op on
/// macOS/Windows, whose device names never take this form.
#[cfg(feature = "frontend")]
pub(crate) fn is_alsa_plugin_variant(name: &str) -> bool {
    let plugin = name.split(':').next().unwrap_or(name);
    matches!(
        plugin,
        "front"
            | "rear"
            | "center_lfe"
            | "side"
            | "surround21"
            | "surround40"
            | "surround41"
            | "surround50"
            | "surround51"
            | "surround71"
            | "dmix"
            | "dsnoop"
            | "hw"
            | "plughw"
            | "sysdefault"
    )
}

/// A GUI audio-output selection: the system default, a specific named device, or
/// disabled -- a [`NullSink`], i.e. no sound, the same effect as `--noaudio`. The
/// launcher picker and runtime menu cycle Default -> devices -> Disabled. This is
/// a GUI-only concept; the CLI keeps its separate `--audio-device`/`--noaudio`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AudioOutput {
    /// The host's system default output device.
    #[default]
    Default,
    /// A specific device, matched by name against the enumerated outputs.
    Device(String),
    /// No audio output at all (null sink).
    Disabled,
}

impl AudioOutput {
    /// Build the selection from config: disabled when output is off, otherwise
    /// the named device, or the default when none is set. A blank/whitespace-only
    /// name is treated as the default, matching how config resolution handles it.
    pub fn from_config(enabled: bool, device: Option<&str>) -> Self {
        let device = device.filter(|name| !name.trim().is_empty());
        match (enabled, device) {
            (false, _) => AudioOutput::Disabled,
            (true, Some(name)) => AudioOutput::Device(name.to_string()),
            (true, None) => AudioOutput::Default,
        }
    }

    /// The named device, if one is selected (`None` for Default and Disabled).
    pub fn device(&self) -> Option<&str> {
        match self {
            AudioOutput::Device(name) => Some(name),
            _ => None,
        }
    }

    /// Whether live audio output is produced (false only for Disabled).
    pub fn is_enabled(&self) -> bool {
        !matches!(self, AudioOutput::Disabled)
    }

    /// Picker label: "Default", the device name, or "Disabled".
    pub fn label(&self) -> &str {
        match self {
            AudioOutput::Default => "Default",
            AudioOutput::Device(name) => name,
            AudioOutput::Disabled => "Disabled",
        }
    }

    /// Step to the next selection, cycling Default -> device0 -> ... -> deviceN
    /// -> Disabled and back. With no devices this is just Default <-> Disabled.
    pub fn cycle(&self, devices: &[String], forward: bool) -> Self {
        // Positions: 0 = Default, 1..=len = devices, len + 1 = Disabled.
        let len = devices.len();
        let here = match self {
            AudioOutput::Default => 0,
            AudioOutput::Device(name) => {
                devices.iter().position(|n| n == name).map_or(0, |i| i + 1)
            }
            AudioOutput::Disabled => len + 1,
        };
        let count = len + 2;
        let next = if forward {
            (here + 1) % count
        } else {
            (here + count - 1) % count
        };
        if next == 0 {
            AudioOutput::Default
        } else if next <= len {
            AudioOutput::Device(devices[next - 1].clone())
        } else {
            AudioOutput::Disabled
        }
    }
}

/// Open the audio sink for a picker selection: a [`NullSink`] when disabled,
/// otherwise a [`CpalSink`] on the chosen (or default) device. Device-open
/// errors propagate so callers can report them; Disabled never fails.
#[cfg(feature = "frontend")]
pub fn open_output_sink(
    realtime_priority: bool,
    output: &AudioOutput,
) -> Result<Box<dyn AudioSink>> {
    Ok(match output {
        AudioOutput::Disabled => {
            // Mirror CpalSink's "sink ready" log so the CLI shows the change too.
            log::info!("audio: disabled (null sink); no sound");
            Box::new(NullSink)
        }
        _ => Box::new(CpalSink::new(realtime_priority, output.device())?),
    })
}

/// Names of the host's audio output devices, for `--list-audio-devices` and as
/// the base for the GUI picker, with ALSA's low-level plugin handles filtered
/// out (see [`is_alsa_plugin_variant`]). ALSA's "default" is kept here so the
/// CLI can name it; the GUI drops it separately (see [`picker_output_devices`]).
/// Empty if the host cannot enumerate. Selection by name still matches the full
/// set, so a hidden entry can be named explicitly.
///
/// On Linux, cpal enumerates via ALSA, and PipeWire/PulseAudio expose only their
/// `default`/`pipewire` bridge there, not one ALSA device per sink -- so a
/// specific sink cannot be named, only the system default (routed in the desktop
/// mixer). Naming individual sinks would need cpal's `jack` backend against
/// pipewire-jack, which adds a libjack build dependency; not worth it for a niche
/// control when macOS/Windows enumerate every device directly.
#[cfg(feature = "frontend")]
pub fn list_output_devices() -> Vec<String> {
    quiet_alsa_probe_logging();
    cpal::default_host()
        .output_devices()
        .map(|devs| {
            devs.filter_map(|d| d.name().ok())
                .filter(|name| !is_alsa_plugin_variant(name))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `name` is redundant with the GUI picker's own "Default" entry: it is
/// ALSA's `default` pseudo-device *and* that is what the system default resolves
/// to, so selecting it and selecting "Default" (the `None` option) do the same
/// thing. `default_name` is the host's default output device name. When the
/// default is some other device, `default` is a distinct choice and kept.
#[cfg(feature = "frontend")]
pub(crate) fn is_redundant_default(name: &str, default_name: Option<&str>) -> bool {
    name.eq_ignore_ascii_case("default")
        && default_name.is_some_and(|d| d.eq_ignore_ascii_case("default"))
}

/// Output-device names for the GUI picker (launcher field + runtime menu). Same
/// as [`list_output_devices`], but drops ALSA's "default" when it is the system
/// default, since the picker already offers a synthetic "Default" (the `None`
/// selection) for that. Still selectable by name in the config/CLI.
#[cfg(feature = "frontend")]
pub fn picker_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());
    list_output_devices()
        .into_iter()
        .filter(|name| !is_redundant_default(name, default_name.as_deref()))
        .collect()
}

/// The output device to open: the first whose name contains `want`
/// (case-insensitive), otherwise the system default. A named-but-missing
/// device warns and falls back to the default rather than leaving the machine
/// silent.
#[cfg(feature = "frontend")]
fn select_output_device(host: &cpal::Host, want: Option<&str>) -> Result<cpal::Device> {
    if let Some(name) = want {
        let needle = name.to_lowercase();
        let matched = host.output_devices().ok().and_then(|mut devs| {
            devs.find(|d| {
                d.name()
                    .map(|n| n.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            })
        });
        match matched {
            Some(device) => return Ok(device),
            None => log::warn!("audio: no output device matches {name:?}; using the default"),
        }
    }
    host.default_output_device()
        .ok_or_else(|| anyhow!("no default audio output device"))
}

#[cfg(feature = "frontend")]
impl CpalSink {
    /// Build the live cpal output sink. When `realtime_priority` is set, the
    /// audio callback thread promotes itself on its first invocation (see
    /// [`crate::priority`]); the flag is resolved by the caller from config and
    /// the `COPPERLINE_REALTIME_PRIORITY` env var.
    pub fn new(realtime_priority: bool, output_device: Option<&str>) -> Result<Self> {
        quiet_alsa_probe_logging();
        let host = cpal::default_host();
        let device = select_output_device(&host, output_device)?;
        let supported = device
            .default_output_config()
            .map_err(|e| anyhow!("query default output config: {e}"))?;

        // Paula mixing always feeds f32 stereo at MIX_SAMPLE_RATE.
        // The host stream uses the device's default rate and the
        // callback performs a small linear resample so live output
        // doesn't slowly drain or grow when the device is e.g. 48 kHz.
        let channels = supported.channels().max(2);
        let output_sample_rate = supported.sample_rate().0;
        let config = cpal::StreamConfig {
            channels,
            sample_rate: supported.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };

        // ~3 s capacity at 44.1 kHz. Playback starts after a short
        // prebuffer so live output comes up promptly; stale-frame
        // dropping still trims back to a bounded latency target.
        let rb = HeapRb::<(f32, f32)>::new(CPAL_BUFFER_FRAMES);
        let (producer, mut consumer) = rb.split();

        let prebuffer_frames = CPAL_PREBUFFER_FRAMES;
        let playback_started = Arc::new(AtomicBool::new(false));
        let playback_started_for_cb = Arc::clone(&playback_started);
        let clear_buffer = Arc::new(AtomicBool::new(false));
        let clear_buffer_for_cb = Arc::clone(&clear_buffer);
        let drop_old_frames = Arc::new(AtomicUsize::new(0));
        let drop_old_frames_for_cb = Arc::clone(&drop_old_frames);
        let dropped_old_frames = Arc::new(AtomicU64::new(0));
        let dropped_old_frames_for_cb = Arc::clone(&dropped_old_frames);
        let total_dropped_old_frames = Arc::new(AtomicU64::new(0));
        let total_dropped_old_frames_for_cb = Arc::clone(&total_dropped_old_frames);
        let underruns = Arc::new(AtomicU64::new(0));
        let underruns_for_cb = Arc::clone(&underruns);
        let total_underruns = Arc::new(AtomicU64::new(0));
        let total_underruns_for_cb = Arc::clone(&total_underruns);
        let device_lost = Arc::new(AtomicBool::new(false));
        let device_lost_for_cb = Arc::clone(&device_lost);
        let live_output_suspended = Arc::new(AtomicBool::new(false));
        let live_output_suspended_for_cb = Arc::clone(&live_output_suspended);
        let profile_enabled = audio_profile_enabled();
        let profile_callbacks = Arc::new(AtomicU64::new(0));
        let profile_callbacks_for_cb = Arc::clone(&profile_callbacks);
        let profile_callback_frames = Arc::new(AtomicU64::new(0));
        let profile_callback_frames_for_cb = Arc::clone(&profile_callback_frames);
        let profile_callback_device_cck = Arc::new(AtomicU64::new(0));
        let profile_callback_device_cck_for_cb = Arc::clone(&profile_callback_device_cck);
        let mut resampler = CpalResampler::new(output_sample_rate);

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                    // Runs on the cpal-owned audio thread. Latched internally,
                    // so only the first callback does the scheduling syscall.
                    if realtime_priority {
                        crate::priority::promote_audio_thread_once();
                    }
                    let chans = channels as usize;
                    if profile_enabled {
                        let frames = data.len() / chans;
                        profile_callbacks_for_cb.fetch_add(1, Ordering::Relaxed);
                        profile_callback_frames_for_cb.fetch_add(frames as u64, Ordering::Relaxed);
                        profile_callback_device_cck_for_cb.fetch_add(
                            callback_device_cck(frames, output_sample_rate),
                            Ordering::Relaxed,
                        );
                    }
                    if clear_buffer_for_cb.swap(false, Ordering::Relaxed) {
                        consumer.clear();
                        resampler.reset();
                    }
                    let requested_drop = drop_old_frames_for_cb.swap(0, Ordering::Relaxed);
                    if requested_drop != 0 {
                        let skipped = consumer.skip(requested_drop);
                        if skipped != 0 {
                            dropped_old_frames_for_cb.fetch_add(skipped as u64, Ordering::Relaxed);
                            total_dropped_old_frames_for_cb
                                .fetch_add(skipped as u64, Ordering::Relaxed);
                            resampler.reset();
                        }
                    }
                    if live_output_suspended_for_cb.load(Ordering::Relaxed) {
                        resampler.reset();
                        for sample in data {
                            *sample = 0.0;
                        }
                        return;
                    }
                    for frame in data.chunks_mut(chans) {
                        let (l, r) = next_live_audio_output_frame(
                            &mut resampler,
                            &mut consumer,
                            &underruns_for_cb,
                            &total_underruns_for_cb,
                            &playback_started_for_cb,
                        );
                        if chans == 1 {
                            frame[0] = 0.5 * (l + r);
                        } else {
                            frame[0] = l;
                            frame[1] = r;
                            for extra in &mut frame[2..] {
                                *extra = 0.0;
                            }
                        }
                    }
                },
                move |err| {
                    log::warn!("cpal stream error: {err}");
                    // A vanished device (unplugged, or the default switched away)
                    // cannot recover on its own; flag it so the host reopens on
                    // the current default output.
                    if matches!(err, cpal::StreamError::DeviceNotAvailable) {
                        device_lost_for_cb.store(true, Ordering::Relaxed);
                    }
                },
                None,
            )
            .map_err(|e| anyhow!("build_output_stream: {e}"))?;
        stream.play().map_err(|e| anyhow!("stream play: {e}"))?;

        log::info!(
            "audio: cpal sink ready, device={:?}, channels={}, output_rate={}, mix_rate={}",
            device.name().unwrap_or_else(|_| "<unknown>".into()),
            channels,
            output_sample_rate,
            MIX_SAMPLE_RATE
        );

        Ok(Self {
            producer,
            _stream: stream,
            device_lost,
            playback_started,
            clear_buffer,
            drop_old_frames,
            dropped_old_frames,
            total_dropped_old_frames,
            underruns,
            total_underruns,
            live_output_suspended,
            profile_callbacks,
            profile_callback_frames,
            profile_callback_device_cck,
            profile_enabled,
            generated_frames: 0,
            overruns: 0,
            total_overruns: 0,
            last_log: Instant::now(),
            prebuffer_frames,
        })
    }
}

#[cfg(feature = "frontend")]
fn next_live_audio_output_frame(
    resampler: &mut CpalResampler,
    consumer: &mut ringbuf::HeapCons<(f32, f32)>,
    underruns: &AtomicU64,
    total_underruns: &AtomicU64,
    playback_started: &AtomicBool,
) -> (f32, f32) {
    if playback_started.load(Ordering::Relaxed) {
        resampler.next_frame(consumer, underruns, total_underruns, playback_started)
    } else {
        resampler.reset();
        (0.0, 0.0)
    }
}

#[cfg(feature = "frontend")]
struct CpalResampler {
    step: f64,
    phase: f64,
    current: (f32, f32),
    next: (f32, f32),
    primed: bool,
}

#[cfg(feature = "frontend")]
impl CpalResampler {
    fn new(output_sample_rate: u32) -> Self {
        Self {
            step: MIX_SAMPLE_RATE as f64 / output_sample_rate.max(1) as f64,
            phase: 0.0,
            current: (0.0, 0.0),
            next: (0.0, 0.0),
            primed: false,
        }
    }

    fn reset(&mut self) {
        self.phase = 0.0;
        self.current = (0.0, 0.0);
        self.next = (0.0, 0.0);
        self.primed = false;
    }

    fn next_frame(
        &mut self,
        consumer: &mut ringbuf::HeapCons<(f32, f32)>,
        underruns: &AtomicU64,
        total_underruns: &AtomicU64,
        playback_started: &AtomicBool,
    ) -> (f32, f32) {
        if !self.primed {
            let Some(current) = pop_live_audio_frame(consumer, underruns, total_underruns) else {
                self.stop_after_underrun(playback_started);
                return (0.0, 0.0);
            };
            let Some(next) = pop_live_audio_frame(consumer, underruns, total_underruns) else {
                self.stop_after_underrun(playback_started);
                return (0.0, 0.0);
            };
            self.current = current;
            self.next = next;
            self.primed = true;
        }

        let left = self.current.0 + (self.next.0 - self.current.0) * self.phase as f32;
        let right = self.current.1 + (self.next.1 - self.current.1) * self.phase as f32;

        self.phase += self.step;
        while self.phase >= 1.0 {
            self.current = self.next;
            let Some(next) = pop_live_audio_frame(consumer, underruns, total_underruns) else {
                self.stop_after_underrun(playback_started);
                return (0.0, 0.0);
            };
            self.next = next;
            self.phase -= 1.0;
        }

        (left, right)
    }

    fn stop_after_underrun(&mut self, playback_started: &AtomicBool) {
        self.reset();
        playback_started.store(false, Ordering::Relaxed);
    }
}

#[cfg(feature = "frontend")]
fn pop_live_audio_frame(
    consumer: &mut ringbuf::HeapCons<(f32, f32)>,
    underruns: &AtomicU64,
    total_underruns: &AtomicU64,
) -> Option<(f32, f32)> {
    consumer.try_pop().or_else(|| {
        underruns.fetch_add(1, Ordering::Relaxed);
        total_underruns.fetch_add(1, Ordering::Relaxed);
        None
    })
}

#[cfg(feature = "frontend")]
impl AudioSink for CpalSink {
    fn push(&mut self, left: f32, right: f32) {
        self.generated_frames = self.generated_frames.saturating_add(1);
        if !self.playback_started.load(Ordering::Relaxed)
            && self.producer.is_empty()
            && !sample_is_audible(left, right)
        {
            return;
        }

        if self.producer.try_push((left, right)).is_err() {
            self.overruns = self.overruns.saturating_add(1);
            self.total_overruns = self.total_overruns.saturating_add(1);
            self.request_stale_frame_drop();
        } else if !self.playback_started.load(Ordering::Relaxed)
            && self.producer.occupied_len() >= self.prebuffer_frames
        {
            self.playback_started.store(true, Ordering::Relaxed);
        } else if self.playback_started.load(Ordering::Relaxed) {
            self.request_stale_frame_drop();
        }

        // Periodically surface underrun counter so it's obvious when
        // the mixer can't keep up.
        if self.last_log.elapsed().as_secs() >= 1 {
            let underruns = self.underruns.swap(0, Ordering::Relaxed);
            let overruns = std::mem::take(&mut self.overruns);
            let dropped_old = self.dropped_old_frames.swap(0, Ordering::Relaxed);
            if underruns > 0 {
                log::warn!("audio: {underruns} cpal underrun frames in the last second");
            }
            if overruns > 0 {
                log::warn!(
                    "audio: {} cpal overrun frames dropped in the last second",
                    overruns
                );
            }
            if dropped_old > 0 {
                log::warn!("audio: skipped {dropped_old} stale cpal frames to bound live latency");
            }
            if self.profile_enabled {
                let callbacks = self.profile_callbacks.swap(0, Ordering::Relaxed);
                let callback_frames = self.profile_callback_frames.swap(0, Ordering::Relaxed);
                let callback_device_cck =
                    self.profile_callback_device_cck.swap(0, Ordering::Relaxed);
                let generated_frames = std::mem::take(&mut self.generated_frames);
                let avg_callback_device_cck = if callbacks == 0 {
                    0.0
                } else {
                    callback_device_cck as f64 / callbacks as f64
                };
                log::info!(
                    "audio profile: queue_depth={} generated_frames={} callback_underruns={} dropped_overrun_frames={} skipped_stale_frames={} callbacks={} callback_output_frames={} device_cck={} avg_device_cck_per_callback={:.1}",
                    self.producer.occupied_len(),
                    generated_frames,
                    underruns,
                    overruns,
                    dropped_old,
                    callbacks,
                    callback_frames,
                    callback_device_cck,
                    avg_callback_device_cck,
                );
            }
            self.last_log = Instant::now();
        }
    }

    fn flush(&mut self) {}

    fn set_live_output_suspended(&mut self, suspended: bool) {
        let previous = self
            .live_output_suspended
            .swap(suspended, Ordering::Relaxed);
        if previous == suspended {
            return;
        }
        // A deliberate host pause or modal filesystem operation can last
        // longer than the queued live-audio lead. Do not report those
        // callback silences as underruns when output resumes.
        self.underruns.store(0, Ordering::Relaxed);
        self.dropped_old_frames.store(0, Ordering::Relaxed);
        self.overruns = 0;
        self.last_log = Instant::now();
    }

    fn reset_live_output_after_timeline_jump(&mut self) {
        self.playback_started.store(false, Ordering::Relaxed);
        self.clear_buffer.store(true, Ordering::Relaxed);
        self.drop_old_frames.store(0, Ordering::Relaxed);
        self.underruns.store(0, Ordering::Relaxed);
        self.dropped_old_frames.store(0, Ordering::Relaxed);
        self.overruns = 0;
        self.last_log = Instant::now();
    }

    fn live_output_lead_seconds(&self) -> f64 {
        live_output_lead_seconds_for_state(
            self.playback_started.load(Ordering::Relaxed),
            self.producer.occupied_len(),
            CPAL_TARGET_BUFFER_FRAMES,
        )
    }

    fn runtime_status(&self) -> AudioRuntimeStatus {
        let playback_started = self.playback_started.load(Ordering::Relaxed);
        let occupied_frames = self.producer.occupied_len();
        AudioRuntimeStatus {
            queue_depth_frames: occupied_frames,
            output_lead_seconds: self.live_output_lead_seconds(),
            callback_underrun_frames: self.total_underruns.load(Ordering::Relaxed),
            dropped_overrun_frames: self.total_overruns,
            skipped_stale_frames: self.total_dropped_old_frames.load(Ordering::Relaxed),
            prebuffering: live_output_prebuffering(
                playback_started,
                occupied_frames,
                CPAL_TARGET_BUFFER_FRAMES,
            ),
        }
    }

    fn device_lost(&self) -> bool {
        self.device_lost.load(Ordering::Relaxed)
    }
}

#[cfg(feature = "frontend")]
impl CpalSink {
    fn request_stale_frame_drop(&self) {
        let stale = stale_live_audio_frames_to_skip(
            self.producer.occupied_len(),
            CPAL_TARGET_BUFFER_FRAMES,
            CPAL_STALE_DROP_THRESHOLD_FRAMES,
        );
        if stale != 0 {
            self.drop_old_frames.fetch_max(stale, Ordering::Relaxed);
        }
    }
}

// -----------------------------------------------------------------
// WavSink: dumps stereo f32 samples to a WAV file. Useful when the
// host doesn't have working audio output (CI, automated tests) or
// when you want to inspect the mixer output offline.
// -----------------------------------------------------------------

pub struct WavSink {
    writer: hound::WavWriter<BufWriter<File>>,
}

impl WavSink {
    pub fn new(path: &Path) -> Result<Self> {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: MIX_SAMPLE_RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let writer = hound::WavWriter::create(path, spec)
            .map_err(|e| anyhow!("create WAV {}: {e}", path.display()))?;
        log::info!(
            "audio: WAV sink writing to {} (stereo f32 @ {} Hz)",
            path.display(),
            MIX_SAMPLE_RATE
        );
        Ok(Self { writer })
    }
}

impl AudioSink for WavSink {
    fn push(&mut self, left: f32, right: f32) {
        let _ = self.writer.write_sample(left);
        let _ = self.writer.write_sample(right);
    }

    fn flush(&mut self) {
        let _ = self.writer.flush();
    }
}

#[cfg(feature = "frontend")]
fn stale_live_audio_frames_to_skip(
    occupied_len: usize,
    target_len: usize,
    drop_threshold: usize,
) -> usize {
    if occupied_len <= drop_threshold {
        0
    } else {
        occupied_len.saturating_sub(target_len)
    }
}

#[cfg(feature = "frontend")]
fn sample_is_audible(left: f32, right: f32) -> bool {
    left != 0.0 || right != 0.0
}

#[cfg(feature = "frontend")]
fn live_output_lead_seconds_for_state(
    playback_started: bool,
    occupied_frames: usize,
    target_frames: usize,
) -> f64 {
    if !playback_started && occupied_frames == 0 {
        0.0
    } else if occupied_frames < target_frames {
        let refill_frames = target_frames - occupied_frames;
        (target_frames + refill_frames) as f64 / MIX_SAMPLE_RATE as f64
    } else {
        target_frames as f64 / MIX_SAMPLE_RATE as f64
    }
}

#[cfg(feature = "frontend")]
fn live_output_prebuffering(
    playback_started: bool,
    occupied_frames: usize,
    target_frames: usize,
) -> bool {
    !playback_started && occupied_frames > 0 && occupied_frames < target_frames
}

#[cfg(feature = "frontend")]
fn callback_device_cck(output_frames: usize, output_sample_rate: u32) -> u64 {
    let rate = u64::from(output_sample_rate.max(1));
    (output_frames as u64)
        .saturating_mul(u64::from(PAL_PAULA_CLOCK_HZ))
        .div_ceil(rate)
}

#[cfg(all(test, feature = "frontend"))]
mod tests {
    use super::{
        callback_device_cck, is_alsa_plugin_variant, is_redundant_default,
        live_output_prebuffering, open_output_sink, sample_is_audible,
        stale_live_audio_frames_to_skip, AudioOutput, CpalResampler,
    };
    use ringbuf::traits::{Producer, Split};
    use ringbuf::HeapRb;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    #[test]
    fn alsa_plugin_variants_filtered_but_real_devices_kept() {
        // The low-level ALSA handles ALSA emits for every card: routing/mixing
        // plugins and the raw/wrapped/sysdefault per-card hardware handles.
        for noise in [
            "front:CARD=Intel,DEV=0",
            "surround51:CARD=Intel,DEV=0",
            "dmix:CARD=Intel,DEV=0",
            "dsnoop:CARD=Intel,DEV=0",
            "hw:CARD=Intel,DEV=0",
            "plughw:CARD=Intel,DEV=0",
            "sysdefault:CARD=Intel",
        ] {
            assert!(is_alsa_plugin_variant(noise), "should hide {noise}");
        }
        // Clean routes and non-ALSA (macOS/Windows) names pass through. Note
        // "default" is not a plugin variant; the GUI drops it separately, only
        // when it is the system default (see is_redundant_default).
        for keep in [
            "default",
            "pipewire",
            "pulse",
            "jack",
            "MacBook Air Speakers",
            "BlackHole 2ch",
        ] {
            assert!(!is_alsa_plugin_variant(keep), "should keep {keep}");
        }
    }

    #[test]
    fn default_is_only_redundant_when_it_is_the_system_default() {
        // "default" is the system default -> redundant with the picker's Default
        // (case-insensitive).
        assert!(is_redundant_default("default", Some("default")));
        assert!(is_redundant_default("Default", Some("DEFAULT")));
        // "default" exists but the system default is a different device -> keep.
        assert!(!is_redundant_default("default", Some("pipewire")));
        assert!(!is_redundant_default("default", None));
        // A normal device is never treated as the redundant default.
        assert!(!is_redundant_default("pipewire", Some("default")));
        assert!(!is_redundant_default(
            "MacBook Air Speakers",
            Some("default")
        ));
    }

    #[test]
    fn audio_output_cycles_default_devices_then_disabled() {
        let devices = vec!["BlackHole".to_string(), "Speakers".to_string()];
        // Forward: Default -> dev0 -> dev1 -> Disabled -> back to Default.
        let a = AudioOutput::Default;
        let b = a.cycle(&devices, true);
        assert_eq!(b, AudioOutput::Device("BlackHole".to_string()));
        let c = b.cycle(&devices, true);
        assert_eq!(c, AudioOutput::Device("Speakers".to_string()));
        let d = c.cycle(&devices, true);
        assert_eq!(d, AudioOutput::Disabled);
        assert_eq!(d.cycle(&devices, true), AudioOutput::Default);
        // Backward from Default lands on Disabled (the last slot).
        assert_eq!(a.cycle(&devices, false), AudioOutput::Disabled);
        // With no devices it is just Default <-> Disabled.
        assert_eq!(AudioOutput::Default.cycle(&[], true), AudioOutput::Disabled);
        assert_eq!(AudioOutput::Disabled.cycle(&[], true), AudioOutput::Default);
    }

    #[test]
    fn audio_output_maps_to_and_from_config() {
        assert_eq!(AudioOutput::from_config(true, None), AudioOutput::Default);
        assert_eq!(
            AudioOutput::from_config(true, Some("BlackHole")),
            AudioOutput::Device("BlackHole".to_string())
        );
        // Disabled regardless of any device name.
        assert_eq!(
            AudioOutput::from_config(false, Some("BlackHole")),
            AudioOutput::Disabled
        );
        // A blank/whitespace-only device name falls back to the default.
        assert_eq!(
            AudioOutput::from_config(true, Some("  ")),
            AudioOutput::Default
        );
        assert_eq!(AudioOutput::Default.device(), None);
        assert_eq!(AudioOutput::Disabled.device(), None);
        assert!(AudioOutput::Default.is_enabled());
        assert!(!AudioOutput::Disabled.is_enabled());
        assert_eq!(AudioOutput::Disabled.label(), "Disabled");
    }

    #[test]
    fn open_output_sink_is_silent_when_disabled() {
        // Disabled never touches the host device and never errors.
        let sink = open_output_sink(false, &AudioOutput::Disabled).unwrap();
        assert!(!sink.device_lost());
    }

    #[test]
    fn live_audio_backlog_uses_hysteresis_before_dropping_stale_frames() {
        assert_eq!(stale_live_audio_frames_to_skip(1024, 2048, 4096), 0);
        assert_eq!(stale_live_audio_frames_to_skip(2048, 2048, 4096), 0);
        assert_eq!(stale_live_audio_frames_to_skip(4096, 2048, 4096), 0);
        assert_eq!(stale_live_audio_frames_to_skip(8192, 2048, 4096), 6144);
    }

    #[test]
    fn live_audio_startup_drops_leading_silence() {
        assert!(!sample_is_audible(0.0, 0.0));
        assert!(sample_is_audible(0.0, 0.25));
        assert!(sample_is_audible(-0.25, 0.0));
    }

    #[test]
    fn live_audio_resampler_tracks_output_device_rate() {
        let resampler = CpalResampler::new(48_000);
        assert!((resampler.step - (44_100.0 / 48_000.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn live_audio_resampler_stops_playback_after_underrun() {
        let rb = HeapRb::<(f32, f32)>::new(4);
        let (mut producer, mut consumer) = rb.split();
        producer.try_push((0.25, -0.25)).unwrap();
        let underruns = AtomicU64::new(0);
        let total_underruns = AtomicU64::new(0);
        let playback_started = AtomicBool::new(true);
        let mut resampler = CpalResampler::new(48_000);

        assert_eq!(
            resampler.next_frame(
                &mut consumer,
                &underruns,
                &total_underruns,
                &playback_started,
            ),
            (0.0, 0.0)
        );
        assert!(!playback_started.load(Ordering::Relaxed));
        assert_eq!(underruns.load(Ordering::Relaxed), 1);
        assert_eq!(total_underruns.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn live_audio_output_keeps_draining_low_queue_until_actual_underrun() {
        let rb = HeapRb::<(f32, f32)>::new(8);
        let (mut producer, mut consumer) = rb.split();
        producer.try_push((0.25, -0.25)).unwrap();
        producer.try_push((0.5, -0.5)).unwrap();
        producer.try_push((0.75, -0.75)).unwrap();
        let underruns = AtomicU64::new(0);
        let total_underruns = AtomicU64::new(0);
        let playback_started = AtomicBool::new(true);
        let mut resampler = CpalResampler::new(super::MIX_SAMPLE_RATE);

        assert_eq!(
            super::next_live_audio_output_frame(
                &mut resampler,
                &mut consumer,
                &underruns,
                &total_underruns,
                &playback_started,
            ),
            (0.25, -0.25)
        );
        assert!(playback_started.load(Ordering::Relaxed));
        assert_eq!(underruns.load(Ordering::Relaxed), 0);
        assert_eq!(total_underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn live_audio_lead_starts_after_audible_queueing() {
        assert_eq!(
            super::live_output_lead_seconds_for_state(false, 0, 4096),
            0.0
        );
        assert!(super::live_output_lead_seconds_for_state(false, 1, 4096) > 0.0);
        assert!(super::live_output_lead_seconds_for_state(true, 0, 4096) > 0.0);
    }

    #[test]
    fn live_audio_reports_prebuffering_before_playback_starts() {
        assert!(!live_output_prebuffering(false, 0, 4096));
        assert!(live_output_prebuffering(false, 1, 4096));
        assert!(live_output_prebuffering(false, 4095, 4096));
        assert!(!live_output_prebuffering(false, 4096, 4096));
        assert!(!live_output_prebuffering(true, 1, 4096));
    }

    #[test]
    fn live_audio_lead_reports_started_queue_deficit_for_refill() {
        let target_seconds = 4096.0 / super::MIX_SAMPLE_RATE as f64;
        let underfilled_seconds = super::live_output_lead_seconds_for_state(true, 1024, 4096);
        let prebuffer_seconds = super::live_output_lead_seconds_for_state(false, 1024, 4096);
        let full_seconds = super::live_output_lead_seconds_for_state(true, 4096, 4096);
        let overfilled_seconds = super::live_output_lead_seconds_for_state(true, 8192, 4096);

        assert!(underfilled_seconds > target_seconds);
        assert_eq!(prebuffer_seconds, underfilled_seconds);
        assert_eq!(full_seconds, target_seconds);
        assert_eq!(overfilled_seconds, target_seconds);
    }

    #[test]
    fn audio_profile_callback_cck_tracks_device_rate() {
        assert_eq!(callback_device_cck(48_000, 48_000), 3_546_895);
        assert_eq!(callback_device_cck(24_000, 48_000), 1_773_448);
    }
}
