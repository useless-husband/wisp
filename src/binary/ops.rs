//! Instruction decoding.
//!
//! Function bodies are kept as bytes and decoded on demand with [`OpReader`]; the validator,
//! the interpreter's translator and the baseline compiler each make one pass over them.

use super::reader::Reader;
use crate::error::{Error, Result};
use crate::types::ValType;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BlockType {
    Empty,
    Value(ValType),
    /// Index into the type section (multi-value blocks).
    Func(u32),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MemArg {
    /// Alignment exponent (log2 of the byte alignment).
    pub align: u32,
    pub offset: u32,
}

macro_rules! numeric_ops {
    ($m:ident) => {
        $m! {
            (0x45, I32Eqz, [I32], I32)
            (0x46, I32Eq, [I32 I32], I32)
            (0x47, I32Ne, [I32 I32], I32)
            (0x48, I32LtS, [I32 I32], I32)
            (0x49, I32LtU, [I32 I32], I32)
            (0x4A, I32GtS, [I32 I32], I32)
            (0x4B, I32GtU, [I32 I32], I32)
            (0x4C, I32LeS, [I32 I32], I32)
            (0x4D, I32LeU, [I32 I32], I32)
            (0x4E, I32GeS, [I32 I32], I32)
            (0x4F, I32GeU, [I32 I32], I32)
            (0x50, I64Eqz, [I64], I32)
            (0x51, I64Eq, [I64 I64], I32)
            (0x52, I64Ne, [I64 I64], I32)
            (0x53, I64LtS, [I64 I64], I32)
            (0x54, I64LtU, [I64 I64], I32)
            (0x55, I64GtS, [I64 I64], I32)
            (0x56, I64GtU, [I64 I64], I32)
            (0x57, I64LeS, [I64 I64], I32)
            (0x58, I64LeU, [I64 I64], I32)
            (0x59, I64GeS, [I64 I64], I32)
            (0x5A, I64GeU, [I64 I64], I32)
            (0x5B, F32Eq, [F32 F32], I32)
            (0x5C, F32Ne, [F32 F32], I32)
            (0x5D, F32Lt, [F32 F32], I32)
            (0x5E, F32Gt, [F32 F32], I32)
            (0x5F, F32Le, [F32 F32], I32)
            (0x60, F32Ge, [F32 F32], I32)
            (0x61, F64Eq, [F64 F64], I32)
            (0x62, F64Ne, [F64 F64], I32)
            (0x63, F64Lt, [F64 F64], I32)
            (0x64, F64Gt, [F64 F64], I32)
            (0x65, F64Le, [F64 F64], I32)
            (0x66, F64Ge, [F64 F64], I32)
            (0x67, I32Clz, [I32], I32)
            (0x68, I32Ctz, [I32], I32)
            (0x69, I32Popcnt, [I32], I32)
            (0x6A, I32Add, [I32 I32], I32)
            (0x6B, I32Sub, [I32 I32], I32)
            (0x6C, I32Mul, [I32 I32], I32)
            (0x6D, I32DivS, [I32 I32], I32)
            (0x6E, I32DivU, [I32 I32], I32)
            (0x6F, I32RemS, [I32 I32], I32)
            (0x70, I32RemU, [I32 I32], I32)
            (0x71, I32And, [I32 I32], I32)
            (0x72, I32Or, [I32 I32], I32)
            (0x73, I32Xor, [I32 I32], I32)
            (0x74, I32Shl, [I32 I32], I32)
            (0x75, I32ShrS, [I32 I32], I32)
            (0x76, I32ShrU, [I32 I32], I32)
            (0x77, I32Rotl, [I32 I32], I32)
            (0x78, I32Rotr, [I32 I32], I32)
            (0x79, I64Clz, [I64], I64)
            (0x7A, I64Ctz, [I64], I64)
            (0x7B, I64Popcnt, [I64], I64)
            (0x7C, I64Add, [I64 I64], I64)
            (0x7D, I64Sub, [I64 I64], I64)
            (0x7E, I64Mul, [I64 I64], I64)
            (0x7F, I64DivS, [I64 I64], I64)
            (0x80, I64DivU, [I64 I64], I64)
            (0x81, I64RemS, [I64 I64], I64)
            (0x82, I64RemU, [I64 I64], I64)
            (0x83, I64And, [I64 I64], I64)
            (0x84, I64Or, [I64 I64], I64)
            (0x85, I64Xor, [I64 I64], I64)
            (0x86, I64Shl, [I64 I64], I64)
            (0x87, I64ShrS, [I64 I64], I64)
            (0x88, I64ShrU, [I64 I64], I64)
            (0x89, I64Rotl, [I64 I64], I64)
            (0x8A, I64Rotr, [I64 I64], I64)
            (0x8B, F32Abs, [F32], F32)
            (0x8C, F32Neg, [F32], F32)
            (0x8D, F32Ceil, [F32], F32)
            (0x8E, F32Floor, [F32], F32)
            (0x8F, F32Trunc, [F32], F32)
            (0x90, F32Nearest, [F32], F32)
            (0x91, F32Sqrt, [F32], F32)
            (0x92, F32Add, [F32 F32], F32)
            (0x93, F32Sub, [F32 F32], F32)
            (0x94, F32Mul, [F32 F32], F32)
            (0x95, F32Div, [F32 F32], F32)
            (0x96, F32Min, [F32 F32], F32)
            (0x97, F32Max, [F32 F32], F32)
            (0x98, F32Copysign, [F32 F32], F32)
            (0x99, F64Abs, [F64], F64)
            (0x9A, F64Neg, [F64], F64)
            (0x9B, F64Ceil, [F64], F64)
            (0x9C, F64Floor, [F64], F64)
            (0x9D, F64Trunc, [F64], F64)
            (0x9E, F64Nearest, [F64], F64)
            (0x9F, F64Sqrt, [F64], F64)
            (0xA0, F64Add, [F64 F64], F64)
            (0xA1, F64Sub, [F64 F64], F64)
            (0xA2, F64Mul, [F64 F64], F64)
            (0xA3, F64Div, [F64 F64], F64)
            (0xA4, F64Min, [F64 F64], F64)
            (0xA5, F64Max, [F64 F64], F64)
            (0xA6, F64Copysign, [F64 F64], F64)
            (0xA7, I32WrapI64, [I64], I32)
            (0xA8, I32TruncF32S, [F32], I32)
            (0xA9, I32TruncF32U, [F32], I32)
            (0xAA, I32TruncF64S, [F64], I32)
            (0xAB, I32TruncF64U, [F64], I32)
            (0xAC, I64ExtendI32S, [I32], I64)
            (0xAD, I64ExtendI32U, [I32], I64)
            (0xAE, I64TruncF32S, [F32], I64)
            (0xAF, I64TruncF32U, [F32], I64)
            (0xB0, I64TruncF64S, [F64], I64)
            (0xB1, I64TruncF64U, [F64], I64)
            (0xB2, F32ConvertI32S, [I32], F32)
            (0xB3, F32ConvertI32U, [I32], F32)
            (0xB4, F32ConvertI64S, [I64], F32)
            (0xB5, F32ConvertI64U, [I64], F32)
            (0xB6, F32DemoteF64, [F64], F32)
            (0xB7, F64ConvertI32S, [I32], F64)
            (0xB8, F64ConvertI32U, [I32], F64)
            (0xB9, F64ConvertI64S, [I64], F64)
            (0xBA, F64ConvertI64U, [I64], F64)
            (0xBB, F64PromoteF32, [F32], F64)
            (0xBC, I32ReinterpretF32, [F32], I32)
            (0xBD, I64ReinterpretF64, [F64], I64)
            (0xBE, F32ReinterpretI32, [I32], F32)
            (0xBF, F64ReinterpretI64, [I64], F64)
            (0xC0, I32Extend8S, [I32], I32)
            (0xC1, I32Extend16S, [I32], I32)
            (0xC2, I64Extend8S, [I64], I64)
            (0xC3, I64Extend16S, [I64], I64)
            (0xC4, I64Extend32S, [I64], I64)
            (0xFC00, I32TruncSatF32S, [F32], I32)
            (0xFC01, I32TruncSatF32U, [F32], I32)
            (0xFC02, I32TruncSatF64S, [F64], I32)
            (0xFC03, I32TruncSatF64U, [F64], I32)
            (0xFC04, I64TruncSatF32S, [F32], I64)
            (0xFC05, I64TruncSatF32U, [F32], I64)
            (0xFC06, I64TruncSatF64S, [F64], I64)
            (0xFC07, I64TruncSatF64U, [F64], I64)
        }
    };
}

macro_rules! define_numop {
    ($(($code:literal, $name:ident, [$($p:ident)*], $r:ident))*) => {
        /// A numeric instruction: pure function of its operands (possibly trapping).
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        pub enum NumOp { $($name),* }

        impl NumOp {
            /// Decode from an opcode (`0xFCxx` for the prefixed saturating truncations).
            pub fn from_code(code: u16) -> Option<NumOp> {
                match code {
                    $($code => Some(NumOp::$name),)*
                    _ => None,
                }
            }

            pub fn code(self) -> u16 {
                match self { $(NumOp::$name => $code,)* }
            }

            /// Operand types and result type.
            pub fn signature(self) -> (&'static [ValType], ValType) {
                match self {
                    $(NumOp::$name => (&[$(ValType::$p),*], ValType::$r),)*
                }
            }

            pub fn name(self) -> &'static str {
                match self { $(NumOp::$name => stringify!($name),)* }
            }

            /// All numeric operators, for exhaustive tests.
            pub const ALL: &'static [NumOp] = &[$(NumOp::$name),*];
        }
    };
}

