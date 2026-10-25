//! Multiple physical direct-memory regions, exercised through public batching.
use crate::core::cpu::CpuCore;
use crate::core::memory::{AddressBus, FastMem, FastMemRegion, MAX_FAST_MEM_REGIONS};
use crate::core::types::CpuType;

#[derive(Clone)]
struct Bus {
    primary: Vec<u8>,
    ram: Vec<u8>,
    rom: Vec<u8>,
    direct: bool,
    primary_enabled: bool,
    reads: usize,
    writes: usize,
}
const PRIMARY: u32 = 0x100000;
const RAM: u32 = 0x200000;
const ROM: u32 = 0x800000;
impl Bus {
    fn new(words: &[u16]) -> Self {
        let mut b = Self {
            primary: vec![0; 1024],
            ram: vec![0; 1024],
            rom: vec![0; 1024],
            direct: true,
            primary_enabled: true,
            reads: 0,
            writes: 0,
        };
        for (chunk, word) in b.rom.chunks_exact_mut(2).zip(words) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        b
    }
}
impl AddressBus for Bus {
    fn read_byte(&mut self, a: u32) -> u8 {
        self.reads += 1;
        for (base, data) in [(PRIMARY, &self.primary), (RAM, &self.ram), (ROM, &self.rom)] {
            if let Some(v) = data.get(a.wrapping_sub(base) as usize) {
                return *v;
            }
        }
        0xa5
    }
    fn read_word(&mut self, a: u32) -> u16 {
        u16::from_be_bytes([self.read_byte(a), self.read_byte(a.wrapping_add(1))])
    }
    fn read_long(&mut self, a: u32) -> u32 {
        (u32::from(self.read_word(a)) << 16) | u32::from(self.read_word(a.wrapping_add(2)))
    }
    fn write_byte(&mut self, a: u32, v: u8) {
        self.writes += 1;
        for (base, data) in [(PRIMARY, &mut self.primary), (RAM, &mut self.ram)] {
            if let Some(byte) = data.get_mut(a.wrapping_sub(base) as usize) {
                *byte = v;
                return;
            }
        }
    }
    fn write_word(&mut self, a: u32, v: u16) {
        for (i, b) in v.to_be_bytes().into_iter().enumerate() {
            self.write_byte(a.wrapping_add(i as u32), b);
        }
    }
    fn write_long(&mut self, a: u32, v: u32) {
        for (i, b) in v.to_be_bytes().into_iter().enumerate() {
            self.write_byte(a.wrapping_add(i as u32), b);
        }
    }
    fn fast_mem(&mut self) -> Option<FastMem> {
        (self.direct && self.primary_enabled).then(|| FastMem {
            ptr: self.primary.as_mut_ptr(),
            base: PRIMARY,
            len: self.primary.len() as u32,
        })
    }
    fn fast_mem_regions(&mut self) -> [Option<FastMemRegion>; MAX_FAST_MEM_REGIONS] {
        let mut out = [None; MAX_FAST_MEM_REGIONS];
        if self.direct {
            out[0] = Some(FastMemRegion {
                ptr: self.rom.as_mut_ptr(),
                base: ROM,
                len: self.rom.len() as u32,
                writable: false,
            });
            out[1] = Some(FastMemRegion {
                ptr: self.ram.as_mut_ptr(),
                base: RAM,
                len: self.ram.len() as u32,
                writable: true,
            });
        }
        out
    }
}
fn cpu() -> CpuCore {
    let mut c = CpuCore::new();
    c.set_cpu_type(CpuType::M68040);
    c.set_sr(0x2700);
    c.pc = ROM;
    c
}
fn compare(c: &CpuCore, b: &Bus, expected: &CpuCore, memory: &Bus) {
    assert_eq!(c.pc, expected.pc);
    assert_eq!(c.dar, expected.dar);
    assert_eq!(c.get_sr(), expected.get_sr());
    assert_eq!(b.ram, memory.ram);
    assert_eq!(b.rom, memory.rom);
    assert_eq!(b.primary, memory.primary);
}
#[test]
fn rom_code_and_separate_data_banks_match_dispatch() {
    // move.l (a0),d0; addq.l #1,d0; move.l d0,(a1); bra.s loop
    for primary_enabled in [false, true] {
        let mut b = Bus::new(&[0x2010, 0x5280, 0x2280, 0x60f8]);
        b.primary_enabled = primary_enabled;
        let mut c = cpu();
        c.set_a(0, RAM);
        c.set_a(1, RAM);
        let mut reference = b.clone();
        reference.direct = false;
        let mut expected = cpu();
        expected.dar = c.dar;
        for _ in 0..16384 {
            expected.step(&mut reference);
        }
        assert_eq!(c.run_batch(&mut b, 16384, &[]).instructions, 16384);
        compare(&c, &b, &expected, &reference);
        assert!(
            b.reads < reference.reads / 4,
            "ROM and second-bank reads bypass bus"
        );
        assert_eq!(b.writes, 0, "second-bank writes use their own allocation");
    }
}
#[test]
fn readonly_stores_and_rmw_fall_back_including_promoted_rom() {
    for primary_enabled in [false, true] {
        for op in [0x2080, 0x5290] {
            // move.l d0,(a0); addq.l #1,(a0)
            let mut b = Bus::new(&[op, 0x60fc]);
            b.primary_enabled = primary_enabled;
            let mut c = cpu();
            c.set_a(0, ROM + 128);
            c.set_d(0, 0x12345678);
            let mut reference = b.clone();
            reference.direct = false;
            let mut expected = cpu();
            expected.dar = c.dar;
            for _ in 0..8192 {
                expected.step(&mut reference);
            }
            c.run_batch(&mut b, 8192, &[]);
            compare(&c, &b, &expected, &reference);
            assert_eq!(b.writes, reference.writes);
            assert!(b.writes > 0);
        }
    }
}
#[test]
fn crossing_region_end_uses_bus_without_partial_postincrement() {
    let mut b = Bus::new(&[0x22d8]); // move.l (a0)+,(a1)+
    let mut c = cpu();
    c.set_a(0, RAM + 1022);
    c.set_a(1, PRIMARY + 128);
    let mut reference = b.clone();
    reference.direct = false;
    let mut expected = cpu();
    expected.dar = c.dar;
    expected.step(&mut reference);
    c.run_batch(&mut b, 1, &[]);
    compare(&c, &b, &expected, &reference);
    assert_eq!(c.a(0), RAM + 1026);
}
#[test]
fn changed_code_and_replaced_backing_are_seen_on_next_batch() {
    let mut b = Bus::new(&[0x2010, 0x5280, 0x2280, 0x60f8]);
    let mut c = cpu();
    c.set_a(0, RAM);
    c.set_a(1, RAM);
    c.run_batch(&mut b, 16384, &[]);
    b.rom[2..4].copy_from_slice(&0x5480u16.to_be_bytes()); // addq.l #2,d0
    b.ram = vec![0; 1024];
    c.run_batch(&mut b, 4096, &[]);
    assert_eq!(u32::from_be_bytes(b.ram[..4].try_into().unwrap()), 2048);
    b.direct = false; // a watchpoint/debugger can withhold every region
    b.reads = 0;
    c.run_batch(&mut b, 4096, &[]);
    assert_eq!(u32::from_be_bytes(b.ram[..4].try_into().unwrap()), 4096);
    assert!(b.reads >= 4096);
}

#[test]
fn resident_mmu_resolves_extra_regions_and_enforces_both_permissions() {
    let mut b = Bus::new(&[0x4e71]);
    let mut c = cpu();
    for region in b.fast_mem_regions().into_iter().flatten() {
        c.capture_extra_window(region);
    }
    c.pmmu_enabled = true;
    c.mmu_tc = 0x8000;
    c.atc.insert(1, true, RAM, false, false);
    c.atc.insert(2, true, ROM, false, false);
    c.atc.insert(3, true, RAM, true, false);
    c.refresh_window_mmu();
    assert!(c.window_access_off(0x1000, 4, true, false).is_some());
    assert!(c.window_access_off(0x2000, 4, false, true).is_some());
    assert!(
        c.window_access_off(0x2000, 4, true, false).is_none(),
        "ROM permission survives ATC"
    );
    assert!(
        c.window_access_off(0x3000, 4, true, false).is_none(),
        "ATC write protection survives RAM mapping"
    );
    assert!(
        c.window_access_off(0x3ffe, 4, false, false).is_none(),
        "page crossing falls back"
    );
}
