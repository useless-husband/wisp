//! The interpreter's internal instruction set.
//!
//! Each function runs in a frame of 64-bit slots: `[params | locals | operand stack]`.
//! Because WebAssembly's operand stack height is known statically at every instruction,
//! the operand at depth `k` always lives in slot `nlocals + k`, so instructions name their
//! operands and result by slot index instead of pushing and popping. Operands may also name
//! a local directly (a `local.get` costs nothing) and many binary operators have a form with
//! an immediate right operand. Branch targets are resolved to instruction indices.

use crate::binary::ops::NumOp;

/// Frame-relative slot index.
pub type Slot = u32;

macro_rules! define_instr {
    (
        unary: [$($u:ident)*]
        binary: [$($b:ident)*]
        imm: [$($i:ident => $ib:ident,)*]
        cmp: [$(($br:ident, $bri:ident, $c:ident))*]
        load: [$($ld:ident)*]
        store: [$($st:ident)*]
        { $($extra:tt)* }
    ) => {
        /// One interpreter instruction (16 bytes).
        #[derive(Copy, Clone, Debug, PartialEq)]
        pub enum Instr {
            $($u { d: Slot, a: Slot },)*
            $($b { d: Slot, a: Slot, b: Slot },)*
            $($i { d: Slot, a: Slot, imm: u32 },)*
            /// Fused i32 compare-and-branch.
            $($br { a: Slot, b: Slot, t: u32 },)*
            $($bri { a: Slot, imm: u32, t: u32 },)*
            $($ld { d: Slot, a: Slot, off: u32 },)*
            $($st { a: Slot, v: Slot, off: u32 },)*
            $($extra)*
        }

        impl Instr {
            pub fn br_cmp(c: Cmp, a: Slot, b: Slot, t: u32) -> Instr {
                match c { $(Cmp::$c => Instr::$br { a, b, t },)* }
            }

            pub fn br_cmp_imm(c: Cmp, a: Slot, imm: u32, t: u32) -> Instr {
                match c { $(Cmp::$c => Instr::$bri { a, imm, t },)* }
            }

            /// Patch the target of any branch instruction.
            pub fn set_target(&mut self, to: u32) {
                match self {
                    $(Instr::$br { t, .. } => *t = to,)*
                    $(Instr::$bri { t, .. } => *t = to,)*
                    Instr::Br { t } | Instr::BrIfNez { t, .. } | Instr::BrIfEqz { t, .. } => *t = to,
                    other => panic!("not a branch: {other:?}"),
                }
            }
        }

        impl Instr {
            /// The instruction computing numeric operator `op`.
            pub fn num(op: NumOp, d: Slot, a: Slot, b: Slot) -> Instr {
                match op {
                    $(NumOp::$u => Instr::$u { d, a },)*
                    $(NumOp::$b => Instr::$b { d, a, b },)*
                }
            }

            /// The immediate form of binary operator `op`, if there is one.
            pub fn num_imm(op: NumOp, d: Slot, a: Slot, imm: u32) -> Option<Instr> {
                Some(match op {
                    $(NumOp::$ib => Instr::$i { d, a, imm },)*
                    _ => return None,
                })
            }

            /// Redirect the result of a pure value-producing instruction to slot `to`.
            pub fn retarget(&mut self, to: Slot) -> bool {
                match self {
                    $(Instr::$u { d, .. } => *d = to,)*
                    $(Instr::$b { d, .. } => *d = to,)*
                    $(Instr::$i { d, .. } => *d = to,)*
                    $(Instr::$ld { d, .. } => *d = to,)*
                    Instr::GlobalGet { d, .. }
                    | Instr::RefFunc { d, .. }
                    | Instr::MemorySize { d }
                    | Instr::TableGet { d, .. }
                    | Instr::TableSize { d, .. } => *d = to,
                    _ => return false,
                }
                true
            }
        }
    };
}

