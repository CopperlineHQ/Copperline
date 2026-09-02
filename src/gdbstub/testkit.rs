// SPDX-License-Identifier: GPL-3.0-or-later

//! Test fixtures shared by the GDB stub drivers: a canned emulator whose
//! ROM program performs a LoadSeg-style hand-off, and a minimal RSP wire
//! client. Compiled only for tests.

use super::core::{checksum, hex_decode};
use crate::emulator::Emulator;
use std::io::{Read, Write};
use std::net::TcpStream;

/// Build an emulator whose ROM program installs a seglist BPTR into
/// a staged CLI structure, mimicking the tail of AmigaDOS
/// RunCommand() after LoadSeg():
///
/// ```text
/// F80010  NOP
/// F80012  MOVE.L #$5000,($1303C).L   ; cli_Module <- seglist BPTR
/// F8001C  BRA.S  *
/// ```
///
/// Chip RAM holds a fake exec world: ExecBase at $10000 (installed
/// at address 4 after reset), ThisTask a process at $12000 whose
/// CLI at $13000 names "dh0:c/hello", and a two-hunk seglist at
/// $14000/$15000.
pub(crate) fn emulator_with_loadseg_program() -> Emulator {
    let mut rom = vec![0u8; crate::memory::ROM_SIZE];
    let put_word = |mem: &mut [u8], off: usize, word: u16| {
        mem[off..off + 2].copy_from_slice(&word.to_be_bytes());
    };
    put_word(&mut rom, 0x10, 0x4E71); // NOP
    put_word(&mut rom, 0x12, 0x23FC); // MOVE.L #imm,(abs).L
    put_word(&mut rom, 0x14, 0x0000);
    put_word(&mut rom, 0x16, 0x5000); // seglist BPTR ($14000 >> 2)
    put_word(&mut rom, 0x18, 0x0001);
    put_word(&mut rom, 0x1A, 0x303C); // cli_Module at $13000 + $3C
    put_word(&mut rom, 0x1C, 0x60FE); // BRA.S *

    let mut chip_ram = vec![0u8; 512 * 1024];
    let put32 = |mem: &mut [u8], addr: usize, value: u32| {
        mem[addr..addr + 4].copy_from_slice(&value.to_be_bytes());
    };
    put32(&mut chip_ram, 0, 0x0000_4000); // reset SSP
    put32(&mut chip_ram, 4, 0x00F8_0010); // reset PC
    let base = 0x0001_0000u32;
    put32(&mut chip_ram, (base + 0x26) as usize, !base); // ChkBase
    put32(&mut chip_ram, (base + 0x114) as usize, 0x0001_2000); // ThisTask
    chip_ram[0x1_2008] = 13; // ln_Type NT_PROCESS
    put32(&mut chip_ram, 0x1_20AC, 0x0001_3000 >> 2); // pr_CLI
    put32(&mut chip_ram, 0x1_3010, 0x0001_3800 >> 2); // cli_CommandName
    chip_ram[0x1_3800] = 11;
    chip_ram[0x1_3801..0x1_380C].copy_from_slice(b"dh0:c/hello");
    put32(&mut chip_ram, 0x1_3FFC, 0x100); // hunk 1 size
    put32(&mut chip_ram, 0x1_4000, 0x0001_5000 >> 2); // hunk 1 next
    put32(&mut chip_ram, 0x1_4FFC, 0x40); // hunk 2 size
    put32(&mut chip_ram, 0x1_5000, 0); // end of list

    let bus = crate::bus::Bus::new(
        crate::memory::Memory {
            chip_ram,
            slow_ram: Vec::new(),
            mb_ram: Vec::new(),
            accel_ram: Vec::new(),
            rom,
            overlay: false,
            zorro: crate::zorro::ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        },
        crate::chipset::paula::Paula::new(
            Box::new(crate::serial::NullSerialSink),
            Box::new(crate::audio::NullSink),
        ),
        crate::floppy::FloppyController::default(),
    );
    let mut emu = Emulator::new(
        bus,
        crate::config::CpuModel::M68000,
        false,
        Default::default(),
        crate::config::PacingBudget::Cycles,
        2,
        false,
    )
    .unwrap();
    // The reset vectors are latched; address 4 can now hold the
    // ExecBase pointer.
    emu.machine.debug_write_memory(4, &base.to_be_bytes());
    emu
}

/// A minimal RSP client for driving a GDB stub session over loopback.
pub(crate) struct GdbClient {
    stream: TcpStream,
}

impl GdbClient {
    pub(crate) fn connect(addr: std::net::SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_nodelay(true).ok();
        Self { stream }
    }

    pub(crate) fn send(&mut self, payload: &str) {
        write!(
            self.stream,
            "${payload}#{:02x}",
            checksum(payload.as_bytes())
        )
        .unwrap();
        self.stream.flush().unwrap();
    }

    pub(crate) fn read_reply(&mut self) -> String {
        let mut byte = [0u8; 1];
        loop {
            self.stream.read_exact(&mut byte).unwrap();
            if byte[0] == b'$' {
                break;
            }
        }
        let mut payload = Vec::new();
        loop {
            self.stream.read_exact(&mut byte).unwrap();
            if byte[0] == b'#' {
                break;
            }
            payload.push(byte[0]);
        }
        let mut sum = [0u8; 2];
        self.stream.read_exact(&mut sum).unwrap();
        self.stream.write_all(b"+").unwrap();
        String::from_utf8(payload).unwrap()
    }

    /// Send a request and collect decoded O (console) packets until
    /// the final non-O reply.
    pub(crate) fn request_collect(&mut self, payload: &str) -> (Vec<String>, String) {
        self.send(payload);
        let mut console = Vec::new();
        loop {
            let reply = self.read_reply();
            if reply.starts_with('O') && reply != "OK" {
                console.push(String::from_utf8(hex_decode(&reply[1..]).unwrap()).unwrap());
                continue;
            }
            return (console, reply);
        }
    }

    pub(crate) fn request(&mut self, payload: &str) -> String {
        self.request_collect(payload).1
    }

    /// Write raw bytes to the stream (interrupts, deliberately bad
    /// frames).
    pub(crate) fn raw(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).unwrap();
        self.stream.flush().unwrap();
    }

    /// Read exactly `n` bytes, for asserting the precise wire encoding
    /// (framing, ack bytes) rather than just the payload.
    pub(crate) fn read_bytes(&mut self, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        self.stream.read_exact(&mut buf).unwrap();
        buf
    }
}
