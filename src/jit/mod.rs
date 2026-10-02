//! The baseline compiler and its runtime glue.
//!
//! * [`a64`]: instruction encoder.
//! * [`compile`]: the single-pass compiler.
//! * [`codemem`]: executable memory (MAP_JIT on macOS).
//!
//! Compiled code is entered from Rust through an *entry trampoline* that saves the
//! callee-saved registers, records its stack pointer in `VmRuntime::entry_sp`, copies the
//! arguments to the callee's parameter area and calls it. A trap anywhere below (in compiled
//! code, or reported by a runtime helper) jumps to an exit sequence that resets `sp` to the
//! recorded value and returns the trap code from the entry trampoline, which Rust turns into
//! an `Err(Trap)`. Compiled frames own no resources, so discarding them is safe. Entries nest:
//! each one saves the previous `entry_sp` and restores it on the way out.
//!
//! Calls to host functions and to functions left to the interpreter go through a *slow
//! trampoline* that hands the function reference and the argument area to Rust.

pub mod a64;
pub mod codemem;
pub mod compile;

use crate::binary::module::ModuleData;
use crate::error::{RAW_PENDING, Trap, TrapCode};
use crate::runtime::store::StoreInner;
use crate::runtime::vm::*;
use crate::validate::FuncInfo;
use a64::{Asm, FP, LR, Mem, SP, ZR};
use codemem::CodeMemory;
use compile::{FuncCompiler, ModCtx, RT, Shims, T0, T1, VMCTX, XCALLER, XREF};
use std::sync::OnceLock;

/// Whether this host can run compiled code.
/// The Linux code paths (mprotect, __clear_cache, pthread_getattr_np) exist but have not
/// been tested, so only macOS is enabled.
pub(crate) fn available() -> bool {
    cfg!(all(target_arch = "aarch64", target_os = "macos"))
}

/// Process-wide trampolines.
struct Stubs {
    _mem: CodeMemory,
    entry: usize,
    slow_tramp: usize,
}

type EntryFn = unsafe extern "C" fn(*mut VmCtx, *const u8, *mut u64, *mut VmRuntime, u64) -> u32;

/// Restore the entry trampoline's frame and return `w0` from it.
fn emit_exit(a: &mut Asm) {
    a.ldst_off(Mem::LdrX, T0, RT, RT_ENTRY_SP as i64, T1);
    a.mov_sp(SP, T0);
    a.ldp_post(T0, 2, SP, 16);
    a.ldst_off(Mem::StrX, T0, RT, RT_ENTRY_SP as i64, T1);
    a.ldp_d_post(14, 15, SP, 16);
    a.ldp_d_post(12, 13, SP, 16);
    a.ldp_d_post(10, 11, SP, 16);
    a.ldp_d_post(8, 9, SP, 16);
    a.ldp_post(27, 28, SP, 16);
    a.ldp_post(25, 26, SP, 16);
    a.ldp_post(23, 24, SP, 16);
    a.ldp_post(21, 22, SP, 16);
    a.ldp_post(19, 20, SP, 16);
    a.ldp_post(FP, LR, SP, 16);
    a.ret();
}