define_instr! {
    unary: [
        I32Eqz I64Eqz I32Clz I32Ctz I32Popcnt I64Clz I64Ctz I64Popcnt
        F32Abs F32Neg F32Ceil F32Floor F32Trunc F32Nearest F32Sqrt
        F64Abs F64Neg F64Ceil F64Floor F64Trunc F64Nearest F64Sqrt
        I32WrapI64 I32TruncF32S I32TruncF32U I32TruncF64S I32TruncF64U
        I64ExtendI32S I64ExtendI32U I64TruncF32S I64TruncF32U I64TruncF64S I64TruncF64U
        F32ConvertI32S F32ConvertI32U F32ConvertI64S F32ConvertI64U F32DemoteF64
        F64ConvertI32S F64ConvertI32U F64ConvertI64S F64ConvertI64U F64PromoteF32
        I32ReinterpretF32 I64ReinterpretF64 F32ReinterpretI32 F64ReinterpretI64
        I32Extend8S I32Extend16S I64Extend8S I64Extend16S I64Extend32S
        I32TruncSatF32S I32TruncSatF32U I32TruncSatF64S I32TruncSatF64U
        I64TruncSatF32S I64TruncSatF32U I64TruncSatF64S I64TruncSatF64U
    ]
    binary: [
        I32Eq I32Ne I32LtS I32LtU I32GtS I32GtU I32LeS I32LeU I32GeS I32GeU
        I64Eq I64Ne I64LtS I64LtU I64GtS I64GtU I64LeS I64LeU I64GeS I64GeU
        F32Eq F32Ne F32Lt F32Gt F32Le F32Ge F64Eq F64Ne F64Lt F64Gt F64Le F64Ge
        I32Add I32Sub I32Mul I32DivS I32DivU I32RemS I32RemU I32And I32Or I32Xor
        I32Shl I32ShrS I32ShrU I32Rotl I32Rotr
        I64Add I64Sub I64Mul I64DivS I64DivU I64RemS I64RemU I64And I64Or I64Xor
        I64Shl I64ShrS I64ShrU I64Rotl I64Rotr
        F32Add F32Sub F32Mul F32Div F32Min F32Max F32Copysign
        F64Add F64Sub F64Mul F64Div F64Min F64Max F64Copysign
    ]
    imm: [
        I32AddImm => I32Add, I32SubImm => I32Sub, I32MulImm => I32Mul,
        I32AndImm => I32And, I32OrImm => I32Or, I32XorImm => I32Xor,
        I32ShlImm => I32Shl, I32ShrSImm => I32ShrS, I32ShrUImm => I32ShrU,
        I32EqImm => I32Eq, I32NeImm => I32Ne, I32LtSImm => I32LtS, I32LtUImm => I32LtU,
        I32GtSImm => I32GtS, I32GtUImm => I32GtU, I32LeSImm => I32LeS, I32LeUImm => I32LeU,
        I32GeSImm => I32GeS, I32GeUImm => I32GeU,
        // i64 forms take a 32-bit immediate, sign-extended.
        I64AddImm => I64Add, I64SubImm => I64Sub, I64MulImm => I64Mul,
        I64AndImm => I64And, I64OrImm => I64Or, I64XorImm => I64Xor,
        I64ShlImm => I64Shl, I64ShrSImm => I64ShrS, I64ShrUImm => I64ShrU,
        I64EqImm => I64Eq, I64NeImm => I64Ne, I64LtSImm => I64LtS, I64LtUImm => I64LtU,
        I64GtSImm => I64GtS, I64GtUImm => I64GtU, I64LeSImm => I64LeS, I64LeUImm => I64LeU,
        I64GeSImm => I64GeS, I64GeUImm => I64GeU,
    ]
    cmp: [
        (BrEq, BrEqImm, Eq) (BrNe, BrNeImm, Ne) (BrLtS, BrLtSImm, LtS) (BrLtU, BrLtUImm, LtU)
        (BrGtS, BrGtSImm, GtS) (BrGtU, BrGtUImm, GtU) (BrLeS, BrLeSImm, LeS) (BrLeU, BrLeUImm, LeU)
        (BrGeS, BrGeSImm, GeS) (BrGeU, BrGeUImm, GeU)
    ]
    load: [
        LoadI32 LoadI64 LoadI32S8 LoadI32U8 LoadI32S16 LoadI32U16
        LoadI64S8 LoadI64U8 LoadI64S16 LoadI64U16 LoadI64S32 LoadI64U32
    ]
    store: [Store8 Store16 Store32 Store64]
    {
        /// Trap with a raw trap code (`unreachable`).
        Trap { code: u32 },
        Br { t: u32 },
        BrIfNez { c: Slot, t: u32 },
        BrIfEqz { c: Slot, t: u32 },
        /// Jump to `pc + 1 + min(slot, len)`; that instruction is a `Br`.
        BrTable { idx: Slot, len: u32 },
        /// Return; results are in slots `0..nresults`.
        Return,
        /// Call function `f` of the current module; the callee frame starts at `base`.
        Call { f: u32, base: Slot },
        /// Indirect call; the table index is in slot `base + nparams`.
        CallIndirect { base: Slot, ty: u32, table: u32 },
        /// Charge `n` units of fuel.
        Fuel { n: u32 },
        Copy { d: Slot, s: Slot },
        Const32 { d: Slot, v: u32 },
        Const64 { d: Slot, v: u64 },
        /// `d = if c != 0 { d } else { b }`.
        Select { d: Slot, b: Slot, c: Slot },
        GlobalGet { d: Slot, g: u32 },
        GlobalSet { s: Slot, g: u32 },
        MemorySize { d: Slot },
        MemoryGrow { d: Slot, n: Slot },
        /// Operands in slots `base, base+1, base+2`.
        MemoryCopy { base: Slot },
        MemoryFill { base: Slot },
        MemoryInit { base: Slot, seg: u32 },
        DataDrop { seg: u32 },
        TableGet { d: Slot, i: Slot, t: u32 },
        TableSet { i: Slot, v: Slot, t: u32 },
        TableSize { d: Slot, t: u32 },
        /// Init value in `base`, delta in `base+1`, result to `base`.
        TableGrow { base: Slot, t: u32 },
        TableFill { base: Slot, t: u32 },
        TableCopy { base: Slot, dst: u32, src: u32 },
        TableInit { base: Slot, seg: u32, t: u32 },
        ElemDrop { seg: u32 },
        RefFunc { d: Slot, f: u32 },
    }
}

