//! Address translation (PMMU table walk)

use crate::core::cpu::CpuCore;
use crate::core::memory::{AddressBus, BusFaultKind};

use super::{MmuFault, MmuFaultCause, MmuFaultKind, MmuResult};

fn buserr(address: u32) -> MmuFault {
    MmuFault {
        kind: MmuFaultKind::BusError,
        address,
        cause: MmuFaultCause::TableWalkBusError,
    }
}

/// Access fault with the generic invalid-page cause (030 walker sites,
/// where the frame does not report the cause).
fn access_fault(address: u32) -> MmuFault {
    access_fault_cause(address, MmuFaultCause::PageFault)
}

fn access_fault_cause(address: u32, cause: MmuFaultCause) -> MmuFault {
    MmuFault {
        kind: MmuFaultKind::AccessLevelViolation,
        address,
        cause,
    }
}

fn config_fault(address: u32) -> MmuFault {
    MmuFault {
        kind: MmuFaultKind::ConfigurationError,
        address,
        cause: MmuFaultCause::PageFault,
    }
}

fn read_u32_phys<B: AddressBus>(bus: &mut B, addr: u32) -> MmuResult<u32> {
    bus.try_read_long(addr).map_err(|f| {
        if matches!(f.kind, BusFaultKind::BusError) {
            buserr(f.address)
        } else {
            buserr(addr)
        }
    })
}

/// Perform 68030/68040 PMMU translation.
///
/// This implementation follows the structure of Musashi's `pmmu_translate_addr()` algorithm.
/// It currently supports:
/// - CRP/SRP selection via TC bit 25 (0x0200_0000)
/// - Root/table modes 2 (4-byte descriptors) and 3 (8-byte descriptors)
/// - Early-termination descriptors (mode 1) at table A/B/C
/// - Transparent Translation Registers (TTRs) for 68030/68040
///
/// TODO:
/// - Access permission checks and precise MMUSR (`mmu_sr`) bits
/// - Page descriptor root mode (root_limit & 3 == 1)
pub fn translate<B: AddressBus>(
    cpu: &mut CpuCore,
    bus: &mut B,
    logical: u32,
    write: bool,
    supervisor: bool,
    instruction: bool,
) -> MmuResult<u32> {
    // If MMU not enabled, identity-map.
    if !cpu.pmmu_enabled || !cpu.has_pmmu {
        return Ok(logical);
    }

    // MOVES carries SFC/DFC instead of the CPU-state function code: the
    // access translates in that address space (mmu.library's 030 MMU
    // detection probes exactly this, remapping the user-data space and
    // reading it back with MOVES from supervisor mode).
    let (supervisor, instruction) = match cpu.mmu_fc_override {
        Some(fc) => ((fc & 4) != 0, (fc & 3) == 2),
        None => (supervisor, instruction),
    };

    // During exception processing, bypass translation to prevent recursive faults.
    // Real hardware uses transparent translation or physical addressing for exception frames.
    if cpu.exception_processing {
        return Ok(logical);
    }

    // Check Transparent Translation Registers first - they bypass page table walk.
    if let Some(phys) = super::ttr::check_transparent_translation(cpu, logical, write, instruction)
    {
        return Ok(phys);
    }

    // The 68040 page table is a fixed three-level format unrelated to the
    // 68030's programmable walk below; dispatch to its own walker.
    if cpu.is_040() || cpu.is_060() {
        return translate_040(cpu, bus, logical, write, supervisor, instruction);
    }

    translate_030(cpu, bus, logical, write, supervisor, instruction)
}

/// One fetched 68030 table descriptor, normalized across the short (4-byte)
/// and long (8-byte) formats. `field` is the address long (the whole
/// descriptor for short format, the second long for long format); `wp` is
/// the write-protect bit and `sup` the long-format supervisor-only bit,
/// both of which accumulate along the walk on real silicon.
struct Desc030 {
    dt: u32,
    field: u32,
    wp: bool,
    sup: bool,
}

