//! Bulk memory and table operations, shared by the interpreter and compiled code.

use super::vm::*;
use crate::error::TrapCode;

/// `memory.copy` within memory 0 (overlapping ranges allowed).
pub(crate) fn memory_copy(vmctx: *mut VmCtx, dst: u32, src: u32, n: u32) -> Result<(), TrapCode> {
    let mem = unsafe { &*(*vmctx).memory };
    let (dst, src, n) = (dst as u64, src as u64, n as u64);
    if src + n > mem.size || dst + n > mem.size {
        return Err(TrapCode::MemoryOutOfBounds);
    }
    unsafe { std::ptr::copy(mem.base.add(src as usize), mem.base.add(dst as usize), n as usize) };
    Ok(())
}

/// `memory.fill`.
pub(crate) fn memory_fill(vmctx: *mut VmCtx, dst: u32, val: u8, n: u32) -> Result<(), TrapCode> {
    let mem = unsafe { &*(*vmctx).memory };
    let (dst, n) = (dst as u64, n as u64);
    if dst + n > mem.size {
        return Err(TrapCode::MemoryOutOfBounds);
    }
    unsafe { std::ptr::write_bytes(mem.base.add(dst as usize), val, n as usize) };
    Ok(())
}

/// `table.fill`.
pub(crate) fn table_fill(vmctx: *mut VmCtx, t: u32, i: u32, v: u64, n: u32) -> Result<(), TrapCode> {
    let tab = unsafe { &mut **(*vmctx).tables.add(t as usize) };
    let (i, n) = (i as u64, n as u64);
    if i + n > tab.size() as u64 {
        return Err(TrapCode::TableOutOfBounds);
    }
    tab.slice_mut()[i as usize..(i + n) as usize].fill(v);
    Ok(())
}

/// `table.copy` (the two tables may be the same; ranges may overlap).
pub(crate) fn table_copy(vmctx: *mut VmCtx, dt: u32, st: u32, d: u32, s: u32, n: u32) -> Result<(), TrapCode> {
    let dtab = unsafe { *(*vmctx).tables.add(dt as usize) };
    let stab = unsafe { *(*vmctx).tables.add(st as usize) };
    let (d, s, n) = (d as u64, s as u64, n as u64);
    unsafe {
        if s + n > (*stab).size() as u64 || d + n > (*dtab).size() as u64 {
            return Err(TrapCode::TableOutOfBounds);
        }
        std::ptr::copy((*stab).elems.add(s as usize), (*dtab).elems.add(d as usize), n as usize);
    }
    Ok(())
}
