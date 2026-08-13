// SPDX-License-Identifier: GPL-3.0-or-later

//! Parallel-port audio sampler (digitizer) -- a [`crate::parallel::ParallelPort`]
//! input peripheral.
//!
//! Classic Amiga 8-bit samplers (AMAS, DSS, Megalosound, the open-amiga-sampler)
//! are a mono ADC on the parallel port: the 8 data lines (D0-D7 = CIA-A port B,
//! `$BFE101`) carry the current sample, which the software reads in a tight,
//! CIA-timer-paced loop to record. The hardware model here -- a straight-binary
//! 8-bit ADC (an ADC0820) on the data lines, mono -- follows the open-amiga-
//! sampler project's schematics (github.com/echolevel/open-amiga-sampler).
//!
//! The emulation approach is an independent Rust implementation following the
//! method in WinUAE's `sampler.cpp` (Toni Wilen; WinUAE is GPL-2.0-or-later,
//! compatible with this GPL-3.0-or-later project): a host capture stream (cpal,
//! mirroring [`crate::audio::CpalSink`]) fills a ring in real time, and each
//! port-B read returns the sample for the elapsed *emulated* time, so the input
//! lines up however fast or slow the Amiga polls. The value is 8-bit
//! offset-binary (128 = silence), as the ADC presents. Host L+R are summed into
//! the single input (these units are mono); [`CpalSampler`] is the live
//! implementation, built only in the `frontend` feature that carries cpal.

#[cfg(feature = "frontend")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "frontend")]
use std::sync::Arc;

#[cfg(feature = "frontend")]
use anyhow::{anyhow, Result};
#[cfg(feature = "frontend")]
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(feature = "frontend")]
use ringbuf::traits::{Consumer, Observer, Producer, Split};
#[cfg(feature = "frontend")]
use ringbuf::HeapRb;

/// Set `COPPERLINE_SAMPLER_DEBUG=1` to log the captured input level (peak
/// amplitude) about once a second -- a CLI VU meter to confirm the host mic is
/// feeding the sampler and to gauge input gain.
#[cfg(feature = "frontend")]
const SAMPLER_DEBUG_ENV: &str = "COPPERLINE_SAMPLER_DEBUG";

/// The largest input gain the sampler preamp will apply (~+24 dB); higher
/// requests are clamped. Past this a real preamp is just clipping anyway.
/// Gain is expressed in decibels, as on a real preamp; +24 dB is ~16x.
pub const MAX_SAMPLER_GAIN_DB: f32 = 24.0;
/// The most the sampler preamp will attenuate (-24 dB is ~1/16x), for taming a
/// hot line input. Lower requests are clamped.
pub const MIN_SAMPLER_GAIN_DB: f32 = -24.0;

/// Convert a preamp gain in decibels to the linear multiplier applied to
/// samples (0 dB = unity).
pub fn gain_db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// A request to attach a parallel-port sampler, carried from the CLI/config
/// through the window so machines launched from the config screen get it too.
#[derive(Clone)]
pub struct SamplerRequest {
    /// Whether a sampler is attached at all.
    pub enabled: bool,
    /// Host capture device; `None` uses the system default.
    pub input_device: Option<String>,
    /// Input gain in decibels, standing in for a real sampler's preamp, applied
    /// before the 8-bit ADC conversion. 0 dB = unity; clamped to
    /// [`MIN_SAMPLER_GAIN_DB`]..[`MAX_SAMPLER_GAIN_DB`].
    pub gain_db: f32,
}

impl Default for SamplerRequest {
    fn default() -> Self {
        Self {
            enabled: false,
            input_device: None,
            gain_db: 0.0,
        }
    }
}

impl SamplerRequest {
    /// Derive the request from resolved `[parallel]` config: enabled only when
    /// the connected device is the sampler.
    pub fn from_config(parallel: &crate::config::ParallelConfig) -> Self {
        Self {
            enabled: parallel.device == crate::config::ParallelDevice::Sampler,
            input_device: parallel.sampler_input.clone(),
            gain_db: parallel.sampler_gain_db,
        }
    }
}