fn read_desc_030<B: AddressBus>(
    bus: &mut B,
    base: u32,
    index: u32,
    long: bool,
) -> MmuResult<Desc030> {
    if long {
        let at = base.wrapping_add(index * 8);
        let hi = read_u32_phys(bus, at)?;
        let lo = read_u32_phys(bus, at.wrapping_add(4))?;
        Ok(Desc030 {
            dt: hi & 3,
            field: lo,
            wp: hi & 0x0000_0004 != 0,
            sup: hi & 0x0000_0100 != 0,
        })
    } else {
        let at = base.wrapping_add(index * 4);
        let e = read_u32_phys(bus, at)?;
        Ok(Desc030 {
            dt: e & 3,
            field: e,
            wp: e & 0x0000_0004 != 0,
            sup: false,
        })
    }
}

/// How a 68030 table walk ended.
enum Walk030End {
    /// A page / early-termination descriptor: `field` is its address long,
    /// `consumed` the logical bits eaten by the levels above it.
    Page { field: u32, consumed: u32 },
    /// An invalid (DT=0) descriptor, or an indirect descriptor whose target
    /// is not a page descriptor (`indirect` tells the two apart).
    Invalid { indirect: bool },
    /// The level limit stopped the walk on a valid table pointer (PTEST
    /// with a level operand; a full walk never ends here).
    LevelStop,
    /// A bus error while fetching a descriptor at this physical address.
    BusErr { at: u32 },
    /// Unusable configuration: invalid root DT or zero configured levels.
    Config,
}

/// A finished 68030 walk: the outcome plus the accumulated protection bits,
/// the number of descriptors fetched, and the physical address of the last
/// descriptor examined (what PTEST's A bit hands back for lazy table fills).
struct Walk030 {
    end: Walk030End,
    wp: bool,
    sup: bool,
    levels: u32,
    desc_addr: u32,
}

