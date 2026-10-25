//! m68k CLI

use m68k::{CpuCore, CpuType};

fn main() {
    println!("m68k - M68000 Family CPU Emulator");
    println!("==================================\n");

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68000);

    println!("CPU Type: {:?}", cpu.cpu_type);
    println!("Initial PC: ${:08X}", cpu.pc);
    println!("Initial SR: ${:04X}", cpu.get_sr());
    println!("\nReady for development!");
}
