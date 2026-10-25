// SPDX-License-Identifier: GPL-3.0-or-later
//! Synthetic JIT throughput diagnostic (host elapsed time, never guest timers).
//!
//! cargo run --release --no-default-features --features cpu-jit \
//!     --example jit_throughput -- 5 mem
//!
//! Arguments: positive trial count (default 3), `mem` or `reg` (default mem).
//! CSV sections: core,trial,batch,seconds,MIPS;
//! machine,trial,code_in_ROM,second_data_bank,guest_clock_factor,seconds,MIPS,CCK;
//! chipset,trial,seconds (ten emulated PAL seconds).
//! Fixed synthetic code, idle DMA, masked IRQs, no OS, no display renderer.
//! Cache models are allocated but guest CACR stays at its reset value.
//! This isolates execution overhead; it is not an AmigaOS benchmark.

use copperline::{
    audio::NullSink,
    bus::Bus,
    chipset::paula::Paula,
    config::CpuModel,
    cpu,
    floppy::FloppyController,
    memory::{Memory, FAST_RAM_BASE, ROM_BASE, ROM_SIZE},
    serial::StdoutSink,
    zorro::{BoardSpec, ZorroChain},
};
use m68k::{AddressBus, CpuCore, CpuType, FastMem};
use std::time::Instant;

const BASE: u32 = FAST_RAM_BASE as u32;
const WARM: usize = 1_048_576;
const COUNT: usize = 67_108_864;
const REG_PROGRAM: [u8; 4] = [0x52, 0x80, 0x60, 0xfc]; // addq.l #1,d0; bra.s loop
const MEM_PROGRAM: [u8; 14] = [
    0x41, 0xf9, 0x00, 0x20, 0x08, 0x00, // lea $200800,a0
    0x20, 0x10, 0x52, 0x80, 0x20, 0x80, 0x60, 0xf8,
]; // move.l (a0),d0; addq.l #1,d0; move.l d0,(a0); bra.s loop

fn bus(rom_code: bool, program: &[u8]) -> Bus {
    let pc = if rom_code {
        ROM_BASE as u32 + 0x100
    } else {
        BASE + 0x100
    };
    let mut rom = vec![0u8; ROM_SIZE];
    rom[..4].copy_from_slice(&(BASE + 0xf000).to_be_bytes());
    rom[4..8].copy_from_slice(&pc.to_be_bytes());
    rom[0x100..0x100 + program.len()].copy_from_slice(program);
    let mut chip_ram = vec![0; 2 * 1024 * 1024];
    chip_ram[..8].copy_from_slice(&rom[..8]);
    let mut zorro = ZorroChain::default();
    zorro
        .add_board_configured_at(BoardSpec::fast_ram(65536), BASE)
        .unwrap();
    zorro.board_ram_mut(0)[0x100..0x100 + program.len()].copy_from_slice(program);
    Bus::new(
        Memory {
            chip_ram,
            slow_ram: vec![],
            mb_ram: vec![],
            accel_ram: vec![],
            rom,
            overlay: false,
            zorro,
            extended_rom: vec![],
            extended_rom_base: 0,
            wcs: vec![],
            wcs_write_protected: false,
        },
        Paula::new(Box::new(StdoutSink::new()), Box::new(NullSink)),
        FloppyController::default(),
    )
}

