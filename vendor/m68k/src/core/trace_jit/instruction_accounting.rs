//! Remove only the synthetic cycle component from the existing packed-return
//! ABI. Instruction retirement and completion/guard bits remain exact.

use super::*;
use cranelift_codegen::cursor::{Cursor, FuncCursor};
use cranelift_codegen::ir::{InstructionData, ValueDef};

enum PackedPart {
    ZeroCycles,
    Constant(i64),
    Retirement(Value),
    Union(Box<PackedPart>, Box<PackedPart>),
}

impl PackedPart {
    fn parse(function: &Function, value: Value, cycle_parts: &mut usize) -> Option<Self> {
        let value = function.dfg.resolve_aliases(value);
        if function.dfg.value_type(value) != types::I64 {
            return None;
        }
        // A trace body's exits return through blocks that store its cached
        // registers; follow such a parameter to the one jump supplying it.
        if let ValueDef::Param(block, index) = function.dfg.value_def(value) {
            let incoming = sole_incoming_argument(function, block, index)?;
            return Self::parse(function, incoming, cycle_parts);
        }
        let inst = function.dfg.value_def(value).inst()?;
        let data = &function.dfg.insts[inst];
        match *data {
            InstructionData::UnaryImm {
                opcode: Opcode::Iconst,
                imm,
            } => {
                // Preserve all retirement bits and the explicit metadata;
                // lower bits belong exclusively to the cycle payload.
                Some(Self::Constant(
                    i64::from(imm) & !(TRACE_RETURN_CYCLES_MASK as i64),
                ))
            }
            InstructionData::Unary {
                opcode: Opcode::Uextend,
                arg,
            } if function.dfg.value_type(arg) == types::I32 => {
                // Every supported emitter constructs the cycle word with
                // this one unshifted extension. Retirement is separately
                // shifted by 32; metadata is an explicit constant union.
                *cycle_parts += 1;
                Some(Self::ZeroCycles)
            }
            InstructionData::Binary {
                opcode: Opcode::Bor,
                args,
            } => Some(Self::Union(
                Box::new(Self::parse(function, args[0], cycle_parts)?),
                Box::new(Self::parse(function, args[1], cycle_parts)?),
            )),
            InstructionData::BinaryImm64 {
                opcode: Opcode::IshlImm,
                imm,
                ..
            } if i64::from(imm) == 32 => Some(Self::Retirement(value)),
            InstructionData::Binary {
                opcode: Opcode::Ishl,
                args,
            } => {
                let shift = function.dfg.resolve_aliases(args[1]);
                let shift_inst = function.dfg.value_def(shift).inst()?;
                if matches!(function.dfg.insts[shift_inst], InstructionData::UnaryImm { opcode: Opcode::Iconst, imm } if i64::from(imm) == 32)
                {
                    Some(Self::Retirement(value))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn emit(self, cursor: &mut FuncCursor<'_>) -> Value {
        match self {
            Self::ZeroCycles => cursor.ins().iconst(types::I64, 0),
            Self::Constant(value) => cursor.ins().iconst(types::I64, value),
            Self::Retirement(value) => value,
            Self::Union(a, b) => {
                let a = a.emit(cursor);
                let b = b.emit(cursor);
                cursor.ins().bor(a, b)
            }
        }
    }
}

/// The value passed as parameter `index` of `block`, when exactly one branch
/// targets `block`. The rewritten return then uses that value directly; it
/// dominates `block` because it is defined before the block's only entry.
fn sole_incoming_argument(function: &Function, block: Block, index: usize) -> Option<Value> {
    let dfg = &function.dfg;
    let mut incoming = None;
    for source in function.layout.blocks() {
        for inst in function.layout.block_insts(source) {
            for call in dfg.insts[inst].branch_destination(&dfg.jump_tables, &dfg.exception_tables)
            {
                if call.block(&dfg.value_lists) != block {
                    continue;
                }
                if incoming.is_some() {
                    return None;
                }
                match call.args(&dfg.value_lists).nth(index)? {
                    BlockArg::Value(value) => incoming = Some(value),
                    _ => return None,
                }
            }
        }
    }
    incoming
}

pub(super) fn without_synthetic_cycles(function: &Function) -> Option<Function> {
    let mut rewrites = Vec::new();
    for block in function.layout.blocks() {
        for inst in function.layout.block_insts(block) {
            let opcode = function.dfg.insts[inst].opcode();
            if opcode == Opcode::Return {
                let args = function.dfg.inst_args(inst);
                if args.len() != 1 {
                    return None;
                }
                let mut cycle_parts = 0;
                let part = PackedPart::parse(function, args[0], &mut cycle_parts)?;
                // Refuse a changed/unrecognized packing convention rather
                // than infer the meaning of an arbitrary integer result.
                if cycle_parts != 1 {
                    return None;
                }
                rewrites.push((inst, part));
            } else if matches!(opcode, Opcode::ReturnCall | Opcode::ReturnCallIndirect) {
                return None;
            }
        }
    }
    if rewrites.is_empty() {
        return None;
    }
    let mut result = function.clone();
    for (inst, part) in rewrites {
        let mut cursor = FuncCursor::new(&mut result);
        cursor.goto_inst(inst);
        let value = part.emit(&mut cursor);
        cursor.func.dfg.replace(inst).return_(&[value]);
    }
    Some(result)
}
