//! Generation-specific PMMU table walks and PTEST evaluation.

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

/// Translate one logical address through the configured 68030, 68040, or
/// 68060 PMMU.
///
/// The 68030 path selects CRP/SRP and supports short and long table
/// descriptors, early termination, indirect descriptors, function-code
/// levels, transparent translation, write protection, and supervisor-only
/// subtrees. The 68040/68060 path implements their fixed three-level table,
/// indirect pages, 4/8 KiB pages, ITT/DTT bypass, protection, and ATC fills.
///
/// If the selected CPU has no enabled PMMU, the address is returned
/// unchanged. Translation and table-fetch failures return [`MmuFault`] with
/// the address and cause required by the CPU's exception-frame builder.
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

    // Exception frame pushes and vector fetches translate like any other
    // access on real silicon: supervisor stacks and VBR hold logical
    // addresses (Linux/m68k runs its kernel at virtual 0 with RAM at
    // 0x04000000 -- an untranslated frame write would land in the wrong
    // physical page). A translation fault raised while exception_processing
    // is set re-enters take_exception, whose double-fault guard halts the
    // CPU -- the same double bus fault real hardware takes.

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
    // bits consumed so far. Every ATC miss and PTEST comes through here, so
    // no allocation.
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

/// The 68030 ATC's page shift for this TC: the logical bits a full-depth
/// walk leaves untranslated (IS plus every configured TIx level, as
/// [`walk_030`] consumes them). That is the smallest region any page
/// descriptor maps, so one entry per such page is exact for every
/// descriptor, early termination included. `None` for a TC whose levels
/// consume the whole address (no page left to cache) or none of it (no
/// address bits to tag a page with), so the shift is always 1..=31.
fn page_shift_030(tc: u32) -> Option<u32> {
    let is = (tc >> 16) & 0xF;
    let levels: u32 = [(tc >> 12) & 0xF, (tc >> 8) & 0xF, (tc >> 4) & 0xF, tc & 0xF]
        .into_iter()
        .take_while(|&bits| bits != 0)
        .sum();
    let consumed = is + levels;
    (1..32).contains(&consumed).then(|| 32 - consumed)
}

const RESIDENT_OFF: u8 = 0;
const RESIDENT_030: u8 = 1;
const RESIDENT_040: u8 = 2;

/// The translation-control state [`probe_resident`] consults, gathered
/// from TC and the TT registers once instead of on every access. The batch
/// executor captures it when it installs the fastmem window and again after
/// every instruction it fully dispatches (the only kind that writes TC or a
/// TT register), so it always describes the live registers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ResidentMmu {
    /// `RESIDENT_OFF` (no enabled PMMU), `RESIDENT_030` or `RESIDENT_040`.
    kind: u8,
    /// The 68030 ATC page shift (0 when TC leaves no page to cache), or the
    /// 68040/68060 page size in bits.
    shift: u8,
    /// Whether a transparent-translation register the access path checks
    /// is enabled.
    tt: bool,
    /// Byte mask of the smallest page any translation or interpreter page
    /// split uses: an access inside one such page translates through a
    /// single TT match or ATC entry, exactly as the interpreter would.
    pub(crate) page_mask: u32,
}

impl ResidentMmu {
    /// The state for `cpu`'s current PMMU registers.
    pub(crate) fn of(cpu: &CpuCore) -> Self {
        if !cpu.has_pmmu || !cpu.pmmu_enabled {
            return Self::default();
        }
        let tt = super::ttr::any_transparent_translation(cpu);
        if cpu.is_040() || cpu.is_060() {
            let page_bits = if cpu.mmu_tc & 0x0000_4000 != 0 {
                13
            } else {
                12
            };
            return Self {
                kind: RESIDENT_040,
                shift: page_bits,
                tt,
                page_mask: (1 << page_bits) - 1,
            };
        }
        let shift = page_shift_030(cpu.mmu_tc).unwrap_or(0);
        // The interpreter splits accesses at TC.PS pages; a malformed TC can
        // make the ATC's pages smaller still.
        let atc_mask = if shift == 0 {
            u32::MAX
        } else {
            u32::MAX >> (32 - shift)
        };
        Self {
            kind: RESIDENT_030,
            shift: shift as u8,
            tt,
            page_mask: cpu.mmu_page_mask().min(atc_mask),
        }
    }

