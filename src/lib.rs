//! wisp: a compact WebAssembly runtime written from scratch.
//!
//! * [`binary`]: decoder for the WebAssembly 2.0 binary format.
//! * [`validate`]: the validation algorithm of the specification's appendix.
//! * [`interp`]: an interpreter over pre-decoded, slot-addressed bytecode.
//! * `jit`: a single-pass baseline compiler to AArch64 machine code.
//! * [`wasi`]: WASI preview 1 with capability-based file system access.
//!
//! ```
//! use wisp::{Engine, Module, Store, Instance, Val};
//! let wasm = wat::parse_str(r#"(module (func (export "add") (param i32 i32) (result i32)
//!     local.get 0 local.get 1 i32.add))"#).unwrap();
//! let engine = Engine::default();
//! let module = Module::new(&engine, &wasm).unwrap();
//! let mut store = Store::new(&engine, ());
//! let instance = Instance::new(&mut store, &module, &[]).unwrap();
//! let add = instance.get_func(&store, "add").unwrap();
//! assert_eq!(add.call(&mut store, &[Val::I32(2), Val::I32(3)]).unwrap(), vec![Val::I32(5)]);
//! ```

pub mod binary;
pub mod config;
pub mod error;
pub mod interp;
pub(crate) mod jit;
pub mod module;
pub mod num;
pub mod runtime;
pub mod types;
pub mod validate;

pub use config::{Config, Engine, Strategy};
pub use error::{Error, Result, Trap, TrapCode};
pub use module::{CompileStats, Module};
pub use runtime::api::{Caller, Linker, Store};
pub use runtime::values::{Extern, Func, Global, Instance, Memory, Table, Val};
pub use types::{ExternType, FuncType, GlobalType, Limits, MemoryType, TableType, ValType};
