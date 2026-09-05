// SPDX-License-Identifier: GPL-3.0-or-later

//! WinUAE ASF/USS interchange. Field order follows WinUAE savestate.cpp,
//! newcpu.cpp, custom.cpp, cia.cpp and audio.cpp (GPL-2.0-or-later).
//! Import reconstructs a hardware boundary, not WinUAE's internal pipeline.

use crate::{config, emulator::Emulator};
use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::Path;

const MAX_CHUNK: usize = 128 * 1024 * 1024;
const MAX_TOTAL: usize = 256 * 1024 * 1024;

#[derive(Debug)]
struct Chunk {
    name: [u8; 4],
    bytes: Vec<u8>,
}

#[derive(Debug)]
pub struct UssFile {
    chunks: Vec<Chunk>,
    pub cpu: CpuState,
    pub custom: [u16; 256],
    pub chipset_flags: u32,
    pub rom_size: usize,
    pub rom_crc: u32,
    pub warnings: Vec<String>,
}

#[derive(Debug, Default)]
pub struct CpuState {
    pub model: u32,
    pub address_24: bool,
    pub registers: [u32; 15],
    pub pc: u32,
    pub usp: u32,
    pub isp: u32,
    pub sr: u16,
    pub stopped: bool,
    pub dfc: u32,
    pub sfc: u32,
    pub vbr: u32,
    pub caar: u32,
    pub cacr: u32,
    pub msp: u32,
}

pub(crate) fn be16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
pub(crate) fn be32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

