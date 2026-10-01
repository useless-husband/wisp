//! `#[repr(C)]` structures shared by the interpreter, compiled code and the host.
//!
//! Compiled code reaches everything it needs from the instance's [`VmCtx`], kept in a pinned
//! register. Field offsets are part of the code generator's contract (see `OFF_*`).

use crate::types::{MemoryType, PAGE_SIZE, TableType};
use std::ptr;

/// A function reference. Funcref values are pointers to these (null is `ref.null func`).
#[repr(C)]
pub struct VmFuncRef {
    /// Entry point of compiled code, or of a trampoline into the interpreter/host.
    pub code: *const u8,
    /// The callee's instance context (null for host functions).
    pub vmctx: *mut VmCtx,
    /// Canonical (store-wide) type id, compared by `call_indirect`.
    pub type_id: u32,
    /// Index of the function in the store.
    pub func_addr: u32,
    /// For wasm functions: index among the module's defined functions.
    pub def_index: u32,
    /// `KIND_INTERP`, `KIND_HOST` or `KIND_COMPILED`.
    pub kind: u32,
}

pub const FUNCREF_CODE: u32 = 0;
pub const FUNCREF_VMCTX: u32 = 8;
pub const FUNCREF_TYPE_ID: u32 = 16;
pub const FUNCREF_KIND: u32 = 28;
/// A wasm function run by the interpreter.
pub const KIND_INTERP: u32 = 0;
/// A host (Rust) function.
pub const KIND_HOST: u32 = 1;
/// A wasm function compiled to machine code.
pub const KIND_COMPILED: u32 = 2;

/// Linear memory. `base` never moves: the whole maximum size is reserved up front and
/// pages are made accessible as the memory grows.
#[repr(C)]
pub struct VmMemory {
    pub base: *mut u8,
    /// Current size in bytes.
    pub size: u64,
    reserved: usize,
    max_pages: u32,
    pub ty: MemoryType,
}

pub const MEM_BASE: u32 = 0;
pub const MEM_SIZE: u32 = 8;

/// Largest reservation per memory: 4 GiB of addressable space.
const MAX_RESERVATION: u64 = 4 << 30;

impl VmMemory {
    pub fn new(ty: MemoryType) -> Result<Box<VmMemory>, String> {
        let max_pages = ty.limits.max.unwrap_or(crate::types::MAX_PAGES);
        let want = (max_pages as u64 * PAGE_SIZE).min(MAX_RESERVATION);
        let min_bytes = ty.limits.min as u64 * PAGE_SIZE;
        // Reserve address space only; fall back to smaller reservations if the address
        // space is crowded (growth past the reservation then fails, which the spec allows).
        let mut reserve = want.max(min_bytes).max(PAGE_SIZE);
        let base = loop {
            let p = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    reserve as usize,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANON,
                    -1,
                    0,
                )
            };
            if p != libc::MAP_FAILED {
                break p as *mut u8;
            }
            if reserve / 2 < min_bytes.max(PAGE_SIZE) {
                return Err("cannot reserve address space for linear memory".into());
            }
            reserve /= 2;
        };
        let mut m = Box::new(VmMemory { base, size: 0, reserved: reserve as usize, max_pages, ty });
        if m.grow(ty.limits.min) < 0 {
            return Err("cannot allocate initial linear memory".into());
        }
        Ok(m)
    }

    pub fn pages(&self) -> u32 {
        (self.size / PAGE_SIZE) as u32
    }

    /// `memory.grow`: returns the old size in pages or -1.
    pub fn grow(&mut self, delta: u32) -> i32 {
        let old = self.pages();
        let new = old as u64 + delta as u64;
        if new > self.max_pages as u64 {
            return -1;
        }
        let new_bytes = new * PAGE_SIZE;
        if new_bytes > self.reserved as u64 {
            return -1;
        }
        if delta > 0 {
            let r = unsafe {
                libc::mprotect(
                    self.base.add(self.size as usize) as *mut libc::c_void,
                    (delta as u64 * PAGE_SIZE) as usize,
                    libc::PROT_READ | libc::PROT_WRITE,
                )
            };
            if r != 0 {
                return -1;
            }
        }
        self.size = new_bytes;
        self.ty.limits.min = new as u32;
        old as i32
    }

    pub fn as_slice(&self) -> &[u8] {
        if self.size == 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.base, self.size as usize) }
    }

    #[allow(clippy::mut_from_ref)]
    pub fn as_mut_slice(&self) -> &mut [u8] {
        if self.size == 0 {
            return &mut [];
        }
        unsafe { std::slice::from_raw_parts_mut(self.base, self.size as usize) }
    }

    /// Current type (minimum = current size), for import matching.
    pub fn current_type(&self) -> MemoryType {
        let mut t = self.ty;
        t.limits.min = self.pages();
        t
    }
}

impl Drop for VmMemory {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.base as *mut libc::c_void, self.reserved);
        }
    }
}

/// Engine limit on table growth (elements).
pub const MAX_TABLE_ELEMS: u64 = 10_000_000;

/// A table of references (raw: funcref pointers or externref ids+1, 0 = null).
#[repr(C)]
pub struct VmTable {
    pub elems: *mut u64,
    pub len: u64,
    vec: Vec<u64>,
    pub ty: TableType,
}

pub const TABLE_ELEMS: u32 = 0;
pub const TABLE_LEN: u32 = 8;

impl VmTable {
    pub fn new(ty: TableType, init: u64) -> Box<VmTable> {
        let mut t = Box::new(VmTable { elems: ptr::null_mut(), len: 0, vec: Vec::new(), ty });
        t.vec = vec![init; ty.limits.min as usize];
        t.sync();
        t
    }

