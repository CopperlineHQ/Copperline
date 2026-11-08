// SPDX-License-Identifier: GPL-3.0-or-later

//! Window-system notification, the queue flush for a skipped present, and
//! optional host presentation timings.

use pixels::{wgpu, Pixels, PixelsContext};
use std::time::Instant;
use winit::window::Window;

type DrawResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

pub(super) struct Presenter {
    profile: bool,
    previous: Option<Instant>,
}

impl Presenter {
    pub(super) fn new() -> Self {
        Self {
            profile: crate::envcfg::flag("COPPERLINE_PRESENT_PROFILE"),
            previous: None,
        }
    }

    pub(super) fn render<F>(
        &mut self,
        pixels: &Pixels<'_>,
        window: Option<&Window>,
        emulated_frame: Option<u64>,
        draw: F,
    ) -> Result<(), pixels::Error>
    where
        F: FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView, &PixelsContext) -> DrawResult,
    {
        let started = self.profile.then(Instant::now);
        let mut acquired = None;
        let mut drawn = None;
        let mut submitting = false;
        let result = pixels.render_with(|encoder, target, context| {
            acquired = self.profile.then(Instant::now);
            draw(encoder, target, context)?;
            // pixels submits and presents immediately after this callback.
            // Notify only once a surface was acquired and drawing succeeded:
            // a timeout/occluded surface must not arm a callback for a buffer
            // that will never be committed. On Wayland this lets winit align
            // subsequent redraws with compositor frame callbacks.
            if let Some(window) = window {
                window.pre_present_notify();
            }
            drawn = self.profile.then(Instant::now);
            submitting = true;
            Ok(())
        });
        if !submitting {
            // Nothing was submitted: pixels skips a timed-out or occluded
            // surface (macOS reports a covered, minimised or other-Space
            // window that way) with Ok(()), and a failed acquire or draw
            // never reaches its submit. Queue writes made since the last
            // submit stay staged until the next one, so flush them here;
            // otherwise anything uploaded outside the render callback
            // piles up for as long as the window stays hidden.
            pixels.queue().submit(std::iter::empty());
        }
        if let Some(started) = started {
            let finished = Instant::now();
            let interval_ms = self
                .previous
                .replace(started)
                .map_or(0.0, |previous| millis(started, previous));
            let acquire_upload_ms = acquired.map_or(-1.0, |at| millis(at, started));
            let draw_ms = acquired.zip(drawn).map_or(-1.0, |(a, d)| millis(d, a));
            let submit_present_ms = drawn.map_or(-1.0, |at| millis(finished, at));
            log::info!(
                "window frame: thread={} emulated_frame={emulated_frame:?} mode={:?} submitted={} interval_ms={interval_ms:.3} acquire_upload_ms={acquire_upload_ms:.3} draw_ms={draw_ms:.3} submit_present_ms={submit_present_ms:.3} total_ms={:.3}",
                std::thread::current().name().unwrap_or("?"),
                pixels.present_mode(),
                submitting && result.is_ok(),
                millis(finished, started),
            );
        }
        result
    }
}

fn millis(later: Instant, earlier: Instant) -> f64 {
    later.duration_since(earlier).as_secs_f64() * 1000.0
}
