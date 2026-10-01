//! Error and trap types.

use std::fmt;

/// Why execution stopped abnormally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrapCode {
    Unreachable,
    MemoryOutOfBounds,
    TableOutOfBounds,
    /// `call_indirect` through a null table entry.
    UninitializedElement,
    /// `call_indirect` with an index past the end of the table.
    UndefinedElement,
    IndirectCallTypeMismatch,
    IntegerDivideByZero,
    IntegerOverflow,
    InvalidConversionToInteger,
    StackExhausted,
    OutOfFuel,
    /// A null reference where a non-null one was required.
    NullReference,
    /// The guest called WASI `proc_exit`.
    Exit(i32),
    /// A host function failed.
    Host(String),
}

impl TrapCode {
    /// The message the WebAssembly reference interpreter uses for this trap.
    pub fn message(&self) -> String {
        match self {
            TrapCode::Unreachable => "unreachable".into(),
            TrapCode::MemoryOutOfBounds => "out of bounds memory access".into(),
            TrapCode::TableOutOfBounds => "out of bounds table access".into(),
            TrapCode::UninitializedElement => "uninitialized element".into(),
            TrapCode::UndefinedElement => "undefined element".into(),
            TrapCode::IndirectCallTypeMismatch => "indirect call type mismatch".into(),
            TrapCode::IntegerDivideByZero => "integer divide by zero".into(),
            TrapCode::IntegerOverflow => "integer overflow".into(),
            TrapCode::InvalidConversionToInteger => "invalid conversion to integer".into(),
            TrapCode::StackExhausted => "call stack exhausted".into(),
            TrapCode::OutOfFuel => "all fuel consumed".into(),
            TrapCode::NullReference => "null reference".into(),
            TrapCode::Exit(c) => format!("exit with status {c}"),
            TrapCode::Host(m) => m.clone(),
        }
    }

    /// Small integer used to pass a trap through machine code. `0` means "no trap".
    pub(crate) fn to_raw(&self) -> u32 {
        match self {
            TrapCode::Unreachable => 1,
            TrapCode::MemoryOutOfBounds => 2,
            TrapCode::TableOutOfBounds => 3,
            TrapCode::UninitializedElement => 4,
            TrapCode::UndefinedElement => 5,
            TrapCode::IndirectCallTypeMismatch => 6,
            TrapCode::IntegerDivideByZero => 7,
            TrapCode::IntegerOverflow => 8,
            TrapCode::InvalidConversionToInteger => 9,
            TrapCode::StackExhausted => 10,
            TrapCode::OutOfFuel => 11,
            TrapCode::NullReference => 12,
            // Exit and Host carry data; they travel in the runtime's pending-trap slot.
            TrapCode::Exit(_) | TrapCode::Host(_) => RAW_PENDING,
        }
    }

    pub(crate) fn from_raw(raw: u32) -> Option<TrapCode> {
        Some(match raw {
            1 => TrapCode::Unreachable,
            2 => TrapCode::MemoryOutOfBounds,
            3 => TrapCode::TableOutOfBounds,
            4 => TrapCode::UninitializedElement,
            5 => TrapCode::UndefinedElement,
            6 => TrapCode::IndirectCallTypeMismatch,
            7 => TrapCode::IntegerDivideByZero,
            8 => TrapCode::IntegerOverflow,
            9 => TrapCode::InvalidConversionToInteger,
            10 => TrapCode::StackExhausted,
            11 => TrapCode::OutOfFuel,
            12 => TrapCode::NullReference,
            _ => return None,
        })
    }
}

/// Raw trap code meaning "the full trap is stored in the runtime's pending slot".
pub(crate) const RAW_PENDING: u32 = 100;

/// A runtime trap, with the WebAssembly call stack at the point it happened when known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trap {
    pub code: TrapCode,
    /// The table index for `UninitializedElement` traps.
    pub index: Option<u32>,
}

impl Trap {
    pub fn new(code: TrapCode) -> Self {
        Trap { code, index: None }
    }

    pub fn host(msg: impl Into<String>) -> Self {
        Trap::new(TrapCode::Host(msg.into()))
    }

    pub(crate) fn uninitialized(index: u32) -> Self {
        Trap {
            code: TrapCode::UninitializedElement,
            index: Some(index),
        }
    }

    /// The exit status if this trap is a WASI `proc_exit`.
    pub fn exit_status(&self) -> Option<i32> {
        match self.code {
            TrapCode::Exit(c) => Some(c),
            _ => None,
        }
    }
}

impl From<TrapCode> for Trap {
    fn from(code: TrapCode) -> Self {
        Trap::new(code)
    }
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.code.message())?;
        if let Some(i) = self.index {
            write!(f, " {i}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Trap {}

/// Any error produced by wisp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The binary does not follow the WebAssembly binary format.
    Malformed { offset: usize, message: String },
    /// The module is well formed but fails validation.
    Invalid { offset: usize, message: String },
    /// Imports could not be resolved or do not match.
    Link(String),
    /// Execution trapped (including during instantiation).
    Trap(Trap),
    /// The module uses a feature wisp does not implement.
    Unsupported(String),
    /// Misuse of the embedding API (wrong argument count or type, ...).
    Api(String),
}

impl Error {
    pub(crate) fn malformed(offset: usize, message: impl Into<String>) -> Self {
        Error::Malformed {
            offset,
            message: message.into(),
        }
    }

    pub(crate) fn invalid(offset: usize, message: impl Into<String>) -> Self {
        Error::Invalid {
            offset,
            message: message.into(),
        }
    }

    /// The bare message, without the error class or offset.
    pub fn message(&self) -> String {
        match self {
            Error::Malformed { message, .. } | Error::Invalid { message, .. } => message.clone(),
            Error::Link(m) | Error::Unsupported(m) | Error::Api(m) => m.clone(),
            Error::Trap(t) => t.to_string(),
        }
    }

    pub fn trap(&self) -> Option<&Trap> {
        match self {
            Error::Trap(t) => Some(t),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Malformed { offset, message } => {
                write!(f, "malformed module at offset {offset:#x}: {message}")
            }
            Error::Invalid { offset, message } => {
                write!(f, "invalid module at offset {offset:#x}: {message}")
            }
            Error::Link(m) => write!(f, "link error: {m}"),
            Error::Trap(t) => write!(f, "trap: {t}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::Api(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<Trap> for Error {
    fn from(t: Trap) -> Self {
        Error::Trap(t)
    }
}

impl From<TrapCode> for Error {
    fn from(t: TrapCode) -> Self {
        Error::Trap(Trap::new(t))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
