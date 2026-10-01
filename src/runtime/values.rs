//! Values and handles exposed by the embedding API.

use crate::types::ValType;

/// A function in a [`Store`](crate::Store).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Func(pub(crate) u32);

/// A table in a store.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Table(pub(crate) u32);

/// A linear memory in a store.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Memory(pub(crate) u32);

/// A global in a store.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Global(pub(crate) u32);

/// An instantiated module in a store.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Instance(pub(crate) u32);

/// Anything that can be imported or exported.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Extern {
    Func(Func),
    Table(Table),
    Memory(Memory),
    Global(Global),
}

impl Extern {
    pub fn into_func(self) -> Option<Func> {
        match self {
            Extern::Func(f) => Some(f),
            _ => None,
        }
    }
    pub fn into_memory(self) -> Option<Memory> {
        match self {
            Extern::Memory(m) => Some(m),
            _ => None,
        }
    }
    pub fn into_table(self) -> Option<Table> {
        match self {
            Extern::Table(t) => Some(t),
            _ => None,
        }
    }
    pub fn into_global(self) -> Option<Global> {
        match self {
            Extern::Global(g) => Some(g),
            _ => None,
        }
    }
}

/// A WebAssembly value. Floats are carried as raw bits so NaN payloads are preserved.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Val {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    FuncRef(Option<Func>),
    /// An opaque host reference, identified by a number.
    ExternRef(Option<u32>),
}

impl Val {
    pub fn f32(x: f32) -> Val {
        Val::F32(x.to_bits())
    }

    pub fn f64(x: f64) -> Val {
        Val::F64(x.to_bits())
    }

    pub fn ty(&self) -> ValType {
        match self {
            Val::I32(_) => ValType::I32,
            Val::I64(_) => ValType::I64,
            Val::F32(_) => ValType::F32,
            Val::F64(_) => ValType::F64,
            Val::FuncRef(_) => ValType::FuncRef,
            Val::ExternRef(_) => ValType::ExternRef,
        }
    }

    /// The zero value of a type (null for references).
    pub fn default_for(ty: ValType) -> Val {
        match ty {
            ValType::I32 => Val::I32(0),
            ValType::I64 => Val::I64(0),
            ValType::F32 => Val::F32(0),
            ValType::F64 => Val::F64(0),
            ValType::FuncRef => Val::FuncRef(None),
            ValType::ExternRef => Val::ExternRef(None),
            ValType::V128 => panic!("v128 is not supported"),
        }
    }

    pub fn i32(&self) -> Option<i32> {
        match self {
            Val::I32(v) => Some(*v),
            _ => None,
        }
    }

    pub fn i64(&self) -> Option<i64> {
        match self {
            Val::I64(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Val::F32(v) => Some(f32::from_bits(*v)),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Val::F64(v) => Some(f64::from_bits(*v)),
            _ => None,
        }
    }
}

impl std::fmt::Display for Val {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Val::I32(v) => write!(f, "{v}:i32"),
            Val::I64(v) => write!(f, "{v}:i64"),
            Val::F32(v) => write!(f, "{}:f32", f32::from_bits(*v)),
            Val::F64(v) => write!(f, "{}:f64", f64::from_bits(*v)),
            Val::FuncRef(None) => write!(f, "null:funcref"),
            Val::FuncRef(Some(x)) => write!(f, "func#{}", x.0),
            Val::ExternRef(None) => write!(f, "null:externref"),
            Val::ExternRef(Some(x)) => write!(f, "extern#{x}"),
        }
    }
}