fn stubs() -> &'static Stubs {
    static S: OnceLock<Stubs> = OnceLock::new();
    S.get_or_init(|| {
        let mut a = Asm::new();
        // entry(vmctx x0, code x1, args x2, runtime x3, n x4) -> w0
        a.stp_pre(FP, LR, SP, -16);
        a.mov_sp(FP, SP);
        a.stp_pre(19, 20, SP, -16);
        a.stp_pre(21, 22, SP, -16);
        a.stp_pre(23, 24, SP, -16);
        a.stp_pre(25, 26, SP, -16);
        a.stp_pre(27, 28, SP, -16);
        a.stp_d_pre(8, 9, SP, -16);
        a.stp_d_pre(10, 11, SP, -16);
        a.stp_d_pre(12, 13, SP, -16);
        a.stp_d_pre(14, 15, SP, -16);
        a.ldst_off(Mem::LdrX, T0, 3, RT_ENTRY_SP as i64, T1);
        a.stp_pre(T0, 2, SP, -16); // [previous entry_sp, args]
        a.mov_sp(T0, SP);
        a.ldst_off(Mem::StrX, T0, 3, RT_ENTRY_SP as i64, T1);
        a.stp_pre(4, ZR, SP, -16); // [n, 0]
        a.mov(true, RT, 3);
        a.mov(true, VMCTX, 0);
        // Parameter area: n slots rounded up to 16 bytes.
        a.add_imm(true, T0, 4, 1);
        a.lsr_imm(true, T0, T0, 1);
        a.lsl_imm(true, T0, T0, 4);
        a.sub_ext(SP, SP, T0);
        let (lp, done) = (a.new_label(), a.new_label());
        a.movz(true, T1, 0, 0);
        a.bind(lp);
        a.cmp(true, T1, 4);
        a.b_cond(a64::Cond::Hs, done);
        a.ldst_regoff(Mem::LdrX, T0, 2, T1, true);
        a.ldst_regoff(Mem::StrX, T0, SP, T1, true);
        a.add_imm(true, T1, T1, 1);
        a.b(lp);
        a.bind(done);
        let nomem = a.new_label();
        a.ldst_off(Mem::LdrX, T0, VMCTX, CTX_MEMORY as i64, T1);
        a.cbz(true, T0, nomem);
        a.ldst_off(Mem::LdrX, compile::MEMBASE, T0, MEM_BASE as i64, T1);
        a.ldst_off(Mem::LdrX, compile::MEMSIZE, T0, MEM_SIZE as i64, T1);
        a.bind(nomem);
        a.blr(1);
        // Copy results back to the caller's array.
        a.ldst_off(Mem::LdrX, T0, RT, RT_ENTRY_SP as i64, T1);
        a.ldst_off(Mem::LdrX, 2, T0, 8, T1);
        a.ldst_off(Mem::LdrX, 4, T0, -16, T1);
        let (lp, done) = (a.new_label(), a.new_label());
        a.movz(true, T1, 0, 0);
        a.bind(lp);
        a.cmp(true, T1, 4);
        a.b_cond(a64::Cond::Hs, done);
        a.ldst_regoff(Mem::LdrX, T0, SP, T1, true);
        a.ldst_regoff(Mem::StrX, T0, 2, T1, true);
        a.add_imm(true, T1, T1, 1);
        a.b(lp);
        a.bind(done);
        a.movz(false, 0, 0, 0);
        emit_exit(&mut a);

        // slow_tramp: x9 = funcref, x10 = caller vmctx, arguments at the caller's sp.
        let slow = a.pos() as usize * 4;
        a.stp_pre(FP, LR, SP, -16);
        a.mov_sp(FP, SP);
        a.mov(true, 0, XREF);
        a.add_imm(true, 1, FP, 16);
        a.mov(true, 2, XCALLER);
        a.mov_imm(true, T0, shim_slow_call as *const () as u64);
        a.blr(T0);
        let trap = a.new_label();
        a.cbnz(false, 0, trap);
        a.ldp_post(FP, LR, SP, 16);
        a.ret();
        a.bind(trap);
        emit_exit(&mut a);
        a.finish().expect("trampolines");
        let mem =
            CodeMemory::new(&a.bytes()).expect("cannot allocate executable memory for trampolines");
        let base = mem.ptr() as usize;
        Stubs {
            entry: base,
            slow_tramp: base + slow,
            _mem: mem,
        }
    })
}

/// Entry point used by function references to host functions.
pub(crate) fn host_trampoline() -> *const u8 {
    if available() {
        stubs().slow_tramp as *const u8
    } else {
        std::ptr::null()
    }
}

/// Entry point used by function references to interpreted functions.
pub(crate) fn interp_trampoline() -> *const u8 {
    host_trampoline()
}

/// The machine code of one module.
pub(crate) struct CompiledModule {
    mem: Option<CodeMemory>,
    /// Byte offset of each compiled defined function.
    entries: Vec<Option<usize>>,
    reasons: Vec<Option<String>>,
    size: usize,
}

impl CompiledModule {
    pub fn entry(&self, def: u32) -> Option<*const u8> {
        let off = (*self.entries.get(def as usize)?)?;
        Some(unsafe { self.mem.as_ref()?.ptr().add(off) })
    }

    pub fn code_size(&self) -> usize {
        self.size
    }

    pub fn fallback_reason(&self, def: u32) -> Option<String> {
        self.reasons.get(def as usize).cloned().flatten()
    }
}

fn shims() -> Shims {
    Shims {
        slow_tramp: stubs().slow_tramp as u64,
        memory_grow: shim_memory_grow as *const () as u64,
        memory_fill: shim_memory_fill as *const () as u64,
        memory_copy: shim_memory_copy as *const () as u64,
        memory_init: shim_memory_init as *const () as u64,
        data_drop: shim_data_drop as *const () as u64,
        table_grow: shim_table_grow as *const () as u64,
        table_fill: shim_table_fill as *const () as u64,
        table_copy: shim_table_copy as *const () as u64,
        table_init: shim_table_init as *const () as u64,
        elem_drop: shim_elem_drop as *const () as u64,
    }
}

