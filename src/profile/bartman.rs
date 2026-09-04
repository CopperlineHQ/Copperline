// SPDX-License-Identifier: GPL-3.0-or-later

//! Binary interchange with vscode-amiga-debug's `ProfileFile` (v1.8.3).
//! The mixed-endian layout and fixed PAL grid are an upstream file contract,
//! not a machine timing model. Native captures retain their actual geometry.

use std::io::{BufWriter, Write};
use std::path::PathBuf;

use super::samples::CompactUnwindTable;
use crate::emulator::Emulator;
use anyhow::{bail, Context, Result};

#[derive(Debug)]
pub struct Request {
    pub frames: u32,
    pub unwind: Option<PathBuf>,
    pub out: PathBuf,
}

impl Request {
    pub fn parse(command: &str) -> Result<Self> {
        // GDB monitor paths are double quoted; preserve backslashes in Windows
        // paths, accepting only an escaped quote or backslash as escapes.
        let mut args = Vec::new();
        let mut chars = command.chars().peekable();
        while chars.peek().is_some() {
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            if chars.peek().is_none() {
                break;
            }
            let quoted = chars.peek() == Some(&'"');
            if quoted {
                chars.next();
            }
            let mut arg = String::new();
            let mut closed = !quoted;
            while let Some(c) = chars.next() {
                if quoted && c == '"' {
                    closed = true;
                    break;
                }
                if !quoted && c.is_whitespace() {
                    break;
                }
                if quoted && c == '\\' && chars.peek().is_some_and(|c| matches!(c, '"' | '\\')) {
                    arg.push(chars.next().unwrap());
                } else {
                    arg.push(c);
                }
            }
            if !closed {
                bail!("unterminated quoted profile path");
            }
            args.push(arg);
        }
        if args.len() != 4 || args[0] != "profile" {
            bail!("usage: monitor profile N \"unwind\" \"out\"");
        }
        let frames: u32 = args[1].parse().context("profile frame count")?;
        if !(1..=100).contains(&frames) {
            bail!("Bartman capture requires 1..100 frames");
        }
        if args[3].is_empty() {
            bail!("profile output path is empty");
        }
        Ok(Self {
            frames,
            unwind: (!args[2].is_empty()).then(|| PathBuf::from(&args[2])),
            out: PathBuf::from(&args[3]),
        })
    }
}

fn u32le(out: &mut impl Write, value: u32) -> Result<()> {
    Ok(out.write_all(&value.to_le_bytes())?)
}
fn blob(out: &mut impl Write, bytes: &[u8]) -> Result<()> {
    u32le(out, bytes.len().try_into()?)?;
    Ok(out.write_all(bytes)?)
}