struct Flat(Vec<u8>);
impl AddressBus for Flat {
    fn read_byte(&mut self, a: u32) -> u8 {
        self.0[(a - BASE) as usize]
    }
    fn read_word(&mut self, a: u32) -> u16 {
        u16::from_be_bytes(self.0[(a - BASE) as usize..][..2].try_into().unwrap())
    }
    fn read_long(&mut self, a: u32) -> u32 {
        u32::from_be_bytes(self.0[(a - BASE) as usize..][..4].try_into().unwrap())
    }
    fn write_byte(&mut self, a: u32, v: u8) {
        self.0[(a - BASE) as usize] = v;
    }
    fn write_word(&mut self, a: u32, v: u16) {
        self.0[(a - BASE) as usize..][..2].copy_from_slice(&v.to_be_bytes());
    }
    fn write_long(&mut self, a: u32, v: u32) {
        self.0[(a - BASE) as usize..][..4].copy_from_slice(&v.to_be_bytes());
    }
    fn fast_mem(&mut self) -> Option<FastMem> {
        Some(FastMem {
            ptr: self.0.as_mut_ptr(),
            base: BASE,
            len: self.0.len() as u32,
        })
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let repeats = args
        .next()
        .map(|s| s.parse::<usize>())
        .transpose()?
        .unwrap_or(3);
    anyhow::ensure!(repeats != 0, "trial count must be positive");
    let memory = match args.next().as_deref().unwrap_or("mem") {
        "mem" => true,
        "reg" => false,
        _ => anyhow::bail!("workload must be mem or reg"),
    };
    anyhow::ensure!(
        args.next().is_none(),
        "usage: jit_throughput [trials] [mem|reg]"
    );
    let program: &[u8] = if memory { &MEM_PROGRAM } else { &REG_PROGRAM };
    let per_increment = if memory { 4 } else { 2 };
    println!(
        "workload={},instructions={COUNT},warmup={WARM}",
        if memory { "memory" } else { "register" }
    );
    for trial in 0..repeats {
        for batch in [64usize, 256, 1024, 4096] {
            let mut mem = Flat(vec![0; 65536]);
            mem.0[0x100..0x100 + program.len()].copy_from_slice(program);
            let mut core = CpuCore::new();
            core.set_cpu_type(CpuType::M68040);
            core.pc = BASE + 0x100;
            for _ in 0..WARM / batch {
                assert_eq!(
                    core.run_batch(&mut mem, batch as u32, &[]).instructions,
                    batch as u32
                );
            }
            let before = core.d(0);
            let start = Instant::now();
            for _ in 0..COUNT / batch {
                assert_eq!(
                    core.run_batch(&mut mem, batch as u32, &[]).instructions,
                    batch as u32
                );
            }
            let elapsed = start.elapsed().as_secs_f64();
            assert_eq!(
                core.d(0).wrapping_sub(before),
                (COUNT / per_increment) as u32
            );
            println!(
                "core,{trial},{batch},{elapsed:.6},{:.2}",
                COUNT as f64 / elapsed / 1e6
            );
        }
        for rom_code in [false, true] {
            for second_data_bank in [false, true] {
                let mut code = program.to_vec();
                if second_data_bank && code.len() == MEM_PROGRAM.len() {
                    code[2..6].copy_from_slice(&0x07e0_0800u32.to_be_bytes());
                }
                for factor in [1.0, 80.0] {
                    let mut bus = bus(rom_code, &code);
                    if second_data_bank {
                        bus.mem.fit_mb_ram(2 * 1024 * 1024);
                    }
                    let mut machine =
                        cpu::build(bus, CpuModel::M68040, true, 7, Default::default(), false)?;
                    machine.set_cache_emulation(true, true);
                    machine.set_jit_enabled(true);
                    machine.set_jit_speed_factor(factor);
                    for _ in 0..WARM / 65536 {
                        machine.step_slice(65536)?;
                    }
                    let before = machine.d(0);
                    let start_cck = machine.bus().emulated_cck();
                    let start = Instant::now();
                    for _ in 0..COUNT / 65536 {
                        assert_eq!(machine.step_slice(65536)?.instructions, 65536);
                    }
                    let elapsed = start.elapsed().as_secs_f64();
                    assert_eq!(
                        machine.d(0).wrapping_sub(before),
                        (COUNT / per_increment) as u32
                    );
                    println!(
                    "machine,{trial},{rom_code},{second_data_bank},{factor},{elapsed:.6},{:.2},{}",
                    COUNT as f64 / elapsed / 1e6,
                    machine.bus().emulated_cck() - start_cck
                );
                }
            }
        }
        let mut chipset = bus(false, program);
        let start = Instant::now();
        for _ in 0..500 {
            chipset.advance_devices(70938);
        }
        println!("chipset,{trial},{:.6}", start.elapsed().as_secs_f64());
    }
    Ok(())
}