    /// Whether a PMMU is enabled (the batch window must translate).
    #[inline(always)]
    pub(crate) fn enabled(self) -> bool {
        self.kind != RESIDENT_OFF
    }
}

/// Translation without a table search, for the batch fastmem paths: the
/// physical address the enabled PMMU's transparent-translation registers or
/// ATC give `logical`, or `None` when only a table search could tell -- an
/// ATC miss, or a cached translation this access would fault on (a write to
/// a write-protected page, a user access to a supervisor-only one). It is
/// [`translate`]'s hit path against the same registers and ATC, so a `Some`
/// is the address `translate` would return, and a `None` leaves the access
/// to `translate` to search the tables and raise any fault.
///
/// It leaves the 68030 ATC's replacement order as it was, and so does
/// `translate` itself while the window translates: no hit in a batch
/// records a use, whether it came through the window, a compiled trace or
/// the interpreter's bus path, so entries age by when table searches loaded
/// them. Table searches happen at the same instructions whichever way an
/// instruction ran, so the order does not depend on which instructions ran
/// from compiled traces (which reach memory through the window and make no
/// instruction fetches): a state saved mid-run resumes, with empty JIT
/// caches, to the same ATC behaviour as the uninterrupted run. Outside
/// `run_batch`, hits record uses and replacement is least recently used.
///
/// `mmu` must be `ResidentMmu::of(cpu)`, and the access an ordinary one (no
/// MOVES function-code override).
#[inline]
pub(crate) fn probe_resident(
    cpu: &CpuCore,
    mmu: ResidentMmu,
    logical: u32,
    write: bool,
    instruction: bool,
) -> Option<u32> {
    debug_assert_eq!(mmu, ResidentMmu::of(cpu), "stale ResidentMmu");
    debug_assert!(cpu.mmu_fc_override.is_none());
    if mmu.tt
        && let Some(phys) =
            super::ttr::check_transparent_translation(cpu, logical, write, instruction)
    {
        return Some(phys);
    }
    let supervisor = cpu.is_supervisor();
    match mmu.kind {
        RESIDENT_030 if mmu.shift != 0 => {
            let shift = u32::from(mmu.shift);
            let fc = fc_030(supervisor, instruction);
            let page_base = cpu.atc_030.probe(fc, logical >> shift, shift, write)?;
            Some(page_base.wrapping_add(low_bits(logical, 32 - shift)))
        }
        RESIDENT_040 => {
            let page_bits = u32::from(mmu.shift);
            let phys_page = cpu.atc.lookup(logical >> page_bits, supervisor, write)?;
            Some(phys_page | (logical & ((1 << page_bits) - 1)))
        }
        _ => None,
    }
}

/// The 68030 function code of an ordinary access.
#[inline]
fn fc_030(supervisor: bool, instruction: bool) -> u8 {
    match (supervisor, instruction) {
        (false, false) => 1,
        (false, true) => 2,
        (true, false) => 5,
        (true, true) => 6,
    }
}

