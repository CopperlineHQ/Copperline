// SPDX-License-Identifier: GPL-3.0-or-later

//! Host sound effects: the sounds of the host's own furniture rather than
//! of the emulated machine (a video-slot card's picture monitors powering
//! on and off, their buttons). They are mixed into the live output after
//! the machine's sound, in the output callback, so they play at once
//! whatever the live lead, and they never reach the machine: not a WAV
//! capture, not a recording, not a save state. With no live output (a
//! headless run, `--noaudio`) they are dropped.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The rate effects are made at; the mixer steps through them at the
/// output's own rate.
pub const FX_RATE: u32 = 48_000;

/// At most this many effects at once; more are dropped.
const MAX_VOICES: usize = 16;

struct Voice {
    samples: Arc<[f32]>,
    /// Position in `samples`, negative while the effect waits to start.
    pos: f64,
    gain: f32,
}

static VOICES: Mutex<Vec<Voice>> = Mutex::new(Vec::new());
/// Live outputs running (a reopened device's new sink can start before
/// the old one is dropped).
static LIVE: AtomicUsize = AtomicUsize::new(0);

/// Play `samples` (mono, at [`FX_RATE`]) `delay` seconds from now, at
/// `gain`.
pub fn play(samples: Arc<[f32]>, delay: f32, gain: f32) {
    if LIVE.load(Ordering::Relaxed) == 0 || samples.is_empty() {
        return;
    }
    let mut voices = VOICES.lock().unwrap_or_else(|e| e.into_inner());
    if voices.len() < MAX_VOICES {
        voices.push(Voice {
            samples,
            pos: -f64::from(delay.max(0.0)) * f64::from(FX_RATE),
            gain,
        });
    }
}

/// A live output started (`true`) or stopped.
pub(crate) fn set_live(live: bool) {
    if live {
        LIVE.fetch_add(1, Ordering::Relaxed);
    } else if LIVE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)) == Ok(1)
    {
        VOICES.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// Add the playing effects to an interleaved output buffer of `chans`
/// channels at `rate`. Called from the output callback: it never waits for
/// the lock, it skips a buffer instead.
pub(crate) fn mix(data: &mut [f32], chans: usize, rate: u32) {
    let Ok(mut voices) = VOICES.try_lock() else {
        return;
    };
    if voices.is_empty() || chans == 0 {
        return;
    }
    let step = f64::from(FX_RATE) / f64::from(rate.max(1));
    for frame in data.chunks_mut(chans) {
        let mut sum = 0.0f32;
        for v in voices.iter_mut() {
            if v.pos >= 0.0 {
                let i = v.pos as usize;
                if i + 1 < v.samples.len() {
                    let t = (v.pos - i as f64) as f32;
                    sum += (v.samples[i] * (1.0 - t) + v.samples[i + 1] * t) * v.gain;
                }
            }
            v.pos += step;
        }
        for s in frame.iter_mut().take(chans.min(2)) {
            *s = (*s + sum).clamp(-1.0, 1.0);
        }
    }
    voices.retain(|v| (v.pos as usize) + 1 < v.samples.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_are_mixed_after_their_delay_and_then_dropped() {
        set_live(true);
        play(Arc::from(vec![0.5f32; 10]), 4.0 / FX_RATE as f32, 1.0);
        let mut out = vec![0.0f32; 2 * 20];
        mix(&mut out, 2, FX_RATE);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[2 * 4], 0.5);
        assert_eq!(out[2 * 4 + 1], 0.5);
        assert!(VOICES.lock().unwrap().is_empty());
        set_live(false);
        play(Arc::from(vec![0.5f32; 10]), 0.0, 1.0);
        assert!(VOICES.lock().unwrap().is_empty());
    }
}