/// Compile every defined function; functions the compiler rejects get a stub that calls
/// the interpreter.
pub(crate) fn compile_module(m: &ModuleData, info: &[FuncInfo], fuel: bool) -> CompiledModule {
    let ndef = m.num_defined_funcs() as usize;
    let all_interp = |why: String| CompiledModule {
        mem: None,
        entries: vec![None; ndef],
        reasons: vec![Some(why); ndef],
        size: 0,
    };
    let shims = shims();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut asm = Asm::new();
        let labels: Vec<a64::Label> = (0..ndef).map(|_| asm.new_label()).collect();
        let trap_exit = asm.new_label();
        let cx = ModCtx {
            m,
            entries: &labels,
            trap_exit,
            shims: &shims,
            fuel,
        };
        let mut reasons: Vec<Option<String>> = vec![None; ndef];
        for def in 0..ndef {
            let cp = asm.checkpoint();
            asm.bind(labels[def]);
            if let Err(e) = FuncCompiler::compile(&mut asm, &cx, def as u32, &info[def]) {
                asm.rollback(cp);
                asm.bind(labels[def]);
                // Stub: call the interpreter through this function's reference.
                let f = m.num_imported_funcs as i64 + def as i64;
                asm.ldst_off(Mem::LdrX, T0, VMCTX, CTX_FUNCS as i64, T1);
                asm.ldst_off(Mem::LdrX, XREF, T0, 8 * f, T1);
                asm.mov(true, XCALLER, VMCTX);
                asm.mov_imm(true, T0, shims.slow_tramp);
                asm.br(T0);
                reasons[def] = Some(e);
            }
        }
        asm.bind(trap_exit);
        emit_exit(&mut asm);
        asm.finish()?;
        let bytes = asm.bytes();
        let entries: Vec<Option<usize>> = (0..ndef)
            .map(|d| {
                if reasons[d].is_some() {
                    None
                } else {
                    Some(asm.label_pos(labels[d]).unwrap() as usize * 4)
                }
            })
            .collect();
        Ok::<_, String>((bytes, entries, reasons))
    }));
    let (bytes, entries, reasons) = match r {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return all_interp(e),
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned());
            return all_interp(format!("compiler failure: {}", msg.unwrap_or_default()));
        }
    };
    match CodeMemory::new(&bytes) {
        Ok(mem) => CompiledModule {
            size: bytes.len(),
            mem: Some(mem),
            entries,
            reasons,
        },
        Err(e) => all_interp(e),
    }
}

// ---- entering compiled code ----

#[cfg(target_os = "macos")]
fn thread_stack_low() -> usize {
    unsafe {
        let t = libc::pthread_self();
        let hi = libc::pthread_get_stackaddr_np(t) as usize;
        hi - libc::pthread_get_stacksize_np(t)
    }
}

#[cfg(target_os = "linux")]
fn thread_stack_low() -> usize {
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
            return 0;
        }
        let mut addr: *mut libc::c_void = std::ptr::null_mut();
        let mut size: libc::size_t = 0;
        libc::pthread_attr_getstack(&attr, &mut addr, &mut size);
        libc::pthread_attr_destroy(&mut attr);
        addr as usize
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn thread_stack_low() -> usize {
    0
}

/// The lowest stack address compiled code may use: `budget` below here, but always leaving
/// headroom above the thread's guard page for Rust code called from wasm.
fn stack_limit(budget: usize) -> usize {
    let marker = 0u8;
    let here = &marker as *const u8 as usize;
    let floor = thread_stack_low() + (256 << 10);
    here.saturating_sub(budget).max(floor)
}

fn take_trap(store: &mut StoreInner, code: u32) -> Trap {
    if code == RAW_PENDING {
        return store
            .pending_trap
            .take()
            .unwrap_or_else(|| Trap::host("trap lost in transit"));
    }
    match TrapCode::from_raw(code) {
        Some(TrapCode::UninitializedElement) => Trap::uninitialized(store.runtime.trap_arg as u32),
        Some(c) => Trap::new(c),
        None => Trap::host(format!("unknown trap code {code}")),
    }
}

