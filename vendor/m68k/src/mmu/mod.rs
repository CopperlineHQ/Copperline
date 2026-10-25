//! PMMU emulation for the M68030, M68040, and M68060.
//!
//! The 68030 path implements its programmable multi-level table format,
//! CRP/SRP selection, function-code lookup, transparent translation, PTEST,
//! accumulated write/supervisor protection, and its 22-entry fully
//! associative address-translation cache (flushed by PFLUSH and PMOVE). The
//! 68040 and 68060 use the fixed root/pointer/page table format,
//! instruction/data transparent translation registers, and a direct-mapped
//! address-translation cache.
//!
//! Table-walk bus errors and protection failures retain enough cause detail
//! for generation-specific exception frames. Used/modified descriptor
//! write-back, 68030 root-descriptor limit checks, and a DT=1 page descriptor
//! directly in the 68030 root pointer are not modeled.

pub mod atc;
mod translation;
pub mod ttr;

use crate::core::cpu::CpuCore;
use crate::core::memory::AddressBus;

pub use atc::{Atc, Atc030};
pub use translation::translate;
pub(crate) use translation::{ResidentMmu, pload_030, probe_resident, ptest_030};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Architectural class of an MMU operation or translation fault.
pub enum MmuFaultKind {
    /// Translation-control or root-pointer configuration is invalid.
    ConfigurationError,
    /// The requested PMMU operation or descriptor form is illegal.
    IllegalOperation,
    /// A mapping is absent or denies the attempted access.
    AccessLevelViolation,
    /// A physical bus error occurred while walking tables / fetching descriptors.
    BusError,
}

/// Why a translation failed, at the granularity the 68060 FSLW reports:
/// which walk level held the invalid descriptor, or which protection or
/// bus condition stopped the access. The 030/040 frames do not consume
/// this detail; the 68060 access-error frame does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmuFaultCause {
    /// Invalid descriptor in the root (level A) table.
    PointerA,
    /// Invalid descriptor in the pointer (level B) table.
    PointerB,
    /// Invalid indirect page descriptor.
    Indirect,
    /// Invalid page descriptor.
    PageFault,
    /// Write to a write-protected page.
    WriteProtect,
    /// User access to a supervisor-only page.
    SupervisorProtect,
    /// Physical bus error while walking the tables.
    TableWalkBusError,
    /// Physical bus error on the access itself.
    AccessBusError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// MMU failure returned to the CPU's exception-delivery path.
pub struct MmuFault {
    /// Architectural fault class used to select the exception vector.
    pub kind: MmuFaultKind,
    /// Logical or table address associated with the failure.
    pub address: u32,
    /// Detailed translation cause used by generation-specific stack frames.
    pub cause: MmuFaultCause,
}

/// Result of an MMU translation or PMMU operation.
pub type MmuResult<T> = Result<T, MmuFault>;

/// Translate a logical address using the configured CPU's PMMU state.
///
/// `write` selects read versus write permission checking, `supervisor`
/// selects the user/supervisor root and protection domain, and `instruction`
/// selects instruction versus data transparent-translation registers.
/// Disabled or absent PMMUs return the logical address unchanged.
pub fn translate_address<B: AddressBus>(
    cpu: &mut CpuCore,
    bus: &mut B,
    logical: u32,
    write: bool,
    supervisor: bool,
    instruction: bool,
) -> MmuResult<u32> {
    translate(cpu, bus, logical, write, supervisor, instruction)
}
