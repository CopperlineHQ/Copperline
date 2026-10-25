//! FPU registers

use super::types::FloatX80;

#[derive(Debug, Clone, Default)]
pub struct FpuRegisters {
    pub fp: [FloatX80; 8],
    pub fpcr: u32,
    pub fpsr: u32,
    pub fpiar: u32,
}