impl UssFile {
    pub fn read(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take((MAX_TOTAL + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_TOTAL {
            bail!("USS exceeds 256 MiB input limit");
        }
        Self::parse(&bytes).with_context(|| format!("importing {}", path.display()))
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut chunks = Vec::new();
        let mut pos = 0usize;
        let mut total = 0usize;
        let mut ended = false;
        while pos < bytes.len() {
            if bytes.len() - pos < 8 {
                bail!("truncated USS chunk header at {pos}");
            }
            let name: [u8; 4] = bytes[pos..pos + 4].try_into()?;
            let length = be32(bytes, pos + 4) as usize;
            if name == *b"END " {
                if length != 8 {
                    bail!("invalid USS END chunk");
                }
                ended = true;
                break;
            }
            if length < 12 || length > bytes.len() - pos {
                bail!("invalid USS chunk length at {pos}");
            }
            if chunks.is_empty() && name != *b"ASF " {
                bail!("not an AmigaStateFile (missing ASF header)");
            }
            let flags = be32(bytes, pos + 8);
            // WinUAE sets bit 1 on ordinary chunks; bit 0 denotes zlib.
            if flags & !3 != 0 {
                bail!("unsupported USS chunk flags {flags:#x}");
            }
            let body = &bytes[pos + 12..pos + length];
            let data = if flags & 1 != 0 {
                if body.len() < 4 {
                    bail!("truncated compressed USS chunk");
                }
                let expected = be32(body, 0) as usize;
                if expected > MAX_CHUNK || total + expected > MAX_TOTAL {
                    bail!("USS decompressed size exceeds limit");
                }
                let mut output = Vec::new();
                let mut decoder = flate2::read::ZlibDecoder::new(&body[4..]);
                (&mut decoder)
                    .take(expected as u64 + 1)
                    .read_to_end(&mut output)?;
                if output.len() != expected || decoder.total_in() as usize != body.len() - 4 {
                    bail!("USS compressed length mismatch");
                }
                output
            } else {
                if body.len() > MAX_CHUNK || total + body.len() > MAX_TOTAL {
                    bail!("USS decompressed size exceeds limit");
                }
                body.to_vec()
            };
            total += data.len();
            if chunks.len() >= 4096 {
                bail!("too many USS chunks");
            }
            chunks.push(Chunk { name, bytes: data });
            // Even an aligned payload has FOUR padding bytes. The compressed
            // length prefix is aligned itself and does not change this rule.
            let pad = 4 - (body.len() & 3);
            pos += length;
            if bytes.len() - pos < pad {
                bail!("truncated USS chunk padding");
            }
            pos += pad;
        }
        if !ended {
            bail!("USS has no END chunk");
        }
        let get = |name: &[u8; 4]| -> Result<&[u8]> {
            let mut matches = chunks.iter().filter(|c| &c.name == name);
            let found = matches
                .next()
                .with_context(|| format!("missing USS {} chunk", String::from_utf8_lossy(name)))?;
            if matches.next().is_some() {
                bail!("duplicate USS {} chunk", String::from_utf8_lossy(name));
            }
            Ok(&found.bytes)
        };
        let cpu = parse_cpu(get(b"CPU ")?)?;
        let chip = get(b"CHIP")?;
        let mut custom = [0; 256];
        match chip.len() {
            356 | 360 => {
                let mut offset = 4;
                for (index, word) in custom.iter_mut().enumerate() {
                    if (0xa0..0xe0).contains(&(index * 2)) || (0x120..0x180).contains(&(index * 2))
                    {
                        continue;
                    }
                    *word = be16(chip, offset);
                    offset += 2;
                }
            }
            516 | 520 => {
                for (index, word) in custom.iter_mut().enumerate() {
                    *word = be16(chip, 4 + index * 2);
                }
            }
            len => bail!("unsupported USS CHIP length {len}"),
        }
        let chipset_flags = be32(chip, 0);
        if chunks.iter().any(|chunk| chunk.name == *b"CHPX") && get(b"CHPX")?.len() < 4 {
            bail!("truncated USS CHPX overlay flags");
        }
        for name in [b"CIAA", b"CIAB"] {
            if get(name)?.len() < 30 {
                bail!("truncated USS CIA state");
            }
        }
        let ram = get(b"CRAM")?;
        if !(256 * 1024..=8 * 1024 * 1024).contains(&ram.len()) || !ram.len().is_power_of_two() {
            bail!("unsupported USS chip RAM size {}", ram.len());
        }
        let rom = get(b"ROM ")?;
        if rom.len() < 21 {
            bail!("truncated USS ROM identity");
        }
        let rom_size = be32(rom, 4) as usize;
        let rom_crc = be32(rom, 16);
        if !matches!(rom_size, 262144 | 524288) || be32(rom, 8) != 0 || rom_crc == 0 {
            bail!("USS requires a 256/512 KiB Kickstart identity with a nonzero CRC");
        }
        let mut warnings = vec!["USS pipelines, prefetch and event phases are reconstructed; discard the first resumed frame".into()];
        for chunk in &chunks {
            let name = &chunk.name;
            if matches!(
                name,
                b"FPU "
                    | b"MMU "
                    | b"CD32"
                    | b"CDTV"
                    | b"DMAC"
                    | b"P96 "
                    | b"FSYS"
                    | b"FSYC"
                    | b"FSYP"
                    | b"BORO"
                    | b"PRAM"
                    | b"ZCRM"
            ) {
                bail!("USS {} state cannot yet be restored; use a chipset-only state without this device", String::from_utf8_lossy(name));
            }
            if matches!(name, b"FRAM" | b"ZRAM" | b"BRAM" | b"A3K1" | b"A3K2") {
                get(name)?;
                if !chunk.bytes.len().is_multiple_of(65536) {
                    bail!("unaligned USS RAM size");
                }
            }
            if name.starts_with(b"AUD") {
                if !matches!(name[3], b'0'..=b'3') || chunk.bytes.len() < 24 {
                    bail!("invalid USS audio chunk");
                }
                get(name)?;
            }
            if name.starts_with(b"SPR") {
                if !matches!(name[3], b'0'..=b'7') || chunk.bytes.len() < 25 {
                    bail!("invalid USS sprite chunk");
                }
                get(name)?;
            }
            if name == b"AGAC" && get(name)?.len() != 1024 {
                bail!("invalid USS AGA palette length");
            }
            if name == b"DISK" {
                if chunk.bytes.len() < 17 {
                    bail!("truncated USS disk controller");
                }
                if chunk.bytes[5] != 0 {
                    bail!("USS has active disk DMA; save after the transfer completes");
                }
            }
        }
        if custom[1] & 0x4000 != 0 {
            bail!("USS has an active blit; save after the blitter becomes idle");
        }
        if chipset_flags & 4 != 0 {
            get(b"AGAC")?;
        }
        if chunks.iter().any(|c| c.name.starts_with(b"DSK")) {
            warnings.push("floppy drive position/media is not imported; configure matching disk images before further disk access".into());
        }
        // Unknown chunks are reported, rather than silently claiming complete
        // support for an expansion device or a future incompatible extension.
        for chunk in &chunks {
            if !matches!(
                &chunk.name,
                b"ASF "
                    | b"CPU "
                    | b"CPUX"
                    | b"CPUT"
                    | b"CHIP"
                    | b"CHPX"
                    | b"CHPD"
                    | b"CHSL"
                    | b"BPLX"
                    | b"AGAC"
                    | b"CIAA"
                    | b"CIAB"
                    | b"ROM "
                    | b"CRAM"
                    | b"BRAM"
                    | b"FRAM"
                    | b"ZRAM"
                    | b"A3K1"
                    | b"A3K2"
                    | b"EXPA"
                    | b"CYCS"
                    | b"BLIT"
                    | b"BLTX"
                    | b"DISK"
            ) && !chunk.name.starts_with(b"AUD")
                && !chunk.name.starts_with(b"SPR")
            {
                warnings.push(format!(
                    "USS {} chunk not restored",
                    String::from_utf8_lossy(&chunk.name)
                ));
            }
        }
        let file = Self {
            chunks,
            cpu,
            custom,
            chipset_flags,
            rom_size,
            rom_crc,
            warnings,
        };
        file.ram_banks()?;
        Ok(file)
    }

    fn chunk(&self, name: &[u8; 4]) -> Option<&[u8]> {
        self.chunks
            .iter()
            .find(|c| &c.name == name)
            .map(|c| c.bytes.as_slice())
    }
    fn size(&self, name: &[u8; 4]) -> usize {
        self.chunk(name).map_or(0, |b| b.len())
    }

    fn ram_banks(&self) -> Result<Vec<(u32, &[u8])>> {
        let expansion = self.chunk(b"EXPA");
        let mut banks = Vec::new();
        for (name, offset) in [(b"FRAM", 0), (b"ZRAM", 4)] {
            if let Some(bytes) = self.chunk(name) {
                let exp = expansion
                    .filter(|b| b.len() >= offset + 4)
                    .context("fast RAM requires USS EXPA addresses")?;
                let base = be32(exp, offset);
                let end = u64::from(base) + bytes.len() as u64;
                if (name == b"FRAM" && (base < 0x200000 || end > 0xa00000))
                    || (name == b"ZRAM" && (base < 0x10000000 || end > 0x80000000))
                    || base & 0xffff != 0
                    || !bytes.len().is_power_of_two()
                {
                    bail!("unsupported USS fast RAM mapping at {base:#x}");
                }
                banks.push((base, bytes));
            }
        }
        if self.size(b"BRAM") > 0x1c0000
            || self.size(b"A3K1") > 0x4000000
            || self.size(b"A3K2") > 0x8000000
        {
            bail!("unsupported USS local RAM size");
        }
        Ok(banks)
    }

    /// Derive the installed hardware from the file, while preserving the
    /// caller's ROM and host presentation/audio configuration.
    pub fn configure(&self, cfg: &mut config::Config) -> Result<()> {
        let model = if self.size(b"A3K1") != 0 {
            if self.chipset_flags & 4 != 0 {
                "A4000"
            } else {
                "A3000"
            }
        } else if self.chipset_flags & 4 != 0 {
            "A1200"
        } else {
            "A500"
        };
        let chipset = if self.chipset_flags & 4 != 0 {
            "AGA"
        } else if self.chipset_flags & 3 != 0 {
            "ECS"
        } else {
            "OCS"
        };
        let cpu_name = if self.cpu.model == 68020 && self.cpu.address_24 {
            "68EC020".to_string()
        } else {
            self.cpu.model.to_string()
        };
        let raw: config::RawConfig = toml::from_str(&format!("[machine]\nmodel = \"{model}\"\n[chipset]\nrevision = \"{chipset}\"\n[cpu]\nmodel = \"{}\"\n[memory]\nchip = \"{}K\"\nslow = \"{}K\"\nfast = \"0\"\n", cpu_name, self.size(b"CRAM") / 1024, self.size(b"BRAM") / 1024))?;
        let hardware = config::Config::try_from(raw)?;
        cfg.machine = hardware.machine;
        cfg.cpu = hardware.cpu;
        cfg.cpu_clock_mhz = hardware.cpu_clock_mhz;
        cfg.cpu_icache = hardware.cpu_icache;
        cfg.cpu_dcache = hardware.cpu_dcache;
        cfg.cpu_jit = false;
        cfg.fpu = false;
        cfg.chipset = hardware.chipset;
        cfg.agnus_revision = hardware.agnus_revision;
        cfg.denise_revision = if self.chipset_flags & 6 == 0 {
            crate::chipset::denise::DeniseRevision::Ocs
        } else {
            hardware.denise_revision
        };
        cfg.gate_array = hardware.gate_array;
        cfg.mem_controller = hardware.mem_controller;
        cfg.chip_ram_bytes = self.size(b"CRAM");
        cfg.slow_ram_bytes = self.size(b"BRAM");
        cfg.fast_ram_bytes = 0;
        cfg.z3_ram_bytes = 0;
        cfg.mb_ram_bytes = self.size(b"A3K1");
        cfg.accel_ram_bytes = self.size(b"A3K2");
        cfg.zorro_boards.clear();
        cfg.identify_board = false;
        cfg.emulation.run_ahead_frames = 0;
        cfg.video_standard = if self.custom[0x1fa / 2] & 0x8001 == 0x8001 {
            crate::chipset::agnus::VideoStandard::Ntsc
        } else {
            crate::chipset::agnus::VideoStandard::Pal
        };
        Ok(())
    }

    /// Import, then discard one reconstructed video frame before exposing
    /// the machine to scheduled captures or a debugger.
    pub fn load(&self, emu: &mut Emulator) -> Result<()> {
        self.apply(emu)?;
        let start = emu.bus().emulated_frames();
        let mut idle = false;
        while emu.bus().emulated_frames() == start {
            emu.debug_step_for_gdb(&mut idle)?;
            if emu.machine.cpu_double_faulted() {
                bail!("CPU double fault in USS warm-up frame");
            }
        }
        emu.reset_live_audio_after_timeline_jump();
        Ok(())
    }

    pub fn apply(&self, emu: &mut Emulator) -> Result<()> {
        // Finish all fallible validation before touching the machine.
        let expected = match self.cpu.model {
            68000 => config::CpuModel::M68000,
            68010 => config::CpuModel::M68010,
            68020 if self.cpu.address_24 => config::CpuModel::M68EC020,
            68020 => config::CpuModel::M68020,
            68030 => config::CpuModel::M68030,
            68040 => config::CpuModel::M68040,
            68060 => config::CpuModel::M68060,
            _ => unreachable!(),
        };
        if emu.machine_descriptor().cpu != expected {
            bail!("USS CPU configuration mismatch");
        }
        let rom = &emu.bus().mem.rom;
        let canonical = if rom.len() == self.rom_size {
            rom.as_slice()
        } else if rom.len() == self.rom_size * 2 && rom[..self.rom_size] == rom[self.rom_size..] {
            &rom[..self.rom_size]
        } else {
            bail!("USS Kickstart size does not match configured ROM");
        };
        let mut crc = flate2::Crc::new();
        crc.update(canonical);
        if crc.sum() != self.rom_crc {
            let label = crate::romdb::identify_crc(self.rom_crc, self.rom_size)
                .map_or("unknown ROM", |r| r.label);
            bail!(
                "USS needs {label}, CRC {:08x}; configured ROM has CRC {:08x}",
                self.rom_crc,
                crc.sum()
            );
        }
        if emu.bus().mem.chip_ram.len() != self.size(b"CRAM")
            || emu.bus().mem.slow_ram.len() != self.size(b"BRAM")
            || emu.bus().mem.mb_ram.len() != self.size(b"A3K1")
            || emu.bus().mem.accel_ram.len() != self.size(b"A3K2")
        {
            bail!("USS RAM configuration mismatch");
        }
        let mut zorro = crate::zorro::ZorroChain::default();
        for (index, (base, ram)) in self.ram_banks()?.into_iter().enumerate() {
            let spec = if base < 0x10000000 {
                crate::zorro::BoardSpec::fast_ram(ram.len())
            } else {
                crate::zorro::BoardSpec::z3_ram(ram.len())
            };
            zorro.add_board_configured_at(spec, base)?;
            zorro.board_ram_mut(index).copy_from_slice(ram);
        }
        let ciaa = crate::chipset::cia::Cia::from_uss(
            crate::chipset::cia::Which::A,
            self.chunk(b"CIAA").unwrap(),
        );
        let ciab = crate::chipset::cia::Cia::from_uss(
            crate::chipset::cia::Which::B,
            self.chunk(b"CIAB").unwrap(),
        );
        let bus = emu.bus_mut();
        bus.mem
            .chip_ram
            .copy_from_slice(self.chunk(b"CRAM").unwrap());
        for (bank, name) in [
            (&mut bus.mem.slow_ram, b"BRAM"),
            (&mut bus.mem.mb_ram, b"A3K1"),
            (&mut bus.mem.accel_ram, b"A3K2"),
        ] {
            if let Some(bytes) = self.chunk(name) {
                bank.copy_from_slice(bytes);
            }
        }
        bus.mem.zorro = zorro;
        bus.cia_a = ciaa;
        bus.cia_b = ciab;
        // CHPX bit 0 validates its flag word; bit 1 is the live ROM overlay.
        // Older files without this extension use the normal chip RAM mapping.
        bus.mem.overlay = self
            .chunk(b"CHPX")
            .is_some_and(|bytes| be32(bytes, 0) & 3 == 3);
        // Write only configuration latches: triggering BLTSIZE, COPJMP or
        // DSKLEN here would start a new transfer that was not in the state.
        for offset in (0x20..0x200u16).step_by(2) {
            if matches!(offset, 0x24 | 0x26 | 0x2a | 0x2c | 0x30 | 0x36..=0x3e | 0x58 | 0x5a | 0x5e | 0x88..=0x8c | 0x96 | 0x9a..=0x9e | 0xa0..=0xde | 0x120..=0x17e | 0x1c0..=0x1e2 | 0x1e6..=0x1fa | 0x1fe)
            {
                continue;
            }
            bus.custom_write(
                0xdff000 + u64::from(offset),
                2,
                u64::from(self.custom[offset as usize / 2]),
            );
        }
        bus.denise.clxdat = self.custom[0xe / 2] & 0x7fff;
        bus.denise.diwhigh_written = self.custom[0x1e4 / 2] & 0x8000 != 0;
        bus.denise.diwhigh = self.custom[0x1e4 / 2] & 0x3f3f;
        bus.paula.intena = self.custom[0x9a / 2] & 0x7fff;
        bus.paula.intreq = self.custom[0x9c / 2] & 0x7fff;
        bus.paula.adkcon = self.custom[0x9e / 2] & 0x7fff;
        bus.custom_write(
            0xdff096,
            2,
            u64::from(self.custom[0x96 / 2] & 0x7fff | 0x8000),
        );
        for ch in 0..4 {
            let name = [b'A', b'U', b'D', b'0' + ch];
            if let Some(bytes) = self.chunk(&name) {
                bus.paula.import_uss_audio(ch as usize, bytes);
            }
        }
        for sprite in 0..8 {
            let name = [b'S', b'P', b'R', b'0' + sprite];
            if let Some(bytes) = self.chunk(&name) {
                let s = sprite as usize;
                bus.denise.sprpt[s] = be32(bytes, 0);
                bus.denise.write_sprpos(s, be16(bytes, 4));
                bus.denise.write_sprctl(s, be16(bytes, 6));
                bus.denise.write_sprdata(s, be16(bytes, 8));
                bus.denise.write_sprdatb(s, be16(bytes, 10));
                bus.denise.spr_armed[s] = bytes[24] & 1 != 0;
                bus.denise.spr_hw_armed[s] = bytes[24] & 1 != 0;
            }
        }
        if let Some(colors) = self.chunk(b"AGAC") {
            for index in 0..256 {
                let rgb = be32(colors, index * 4);
                let hi = ((rgb >> 12) & 0xf00) | ((rgb >> 8) & 0xf0) | ((rgb >> 4) & 0xf);
                let lo = ((rgb >> 8) & 0xf00) | ((rgb >> 4) & 0xf0) | (rgb & 0xf);
                bus.denise.palette.write_entry(index, false, hi as u16);
                bus.denise.palette.write_entry(index, true, lo as u16);
            }
        }
        bus.copper
            .jump((u32::from(self.custom[0x80 / 2]) << 16) | u32::from(self.custom[0x82 / 2]));
        emu.machine.import_uss_cpu(&self.cpu);
        let mut descriptor = emu.machine_descriptor().clone();
        descriptor.fast_ram_bytes = self.size(b"FRAM");
        emu.set_machine_descriptor(descriptor);
        for warning in &self.warnings {
            log::warn!("{warning}");
        }
        Ok(())
    }
}

fn parse_cpu(bytes: &[u8]) -> Result<CpuState> {
    if bytes.len() < 90 {
        bail!("truncated USS CPU state");
    }
    let model = be32(bytes, 0);
    if !matches!(model, 68000 | 68010 | 68020 | 68030 | 68040 | 68060) {
        bail!("unsupported USS CPU {model}");
    }
    let mut cpu = CpuState {
        model,
        address_24: be32(bytes, 4) & 1 != 0,
        pc: be32(bytes, 68),
        usp: be32(bytes, 76),
        isp: be32(bytes, 80),
        sr: be16(bytes, 84),
        stopped: be32(bytes, 86) & 1 != 0,
        ..Default::default()
    };
    for (i, reg) in cpu.registers.iter_mut().enumerate() {
        *reg = be32(bytes, 8 + i * 4);
    }
    if model >= 68010 {
        if bytes.len() < 102 {
            bail!("truncated USS 68010 controls");
        }
        cpu.dfc = be32(bytes, 90);
        cpu.sfc = be32(bytes, 94);
        cpu.vbr = be32(bytes, 98);
    }
    if model >= 68020 {
        if bytes.len() < 114 {
            bail!("truncated USS 68020 controls");
        }
        cpu.caar = be32(bytes, 102);
        cpu.cacr = be32(bytes, 106);
        cpu.msp = be32(bytes, 110);
    }
    if model >= 68030 {
        if bytes.len() < 144 {
            bail!("truncated USS 68030 controls");
        }
        if be32(bytes, 138) & 0x80000000 != 0 {
            bail!("USS has an enabled 68030 MMU");
        }
    }
    if model >= 68040 {
        if bytes.len() < 172 {
            bail!("truncated USS 68040 controls");
        }
        if be32(bytes, 160) & 0x8000 != 0 {
            bail!("USS has an enabled 68040 MMU");
        }
    }
    if cpu.address_24 && model >= 68030 {
        bail!("USS 24-bit addressing on 68030 or later is unsupported");
    }
    if be32(bytes, 86) & 2 != 0 {
        bail!("USS CPU is halted after a fault");
    }
    Ok(cpu)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn chunk(out: &mut Vec<u8>, name: &[u8; 4], data: &[u8], compress: bool) {
        let mut body = Vec::new();
        if compress {
            body.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(data).unwrap();
            body.extend_from_slice(&z.finish().unwrap());
        } else {
            body.extend_from_slice(data);
        }
        out.extend_from_slice(name);
        out.extend_from_slice(&((body.len() + 12) as u32).to_be_bytes());
        out.extend_from_slice(&(2 | u32::from(compress)).to_be_bytes());
        out.extend_from_slice(&body);
        out.extend(std::iter::repeat_n(0, 4 - (body.len() & 3)));
    }

    fn fixture(rom: &[u8], compressed: bool) -> Vec<u8> {
        let mut out = Vec::new();
        chunk(
            &mut out,
            b"ASF ",
            b"\0\0\0\0Copperline tests\0fixture\0\0",
            false,
        );
        let mut cpu = vec![0; 90];
        cpu[..4].copy_from_slice(&68000u32.to_be_bytes());
        cpu[8..12].copy_from_slice(&0x12345678u32.to_be_bytes());
        cpu[68..72].copy_from_slice(&0x100u32.to_be_bytes());
        cpu[76..80].copy_from_slice(&0x3000u32.to_be_bytes());
        cpu[80..84].copy_from_slice(&0x4000u32.to_be_bytes());
        cpu[84..86].copy_from_slice(&0x2700u16.to_be_bytes());
        chunk(&mut out, b"CPU ", &cpu, false);
        // Compact CHIP omits 0xa0..0xdf and 0x120..0x17f.
        let mut chip = vec![0; 360];
        chip[4 + 0x20..4 + 0x22].copy_from_slice(&0u16.to_be_bytes());
        chip[4 + 0x22..4 + 0x24].copy_from_slice(&0x2000u16.to_be_bytes());
        chip[4 + 0x180 - 160..4 + 0x182 - 160].copy_from_slice(&0x123u16.to_be_bytes());
        chunk(&mut out, b"CHIP", &chip, compressed);
        // Older WinUAE states use 30 bytes; newer versions append fields.
        let mut cia = vec![0; 30];
        cia[0] = 0xc0;
        cia[1] = 0xff;
        cia[4..6].copy_from_slice(&0x4321u16.to_be_bytes());
        cia[17..19].copy_from_slice(&0x5678u16.to_le_bytes());
        cia[8..11].copy_from_slice(&[0x56, 0x34, 0x12]);
        for name in [b"CIAA", b"CIAB"] {
            chunk(&mut out, name, &cia, compressed);
        }
        let mut ram = vec![0; 512 * 1024];
        ram[0x100..0x102].copy_from_slice(&0x60feu16.to_be_bytes());
        chunk(&mut out, b"CRAM", &ram, compressed);
        let mut identity = vec![0; 22];
        identity[..4].copy_from_slice(&0xf80000u32.to_be_bytes());
        identity[4..8].copy_from_slice(&(rom.len() as u32).to_be_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(rom);
        identity[16..20].copy_from_slice(&crc.sum().to_be_bytes());
        chunk(&mut out, b"ROM ", &identity, false);
        out.extend_from_slice(b"END \0\0\0\x08");
        out
    }

    #[test]
    fn framing_compressed_and_plain_restore_the_compact_register_layout() {
        let rom = vec![0; 512 * 1024];
        for compressed in [false, true] {
            let state = UssFile::parse(&fixture(&rom, compressed)).unwrap();
            assert_eq!(state.cpu.pc, 0x100);
            assert_eq!(state.custom[0x180 / 2], 0x123);
            assert_eq!(state.custom[0x22 / 2], 0x2000);
            assert_eq!(state.size(b"CRAM"), 512 * 1024);
            let mut cfg = config::Config::default();
            state.configure(&mut cfg).unwrap();
            assert_eq!(cfg.chip_ram_bytes, 512 * 1024);
            assert_eq!(cfg.slow_ram_bytes, 0);
        }
    }

    #[test]
    fn incomplete_chunks_and_inflated_size_mismatch_are_errors() {
        let bytes = fixture(&vec![0; 512 * 1024], true);
        for end in [0, 1, 7, 11, 20, bytes.len() - 1] {
            assert!(UssFile::parse(&bytes[..end]).is_err());
        }
        let mut bad = bytes.clone();
        let offset = bad.windows(4).position(|b| b == b"CHIP").unwrap();
        bad[offset + 12..offset + 16].copy_from_slice(&0xffffffffu32.to_be_bytes());
        assert!(UssFile::parse(&bad).is_err());
    }

    #[cfg(feature = "gdb")]
    #[test]
    fn custom_extra_overlay_is_validated_and_restored() {
        let mut emu = crate::gdbstub::testkit::emulator_with_loadseg_program();
        emu.bus_mut().mem.rom[..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        let original = fixture(&emu.bus().mem.rom, false);
        for flags in [0u32, 1, 2, 3] {
            let mut bytes = original[..original.len() - 8].to_vec();
            chunk(&mut bytes, b"CHPX", &flags.to_be_bytes(), false);
            bytes.extend_from_slice(b"END \0\0\0\x08");
            let state = UssFile::parse(&bytes).unwrap();
            state.apply(&mut emu).unwrap();
            assert_eq!(emu.bus().mem.overlay, flags == 3);
            let expected = if flags == 3 {
                &emu.bus().mem.rom[..4]
            } else {
                &emu.bus().mem.chip_ram[..4]
            };
            assert_eq!(emu.machine.debug_read_memory(0, 4), expected);
        }
        let mut bytes = original[..original.len() - 8].to_vec();
        chunk(&mut bytes, b"CHPX", &[1, 2, 3], false);
        bytes.extend_from_slice(b"END \0\0\0\x08");
        assert!(UssFile::parse(&bytes).is_err());
        let mut bytes = original[..original.len() - 8].to_vec();
        for _ in 0..2 {
            chunk(&mut bytes, b"CHPX", &[0, 0, 0, 3], false);
        }
        bytes.extend_from_slice(b"END \0\0\0\x08");
        assert!(UssFile::parse(&bytes).is_err());
    }

    #[cfg(feature = "gdb")]
    #[test]
    fn import_restores_cpu_cia_and_disk_pointer_then_runs_deterministically() {
        let mut emu = crate::gdbstub::testkit::emulator_with_loadseg_program();
        let bytes = fixture(&emu.bus().mem.rom, true);
        let state = UssFile::parse(&bytes).unwrap();
        state.apply(&mut emu).unwrap();
        assert_eq!(emu.machine.debug_register(0), Some(0x12345678));
        assert_eq!(emu.machine.debug_register(15), Some(0x4000));
        assert_eq!(emu.machine.usp(), 0x3000);
        assert_eq!(emu.bus().cia_a.ta_count, 0x4321);
        assert_eq!(emu.bus().cia_a.ta_latch, 0x5678);
        assert_eq!(emu.bus().floppy.dskpt(), 0x2000);
        assert_eq!(emu.bus().denise.palette.rgb24(0), 0x112233);
        let mut second = crate::gdbstub::testkit::emulator_with_loadseg_program();
        state.load(&mut emu).unwrap();
        state.load(&mut second).unwrap();
        for _ in 0..2 {
            emu.step_frame().unwrap();
            second.step_frame().unwrap();
        }
        assert_eq!(emu.machine.pc(), 0x100);
        assert_eq!(
            emu.save_state_bytes().unwrap(),
            second.save_state_bytes().unwrap()
        );
        let before = emu.machine.pc();
        let mut mismatch = UssFile::parse(&bytes).unwrap();
        mismatch.rom_crc ^= 1;
        assert!(mismatch.apply(&mut emu).is_err());
        assert_eq!(emu.machine.pc(), before);
    }
}