/// The 68030 programmable table walk, shared between address translation
/// and PTEST.
///
/// TC configures an optional function-code level (FCL, bit 24) followed by
/// up to four address-indexed levels (TIA/TIB/TIC/TID, bits 15:0), with IS
/// (bits 19:16) leading address bits ignored; the page offset is whatever
/// logical bits the configured levels leave unconsumed (PS, bits 23:20, is
/// redundant with that on a well-formed TC and is not consulted). At every
/// level a descriptor can be: invalid (DT=0), a page / early termination
/// descriptor (DT=1, maps the whole remaining space), or a pointer to the
/// next table in short or long format (DT=2/3). A pointer found when no
/// configured level remains is an *indirect* descriptor: its address field
/// names the real page descriptor, fetched in the pointed-to format, which
/// must itself be DT=1 (mmu.library shares per-page descriptors between its
/// user and supervisor trees this way, exactly as on the 68040; the fetch
/// counts as a walk level, which is how its lazy fault handler locates the
/// poisoned target with level-limited PTESTs). Write-protect accumulates
/// across all fetched descriptors; the long-format S bit restricts a
/// subtree to supervisor accesses. `max_levels` bounds how many descriptors
/// are fetched (PTEST's level operand); `u32::MAX` walks to completion.
///
/// Not modelled: CRP/long-descriptor limit checks, the DT=1 page-descriptor
/// root, and used/modified descriptor write-back.
fn walk_030<B: AddressBus>(
    cpu: &CpuCore,
    bus: &mut B,
    logical: u32,
    fc: u32,
    max_levels: u32,
) -> Walk030 {
    let supervisor = fc & 4 != 0;
    // Root pointer selection: if SRE set and supervisor space, SRP; else CRP.
    let use_srp = (cpu.mmu_tc & 0x0200_0000) != 0 && supervisor;
    let (root_aptr, root_limit) = if use_srp {
        (cpu.mmu_srp_aptr, cpu.mmu_srp_limit)
    } else {
        (cpu.mmu_crp_aptr, cpu.mmu_crp_limit)
    };

    let fcl = (cpu.mmu_tc & 0x0100_0000) != 0;
    let is = (cpu.mmu_tc >> 16) & 0xF;
    let ti = [
        (cpu.mmu_tc >> 12) & 0xF, // TIA
        (cpu.mmu_tc >> 8) & 0xF,  // TIB
        (cpu.mmu_tc >> 4) & 0xF,  // TIC
        cpu.mmu_tc & 0xF,         // TID
    ];

    #[inline]
    fn top_index(addr: u32, left_shift: u32, bits: u32) -> u32 {
        // bits is 1..=15 for a configured level.
        addr.wrapping_shl(left_shift) >> (32 - bits)
    }

    let mut w = Walk030 {
        end: Walk030End::Config,
        wp: false,
        sup: false,
        levels: 0,
        desc_addr: root_aptr,
    };

    // Walk state: current table pointer + descriptor format and the logical
    // bits consumed so far. This is the per-access hot path (the 030 walk
    // has no ATC), so no allocation.
    let mut long = match root_limit & 3 {
        2 => false,
        3 => true,
        // DT=0 root is invalid; the DT=1 page-descriptor root is not
        // implemented (nothing exercises it).
        _ => return w,
    };
    let mut ptr = root_aptr & 0xFFFF_FFF0;
    let mut consumed = is;

    // An FC level first when FCL is set (consumes no logical bits), then
    // the configured TIA/TIB/TIC/TID levels.
    let n_addr = ti.iter().take_while(|&&b| b != 0).count();
    let n = n_addr + fcl as usize;
    if n == 0 {
        return w;
    }

    for li in 0..n {
        if w.levels >= max_levels {
            w.end = Walk030End::LevelStop;
            return w;
        }
        let index = if fcl && li == 0 {
            fc
        } else {
            let bits = ti[li - fcl as usize];
            let idx = top_index(logical, consumed, bits);
            consumed += bits;
            idx
        };
        w.desc_addr = ptr.wrapping_add(index * if long { 8 } else { 4 });
        let d = match read_desc_030(bus, ptr, index, long) {
            Ok(d) => d,
            Err(f) => {
                w.end = Walk030End::BusErr { at: f.address };
                return w;
            }
        };
        w.levels += 1;

        match d.dt {
            0 => {
                w.end = Walk030End::Invalid { indirect: false };
                return w;
            }
            1 => {
                w.wp |= d.wp;
                w.sup |= d.sup;
                w.end = Walk030End::Page {
                    field: d.field,
                    consumed,
                };
                return w;
            }
            _ => {
                if li + 1 < n {
                    // Pointer to the next table level.
                    w.wp |= d.wp;
                    w.sup |= d.sup;
                    ptr = d.field & 0xFFFF_FFF0;
                    long = d.dt == 3;
                } else {
                    // Walk exhausted: DT=2/3 here is an indirect descriptor
                    // naming the real page descriptor (in the pointed-to
                    // format), which must itself be a page descriptor. The
                    // target fetch belongs to this same walk level (it does
                    // not count towards N or the PTEST level limit), and an
                    // indirect descriptor carries no protection bits -- its
                    // address field reaches down to bit 2 -- so only the
                    // fetched target contributes WP/S. PTEST's A bit hands
                    // back the target's address: that is the shared slot
                    // mmu.library's lazy fault handler materializes.
                    w.desc_addr = d.field & 0xFFFF_FFFC;
                    let t = match read_desc_030(bus, w.desc_addr, 0, d.dt == 3) {
                        Ok(t) => t,
                        Err(f) => {
                            w.end = Walk030End::BusErr { at: f.address };
                            return w;
                        }
                    };
                    w.wp |= t.wp;
                    w.sup |= t.sup;
                    w.end = if t.dt == 1 {
                        Walk030End::Page {
                            field: t.field,
                            consumed,
                        }
                    } else {
                        Walk030End::Invalid { indirect: true }
                    };
                    return w;
                }
            }
        }
    }
    unreachable!("the last level always terminates or indirects");
}

#[inline]
fn low_bits(addr: u32, shift: u32) -> u32 {
    if shift >= 32 {
        0
    } else {
        addr.wrapping_shl(shift) >> shift
    }
}