/// Run a bounded capture, delivering progress before each frame. On every
/// exit path the sampler and analyzer return to their original ownership.
/// Only a completed file is renamed onto the requested output.
pub fn capture(
    emu: &mut Emulator,
    request: &Request,
    mut progress: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    if !(1..=100).contains(&request.frames) {
        bail!("Bartman capture requires 1..100 frames");
    }
    if emu.profile_active() {
        bail!("a native profile capture is already running");
    }
    if emu.bus().agnus.video_standard() != crate::chipset::agnus::VideoStandard::Pal
        || emu.bus().agnus.current_line_cck() != 227
        || emu.bus().agnus.current_frame_lines() > 313
    {
        bail!("Bartman's viewer requires a 227 by 313 grid; use native profile.start for this geometry");
    }
    let segs = crate::amigaos::segments_on_bus(emu.bus()).unwrap_or_default();
    let unwind = request
        .unwind
        .as_ref()
        .map(|path| -> Result<_> {
            let base = segs
                .first()
                .context("an unwind table requires a loaded program")?
                .start;
            let bytes = std::fs::read(path).context("reading compact unwind table")?;
            CompactUnwindTable::decode(base, &bytes).map_err(anyhow::Error::msg)
        })
        .transpose()?;
    crate::paths::ensure_parent(&request.out)?;
    let temp = request
        .out
        .with_extension(format!("partial-{}", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let analyzer = emu.bus().frame_analyzer_enabled();
    let full = emu.bus().frame_analyzer_full();
    let result = (|| {
        let mut out = BufWriter::new(file);
        // Finish the interrupted frame before taking the replay RAM snapshot.
        advance_frame(emu)?;
        u32le(&mut out, request.frames)?;
        u32le(&mut out, segs.len() as u32)?;
        for seg in &segs {
            u32le(&mut out, seg.start)?;
        }
        let bounds = crate::amigaos::stack_bounds_on_bus(emu.bus());
        for value in bounds
            .map(|b| [b.system_lower, b.system_upper, b.task_lower, b.task_upper])
            .unwrap_or([0; 4])
        {
            u32le(&mut out, value)?;
        }
        blob(&mut out, &emu.bus().mem.rom)?;
        blob(&mut out, &emu.bus().mem.chip_ram)?;
        blob(&mut out, &emu.bus().mem.slow_ram)?;
        u32le(&mut out, 28_375_160)?; // PAL master oscillator, Hz
        u32le(&mut out, 512)?; // samples are in colour clocks
        emu.bus_mut().set_frame_analyzer_enabled(true);
        emu.bus_mut().set_frame_analyzer_full(true);
        emu.bus_mut().restart_frame_analyzer_trace();
        emu.machine.start_profile_samples(
            unwind,
            segs.iter().map(|s| (s.start, s.size)).collect(),
            true,
        );
        for frame in 0..request.frames {
            progress(&format!("PRF: {}/{}\n", frame + 1, request.frames))?;
            let start = emu.bus().emulated_cck();
            advance_frame(emu)?;
            let samples = emu.machine.take_profile_samples();
            let bus = emu.bus();
            let trace = bus
                .frame_bus_trace()
                .context("missing completed DMA trace")?;
            if trace.cols != 227
                || trace.rows > 313
                || bus.agnus.video_standard() != crate::chipset::agnus::VideoStandard::Pal
            {
                bail!("video geometry changed during Bartman capture; use native profile.start");
            }
            let snap = &trace.registers;
            let flags = if snap.chipset_flags & 1 != 0 {
                7u32
            } else if snap.chipset_flags & 2 != 0 {
                3
            } else {
                0
            };
            u32le(&mut out, 520)?;
            out.write_all(&flags.to_be_bytes())?;
            for word in &snap.custom {
                out.write_all(&word.to_be_bytes())?;
            }
            out.write_all(&[0; 4])?; // REFPTR extension
            u32le(&mut out, if flags & 4 != 0 { 1024 } else { 0 })?;
            if flags & 4 != 0 {
                for (&hi, &lo) in snap.palette_hi.iter().zip(&snap.palette_lo) {
                    let rgb = (u32::from(hi & 0xf00) << 12)
                        | (u32::from(lo & 0xf00) << 8)
                        | (u32::from(hi & 0xf0) << 8)
                        | (u32::from(lo & 0xf0) << 4)
                        | (u32::from(hi & 0xf) << 4)
                        | u32::from(lo & 0xf);
                    out.write_all(&rgb.to_be_bytes())?;
                }
            }
            u32le(&mut out, 58)?;
            u32le(&mut out, 227 * 313)?;
            let records = trace.records().context("missing full DMA records")?;
            for y in 0..313 {
                for x in 0..227 {
                    let slot = if y < trace.rows {
                        records.get(y * trace.cols + x).copied().unwrap_or_default()
                    } else {
                        Default::default()
                    };
                    out.write_all(&dma_record(slot))?;
                }
            }
            let resources = bus.uaelib.as_ref().map(|u| u.resources()).unwrap_or(&[]);
            u32le(&mut out, 52)?;
            u32le(&mut out, resources.len() as u32)?;
            for resource in resources {
                out.write_all(&resource_record(resource))?;
            }
            u32le(&mut out, (bus.emulated_cck() - start).try_into()?)?;
            u32le(
                &mut out,
                bus.uaelib
                    .as_ref()
                    .and_then(|u| u.idle().last_frame())
                    .map_or(0, |(idle, _)| idle as u32),
            )?;
            let mut words = Vec::new();
            for sample in &samples {
                let mut remaining = sample.total_cck.max(1);
                while remaining > 0 {
                    let count = remaining.min(65535);
                    words.extend_from_slice(&sample.callstack[..sample.callstack_depth]);
                    words.push(u32::MAX - count);
                    words.extend_from_slice(&sample.registers.unwrap_or([0; 17]));
                    remaining -= count;
                }
            }
            u32le(&mut out, words.len().try_into()?)?;
            for word in words {
                u32le(&mut out, word)?;
            }
            let (fb, height, width) = crate::control::exec::render_frame(emu);
            let mut png = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut png, width as u32, height as u32);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let bytes: Vec<_> = fb[..width * height]
                    .iter()
                    .flat_map(|p| p.to_ne_bytes())
                    .collect();
                encoder.write_header()?.write_image_data(&bytes)?;
            }
            u32le(&mut out, png.len().try_into()?)?;
            u32le(&mut out, 1)?;
            out.write_all(&png)?;
        }
        out.flush()?;
        drop(out);
        std::fs::rename(&temp, &request.out)?;
        Ok(())
    })();
    emu.machine.stop_profile_samples();
    emu.bus_mut().set_frame_analyzer_full(full);
    emu.bus_mut().set_frame_analyzer_enabled(analyzer);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn advance_frame(emu: &mut Emulator) -> Result<()> {
    let frame = emu.bus().emulated_frames();
    let mut idle = false;
    emu.machine.take_ui_debug_stop();
    while emu.bus().emulated_frames() == frame {
        emu.debug_step_for_gdb(&mut idle)?;
        if emu.machine.cpu_double_faulted() {
            bail!("CPU double fault during profile capture");
        }
        if let Some(stop) = emu.machine.take_ui_debug_stop() {
            bail!("profile interrupted: {stop:?}");
        }
    }
    Ok(())
}

fn dma_record(slot: crate::bus::BusSlotRecord) -> [u8; 58] {
    let mut bytes = [0; 58];
    bytes[0..2].copy_from_slice(&slot.reg.to_le_bytes());
    bytes[2..10].copy_from_slice(&slot.data.to_le_bytes());
    bytes[10..12].copy_from_slice(&u16::from(slot.size).to_le_bytes());
    bytes[12..16].copy_from_slice(&slot.addr.to_le_bytes());
    bytes[16..20].copy_from_slice(&slot.events.to_le_bytes());
    bytes[29..31].copy_from_slice(&i16::from(slot.kind).to_le_bytes());
    bytes[31..33].copy_from_slice(&u16::from(slot.subtype).to_le_bytes());
    bytes[33] = slot.ipl;
    bytes[34] = slot.ipl;
    bytes[35] = slot.ipl;
    if slot.flags & 2 != 0 {
        let cia = (slot.flags >> 2) & 1;
        let reg = u32::from((slot.flags >> 8) & 15);
        bytes[42..46].copy_from_slice(&reg.to_le_bytes());
        bytes[46..50].copy_from_slice(&(1u32 << cia).to_le_bytes());
        bytes[50] = if slot.flags & 1 != 0 { 1 } else { 0 };
        // WinUAE marks the completed access with phase -1. Positive
        // phases denote its internal E-clock wait pipeline.
        bytes[51..55].copy_from_slice(&(-1i32).to_le_bytes());
        bytes[55..57].copy_from_slice(&(slot.data as u16).to_le_bytes());
    }
    bytes
}

fn resource_record(resource: &crate::uaelib::DebugResource) -> [u8; 52] {
    use crate::uaelib::ResourceKind;
    let mut out = [0; 52];
    out[0..4].copy_from_slice(&resource.address.to_le_bytes());
    out[4..8].copy_from_slice(&resource.size.to_le_bytes());
    let name = resource.name.as_bytes();
    let len = name.len().min(31);
    out[8..8 + len].copy_from_slice(&name[..len]);
    let (kind, dimensions) = match resource.kind {
        ResourceKind::Bitmap {
            width,
            height,
            planes,
        } => (0, [width, height, planes]),
        ResourceKind::Palette { entries } => (1, [entries, 0, 0]),
        ResourceKind::Copperlist => (2, [0; 3]),
        ResourceKind::Unknown(kind) => (kind, [0; 3]),
    };
    out[40..42].copy_from_slice(&kind.to_le_bytes());
    out[42..44].copy_from_slice(&resource.flags.to_le_bytes());
    for (i, value) in dimensions.iter().enumerate() {
        out[44 + i * 2..46 + i * 2].copy_from_slice(&value.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_monitor_paths_and_limits() {
        let request =
            Request::parse(r#"profile 2 "C:\my project\test.unwind" "/tmp/my capture""#).unwrap();
        assert_eq!(
            request.unwind.unwrap().to_str().unwrap(),
            r"C:\my project\test.unwind"
        );
        assert_eq!(request.out.to_str().unwrap(), "/tmp/my capture");
        assert!(Request::parse("profile 0 x y").is_err());
        assert!(Request::parse("profile 101 x y").is_err());
        assert!(Request::parse("profile 1 \"unclosed").is_err());
    }
    #[test]
    fn dma_wire_offsets_preserve_wide_data_and_cia() {
        let bytes = dma_record(crate::bus::BusSlotRecord {
            reg: 0x1000,
            addr: 0xbfd200,
            data: 0x1122334455667788,
            events: 0x40000000,
            kind: 2,
            subtype: 1,
            size: 8,
            ipl: 3,
            flags: 2 | 4 | 1 | (2 << 8) | (3 << 12),
        });
        assert_eq!(&bytes[2..10], &0x1122334455667788u64.to_le_bytes());
        assert_eq!(&bytes[42..46], &2u32.to_le_bytes());
        assert_eq!(&bytes[46..50], &2u32.to_le_bytes());
        assert_eq!(&bytes[51..55], &(-1i32).to_le_bytes());
        assert_eq!(&bytes[55..57], &0x7788u16.to_le_bytes());
    }
    #[cfg(feature = "gdb")]
    #[test]
    fn binary_capture_has_complete_frames_and_restores_instrumentation() {
        let mut emu = crate::gdbstub::testkit::emulator_with_loadseg_program();
        let path = std::env::temp_dir().join(format!(
            "bartman-wire-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let request = Request {
            frames: 2,
            unwind: None,
            out: path.clone(),
        };
        let mut progress = Vec::new();
        capture(&mut emu, &request, |line| {
            progress.push(line.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(progress, ["PRF: 1/2\n", "PRF: 2/2\n"]);
        assert!(!emu.bus().frame_analyzer_enabled());
        let bytes = std::fs::read(&path).unwrap();
        let mut pos = 0;
        let word = |pos: &mut usize| {
            let value = u32::from_le_bytes(bytes[*pos..*pos + 4].try_into().unwrap());
            *pos += 4;
            value as usize
        };
        assert_eq!(word(&mut pos), 2);
        let sections = word(&mut pos);
        pos += sections * 4 + 16;
        for _ in 0..3 {
            let len = word(&mut pos);
            pos += len;
        }
        pos += 8;
        for _ in 0..2 {
            assert_eq!(word(&mut pos), 520);
            pos += 520;
            let aga = word(&mut pos);
            pos += aga;
            assert_eq!(word(&mut pos), 58);
            assert_eq!(word(&mut pos), 227 * 313);
            pos += 58 * 227 * 313;
            assert_eq!(word(&mut pos), 52);
            let resources = word(&mut pos);
            pos += resources * 52;
            assert!(word(&mut pos) > 70000);
            pos += 4;
            let samples = word(&mut pos);
            assert!(samples > 100);
            pos += samples * 4;
            let screenshot = word(&mut pos);
            assert_eq!(word(&mut pos), 1);
            assert_eq!(&bytes[pos..pos + 8], b"\x89PNG\r\n\x1a\n");
            pos += screenshot;
        }
        assert_eq!(pos, bytes.len());
        std::fs::remove_file(path).unwrap();
    }
}