numeric_ops!(define_numop);

/// The memory load instructions.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum LoadOp {
    I32Load,
    I64Load,
    F32Load,
    F64Load,
    I32Load8S,
    I32Load8U,
    I32Load16S,
    I32Load16U,
    I64Load8S,
    I64Load8U,
    I64Load16S,
    I64Load16U,
    I64Load32S,
    I64Load32U,
}

impl LoadOp {
    /// Access width in bytes, as log2.
    pub fn width_log2(self) -> u32 {
        use LoadOp::*;
        match self {
            I32Load8S | I32Load8U | I64Load8S | I64Load8U => 0,
            I32Load16S | I32Load16U | I64Load16S | I64Load16U => 1,
            I32Load | F32Load | I64Load32S | I64Load32U => 2,
            I64Load | F64Load => 3,
        }
    }

    pub fn result(self) -> ValType {
        use LoadOp::*;
        match self {
            I32Load | I32Load8S | I32Load8U | I32Load16S | I32Load16U => ValType::I32,
            F32Load => ValType::F32,
            F64Load => ValType::F64,
            _ => ValType::I64,
        }
    }
}

/// The memory store instructions.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum StoreOp {
    I32Store,
    I64Store,
    F32Store,
    F64Store,
    I32Store8,
    I32Store16,
    I64Store8,
    I64Store16,
    I64Store32,
}