/// Call compiled function `fr` with arguments/results in `args`.
///
/// # Safety
/// `fr` must be a compiled function of a live instance in `store`; `args` must have room
/// for `max(params, results)` values.
pub(crate) unsafe fn call_compiled(
    store: &mut StoreInner,
    fr: *const VmFuncRef,
    args: *mut u64,
) -> Result<(), Trap> {
    unsafe {
        let st = stubs();
        let ty = &store.funcs[(*fr).func_addr as usize].ty;
        let n = ty.params.len().max(ty.results.len()) as u64;
        if store.native_entries == 0 {
            store.runtime.stack_limit = stack_limit(store.config.native_stack_budget);
        }
        store.native_entries += 1;
        let rt: *mut VmRuntime = &mut *store.runtime;
        (*rt).store = store as *mut StoreInner;
        let entry: EntryFn = std::mem::transmute(st.entry);
        let code = entry((*fr).vmctx, (*fr).code, args, rt, n);
        store.native_entries -= 1;
        if code == 0 {
            Ok(())
        } else {
            Err(take_trap(store, code))
        }
    }
}

// ---- runtime helpers called from compiled code ----

unsafe fn store_of(vmctx: *mut VmCtx) -> *mut StoreInner {
    unsafe { (*(*vmctx).runtime).store }
}

fn stash(store: *mut StoreInner, t: Trap) -> u32 {
    unsafe { (*store).pending_trap = Some(t) };
    RAW_PENDING
}

fn code_of(r: Result<(), TrapCode>) -> u32 {
    match r {
        Ok(()) => 0,
        Err(t) => t.to_raw(),
    }
}

unsafe extern "C" fn shim_slow_call(
    fr: *const VmFuncRef,
    args: *mut u64,
    caller: *mut VmCtx,
) -> u32 {
    unsafe {
        let store = store_of(caller);
        let inst = Some((*caller).instance);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (*store).call_raw(fr, args, inst)
        }));
        match r {
            Ok(Ok(())) => 0,
            Ok(Err(t)) => stash(store, t),
            Err(_) => stash(store, Trap::host("host function panicked")),
        }
    }
}

unsafe extern "C" fn shim_memory_grow(vmctx: *mut VmCtx, delta: u32) -> u32 {
    unsafe { (*(*vmctx).memory).grow(delta) as u32 }
}

unsafe extern "C" fn shim_memory_fill(vmctx: *mut VmCtx, dst: u32, val: u32, n: u32) -> u32 {
    code_of(crate::runtime::ops::memory_fill(vmctx, dst, val as u8, n))
}

unsafe extern "C" fn shim_memory_copy(vmctx: *mut VmCtx, dst: u32, src: u32, n: u32) -> u32 {
    code_of(crate::runtime::ops::memory_copy(vmctx, dst, src, n))
}

unsafe extern "C" fn shim_memory_init(
    vmctx: *mut VmCtx,
    seg: u32,
    dst: u32,
    src: u32,
    n: u32,
) -> u32 {
    unsafe { code_of((*store_of(vmctx)).memory_init(vmctx, seg, dst, src, n)) }
}

unsafe extern "C" fn shim_data_drop(vmctx: *mut VmCtx, seg: u32) -> u32 {
    unsafe { (*store_of(vmctx)).data_drop(vmctx, seg) };
    0
}

unsafe extern "C" fn shim_table_grow(vmctx: *mut VmCtx, t: u32, init: u64, delta: u32) -> u32 {
    unsafe { (**(*vmctx).tables.add(t as usize)).grow(delta, init) as u32 }
}

unsafe extern "C" fn shim_table_fill(vmctx: *mut VmCtx, t: u32, i: u32, val: u64, n: u32) -> u32 {
    code_of(crate::runtime::ops::table_fill(vmctx, t, i, val, n))
}

unsafe extern "C" fn shim_table_copy(
    vmctx: *mut VmCtx,
    dt: u32,
    st: u32,
    d: u32,
    s: u32,
    n: u32,
) -> u32 {
    code_of(crate::runtime::ops::table_copy(vmctx, dt, st, d, s, n))
}

unsafe extern "C" fn shim_table_init(
    vmctx: *mut VmCtx,
    t: u32,
    seg: u32,
    d: u32,
    s: u32,
    n: u32,
) -> u32 {
    unsafe { code_of((*store_of(vmctx)).table_init(vmctx, t, seg, d, s, n)) }
}

unsafe extern "C" fn shim_elem_drop(vmctx: *mut VmCtx, seg: u32) -> u32 {
    unsafe { (*store_of(vmctx)).elem_drop(vmctx, seg) };
    0
}