    fn sync(&mut self) {
        self.elems = self.vec.as_mut_ptr();
        self.len = self.vec.len() as u64;
        self.ty.limits.min = self.vec.len() as u32;
    }

    pub fn size(&self) -> u32 {
        self.vec.len() as u32
    }

    pub fn grow(&mut self, delta: u32, init: u64) -> i32 {
        let old = self.vec.len() as u64;
        let new = old + delta as u64;
        let max = self.ty.limits.max.map(|m| m as u64).unwrap_or(u32::MAX as u64);
        if new > max || new > MAX_TABLE_ELEMS {
            return -1;
        }
        self.vec.resize(new as usize, init);
        self.sync();
        old as i32
    }

    pub fn get(&self, i: u32) -> Option<u64> {
        self.vec.get(i as usize).copied()
    }

    pub fn set(&mut self, i: u32, v: u64) -> bool {
        match self.vec.get_mut(i as usize) {
            Some(e) => {
                *e = v;
                true
            }
            None => false,
        }
    }

    pub fn slice_mut(&mut self) -> &mut [u64] {
        &mut self.vec
    }

    pub fn slice(&self) -> &[u64] {
        &self.vec
    }
}

/// Per-store state reachable from compiled code.
#[repr(C)]
pub struct VmRuntime {
    /// Lowest native stack pointer compiled code may use.
    pub stack_limit: usize,
    /// Remaining fuel; only consulted when metering is enabled.
    pub fuel: i64,
    /// Stack pointer saved by the innermost compiled-code entry, used to unwind on traps.
    pub entry_sp: usize,
    /// Raw trap code being delivered.
    pub trap: u32,
    pub _pad: u32,
    /// The store, for host calls and slow paths.
    pub store: *mut crate::runtime::store::StoreInner,
}

pub const RT_STACK_LIMIT: u32 = 0;
pub const RT_FUEL: u32 = 8;
pub const RT_ENTRY_SP: u32 = 16;
pub const RT_TRAP: u32 = 24;

/// Per-instance context. Compiled code keeps a pointer to it in a pinned register.
#[repr(C)]
pub struct VmCtx {
    /// Memory 0, or null.
    pub memory: *mut VmMemory,
    pub runtime: *mut VmRuntime,
    /// `[*const VmFuncRef; nfuncs]`, imports first.
    pub funcs: *const *const VmFuncRef,
    /// `[*mut u64; nglobals]`: each global's cell.
    pub globals: *const *mut u64,
    /// `[*mut VmTable; ntables]`.
    pub tables: *const *mut VmTable,
    /// `[u32; ntypes]`: canonical id of each module type index.
    pub type_ids: *const u32,
    /// Interpreter code of the module (`*const InterpModule`).
    pub interp: *const u8,
    /// Index of the instance in the store.
    pub instance: u32,
    pub _pad: u32,
}

pub const CTX_MEMORY: u32 = 0;
pub const CTX_RUNTIME: u32 = 8;
pub const CTX_FUNCS: u32 = 16;
pub const CTX_GLOBALS: u32 = 24;
pub const CTX_TABLES: u32 = 32;
pub const CTX_TYPE_IDS: u32 = 40;

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    #[test]
    fn offsets_match_codegen_contract() {
        assert_eq!(offset_of!(VmFuncRef, code), FUNCREF_CODE as usize);
        assert_eq!(offset_of!(VmFuncRef, vmctx), FUNCREF_VMCTX as usize);
        assert_eq!(offset_of!(VmFuncRef, type_id), FUNCREF_TYPE_ID as usize);
        assert_eq!(offset_of!(VmFuncRef, kind), FUNCREF_KIND as usize);
        assert_eq!(offset_of!(VmMemory, base), MEM_BASE as usize);
        assert_eq!(offset_of!(VmMemory, size), MEM_SIZE as usize);
        assert_eq!(offset_of!(VmTable, elems), TABLE_ELEMS as usize);
        assert_eq!(offset_of!(VmTable, len), TABLE_LEN as usize);
        assert_eq!(offset_of!(VmRuntime, stack_limit), RT_STACK_LIMIT as usize);
        assert_eq!(offset_of!(VmRuntime, fuel), RT_FUEL as usize);
        assert_eq!(offset_of!(VmRuntime, entry_sp), RT_ENTRY_SP as usize);
        assert_eq!(offset_of!(VmRuntime, trap), RT_TRAP as usize);
        assert_eq!(offset_of!(VmCtx, memory), CTX_MEMORY as usize);
        assert_eq!(offset_of!(VmCtx, runtime), CTX_RUNTIME as usize);
        assert_eq!(offset_of!(VmCtx, funcs), CTX_FUNCS as usize);
        assert_eq!(offset_of!(VmCtx, globals), CTX_GLOBALS as usize);
        assert_eq!(offset_of!(VmCtx, tables), CTX_TABLES as usize);
        assert_eq!(offset_of!(VmCtx, type_ids), CTX_TYPE_IDS as usize);
    }

    #[test]
    fn memory_grows_in_place() {
        let ty = MemoryType { limits: crate::types::Limits { min: 1, max: Some(4) } };
        let mut m = VmMemory::new(ty).unwrap();
        let base = m.base;
        m.as_mut_slice()[100] = 7;
        assert_eq!(m.grow(2), 1);
        assert_eq!(m.base, base);
        assert_eq!(m.as_slice()[100], 7);
        assert_eq!(m.as_slice()[3 * 65536 - 1], 0);
        assert_eq!(m.grow(2), -1);
        assert_eq!(m.pages(), 3);
    }
}