impl StoreOp {
    pub fn width_log2(self) -> u32 {
        use StoreOp::*;
        match self {
            I32Store8 | I64Store8 => 0,
            I32Store16 | I64Store16 => 1,
            I32Store | F32Store | I64Store32 => 2,
            I64Store | F64Store => 3,
        }
    }

    pub fn operand(self) -> ValType {
        use StoreOp::*;
        match self {
            I32Store | I32Store8 | I32Store16 => ValType::I32,
            F32Store => ValType::F32,
            F64Store => ValType::F64,
            _ => ValType::I64,
        }
    }
}

/// One decoded instruction.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Unreachable,
    Nop,
    Block(BlockType),
    Loop(BlockType),
    If(BlockType),
    Else,
    End,
    Br(u32),
    BrIf(u32),
    BrTable { targets: Box<[u32]>, default: u32 },
    Return,
    Call(u32),
    CallIndirect { ty: u32, table: u32 },
    Drop,
    /// Untyped `select` (numeric operands only).
    Select,
    /// `select t` (required for reference operands).
    SelectT(ValType),
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),
    TableGet(u32),
    TableSet(u32),
    Load(LoadOp, MemArg),
    Store(StoreOp, MemArg),
    MemorySize,
    MemoryGrow,
    I32Const(i32),
    I64Const(i64),
    /// Raw IEEE bits, so NaN payloads survive.
    F32Const(u32),
    F64Const(u64),
    Num(NumOp),
    RefNull(ValType),
    RefIsNull,
    RefFunc(u32),
    MemoryInit(u32),
    DataDrop(u32),
    MemoryCopy,
    MemoryFill,
    TableInit { elem: u32, table: u32 },
    ElemDrop(u32),
    TableCopy { dst: u32, src: u32 },
    TableGrow(u32),
    TableSize(u32),
    TableFill(u32),
}

