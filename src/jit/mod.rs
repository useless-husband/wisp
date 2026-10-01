//! Baseline compiler to AArch64 (placeholder until the compiler lands).

use crate::binary::module::ModuleData;
use crate::error::{Trap, TrapCode};
use crate::runtime::store::StoreInner;
use crate::runtime::vm::VmFuncRef;
use crate::validate::FuncInfo;

pub(crate) struct CompiledModule;

impl CompiledModule {
    pub fn entry(&self, _def: u32) -> Option<*const u8> {
        None
    }
    pub fn code_size(&self) -> usize {
        0
    }
    pub fn fallback_reason(&self, _def: u32) -> Option<String> {
        Some("compiler not available".into())
    }
}

pub(crate) fn available() -> bool {
    false
}

pub(crate) fn compile_module(_m: &ModuleData, _info: &[FuncInfo], _fuel: bool) -> CompiledModule {
    CompiledModule
}

pub(crate) fn host_trampoline() -> *const u8 {
    std::ptr::null()
}

pub(crate) fn interp_trampoline() -> *const u8 {
    std::ptr::null()
}

pub(crate) unsafe fn call_compiled(
    _s: &mut StoreInner,
    _fr: *const VmFuncRef,
    _args: *mut u64,
) -> Result<(), Trap> {
    Err(TrapCode::Host("compiled code is not available".into()).into())
}
