//! Transparent Translation Register (TTR) handling.
//!
//! Implements TTR matching for 68030 (TT0/TT1) and 68040 (ITT0/ITT1, DTT0/DTT1).
//! TTRs allow certain address ranges to bypass page table translation.

use crate::core::cpu::CpuCore;
use crate::core::types::CpuType;

/// TTR register format (68030/68040):
/// ```text
/// [31:24] Base Address (compared against address[31:24])
/// [23:16] Address Mask (1 = ignore bit during comparison)
/// [15]    E: Enable
/// [14]    CI: Cache Inhibit (ignored by us)
/// [13]    R/W: 0=read-only, 1=read/write (68030) or W (68040)
/// [12]    RWM: R/W Mask (68030) or 0 (68040)
/// [10:8]  FC Base (function code to match)
/// [4:2]   FC Mask (1 = ignore FC bit)
/// ```
const TTR_ENABLE: u32 = 0x8000;
const TTR_BASE_MASK: u32 = 0xFF00_0000;
const TTR_ADDR_MASK_SHIFT: u32 = 16;
const TTR_FC_BASE_SHIFT: u32 = 8;
const TTR_FC_MASK_SHIFT: u32 = 2;

/// Check if a single TTR matches the given address and function code.
///
/// Returns `true` if the TTR is enabled and matches.
pub fn ttr_matches(ttr: u32, addr: u32, fc: u8, _write: bool) -> bool {
    // Check enable bit
    if (ttr & TTR_ENABLE) == 0 {
        return false;
    }

    // Extract fields
    let base = (ttr & TTR_BASE_MASK) >> 24;
    let addr_mask = (ttr >> TTR_ADDR_MASK_SHIFT) & 0xFF;
    let fc_base = ((ttr >> TTR_FC_BASE_SHIFT) & 0x07) as u8;
    let fc_mask = ((ttr >> TTR_FC_MASK_SHIFT) & 0x07) as u8;

    // Compare address (masked)
    let addr_high = (addr >> 24) & 0xFF;
    let addr_match = (addr_high & !addr_mask) == (base & !addr_mask);

    // Compare function code (masked)
    let fc_match = (fc & !fc_mask) == (fc_base & !fc_mask);

    addr_match && fc_match
}

/// Check if transparent translation applies for the given access.
///
/// For 68030: Checks TT0 and TT1.
/// For 68040: Checks ITT0/ITT1 for instruction accesses, DTT0/DTT1 for data.
///
/// Returns `Some(physical_addr)` if transparent translation applies (identity mapping),
/// or `None` if normal page table translation should be used.
pub fn check_transparent_translation(
    cpu: &CpuCore,
    addr: u32,
    write: bool,
    instruction: bool,
) -> Option<u32> {
    // Determine function code based on access type and privilege level
    let fc = compute_function_code(cpu, instruction);

    match cpu.cpu_type {
        CpuType::M68030 => {
            // 68030 has two shared TTRs for both instruction and data
            if ttr_matches(cpu.mmu_tt0, addr, fc, write) {
                return Some(addr);
            }
            if ttr_matches(cpu.mmu_tt1, addr, fc, write) {
                return Some(addr);
            }
        }
        CpuType::M68EC040 | CpuType::M68LC040 | CpuType::M68040 => {
            if instruction {
                // Instruction access: check ITT0, ITT1
                if ttr_matches(cpu.itt0, addr, fc, write) {
                    return Some(addr);
                }
                if ttr_matches(cpu.itt1, addr, fc, write) {
                    return Some(addr);
                }
            } else {
                // Data access: check DTT0, DTT1
                if ttr_matches(cpu.dtt0, addr, fc, write) {
                    return Some(addr);
                }
                if ttr_matches(cpu.dtt1, addr, fc, write) {
                    return Some(addr);
                }
            }
        }
        _ => {
            // Other CPUs don't have TTRs
        }
    }

    None
}

/// Compute function code for the current access.
///
/// FC is a 3-bit value:
/// - 0: Reserved
/// - 1: User Data
/// - 2: User Program (instruction)
/// - 3: Reserved
/// - 4: Reserved
/// - 5: Supervisor Data
/// - 6: Supervisor Program (instruction)
/// - 7: CPU Space (interrupt acknowledge, etc.)
fn compute_function_code(cpu: &CpuCore, instruction: bool) -> u8 {
    // A MOVES data access carries SFC/DFC instead of the CPU-state code.
    if let Some(fc) = cpu.mmu_fc_override {
        return fc;
    }
    let is_supervisor = cpu.is_supervisor();
    match (is_supervisor, instruction) {
        (false, false) => 1, // User Data
        (false, true) => 2,  // User Program
        (true, false) => 5,  // Supervisor Data
        (true, true) => 6,   // Supervisor Program
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ttr_disabled() {
        // TTR with E=0 should not match
        let ttr = 0x0000_0000; // Disabled
        assert!(!ttr_matches(ttr, 0x1000_0000, 5, false));
    }

    #[test]
    fn test_ttr_address_match() {
        // TTR matching addresses 0x40xxxxxx (base=0x40, mask=0x00)
        // FCMask=7 (bits 4:2) to match any FC
        let ttr = 0x4000_801C; // Base=0x40, Mask=0x00, E=1, FCMask=7
        assert!(ttr_matches(ttr, 0x4000_0000, 5, false));
        assert!(ttr_matches(ttr, 0x40FF_FFFF, 5, false));
        assert!(!ttr_matches(ttr, 0x4100_0000, 5, false));
        assert!(!ttr_matches(ttr, 0x3F00_0000, 5, false));
    }

    #[test]
    fn test_ttr_address_mask() {
        // TTR matching addresses 0x40-0x4F (base=0x40, mask=0x0F)
        // FCMask=7 (bits 4:2) to match any FC
        let ttr = 0x400F_801C; // Base=0x40, Mask=0x0F, E=1, FCMask=7
        assert!(ttr_matches(ttr, 0x4000_0000, 5, false));
        assert!(ttr_matches(ttr, 0x4F00_0000, 5, false));
        assert!(!ttr_matches(ttr, 0x5000_0000, 5, false));
    }

    #[test]
    fn test_ttr_fc_match() {
        // TTR matching FC=5 (supervisor data) only
        let ttr = 0x4000_8500; // Base=0x40, E=1, FC=5, FCMask=0
        assert!(ttr_matches(ttr, 0x4000_0000, 5, false));
        assert!(!ttr_matches(ttr, 0x4000_0000, 1, false)); // User data
        assert!(!ttr_matches(ttr, 0x4000_0000, 6, false)); // Supervisor program
    }

    #[test]
    fn test_ttr_fc_mask() {
        // TTR matching any supervisor access (FC=4-7, FCMask=3)
        let ttr = 0x4000_840C; // Base=0x40, E=1, FC=4, FCMask=3
        assert!(ttr_matches(ttr, 0x4000_0000, 4, false));
        assert!(ttr_matches(ttr, 0x4000_0000, 5, false));
        assert!(ttr_matches(ttr, 0x4000_0000, 6, false));
        assert!(ttr_matches(ttr, 0x4000_0000, 7, false));
        assert!(!ttr_matches(ttr, 0x4000_0000, 1, false)); // User data
    }
}