/// Decodes the instructions of one function body or constant expression.
pub struct OpReader<'a> {
    pub r: Reader<'a>,
}

impl<'a> OpReader<'a> {
    pub fn new(r: Reader<'a>) -> Self {
        OpReader { r }
    }

    pub fn pos(&self) -> usize {
        self.r.pos
    }

    pub fn eof(&self) -> bool {
        self.r.eof()
    }

    fn block_type(&mut self) -> Result<BlockType> {
        let b = self.r.peek_u8()?;
        if b == 0x40 {
            self.r.pos += 1;
            return Ok(BlockType::Empty);
        }
        if let Some(t) = ValType::from_byte(b) {
            self.r.pos += 1;
            return Ok(BlockType::Value(t));
        }
        let start = self.r.pos;
        let idx = self.r.s33()?;
        if idx < 0 {
            return Err(Error::malformed(start, "malformed block type"));
        }
        Ok(BlockType::Func(idx as u32))
    }

    fn memarg(&mut self) -> Result<MemArg> {
        let start = self.r.pos;
        let align = self.r.u32()?;
        if align >= 32 {
            // Bit 6 would select a memory index (multi-memory, not part of 2.0).
            return Err(Error::malformed(start, "malformed memop flags"));
        }
        let offset = self.r.u32()?;
        Ok(MemArg { align, offset })
    }

    fn zero_byte(&mut self) -> Result<()> {
        let start = self.r.pos;
        if self.r.u8()? != 0 {
            return Err(Error::malformed(start, "zero byte expected"));
        }
        Ok(())
    }