/// 68030 address translation on top of [`walk_030`], enforcing the
/// accumulated write-protect and supervisor-only bits for the access.
fn translate_030<B: AddressBus>(
    cpu: &mut CpuCore,
    bus: &mut B,
    logical: u32,
    write: bool,
    supervisor: bool,
    instruction: bool,
) -> MmuResult<u32> {
    let fc = if supervisor {
        if instruction { 6 } else { 5 }
    } else if instruction {
        2
    } else {
        1
    };
    let w = walk_030(cpu, bus, logical, fc, u32::MAX);
    match w.end {
        Walk030End::Page { field, consumed } => {
            if w.sup && !supervisor {
                return Err(access_fault_cause(
                    logical,
                    MmuFaultCause::SupervisorProtect,
                ));
            }
            if write && w.wp {
                return Err(access_fault_cause(logical, MmuFaultCause::WriteProtect));
            }
            let base = field & 0xFFFF_FF00;
            Ok(low_bits(logical, consumed).wrapping_add(base))
        }
        Walk030End::Invalid { indirect: false } => Err(access_fault(logical)),
        Walk030End::Invalid { indirect: true } => {
            Err(access_fault_cause(logical, MmuFaultCause::Indirect))
        }
        Walk030End::BusErr { at } => Err(buserr(at)),
        Walk030End::Config => Err(config_fault(logical)),
        // A full walk never stops on the level limit.
        Walk030End::LevelStop => Err(config_fault(logical)),
    }
}

/// 68030 PTEST: walk the tables for `logical` in the `fc` address space,
/// fetching at most `level` descriptors (level 0 tests only transparent
/// translation), and compose the 16-bit MMUSR: B (bit 15, bus error during
/// search), S (bit 13, supervisor-only mapping probed from a user space),
/// W (bit 11, write-protect accumulated along the walk -- reported for
/// reads too), I (bit 10, invalid translation), T (bit 6, transparent-
/// translation hit, level 0 only), N (bits 2:0, descriptors fetched).
/// Returns the MMUSR and the physical address of the last descriptor
/// examined (handed back through PTEST's A bit; mmu.library's lazy fault
/// handler locates the shared descriptor slot to materialize with exactly
/// this). The L (limit) and M (modified) bits are not modelled.
pub(crate) fn ptest_030<B: AddressBus>(
    cpu: &CpuCore,
    bus: &mut B,
    logical: u32,
    fc: u32,
    write: bool,
    level: u32,
) -> (u16, u32) {
    if level == 0 {
        let fc8 = fc as u8;
        let t = super::ttr::ttr_matches(cpu.mmu_tt0, logical, fc8, write)
            || super::ttr::ttr_matches(cpu.mmu_tt1, logical, fc8, write);
        return (if t { 0x0040 } else { 0 }, 0);
    }
    let w = walk_030(cpu, bus, logical, fc, level);
    let n = (w.levels & 7) as u16;
    let mut sr = n;
    if w.wp {
        sr |= 0x0800; // W
    }
    if w.sup && fc & 4 == 0 {
        sr |= 0x2000; // S: supervisor-only mapping probed from user space
    }
    match w.end {
        Walk030End::Page { .. } | Walk030End::LevelStop => {}
        Walk030End::Invalid { .. } => sr |= 0x0400, // I
        Walk030End::BusErr { .. } | Walk030End::Config => sr |= 0x8000, // B
    }
    (sr, w.desc_addr)
}

