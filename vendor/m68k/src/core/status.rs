//! Status-register and condition-code bit masks.

/// Status Register bit positions.
pub const SR_CARRY: u16 = 0x0001;
/// Overflow (V) condition-code bit.
pub const SR_OVERFLOW: u16 = 0x0002;
/// Zero (Z) condition-code bit.
pub const SR_ZERO: u16 = 0x0004;
/// Negative (N) condition-code bit.
pub const SR_NEGATIVE: u16 = 0x0008;
/// Extend (X) condition-code bit.
pub const SR_EXTEND: u16 = 0x0010;
/// Three-bit interrupt-priority mask.
pub const SR_INT_MASK: u16 = 0x0700;
/// Supervisor-mode (S) bit.
pub const SR_SUPERVISOR: u16 = 0x2000;
/// Trace-on-every-instruction (T1) bit.
pub const SR_TRACE: u16 = 0x8000;