    pub fn read(&mut self) -> Result<Op> {
        let start = self.r.pos;
        let b = self.r.u8()?;
        Ok(match b {
            0x00 => Op::Unreachable,
            0x01 => Op::Nop,
            0x02 => Op::Block(self.block_type()?),
            0x03 => Op::Loop(self.block_type()?),
            0x04 => Op::If(self.block_type()?),
            0x05 => Op::Else,
            0x0B => Op::End,
            0x0C => Op::Br(self.r.u32()?),
            0x0D => Op::BrIf(self.r.u32()?),
            0x0E => {
                let n = self.r.u32()? as usize;
                // Each target takes at least one byte; reject absurd counts before allocating.
                if n > self.r.remaining() {
                    return Err(Error::malformed(self.r.pos, "unexpected end"));
                }
                let mut targets = Vec::with_capacity(n);
                for _ in 0..n {
                    targets.push(self.r.u32()?);
                }
                let default = self.r.u32()?;
                Op::BrTable { targets: targets.into_boxed_slice(), default }
            }
            0x0F => Op::Return,
            0x10 => Op::Call(self.r.u32()?),
            0x11 => {
                let ty = self.r.u32()?;
                let table = self.r.u32()?;
                Op::CallIndirect { ty, table }
            }
            0x1A => Op::Drop,
            0x1B => Op::Select,
            0x1C => {
                let n = self.r.u32()?;
                if n != 1 {
                    return Err(Error::invalid(start, "invalid result arity"));
                }
                Op::SelectT(self.r.val_type()?)
            }
            0x20 => Op::LocalGet(self.r.u32()?),
            0x21 => Op::LocalSet(self.r.u32()?),
            0x22 => Op::LocalTee(self.r.u32()?),
            0x23 => Op::GlobalGet(self.r.u32()?),
            0x24 => Op::GlobalSet(self.r.u32()?),
            0x25 => Op::TableGet(self.r.u32()?),
            0x26 => Op::TableSet(self.r.u32()?),
            0x28..=0x35 => {
                let op = match b {
                    0x28 => LoadOp::I32Load,
                    0x29 => LoadOp::I64Load,
                    0x2A => LoadOp::F32Load,
                    0x2B => LoadOp::F64Load,
                    0x2C => LoadOp::I32Load8S,
                    0x2D => LoadOp::I32Load8U,
                    0x2E => LoadOp::I32Load16S,
                    0x2F => LoadOp::I32Load16U,
                    0x30 => LoadOp::I64Load8S,
                    0x31 => LoadOp::I64Load8U,
                    0x32 => LoadOp::I64Load16S,
                    0x33 => LoadOp::I64Load16U,
                    0x34 => LoadOp::I64Load32S,
                    _ => LoadOp::I64Load32U,
                };
                Op::Load(op, self.memarg()?)
            }
            0x36..=0x3E => {
                let op = match b {
                    0x36 => StoreOp::I32Store,
                    0x37 => StoreOp::I64Store,
                    0x38 => StoreOp::F32Store,
                    0x39 => StoreOp::F64Store,
                    0x3A => StoreOp::I32Store8,
                    0x3B => StoreOp::I32Store16,
                    0x3C => StoreOp::I64Store8,
                    0x3D => StoreOp::I64Store16,
                    _ => StoreOp::I64Store32,
                };
                Op::Store(op, self.memarg()?)
            }
            0x3F => {
                self.zero_byte()?;
                Op::MemorySize
            }
            0x40 => {
                self.zero_byte()?;
                Op::MemoryGrow
            }
            0x41 => Op::I32Const(self.r.s32()?),
            0x42 => Op::I64Const(self.r.s64()?),
            0x43 => Op::F32Const(self.r.u32_fixed()?),
            0x44 => Op::F64Const(self.r.u64_fixed()?),
            0x45..=0xC4 => Op::Num(NumOp::from_code(b as u16).expect("numeric opcode range")),
            0xD0 => Op::RefNull(self.r.ref_type()?),
            0xD1 => Op::RefIsNull,
            0xD2 => Op::RefFunc(self.r.u32()?),
            0xFC => {
                let sub = self.r.u32()?;
                match sub {
                    0..=7 => Op::Num(NumOp::from_code(0xFC00 | sub as u16).unwrap()),
                    8 => {
                        let seg = self.r.u32()?;
                        self.zero_byte()?;
                        Op::MemoryInit(seg)
                    }
                    9 => Op::DataDrop(self.r.u32()?),
                    10 => {
                        self.zero_byte()?;
                        self.zero_byte()?;
                        Op::MemoryCopy
                    }
                    11 => {
                        self.zero_byte()?;
                        Op::MemoryFill
                    }
                    12 => {
                        let elem = self.r.u32()?;
                        let table = self.r.u32()?;
                        Op::TableInit { elem, table }
                    }
                    13 => Op::ElemDrop(self.r.u32()?),
                    14 => {
                        let dst = self.r.u32()?;
                        let src = self.r.u32()?;
                        Op::TableCopy { dst, src }
                    }
                    15 => Op::TableGrow(self.r.u32()?),
                    16 => Op::TableSize(self.r.u32()?),
                    17 => Op::TableFill(self.r.u32()?),
                    _ => return Err(Error::malformed(start, "illegal opcode")),
                }
            }
            0xFD => return Err(Error::Unsupported("SIMD (v128) instructions are not implemented".into())),
            _ => return Err(Error::malformed(start, "illegal opcode")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_table_is_consistent() {
        for &op in NumOp::ALL {
            assert_eq!(NumOp::from_code(op.code()), Some(op));
        }
        // Every single-byte code in the numeric range decodes.
        for b in 0x45u16..=0xC4 {
            assert!(NumOp::from_code(b).is_some(), "{b:#x}");
        }
        assert_eq!(NumOp::ALL.len(), 0xC4 - 0x45 + 1 + 8);
    }

    #[test]
    fn decodes_prefixed_and_memargs() {
        let bytes = [0x28, 0x02, 0x10, 0xFC, 0x0A, 0x00, 0x00, 0xFC, 0x05, 0x0B];
        let mut r = OpReader::new(Reader::new(&bytes));
        assert_eq!(r.read().unwrap(), Op::Load(LoadOp::I32Load, MemArg { align: 2, offset: 16 }));
        assert_eq!(r.read().unwrap(), Op::MemoryCopy);
        assert_eq!(r.read().unwrap(), Op::Num(NumOp::I64TruncSatF32U));
        assert_eq!(r.read().unwrap(), Op::End);
        let bad = [0x28, 0x20, 0x00];
        assert_eq!(OpReader::new(Reader::new(&bad)).read().unwrap_err().message(), "malformed memop flags");
        let bad = [0x3F, 0x01];
        assert_eq!(OpReader::new(Reader::new(&bad)).read().unwrap_err().message(), "zero byte expected");
    }
}