/// 68030 address translation on top of [`walk_030`], enforcing the
/// accumulated write-protect and supervisor-only bits for the access. The
/// ATC ([`crate::mmu::Atc030`]) answers repeat accesses to a page without a
/// table search, as the silicon's does.
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
    let shift = page_shift_030(cpu.mmu_tc);
    if let Some(shift) = shift {
        let page = logical >> shift;
        // While `run_batch` translates through its fastmem window, no hit
        // records a use, this one included: compiled traces reach memory
        // through the window and fetch no instructions, so uses recorded
        // here would make the saved replacement order depend on which
        // instructions ran from traces (see `probe_resident`).
        let hit = if cpu.fm_mmu.enabled() {
            cpu.atc_030.probe(fc as u8, page, shift, write)
        } else {
            cpu.atc_030.lookup(fc as u8, page, shift, write)
        };
        if let Some(page_base) = hit {
            return Ok(page_base.wrapping_add(low_bits(logical, 32 - shift)));
        }
    }
    let w = walk_030(cpu, bus, logical, fc, u32::MAX);
    match w.end {
        Walk030End::Page { field, consumed } => {
            if w.sup && !supervisor {
                return Err(access_fault_cause(
                    logical,
                    MmuFaultCause::SupervisorProtect,
                ));
            }
            let base = field & 0xFFFF_FF00;
            let phys = low_bits(logical, consumed).wrapping_add(base);
            if let Some(shift) = shift {
                // The search loads the entry even for a write that then
                // faults on write protection. The entry keeps the page's
                // translation exactly as the walk forms it, so a hit adds
                // the in-page offset back the same way.
                let page_base = phys.wrapping_sub(low_bits(logical, 32 - shift));
                cpu.atc_030
                    .insert(fc as u8, logical >> shift, shift, page_base, w.wp);
            }
            if write && w.wp {
                return Err(access_fault_cause(logical, MmuFaultCause::WriteProtect));
            }
            Ok(phys)
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

/// 68030 PLOAD: search the tables for `logical` in the `fc` address space
/// and load the resulting translation into the ATC, as a translated access
/// would but without making one. The page's previous entry goes first, so
/// a search that finds no usable translation -- a transparently translated
/// address, an invalid descriptor, a supervisor-only page probed from a
/// user space -- leaves the next access to search and fault rather than
/// hit a stale mapping (the silicon loads an entry marking the translation
/// invalid; this cache keeps no such entries). Not modelled: the
/// used/modified write-back the R/W form selects.
pub(crate) fn pload_030<B: AddressBus>(
    cpu: &mut CpuCore,
    bus: &mut B,
    logical: u32,
    fc: u8,
    write: bool,
) {
    if !cpu.pmmu_enabled || !cpu.has_pmmu {
        return;
    }
    cpu.atc_030.flush_fc_page(fc, 7, logical);
    let Some(shift) = page_shift_030(cpu.mmu_tc) else {
        return;
    };
    if super::ttr::ttr_matches(cpu.mmu_tt0, logical, fc, write)
        || super::ttr::ttr_matches(cpu.mmu_tt1, logical, fc, write)
    {
        return;
    }
    let w = walk_030(cpu, bus, logical, u32::from(fc), u32::MAX);
    if let Walk030End::Page { field, consumed } = w.end {
        if w.sup && fc & 4 == 0 {
            return;
        }
        let phys = low_bits(logical, consumed).wrapping_add(field & 0xFFFF_FF00);
        let page_base = phys.wrapping_sub(low_bits(logical, 32 - shift));
        cpu.atc_030
            .insert(fc, logical >> shift, shift, page_base, w.wp);
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
/// 4-byte descriptors, indexed by logical bits `31:25` / `24:18` / `17:12`
/// (4 KB pages) or `17:13` (8 KB pages, TC bit 14 set). The root pointer is
/// URP in user mode and SRP in supervisor mode (no TC bit-25 gate -- that is
/// 68030-only). Table-level descriptors use UDT (bits `1:0`): >=2 = resident;
/// page descriptors use PDT (bits `1:0`): 0 = invalid, 2 = indirect, 1/3 =
/// resident.
///
/// Any access through an invalid/unconfigured descriptor raises an access
/// fault, instruction fetches included: demand-paged operating systems
/// (Linux/m68k) fault code pages in exactly this way, and Enforcer/MuForce
/// catch low-memory and freed-memory hits with the data-side faults. Code
/// that must keep executing across an MMU enable covers itself with the
/// transparent translation registers, which are checked before the walk.
/// Resident pages additionally enforce the W (write-protect) and S
/// (supervisor-only) descriptor bits.
fn translate_040<B: AddressBus>(
    cpu: &mut CpuCore,
    bus: &mut B,
    logical: u32,
    write: bool,
    supervisor: bool,
    _instruction: bool,
) -> MmuResult<u32> {
    // Invalid-descriptor outcome: an access fault whose cause records which
    // walk level held the invalid descriptor (the 68060 FSLW reports it).
    let invalid = |logical: u32, cause: MmuFaultCause| -> MmuResult<u32> {
        Err(access_fault_cause(logical, cause))
    };
    // Page size: TC bit 14 (P) selects 8 KB, else 4 KB.
    let page_bits = if cpu.mmu_tc & 0x0000_4000 != 0 {
        13
    } else {
        12
    };
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
    cpu.atc.insert(
        page_frame,
        supervisor,
        phys_page,
        write_protected,
        supervisor_only,
    );
    Ok(phys_page | (logical & page_mask))
}
