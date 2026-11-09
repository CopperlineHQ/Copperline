//! Hunk/LINE/DWARF and ELF debug information, with no guest execution or
//! access to source files named in the debug records.

use copperline::debuginfo::DebugInfo;

pub fn exercise(data: &[u8]) {
    if let Ok(mut info) = DebugInfo::load(data, None) {
        info.relocate(vec![0x10000; info.hunks.len()]);
        let _ = info.line_for(0x10000);
        let _ = info.symbol_at(0x10000);
        let _ = info.bartman_unwind_table();
    }
    let mut info = DebugInfo::default();
    let _ = info.add_elf(data);
}