/// Map a normalised sample in roughly [-1.0, 1.0] to the ADC's 8-bit
/// offset-binary output (0 = -full, 128 = silence, 255 = +full), like the
/// straight-binary ADC0820 on a real sampler.
#[cfg(any(feature = "frontend", test))]
fn sample_to_byte(v: f32) -> u8 {
    let scaled = (v.clamp(-1.0, 1.0) * 128.0).round() as i32 + 128;
    scaled.clamp(0, 255) as u8
}

/// Names of the host's audio *input* devices, for `--sampler-list-audio-inputs`
/// and as the base for the GUI picker, with ALSA's plugin handles filtered out
/// (as for outputs). ALSA's "default" is kept here so the CLI can name it; the
/// GUI drops it separately (see [`picker_input_devices`]). Empty if the host
/// cannot enumerate; a hidden entry is still selectable by name.
#[cfg(feature = "frontend")]
pub fn list_input_devices() -> Vec<String> {
    crate::audio::quiet_alsa_probe_logging();
    cpal::default_host()
        .input_devices()
        .map(|devs| {
            devs.filter_map(|d| crate::audio::device_name(&d))
                .filter(|name| !crate::audio::is_alsa_plugin_variant(name))
                .collect()
        })
        .unwrap_or_default()
}

/// Input-device names for the GUI picker (launcher field + runtime menu). Same
/// as [`list_input_devices`], but drops ALSA's "default" when it is the system
/// default input, since the picker already offers a synthetic "Default" (the
/// `None` selection). Re-enumerated on demand, so a device that came online
/// since the screen opened appears. Still selectable by name in the config/CLI.
#[cfg(feature = "frontend")]
pub fn picker_input_devices() -> Vec<String> {
    let default_name = cpal::default_host()
        .default_input_device()
        .and_then(|d| crate::audio::device_name(&d));
    list_input_devices()
        .into_iter()
        .filter(|name| !crate::audio::is_redundant_default(name, default_name.as_deref()))
        .collect()
}

/// Cycle a sampler input selection through "Default" (`None`) then the named
/// devices and back, for the runtime menu.
#[cfg(feature = "frontend")]
pub fn next_input_device(current: Option<&str>, names: &[String], forward: bool) -> Option<String> {
    let here = current
        .and_then(|c| names.iter().position(|n| n == c))
        .map_or(0, |i| i + 1);
    let count = names.len() + 1;
    let next = if forward {
        (here + 1) % count
    } else {
        (here + count - 1) % count
    };
    (next > 0).then(|| names[next - 1].clone())
}

