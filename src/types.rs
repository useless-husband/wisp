//! WebAssembly types (value, function, limits, tables, memories, globals).

use std::fmt;

/// A WebAssembly value type.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ValType {
    I32,
    I64,
    F32,
    F64,
    /// 128-bit vector. Decoded so error messages are precise, but not executed.
    V128,
    FuncRef,
    ExternRef,
}

impl ValType {
    pub fn is_ref(self) -> bool {
        matches!(self, ValType::FuncRef | ValType::ExternRef)
    }

    pub fn is_float(self) -> bool {
        matches!(self, ValType::F32 | ValType::F64)
    }

    pub(crate) fn from_byte(b: u8) -> Option<ValType> {
        Some(match b {
            0x7F => ValType::I32,
            0x7E => ValType::I64,
            0x7D => ValType::F32,
            0x7C => ValType::F64,
            0x7B => ValType::V128,
            0x70 => ValType::FuncRef,
            0x6F => ValType::ExternRef,
            _ => return None,
        })
    }
}

impl fmt::Display for ValType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ValType::I32 => "i32",
            ValType::I64 => "i64",
            ValType::F32 => "f32",
            ValType::F64 => "f64",
            ValType::V128 => "v128",
            ValType::FuncRef => "funcref",
            ValType::ExternRef => "externref",
        })
    }
}

/// A function signature.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct FuncType {
    pub params: Box<[ValType]>,
    pub results: Box<[ValType]>,
}

impl FuncType {
    pub fn new(params: impl Into<Box<[ValType]>>, results: impl Into<Box<[ValType]>>) -> Self {
        FuncType {
            params: params.into(),
            results: results.into(),
        }
    }
}

impl fmt::Display for FuncType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[")?;
        for (i, p) in self.params.iter().enumerate() {
            if i > 0 {
                write!(f, " ")?;
            }
            write!(f, "{p}")?;
        }
        write!(f, "] -> [")?;
        for (i, r) in self.results.iter().enumerate() {
            if i > 0 {
                write!(f, " ")?;
            }
            write!(f, "{r}")?;
        }
        write!(f, "]")
    }
}

/// Size limits of a table (in elements) or memory (in 64 KiB pages).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub min: u32,
    pub max: Option<u32>,
}

impl Limits {
    /// Import subtyping: `self` (the provided entity) matches `want` (the import's type).
    pub fn matches(&self, want: &Limits) -> bool {
        if self.min < want.min {
            return false;
        }
        match (self.max, want.max) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(a), Some(b)) => a <= b,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TableType {
    pub elem: ValType,
    pub limits: Limits,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MemoryType {
    pub limits: Limits,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct GlobalType {
    pub ty: ValType,
    pub mutable: bool,
}

/// The type of an import or export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternType {
    Func(FuncType),
    Table(TableType),
    Memory(MemoryType),
    Global(GlobalType),
}

/// The 64 KiB WebAssembly page size.
pub const PAGE_SIZE: u64 = 65536;
/// The maximum number of pages of a 32-bit memory (4 GiB).
pub const MAX_PAGES: u32 = 65536;
