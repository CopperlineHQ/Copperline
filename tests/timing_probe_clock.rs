//! Exercise the real timing probe's beam conversion, independent of an emulator beam.
use m68k::{AddressBus, CpuCore, CpuType, LinearMemoryBus, StepResult};

struct BeamBus {
    memory: LinearMemoryBus,
    vpos: u16,
    hpos: u16,
}

impl AddressBus for BeamBus {
    fn read_byte(&mut self, address: u32) -> u8 {
        self.memory.read_byte(address)
    }
    fn read_word(&mut self, address: u32) -> u16 {
        match address {
            0x00df_f004 => (self.vpos >> 8) & 1,
            0x00df_f006 => ((self.vpos & 255) << 8) | self.hpos,
            _ => self.memory.read_word(address),
        }
    }
    fn read_long(&mut self, address: u32) -> u32 {
        self.memory.read_long(address)
    }
    fn write_byte(&mut self, address: u32, value: u8) {
        self.memory.write_byte(address, value);
    }
    fn write_word(&mut self, address: u32, value: u16) {
        self.memory.write_word(address, value);
    }
    fn write_long(&mut self, address: u32, value: u32) {
        self.memory.write_long(address, value);
    }
}

fn convert(vpos: u16, hpos: u16) -> u32 {
    let program = include_bytes!("../timing-test/test.bin");
    let entry = [
        0x30, 0x2e, 0, 4, 0xc0, 0x7c, 0, 1, 0xe1, 0x48, 0x32, 0x2e, 0, 6, 0x74, 0,
    ];
    let matches: Vec<_> = program
        .windows(entry.len())
        .enumerate()
        .filter_map(|(offset, bytes)| (bytes == entry).then_some(offset))
        .collect();
    assert_eq!(matches.len(), 1, "identify getbeam in the committed binary");
    let mut memory = LinearMemoryBus::new(0x80000);
    memory.load(0x30000, program);
    memory.write_long(0x7fff0, 0x1000);
    let mut bus = BeamBus { memory, vpos, hpos };
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68000);
    cpu.set_sr(0x2700);
    cpu.set_a(6, 0x00df_f000);
    cpu.set_a(7, 0x7fff0);
    cpu.pc = 0x30000 + matches[0] as u32;
    for _ in 0..32 {
        assert!(matches!(cpu.step(&mut bus), StepResult::Ok { .. }));
        if cpu.pc == 0x1000 {
            return cpu.d(0);
        }
    }
    panic!("getbeam failed to return");
}

#[test]
fn beam_metric_has_one_colour_clock_per_horizontal_step_and_no_line_jump() {
    assert_eq!(convert(60, 101) - convert(60, 100), 1);
    assert_eq!(convert(61, 0) - convert(60, 226), 1);
    assert_eq!(convert(256, 0) - convert(255, 226), 1);
    assert_eq!(convert(312, 226), 313 * 227 - 1);
}