/// Perform 68040 PMMU translation.
///
/// The 68040 uses a fixed three-level table (root -> pointer -> page) with
/// 4-byte descriptors, indexed by logical bits [31:25] / [24:18] / [17:12]
/// (4 KB pages) or [17:13] (8 KB pages, TC bit 14 set). The root pointer is
/// URP in user mode and SRP in supervisor mode (no TC bit-25 gate -- that is
/// 68030-only). Table-level descriptors use UDT (bits [1:0]): >=2 = resident;
/// page descriptors use PDT (bits [1:0]): 0 = invalid, 2 = indirect, 1/3 =
/// resident.
///
/// A *data* access through an invalid/unconfigured descriptor raises an access
/// fault (this is how Enforcer/MuForce catch low-memory and freed-memory hits).
/// An *instruction fetch* through an invalid descriptor instead falls back to
/// identity translation: a 68040 enables TC before all of its code is mapped
/// during boot, and faulting the fetch stream there would derail it (the
/// codebase's "safe direction"). Resident pages additionally enforce the W
/// (write-protect) and S (supervisor-only) descriptor bits.
fn translate_040<B: AddressBus>(
    cpu: &mut CpuCore,
    bus: &mut B,
    logical: u32,
    write: bool,
    supervisor: bool,
    instruction: bool,
) -> MmuResult<u32> {
    // Invalid-descriptor outcome: fault on data, identity-fallback on a
    // fetch. The cause records which walk level held the invalid
    // descriptor (the 68060 FSLW reports it).
    let invalid = |logical: u32, cause: MmuFaultCause| -> MmuResult<u32> {
        if instruction {
            Ok(logical)
        } else {
            Err(access_fault_cause(logical, cause))
        }
    };
    // Page size: TC bit 14 (P) selects 8 KB, else 4 KB.
    let page_bits = if cpu.mmu_tc & 0x0000_4000 != 0 { 13 } else { 12 };
    let page_mask = (1u32 << page_bits) - 1;

    // ATC fast path: a recent walk for this page avoids the descriptor fetches.
    // A cached entry the access would violate (write to a write-protected page,
    // user access to a supervisor page) misses here, so we re-walk and fault.
    let page_frame = logical >> page_bits;
    if let Some(phys_page) = cpu.atc.lookup(page_frame, supervisor, write) {
        return Ok(phys_page | (logical & page_mask));
    }

    // Root pointer: SRP in supervisor mode, URP (stored in mmu_crp_aptr) in user
    // mode. The 128-entry root table is 512-byte aligned.
    let root = if supervisor {
        cpu.mmu_srp_aptr
    } else {
        cpu.mmu_crp_aptr
    };

    // Level 1: root table, indexed by logical[31:25] (128 x 4 bytes).
    let root_idx = (logical >> 25) & 0x7F;
    let root_desc = read_u32_phys(bus, (root & 0xFFFF_FE00).wrapping_add(root_idx * 4))?;
    if root_desc & 3 < 2 {
        // UDT invalid: data faults, fetch falls back
        return invalid(logical, MmuFaultCause::PointerA);
    }

    // Level 2: pointer table (512-byte aligned), indexed by logical[24:18].
    let ptr_table = root_desc & 0xFFFF_FE00;
    let ptr_idx = (logical >> 18) & 0x7F;
    let ptr_desc = read_u32_phys(bus, ptr_table.wrapping_add(ptr_idx * 4))?;
    if ptr_desc & 3 < 2 {
        // UDT invalid: data faults, fetch falls back
        return invalid(logical, MmuFaultCause::PointerB);
    }

    // Level 3: page table. With 4 KB pages it has 64 entries (256-byte aligned,
    // indexed by logical[17:12]); with 8 KB pages, 32 entries (128-byte aligned,
    // indexed by logical[17:13]).
    let (page_table_mask, page_idx) = if page_bits == 13 {
        (0xFFFF_FF80u32, (logical >> 13) & 0x1F)
    } else {
        (0xFFFF_FF00u32, (logical >> 12) & 0x3F)
    };
    let page_table = ptr_desc & page_table_mask;
    let mut page_desc = read_u32_phys(bus, page_table.wrapping_add(page_idx * 4))?;

    // PDT: 0 invalid, 2 indirect (the descriptor points to the real page
    // descriptor), 1/3 resident.
    let was_indirect = page_desc & 3 == 2;
    if was_indirect {
        page_desc = read_u32_phys(bus, page_desc & 0xFFFF_FFFC)?;
    }
    if page_desc & 3 == 0 {
        // PDT invalid: data faults, fetch falls back
        return invalid(
            logical,
            if was_indirect {
                MmuFaultCause::Indirect
            } else {
                MmuFaultCause::PageFault
            },
        );
    }

    // Protection bits: W (write-protect, bit 2) accumulates across the table and
    // page descriptors; S (supervisor-only, bit 7) lives on the page descriptor.
    // A violating access faults (resumable on the 040, vector 2 / format 7).
    let write_protected = (root_desc | ptr_desc | page_desc) & 0x0000_0004 != 0;
    let supervisor_only = page_desc & 0x0000_0080 != 0;
    if write && write_protected {
        return Err(access_fault_cause(logical, MmuFaultCause::WriteProtect));
    }
    if !supervisor && supervisor_only {
        return Err(access_fault_cause(
            logical,
            MmuFaultCause::SupervisorProtect,
        ));
    }

    let phys_page = page_desc & !page_mask;
    // Cache only real resident translations -- never the identity fallbacks
    // above, so a page that is later given a valid mapping (after PFLUSH) is not
    // masked by a stale identity entry. The protection bits ride along so a
    // later violating access to the same page is caught on the ATC path too.
    cpu.atc
        .insert(page_frame, supervisor, phys_page, write_protected, supervisor_only);
    Ok(phys_page | (logical & page_mask))
}