/// The input device to open: the first whose name contains `want`
/// (case-insensitive), otherwise the system default. A named-but-missing device
/// warns and falls back to the default rather than failing to attach.
#[cfg(feature = "frontend")]
fn select_input_device(host: &cpal::Host, want: Option<&str>) -> Result<cpal::Device> {
    if let Some(name) = want {
        let needle = name.to_lowercase();
        let matched = host.input_devices().ok().and_then(|mut devs| {
            devs.find(|d| {
                crate::audio::device_name(d)
                    .map(|n| n.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            })
        });
        match matched {
            Some(device) => return Ok(device),
            None => log::warn!("sampler: no input device matches {name:?}; using the default"),
        }
    }
    host.default_input_device()
        .ok_or_else(|| anyhow!("no default audio input device"))
}

/// The bus-side of a live cpal capture: the ring consumer and the read logic
/// that turns it into 8-bit ADC values. This is `Send` (unlike the cpal stream
/// that feeds it), so it can live inside the `Send` [`crate::parallel::ParallelPort`]
/// slot in the bus; the caller keeps the paired [`cpal::Stream`] alive on the
/// main thread. Build both with [`CpalSampler::open`].
#[cfg(feature = "frontend")]
pub struct CpalSampler {
    consumer: ringbuf::HeapCons<(f32, f32)>,
    /// Host capture sample rate (frames per second).
    capture_rate: f64,
    /// Drop capture backlog beyond this many frames so reads stay near the live
    /// edge (bounded latency), the role of WinUAE's `safediff`.
    max_latency_frames: usize,
    device_label: String,
    /// Emulated time of the previous read, for advancing the read position.
    last_seconds: Option<f64>,
    /// Last summed L+R frame returned, held during underruns and sub-sample reads.
    last_mono: f32,
    /// Count of CIA-A port-B reads routed here since the meter last reported, so
    /// the debug log shows whether the Amiga software is actually polling.
    reads: Arc<AtomicU64>,
}

#[cfg(feature = "frontend")]
impl CpalSampler {
    /// Open the host capture device (`None` = system default) and start feeding
    /// a ring. `gain_db` is the preamp gain in decibels (0 dB = unity), clamped
    /// to the sampler's range and applied before the ADC conversion. Returns the
    /// live cpal input stream -- which the caller must keep alive on the main
    /// thread, since it is `!Send` on some hosts -- paired with the `Send` port
    /// that reads from it and attaches to the bus. Errors if no device/stream can
    /// open.
    pub fn open(input_device: Option<&str>, gain_db: f32) -> Result<(cpal::Stream, Self)> {
        crate::audio::quiet_alsa_probe_logging();
        let clamped_db = gain_db.clamp(MIN_SAMPLER_GAIN_DB, MAX_SAMPLER_GAIN_DB);
        if clamped_db != gain_db {
            log::warn!(
                "sampler: gain {gain_db} dB out of range [{MIN_SAMPLER_GAIN_DB}, \
                 {MAX_SAMPLER_GAIN_DB}] dB; clamping"
            );
        }
        // The capture callback multiplies samples by the linear equivalent.
        let gain = gain_db_to_linear(clamped_db);
        let host = cpal::default_host();
        let device = select_input_device(&host, input_device)?;
        let supported = device
            .default_input_config()
            .map_err(|e| anyhow!("query default input config: {e}"))?;
        let sample_format = supported.sample_format();
        let channels = supported.channels() as usize;
        let capture_rate = supported.sample_rate() as f64;
        let config: cpal::StreamConfig = supported.into();

        // ~2 s ring at the capture rate; we trim backlog on read to stay live.
        let capacity = (capture_rate as usize * 2).max(4096);
        let rb = HeapRb::<(f32, f32)>::new(capacity);
        let (mut producer, consumer) = rb.split();

        // Opt-in input-level meter: log the peak captured amplitude ~once a
        // second so it is obvious whether the host mic is actually feeding the
        // sampler (and to gauge input gain). See `SAMPLER_DEBUG_ENV`.
        let level_debug = crate::envcfg::flag(SAMPLER_DEBUG_ENV);
        let log_every = capture_rate.max(1.0) as u64;
        let reads = Arc::new(AtomicU64::new(0));
        let reads_meter = Arc::clone(&reads);
        let mut peak = 0.0f32;
        let mut counted = 0u64;
        // Shared per-frame handler: meter, then push the (l, r) frame. A mono
        // device mirrors its one channel to both sides; a full ring drops the
        // newest frame (the reader trims to the live edge anyway).
        let mut on_frame = move |l: f32, r: f32| {
            // Apply the preamp gain up front, so the meter and the ring (and thus
            // what the Amiga reads) all reflect the post-gain level.
            let l = l * gain;
            let r = r * gain;
            if level_debug {
                peak = peak.max(l.abs()).max(r.abs());
                counted += 1;
                if counted >= log_every {
                    let prb = reads_meter.swap(0, Ordering::Relaxed);
                    log::info!(
                        "sampler: input level {:.0}% (peak {peak:.3}), port-B reads/s {prb}",
                        peak * 100.0
                    );
                    peak = 0.0;
                    counted = 0;
                }
            }
            let _ = producer.try_push((l, r));
        };

        let err_fn = |err| log::warn!("sampler: cpal input stream error: {err}");

        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    for frame in data.chunks(channels) {
                        let l = frame.first().copied().unwrap_or(0.0);
                        let r = if channels >= 2 { frame[1] } else { l };
                        on_frame(l, r);
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    for frame in data.chunks(channels) {
                        let l = frame.first().copied().unwrap_or(0) as f32 / 32768.0;
                        let r = if channels >= 2 {
                            frame[1] as f32 / 32768.0
                        } else {
                            l
                        };
                        on_frame(l, r);
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    for frame in data.chunks(channels) {
                        let l =
                            (frame.first().copied().unwrap_or(32768) as f32 - 32768.0) / 32768.0;
                        let r = if channels >= 2 {
                            (frame[1] as f32 - 32768.0) / 32768.0
                        } else {
                            l
                        };
                        on_frame(l, r);
                    }
                },
                err_fn,
                None,
            ),
            other => return Err(anyhow!("unsupported sampler input format {other:?}")),
        }
        .map_err(|e| anyhow!("build_input_stream: {e}"))?;
        stream
            .play()
            .map_err(|e| anyhow!("input stream play: {e}"))?;

        let device_label = crate::audio::device_name(&device).unwrap_or_else(|| "<unknown>".into());
        log::info!(
            "sampler: parallel-port sampler ready, device={device_label:?}, \
             capture_rate={capture_rate}, format={sample_format:?}"
        );

        Ok((
            stream,
            Self {
                consumer,
                capture_rate,
                // ~50 ms of bounded latency.
                max_latency_frames: (capture_rate * 0.05) as usize,
                device_label,
                last_seconds: None,
                last_mono: 0.0,
                reads,
            },
        ))
    }

    /// The host capture device name, for logging and the UI.
    pub fn device_label(&self) -> &str {
        &self.device_label
    }

    /// The current 8-bit offset-binary ADC value (128 = silence), advanced
    /// through the live capture by the emulated time since the previous read.
    /// A mono digitizer: the host L+R are summed into the single ADC input.
    fn next_byte(&mut self, emu_seconds: f64) -> u8 {
        self.reads.fetch_add(1, Ordering::Relaxed);
        // Trim backlog so reads track the live edge instead of a growing delay.
        let backlog = self.consumer.occupied_len();
        if backlog > self.max_latency_frames {
            self.consumer.skip(backlog - self.max_latency_frames);
        }

        // Advance the read position by the emulated time since the last read
        // (WinUAE sampler.cpp's approach): faster-than-capture polling repeats a
        // frame (advance 0), slower polling averages the frames it skips over --
        // cheap anti-aliasing.
        let advance = match self.last_seconds {
            Some(prev) => ((emu_seconds - prev).max(0.0) * self.capture_rate).round() as usize,
            None => 1,
        };
        self.last_seconds = Some(emu_seconds);

        if advance > 0 {
            // Sum L+R into the mono input a single-input sampler sees.
            let mut sum = 0.0f32;
            let mut n = 0usize;
            for _ in 0..advance {
                match self.consumer.try_pop() {
                    Some((l, r)) => {
                        sum += 0.5 * (l + r);
                        n += 1;
                    }
                    None => break,
                }
            }
            if n > 0 {
                self.last_mono = sum / n as f32;
            }
        }

        sample_to_byte(self.last_mono)
    }
}

#[cfg(feature = "frontend")]
impl crate::parallel::ParallelPort for CpalSampler {
    /// A sampler is input-only: it digitizes the data lines and never accepts a
    /// strobed output byte, so it drives no `/ACK`.
    fn strobe(&mut self, _data: u8, _at_cck: u64) -> bool {
        false
    }

    /// A port-B read returns the digitized sample. The ADC always drives the
    /// data lines, so this is never `None`. `at_cck` is converted to emulated
    /// seconds against Paula's colour clock to advance the live capture.
    fn read_data(&mut self, at_cck: u64) -> Option<u8> {
        let emu_seconds = at_cck as f64 / crate::chipset::paula::PAULA_CLOCK_HZ as f64;
        Some(self.next_byte(emu_seconds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_binary_conversion_centres_silence() {
        assert_eq!(sample_to_byte(0.0), 128);
        // Full scale hits the rails: -full = 0, +full clamps to 255.
        assert_eq!(sample_to_byte(-1.0), 0);
        assert_eq!(sample_to_byte(1.0), 255);
        // Clamps beyond full-scale rather than wrapping.
        assert_eq!(sample_to_byte(2.0), 255);
        assert_eq!(sample_to_byte(-2.0), 0);
    }
}