/// Integer comparison for fused compare-and-branch.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Ne,
    LtS,
    LtU,
    GtS,
    GtU,
    LeS,
    LeU,
    GeS,
    GeU,
}

impl Cmp {
    pub fn from_num(op: NumOp) -> Option<Cmp> {
        Some(match op {
            NumOp::I32Eq => Cmp::Eq,
            NumOp::I32Ne => Cmp::Ne,
            NumOp::I32LtS => Cmp::LtS,
            NumOp::I32LtU => Cmp::LtU,
            NumOp::I32GtS => Cmp::GtS,
            NumOp::I32GtU => Cmp::GtU,
            NumOp::I32LeS => Cmp::LeS,
            NumOp::I32LeU => Cmp::LeU,
            NumOp::I32GeS => Cmp::GeS,
            NumOp::I32GeU => Cmp::GeU,
            _ => return None,
        })
    }

    pub fn negate(self) -> Cmp {
        match self {
            Cmp::Eq => Cmp::Ne,
            Cmp::Ne => Cmp::Eq,
            Cmp::LtS => Cmp::GeS,
            Cmp::LtU => Cmp::GeU,
            Cmp::GtS => Cmp::LeS,
            Cmp::GtU => Cmp::LeU,
            Cmp::LeS => Cmp::GtS,
            Cmp::LeU => Cmp::GtU,
            Cmp::GeS => Cmp::LtS,
            Cmp::GeU => Cmp::LtU,
        }
    }

    #[inline(always)]
    pub fn eval(self, a: u32, b: u32) -> bool {
        match self {
            Cmp::Eq => a == b,
            Cmp::Ne => a != b,
            Cmp::LtS => (a as i32) < (b as i32),
            Cmp::LtU => a < b,
            Cmp::GtS => (a as i32) > (b as i32),
            Cmp::GtU => a > b,
            Cmp::LeS => (a as i32) <= (b as i32),
            Cmp::LeU => a <= b,
            Cmp::GeS => (a as i32) >= (b as i32),
            Cmp::GeU => a >= b,
        }
    }
}

/// A translated function.
#[derive(Debug)]
pub struct InterpFunc {
    pub code: Box<[Instr]>,
    pub nparams: u32,
    pub nresults: u32,
    /// Non-parameter locals, zeroed on entry.
    pub nlocals: u32,
    /// Slots used by the frame (params + locals + operand stack).
    pub frame_size: u32,
}

/// The translated code of a module's defined functions.
#[derive(Debug, Default)]
pub struct InterpModule {
    /// Translated defined functions (`None` when compiled to machine code instead).
    pub funcs: Vec<Option<InterpFunc>>,
    /// Parameter count of each type index (for `call_indirect`).
    pub type_params: Vec<u32>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn instr_is_16_bytes() {
        assert_eq!(std::mem::size_of::<super::Instr>(), 16);
    }
}
