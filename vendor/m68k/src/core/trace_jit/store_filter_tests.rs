//! The inline store filter for tracked windows (`AddressBus::tracked_store_filter`).
//!
//! The oracle is the bus itself: every store that reaches it is counted. Once a
//! loop runs as native code, a store the filter clears must produce no bus
//! write, one it marks must produce exactly one, and in every case guest RAM
//! and registers must equal a run without any filter.
use super::*;
use crate::core::memory::TrackedMem;

const RAM: usize = 0x1_0000;
const PAGES: usize = RAM >> 12;
const LOOP: u32 = 0x1000;

struct Bus {
    ram: Vec<u8>,
    filter: Option<Vec<u8>>,
    writes: usize,
    copies: usize,
    /// When set, the bus clears this page's filter byte inside its own write
    /// callback, to show generated code re-reads the filter after a callback.
    clear_on_write: Option<usize>,
}

impl Bus {
    fn new(program: &[u16], filter: bool) -> Self {
        let mut ram = vec![0u8; RAM];
        for (i, word) in program.iter().enumerate() {
            let at = LOOP as usize + 2 * i;
            ram[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
        Self {
            ram,
            filter: filter.then(|| vec![0u8; 1 + PAGES + 1]),
            writes: 0,
            copies: 0,
            clear_on_write: None,
        }
    }
    fn mark(&mut self, page: usize) {
        self.filter.as_mut().unwrap()[1 + page] = 1;
    }
    fn long(&self, address: u32) -> u32 {
        let a = address as usize;
        u32::from_be_bytes(self.ram[a..a + 4].try_into().unwrap())
    }
    fn store(&mut self, address: u32, value: u32, bytes: u32) {
        self.writes += 1;
        for i in 0..bytes {
            self.ram[(address + i) as usize] = (value >> ((bytes - 1 - i) * 8)) as u8;
        }
        if let (Some(page), Some(filter)) = (self.clear_on_write, self.filter.as_mut()) {
            filter[1 + page] = 0;
        }
    }
}

impl AddressBus for Bus {
    fn read_byte(&mut self, a: u32) -> u8 {
        self.ram[a as usize]
    }
    fn read_word(&mut self, a: u32) -> u16 {
        u16::from_be_bytes([self.ram[a as usize], self.ram[a as usize + 1]])
    }
    fn read_long(&mut self, a: u32) -> u32 {
        self.long(a)
    }
    fn write_byte(&mut self, a: u32, v: u8) {
        self.store(a, u32::from(v), 1);
    }
    fn write_word(&mut self, a: u32, v: u16) {
        self.store(a, u32::from(v), 2);
    }
    fn write_long(&mut self, a: u32, v: u32) {
        self.store(a, v, 4);
    }
    fn begin_memory_copy(&mut self, _source: u32, _bytes: u32) -> bool {
        self.copies += 1;
        false
    }
    fn tracked_mem(&mut self) -> Option<TrackedMem> {
        Some(TrackedMem {
            ptr: self.ram.as_ptr(),
            base: 0,
            len: RAM as u32,
        })
    }
    fn tracked_store_filter(&mut self) -> *const u8 {
        self.filter
            .as_deref()
            .map_or(std::ptr::null(), <[u8]>::as_ptr)
    }
}

fn fresh_cpu(a0: u32, a1: u32) -> CpuCore {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    cpu.set_sr(0x2700);
    cpu.pc = LOOP;
    cpu.set_a(0, a0);
    cpu.set_a(1, a1);
    cpu.set_a(7, 0x8000);
    cpu
}

/// `move.l d0,(a0) / addq.l #1,d0 / bra.s loop`.
const STORE: [u16; 3] = [0x2080, 0x5280, 0x60fa];
/// `move.l (a1),(a0) / addq.l #1,(a1)... ` is not needed: a copy store is
/// `move.l (a1),(a0)`, then `addq.l #1,d0 / bra.s loop`.
const COPY: [u16; 3] = [0x2091, 0x5280, 0x60fa];

/// Warm the loop until it runs natively, then run a measured stretch and
/// return the bus writes it made.
fn measured(cpu: &mut CpuCore, bus: &mut Bus) -> usize {
    for _ in 0..200 {
        cpu.run_batch(bus, 300, &[]);
    }
    bus.writes = 0;
    bus.copies = 0;
    cpu.run_batch(bus, 3_000, &[]);
    bus.writes
}

/// Same program with no filter: every store reaches the bus.
fn reference(program: &[u16], a0: u32, a1: u32) -> (CpuCore, Bus, usize) {
    let mut bus = Bus::new(program, false);
    let mut cpu = fresh_cpu(a0, a1);
    let writes = measured(&mut cpu, &mut bus);
    (cpu, bus, writes)
}

fn same_state(a: &CpuCore, a_bus: &Bus, b: &CpuCore, b_bus: &Bus) {
    assert_eq!(a.dar, b.dar, "registers");
    assert_eq!(a.pc, b.pc, "pc");
    assert!(a_bus.ram == b_bus.ram, "guest RAM");
}

#[test]
fn clear_pages_store_directly_with_identical_results() {
    let (ref_cpu, ref_bus, ref_writes) = reference(&STORE, 0x4000, 0);
    assert_eq!(
        ref_writes, 1_000,
        "without a filter every store reaches the bus"
    );

    let mut bus = Bus::new(&STORE, true);
    let mut cpu = fresh_cpu(0x4000, 0);
    assert_eq!(
        measured(&mut cpu, &mut bus),
        0,
        "clear pages bypass the bus"
    );
    same_state(&cpu, &bus, &ref_cpu, &ref_bus);
    assert_eq!(
        bus.long(0x4000),
        cpu.d(0).wrapping_sub(1),
        "the last store landed"
    );
}

#[test]
fn a_marked_page_or_the_global_byte_keeps_the_bus_path() {
    let (ref_cpu, ref_bus, ref_writes) = reference(&STORE, 0x4000, 0);

    let mut bus = Bus::new(&STORE, true);
    bus.mark(4);
    let mut cpu = fresh_cpu(0x4000, 0);
    assert_eq!(measured(&mut cpu, &mut bus), ref_writes);
    same_state(&cpu, &bus, &ref_cpu, &ref_bus);

    let mut bus = Bus::new(&STORE, true);
    bus.filter.as_mut().unwrap()[0] = 1;
    let mut cpu = fresh_cpu(0x4000, 0);
    assert_eq!(measured(&mut cpu, &mut bus), ref_writes);
    same_state(&cpu, &bus, &ref_cpu, &ref_bus);

    // An unrelated marked page changes nothing.
    let mut bus = Bus::new(&STORE, true);
    bus.mark(5);
    let mut cpu = fresh_cpu(0x4000, 0);
    assert_eq!(measured(&mut cpu, &mut bus), 0);
    same_state(&cpu, &bus, &ref_cpu, &ref_bus);
}

#[test]
fn a_store_straddling_two_pages_checks_the_second() {
    // A long at 0x4ffe covers pages 4 and 5; only page 5 is marked.
    let (ref_cpu, ref_bus, ref_writes) = reference(&STORE, 0x4ffe, 0);
    let mut bus = Bus::new(&STORE, true);
    bus.mark(5);
    let mut cpu = fresh_cpu(0x4ffe, 0);
    assert_eq!(measured(&mut cpu, &mut bus), ref_writes);
    same_state(&cpu, &bus, &ref_cpu, &ref_bus);
}

#[test]
fn a_copy_store_also_checks_its_source_pages() {
    // Source at 0x6000 (page 6), destination at 0x4000 (page 4).
    let (ref_cpu, ref_bus, ref_writes) = reference(&COPY, 0x4000, 0x6000);
    assert!(ref_bus.writes > 0);

    let mut bus = Bus::new(&COPY, true);
    bus.mark(6);
    let mut cpu = fresh_cpu(0x4000, 0x6000);
    assert_eq!(measured(&mut cpu, &mut bus), ref_writes, "marked source");
    assert_eq!(bus.copies, ref_writes, "each copy still notifies the bus");
    same_state(&cpu, &bus, &ref_cpu, &ref_bus);

    let mut bus = Bus::new(&COPY, true);
    let mut cpu = fresh_cpu(0x4000, 0x6000);
    assert_eq!(measured(&mut cpu, &mut bus), 0, "both pages clear");
    assert_eq!(bus.copies, 0);
    same_state(&cpu, &bus, &ref_cpu, &ref_bus);
}

#[test]
fn generated_code_rereads_the_filter_after_a_bus_callback() {
    let (ref_cpu, ref_bus, _) = reference(&STORE, 0x4000, 0);
    let mut bus = Bus::new(&STORE, true);
    let mut cpu = fresh_cpu(0x4000, 0);
    for _ in 0..200 {
        cpu.run_batch(&mut bus, 300, &[]);
    }
    // Mark the page, and have the bus clear it again from inside the first
    // store that reaches it. Exactly that one store may use the bus.
    bus.mark(4);
    bus.clear_on_write = Some(4);
    bus.writes = 0;
    cpu.run_batch(&mut bus, 3_000, &[]);
    assert_eq!(bus.writes, 1);
    same_state(&cpu, &bus, &ref_cpu, &ref_bus);
}
