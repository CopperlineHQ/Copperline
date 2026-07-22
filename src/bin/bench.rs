// SPDX-License-Identifier: GPL-3.0-or-later

//! Headless core benchmark (`copperline-bench`, behind the `bench-bin`
//! feature): step the deterministic core unpaced for N emulated seconds and
//! report wall-clock frame times. Builds for native targets and for
//! wasm32-wasip1, where it measures what a browser build can sustain; with
//! `--render` each completed frame also runs the full presentation pipeline
//! (bitplane render, post-process, deinterlace) the way an interactive
//! frontend would, so the numbers include the per-frame render cost.
//!
//! Deliberately frontend-free: no winit/cpal/env_logger. Log output from the
//! core goes to stdout through a minimal logger so ROM/config banners stay
//! visible under WASI runtimes.

use anyhow::{anyhow, Context, Result};
use copperline::audio::NullSink;
use copperline::config::{Config, ConfigOverrides, Overscan};
use copperline::emulator::build_machine;
use copperline::timebase::Instant;
use copperline::video::deinterlace::Deinterlacer;
use copperline::video::{bitplane, present_common, FB_WIDTH, MAX_FB_PIXELS};
use std::path::PathBuf;

struct StdoutLogger;

impl log::Log for StdoutLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            println!("[{}] {}", record.level(), record.args());
        }
    }
    fn flush(&self) {}
}

static LOGGER: StdoutLogger = StdoutLogger;

struct Args {
    rom: Option<PathBuf>,
    ext: Option<PathBuf>,
    df0: Option<PathBuf>,
    config: Option<PathBuf>,
    seconds: f64,
    render: bool,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        rom: None,
        ext: None,
        df0: None,
        config: None,
        seconds: 30.0,
        render: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let path_arg = |it: &mut dyn Iterator<Item = String>| -> Result<PathBuf> {
            it.next()
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--rom" => args.rom = Some(path_arg(&mut it)?),
            "--ext" => args.ext = Some(path_arg(&mut it)?),
            "--df0" => args.df0 = Some(path_arg(&mut it)?),
            "--config" => args.config = Some(path_arg(&mut it)?),
            "--seconds" => {
                args.seconds = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow!("--seconds needs a number"))?;
            }
            "--render" => args.render = true,
            other => {
                return Err(anyhow!(
                    "unknown argument {other}; usage: copperline-bench [--config P] [--rom P] \
                     [--ext P] [--df0 P] [--seconds F] [--render]"
                ));
            }
        }
    }
    Ok(args)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn main() -> Result<()> {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);

    let args = parse_args()?;
    let raw = Config::load_raw(args.config.as_deref(), &ConfigOverrides::default())?;
    let mut cfg: Config = raw.try_into()?;
    if let Some(rom) = &args.rom {
        cfg.rom_path = rom.clone();
        // A ROM given explicitly replaces the whole ROM setup; keep whatever
        // extended ROM was requested on the command line only.
        cfg.extended_rom_path = None;
    }
    if let Some(ext) = &args.ext {
        cfg.extended_rom_path = Some(ext.clone());
    }

    let mut emu = build_machine(&cfg, Box::new(NullSink), false, false)?;
    if let Some(df0) = &args.df0 {
        emu.bus_mut()
            .floppy
            .insert_disk_image(0, df0.clone(), true)
            .with_context(|| format!("inserting {}", df0.display()))?;
    }

    emu.set_paced(false);
    emu.reset_stats();

    let mut fb = vec![0u32; MAX_FB_PIXELS];
    let mut deinterlacer = Deinterlacer::new();
    let mut last_rendered: Option<u64> = None;
    let mut rendered_frames: u64 = 0;

    let start_emulated = emu.bus().emulated_seconds();
    let target = start_emulated + args.seconds;
    let start_frames = emu.bus().emulated_frames();
    let started = Instant::now();
    let mut frame_times: Vec<f64> = Vec::new();

    while emu.bus().emulated_seconds() < target {
        let frame_started = Instant::now();
        emu.step_frame()?;
        if args.render && emu.bus().frame_render_available() {
            let emulated_frame = emu.bus().emulated_frames();
            if present_common::should_render_emulated_frame(last_rendered, emulated_frame) {
                let visible_start_vpos = emu.bus().frame_visible_start_vpos();
                bitplane::render(emu.bus_mut(), &mut fb);
                let geometry = emu.bus().frame_geometry();
                let field_rows = present_common::post_process_rendered_field(
                    &mut fb,
                    geometry,
                    emu.bus().frame_presentation_h_window(),
                    emu.bus().frame_presentation_v_window(),
                    visible_start_vpos,
                    0,
                    Overscan::Tv,
                );
                let base = emu.bus().frame_render_base();
                deinterlacer.push_field(
                    &fb,
                    field_rows,
                    base.bplcon0 & 0x0004 != 0,
                    base.long_field,
                    !geometry.programmable,
                );
                last_rendered = Some(emulated_frame);
                rendered_frames += 1;
            }
        }
        frame_times.push(frame_started.elapsed().as_secs_f64() * 1_000.0);
    }

    let elapsed = started.elapsed().as_secs_f64();
    let frames = emu.bus().emulated_frames().saturating_sub(start_frames);
    let emulated = emu.bus().emulated_seconds() - start_emulated;

    let mut sorted = frame_times.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = sorted.iter().sum::<f64>() / sorted.len().max(1) as f64;
    let over_budget = sorted.iter().filter(|&&t| t > 20.0).count();

    println!(
        "bench: {:.3}s emulated in {:.3}s wall, {} frames ({:.1}/s), realtime x{:.2}{}",
        emulated,
        elapsed,
        frames,
        frames as f64 / elapsed.max(f64::EPSILON),
        emulated / elapsed.max(f64::EPSILON),
        if args.render {
            format!(", {rendered_frames} rendered")
        } else {
            String::new()
        }
    );
    println!(
        "bench frame ms: mean={:.2} p50={:.2} p90={:.2} p95={:.2} p99={:.2} max={:.2}",
        mean,
        percentile(&sorted, 0.50),
        percentile(&sorted, 0.90),
        percentile(&sorted, 0.95),
        percentile(&sorted, 0.99),
        sorted.last().copied().unwrap_or(0.0),
    );
    println!(
        "bench budget: {} of {} frames over the 20ms PAL budget",
        over_budget,
        sorted.len()
    );
    // Keep the deinterlacer's output observable so the render path cannot be
    // optimized out entirely.
    if args.render {
        let checksum: u64 = deinterlacer.output()[..FB_WIDTH]
            .iter()
            .map(|&px| px as u64)
            .sum();
        println!("bench render checksum: {checksum:#x}");
    }
    Ok(())
}
