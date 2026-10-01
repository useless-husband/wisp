//! Single-pass baseline compiler from WebAssembly to AArch64.
//!
//! The compiler walks a validated function body once, keeping an abstract operand stack.
//! Each entry is in a register, in its spill slot (one 8-byte slot per stack depth), in the
//! condition flags (the result of a comparison not yet materialised), or a constant not yet
//! materialised. Registers are taken from a pool and the deepest register-held entry is
//! spilled when the pool runs out. At control-flow boundaries (block/loop/if entries, label
//! targets, calls) everything is in its slot, so merges need no reconciliation: a branch only
//! stores the values it carries into the target's slots.
//!
//! Register conventions inside compiled code:
//!
//! | register | use                                                       |
//! |----------|-----------------------------------------------------------|
//! | x28      | the instance's `VmCtx`                                    |
//! | x27/x26  | linear memory base / size in bytes (size reloaded after calls) |
//! | x25      | the store's `VmRuntime` (stack limit, fuel, trap unwinding) |
//! | x29/x30  | frame pointer / link register                              |
//! | x16/x17  | scratch                                                    |
//! | x9/x10   | callee function reference / caller context for slow calls  |
//! | x0-x8, x11-x15, x19-x24 | operand cache                               |
//! | v0-v29   | operand cache (floats); v30/v31 scratch                    |
//!
//! Frame layout (sp-relative unless noted): `[sp, sp+OUT)` outgoing call area,
//! `sp+OUT` saved `VmCtx`, then one slot per operand stack depth, then the non-parameter
//! locals, then the saved fp/lr pair. Parameters (and results) live in the caller's outgoing
//! area at `[x29 + 16 + 8*i]`.

use super::a64::*;
use crate::binary::module::ModuleData;
use crate::binary::ops::*;
use crate::binary::reader::Reader;
use crate::error::TrapCode;
use crate::runtime::vm::*;
use crate::types::ValType;
use crate::validate::FuncInfo;
use std::collections::HashMap;

pub const VMCTX: u8 = 28;
pub const MEMBASE: u8 = 27;
pub const MEMSIZE: u8 = 26;
pub const RT: u8 = 25;
pub const T0: u8 = 16;
pub const T1: u8 = 17;
pub const XREF: u8 = 9;
pub const XCALLER: u8 = 10;
const F0: u8 = 31;
const F1: u8 = 30;

const GPR_POOL: u32 = 0x01F8_F9FF; // x0-x8, x11-x15, x19-x24
const FPR_POOL: u32 = 0x3FFF_FFFF; // v0-v29

/// Addresses of the runtime helpers compiled code calls.
pub(crate) struct Shims {
    pub slow_tramp: u64,
    pub memory_grow: u64,
    pub memory_fill: u64,
    pub memory_copy: u64,
    pub memory_init: u64,
    pub data_drop: u64,
    pub table_grow: u64,
    pub table_fill: u64,
    pub table_copy: u64,
    pub table_init: u64,
    pub elem_drop: u64,
}

pub(crate) struct ModCtx<'a> {
    pub m: &'a ModuleData,
    /// Entry label of every defined function.
    pub entries: &'a [Label],
    /// Module-local trap exit (expects the raw trap code in w0).
    pub trap_exit: Label,
    pub shims: &'a Shims,
    pub fuel: bool,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Loc {
    Gpr(u8),
    Fpr(u8),
    Slot,
    Imm(u64),
    Flags(Cond),
}

#[derive(Copy, Clone, Debug)]
struct Val {
    ty: ValType,
    loc: Loc,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Kind {
    Block,
    Loop,
    If,
    Else,
}

struct Ctrl {
    kind: Kind,
    height: usize,
    params: Vec<ValType>,
    results: Vec<ValType>,
    /// End label (block/if) or header (loop).
    label: Label,
    /// For `if`: the label of the else arm while it is still unbound.
    else_label: Option<Label>,
    /// Some branch targets the end label.
    branched: bool,
}

fn is_float(t: ValType) -> bool {
    matches!(t, ValType::F32 | ValType::F64)
}

fn is64(t: ValType) -> bool {
    !matches!(t, ValType::I32 | ValType::F32)
}

fn load_kind(t: ValType) -> Mem {
    match t {
        ValType::I32 => Mem::LdrW,
        ValType::F32 => Mem::LdrS,
        ValType::F64 => Mem::LdrD,
        _ => Mem::LdrX,
    }
}

fn store_kind(t: ValType) -> Mem {
    match t {
        ValType::F32 => Mem::StrS,
        ValType::F64 => Mem::StrD,
        _ => Mem::StrX,
    }
}

pub(crate) struct FuncCompiler<'a, 'b> {
    a: &'b mut Asm,
    cx: &'b ModCtx<'a>,
    func: u32,
    locals: Vec<ValType>,
    nparams: usize,
    results: Vec<ValType>,
    stack: Vec<Val>,
    ctrls: Vec<Ctrl>,
    free_gpr: u32,
    free_fpr: u32,
    out: i64,
    slots_base: i64,
    locals_base: i64,
    frame: i64,
    dead: bool,
    dead_depth: u32,
    traps: HashMap<u32, Label>,
    epilogue: Label,
    has_memory: bool,
    fuel_at: Option<usize>,
    fuel_n: u32,
}

macro_rules! bail {
    ($($t:tt)*) => { return Err(format!($($t)*)) };
}

impl<'a, 'b> FuncCompiler<'a, 'b> {
    pub fn compile(
        a: &'b mut Asm,
        cx: &'b ModCtx<'a>,
        def: u32,
        info: &FuncInfo,
    ) -> Result<(), String> {
        let m = cx.m;
        let func = m.num_imported_funcs + def;
        let ft = m.func_type(func);
        let body = &m.bodies[def as usize];
        let mut locals = ft.params.to_vec();
        locals.extend_from_slice(&body.locals);
        let nl = body.locals.len() as i64;
        let out = 8 * info.max_call_slots as i64;
        let slots_base = out + 8;
        let locals_base = slots_base + 8 * info.max_height as i64;
        let frame = (locals_base + 8 * nl + 15) & !15;
        if frame > (1 << 24) {
            bail!("frame of {frame} bytes is too large");
        }
        let epilogue = a.new_label();
        let mut c = FuncCompiler {
            a,
            cx,
            func,
            nparams: ft.params.len(),
            results: ft.results.to_vec(),
            locals,
            stack: Vec::new(),
            ctrls: Vec::new(),
            free_gpr: GPR_POOL,
            free_fpr: FPR_POOL,
            out,
            slots_base,
            locals_base,
            frame,
            dead: false,
            dead_depth: 0,
            traps: HashMap::new(),
            epilogue,
            has_memory: !m.memories.is_empty(),
            fuel_at: None,
            fuel_n: 0,
        };
        c.prologue();
        let r = Reader::new(&m.bytes).sub(body.code_start, body.code_end, "unexpected end");
        let mut ops = OpReader::new(r);
        // The function body is an implicit block whose end returns.
        let end = c.a.new_label();
        c.ctrls.push(Ctrl {
            kind: Kind::Block,
            height: 0,
            params: vec![],
            results: c.results.clone(),
            label: end,
            else_label: None,
            branched: false,
        });
        while !c.ctrls.is_empty() {
            let op = ops.read().map_err(|e| e.to_string())?;
            c.op(op)?;
        }
        c.finish_fuel();
        // Epilogue: results are already in the caller's area.
        c.a.bind(c.epilogue);
        c.a.mov_sp(SP, FP);
        c.a.ldp_post(FP, LR, SP, 16);
        c.a.ret();
        let mut traps: Vec<(u32, Label)> = c.traps.iter().map(|(k, v)| (*k, *v)).collect();
        traps.sort_by_key(|t| t.0);
        for (code, l) in traps {
            c.a.bind(l);
            c.a.movz(false, 0, code as u16, 0);
            c.a.b(c.cx.trap_exit);
        }
        Ok(())
    }

    // ---- frame and registers ----

    fn slot_off(&self, depth: usize) -> i64 {
        self.slots_base + 8 * depth as i64
    }

    /// (base register, offset) of local `i`.
    fn local_addr(&self, i: u32) -> (u8, i64) {
        let i = i as usize;
        if i < self.nparams {
            (FP, 16 + 8 * i as i64)
        } else {
            (SP, self.locals_base + 8 * (i - self.nparams) as i64)
        }
    }

    fn trap(&mut self, code: TrapCode) -> Label {
        let raw = code.to_raw();
        if let Some(l) = self.traps.get(&raw) {
            return *l;
        }
        let l = self.a.new_label();
        self.traps.insert(raw, l);
        l
    }

    fn prologue(&mut self) {
        let need = self.frame + 16;
        // x16 = sp - frame - 16; trap if below the stack limit.
        if Asm::addsub_imm_ok(need as u64) {
            self.a.sub_imm(true, T0, SP, need as u32);
        } else {
            self.a.mov_imm(true, T1, need as u64);
            self.a.sub_ext(T0, SP, T1);
        }
        self.a
            .ldst_off(Mem::LdrX, T1, RT, RT_STACK_LIMIT as i64, T1);
        self.a.cmp(true, T0, T1);
        let so = self.trap(TrapCode::StackExhausted);
        self.a.b_cond(Cond::Lo, so);
        self.a.stp_pre(FP, LR, SP, -16);
        self.a.mov_sp(FP, SP);
        self.a.mov_sp(SP, T0);
        self.a.ldst_off(Mem::StrX, VMCTX, SP, self.out, T1);
        // Zero the declared locals.
        let nl = self.locals.len() - self.nparams;
        let mut j = 0;
        while j < nl {
            let off = self.locals_base + 8 * j as i64;
            if j + 1 < nl && off % 8 == 0 && off < 504 {
                self.a.stp(ZR, ZR, SP, off as i32);
                j += 2;
            } else {
                self.a.ldst_off(Mem::StrX, ZR, SP, off, T1);
                j += 1;
            }
        }
    }

    fn alloc(&mut self, float: bool) -> u8 {
        let pool = if float { self.free_fpr } else { self.free_gpr };
        if pool == 0 {
            self.spill_one(float);
        }
        let pool = if float {
            &mut self.free_fpr
        } else {
            &mut self.free_gpr
        };
        let r = pool.trailing_zeros() as u8;
        *pool &= !(1 << r);
        r
    }

    fn alloc_for(&mut self, t: ValType) -> u8 {
        self.alloc(is_float(t))
    }

    fn free_loc(&mut self, l: Loc) {
        match l {
            Loc::Gpr(r) => {
                debug_assert!(GPR_POOL & (1 << r) != 0);
                self.free_gpr |= 1 << r;
            }
            Loc::Fpr(r) => self.free_fpr |= 1 << r,
            _ => {}
        }
    }

    fn take_reg(&mut self, float: bool, r: u8) {
        if float {
            self.free_fpr &= !(1 << r);
        } else {
            self.free_gpr &= !(1 << r);
        }
    }

    fn spill_one(&mut self, float: bool) {
        for i in 0..self.stack.len() {
            let hit = match self.stack[i].loc {
                Loc::Gpr(_) => !float,
                Loc::Fpr(_) => float,
                _ => false,
            };
            if hit {
                self.spill(i);
                return;
            }
        }
        panic!("register pool exhausted with nothing to spill");
    }

    /// Store entry `i` into its slot (constants stay constants).
    fn spill(&mut self, i: usize) {
        let v = self.stack[i];
        let off = self.slot_off(i);
        match v.loc {
            Loc::Gpr(r) => {
                self.a.ldst_off(Mem::StrX, r, SP, off, T1);
                self.free_loc(v.loc);
            }
            Loc::Fpr(r) => {
                self.a.ldst_off(store_kind(v.ty), r, SP, off, T1);
                self.free_loc(v.loc);
            }
            Loc::Flags(c) => {
                self.a.cset(false, T0, c);
                self.a.ldst_off(Mem::StrX, T0, SP, off, T1);
            }
            Loc::Slot | Loc::Imm(_) => return,
        }
        self.stack[i].loc = Loc::Slot;
    }

    fn spill_all(&mut self) {
        for i in 0..self.stack.len() {
            self.spill(i);
        }
    }

    /// Write entry `i` into its own slot, constants included.
    fn to_slot(&mut self, i: usize) {
        if let Loc::Imm(v) = self.stack[i].loc {
            let off = self.slot_off(i);
            self.store_imm(v, SP, off);
            self.stack[i].loc = Loc::Slot;
        } else {
            self.spill(i);
        }
    }

    fn store_imm(&mut self, v: u64, base: u8, off: i64) {
        if v == 0 {
            self.a.ldst_off(Mem::StrX, ZR, base, off, T1);
        } else {
            self.a.mov_imm(true, T0, v);
            self.a.ldst_off(Mem::StrX, T0, base, off, T1);
        }
    }

    /// Store value `v` (from stack depth `depth`) at `[base + off]` without changing the stack.
    fn store_val(&mut self, v: Val, depth: usize, base: u8, off: i64) {
        match v.loc {
            Loc::Gpr(r) => self.a.ldst_off(Mem::StrX, r, base, off, T1),
            Loc::Fpr(r) => self.a.ldst_off(store_kind(v.ty), r, base, off, T1),
            Loc::Imm(x) => self.store_imm(x, base, off),
            Loc::Slot => {
                let src = self.slot_off(depth);
                if base == SP && src == off {
                    return;
                }
                self.a.ldst_off(Mem::LdrX, T0, SP, src, T1);
                self.a.ldst_off(Mem::StrX, T0, base, off, T1);
            }
            Loc::Flags(c) => {
                self.a.cset(false, T0, c);
                self.a.ldst_off(Mem::StrX, T0, base, off, T1);
            }
        }
    }

    fn push(&mut self, ty: ValType, loc: Loc) {
        self.stack.push(Val { ty, loc });
    }

    fn push_reg(&mut self, ty: ValType, r: u8) {
        let loc = if is_float(ty) {
            Loc::Fpr(r)
        } else {
            Loc::Gpr(r)
        };
        self.push(ty, loc);
    }

    fn pop(&mut self) -> (Val, usize) {
        let v = self.stack.pop().expect("validated stack");
        (v, self.stack.len())
    }

    /// Materialise a popped integer/reference value in a general register we own.
    fn gpr(&mut self, (v, depth): (Val, usize)) -> u8 {
        match v.loc {
            Loc::Gpr(r) => r,
            Loc::Slot => {
                let r = self.alloc(false);
                self.a
                    .ldst_off(load_kind(v.ty), r, SP, self.slot_off(depth), T1);
                r
            }
            Loc::Imm(x) => {
                let r = self.alloc(false);
                self.a.mov_imm(is64(v.ty), r, x);
                r
            }
            Loc::Flags(c) => {
                let r = self.alloc(false);
                self.a.cset(false, r, c);
                r
            }
            Loc::Fpr(_) => unreachable!("float in a general register"),
        }
    }

    /// Materialise a popped float in a vector register we own.
    fn fpr(&mut self, (v, depth): (Val, usize)) -> u8 {
        let dbl = v.ty == ValType::F64;
        match v.loc {
            Loc::Fpr(r) => r,
            Loc::Slot => {
                let r = self.alloc(true);
                self.a
                    .ldst_off(load_kind(v.ty), r, SP, self.slot_off(depth), T1);
                r
            }
            Loc::Imm(x) => {
                let r = self.alloc(true);
                if x == 0 {
                    self.a.fmov_from_gpr(dbl, r, ZR);
                } else {
                    self.a.mov_imm(dbl, T0, x);
                    self.a.fmov_from_gpr(dbl, r, T0);
                }
                r
            }
            _ => unreachable!("integer in a float register"),
        }
    }

    fn reg(&mut self, pv: (Val, usize)) -> u8 {
        if is_float(pv.0.ty) {
            self.fpr(pv)
        } else {
            self.gpr(pv)
        }
    }

    /// Materialise a pending comparison on top of the stack.
    fn flush_flags(&mut self) {
        if let Some(Val {
            loc: Loc::Flags(c),
            ty,
        }) = self.stack.last().copied()
        {
            let r = self.alloc(false);
            self.a.cset(false, r, c);
            let n = self.stack.len();
            self.stack[n - 1] = Val {
                ty,
                loc: Loc::Gpr(r),
            };
        }
    }

    fn reload_memsize(&mut self) {
        if self.has_memory {
            self.a.ldst_off(Mem::LdrX, T0, VMCTX, CTX_MEMORY as i64, T1);
            self.a.ldst_off(Mem::LdrX, MEMSIZE, T0, MEM_SIZE as i64, T1);
        }
    }

    // ---- fuel ----

    fn charge(&mut self) {
        if !self.cx.fuel {
            return;
        }
        if self.fuel_at.is_some() && self.fuel_n < 4095 {
            self.fuel_n += 1;
            return;
        }
        self.finish_fuel();
        // SUBS below clobbers the flags a pending comparison lives in.
        self.flush_flags();
        self.a.ldst_off(Mem::LdrX, T0, RT, RT_FUEL as i64, T1);
        self.fuel_at = Some(self.a.pos() as usize);
        self.a.subs_imm(true, T0, T0, 0);
        self.a.ldst_off(Mem::StrX, T0, RT, RT_FUEL as i64, T1);
        let l = self.trap(TrapCode::OutOfFuel);
        self.a.b_cond(Cond::Lt, l);
        self.fuel_n = 1;
    }

    fn finish_fuel(&mut self) {
        if let Some(p) = self.fuel_at.take() {
            self.a.code[p] |= self.fuel_n << 10;
        }
    }

    /// A position other code can branch to.
    fn label_here(&mut self, l: Label) {
        self.finish_fuel();
        self.a.bind(l);
    }

    // ---- control helpers ----

    fn block_sig(&self, bt: BlockType) -> (Vec<ValType>, Vec<ValType>) {
        match bt {
            BlockType::Empty => (vec![], vec![]),
            BlockType::Value(t) => (vec![], vec![t]),
            BlockType::Func(i) => {
                let t = &self.cx.m.types[i as usize];
                (t.params.to_vec(), t.results.to_vec())
            }
        }
    }

    fn frame_index(&self, depth: u32) -> usize {
        self.ctrls.len() - 1 - depth as usize
    }

    fn arity(&self, fi: usize) -> usize {
        let c = &self.ctrls[fi];
        if c.kind == Kind::Loop {
            c.params.len()
        } else {
            c.results.len()
        }
    }

    /// Whether branching to frame `fi` (0 = the function) needs value moves.
    fn needs_moves(&self, fi: usize) -> bool {
        if fi == 0 {
            return !self.results.is_empty();
        }
        let n = self.arity(fi);
        let base = self.stack.len() - n;
        let h = self.ctrls[fi].height;
        (0..n).any(|i| !(base + i == h + i && self.stack[base + i].loc == Loc::Slot))
    }

    /// Store the branch values into the target's slots (or the results area for the function).
    fn branch_moves(&mut self, fi: usize) {
        if fi == 0 {
            let n = self.results.len();
            let base = self.stack.len() - n;
            for i in 0..n {
                let v = self.stack[base + i];
                self.store_val(v, base + i, FP, 16 + 8 * i as i64);
            }
            return;
        }
        let n = self.arity(fi);
        let base = self.stack.len() - n;
        let h = self.ctrls[fi].height;
        // Sources are at or above their destinations, so ascending order is safe.
        for i in 0..n {
            let v = self.stack[base + i];
            let off = self.slot_off(h + i);
            self.store_val(v, base + i, SP, off);
        }
    }

    fn jump(&mut self, fi: usize) {
        if fi == 0 {
            self.a.b(self.epilogue);
            return;
        }
        let l = self.ctrls[fi].label;
        if self.ctrls[fi].kind != Kind::Loop {
            self.ctrls[fi].branched = true;
        }
        self.a.b(l);
    }

    fn set_dead(&mut self) {
        let h = self.ctrls.last().unwrap().height;
        while self.stack.len() > h {
            let (v, _) = self.pop();
            self.free_loc(v.loc);
        }
        self.dead = true;
    }

    /// Pop a condition and return how to test it: flags, or a register (nonzero = true).
    fn pop_cond(&mut self) -> Result<Cond, u8> {
        let pv = self.pop();
        if let Loc::Flags(c) = pv.0.loc {
            return Ok(c);
        }
        if let Loc::Imm(x) = pv.0.loc {
            // Constant condition: compare a zero register against itself.
            self.a.cmp(false, ZR, ZR);
            return Ok(if x as u32 != 0 { Cond::Eq } else { Cond::Ne });
        }
        Err(self.gpr(pv))
    }

    /// Branch to `l` when the condition is `want`.
    fn branch_on(&mut self, c: &Result<Cond, u8>, want: bool, l: Label) {
        match *c {
            Ok(cc) => self.a.b_cond(if want { cc } else { cc.invert() }, l),
            Err(r) => {
                if want {
                    self.a.cbnz(false, r, l)
                } else {
                    self.a.cbz(false, r, l)
                }
            }
        }
    }

    fn free_cond(&mut self, c: Result<Cond, u8>) {
        if let Err(r) = c {
            self.free_loc(Loc::Gpr(r));
        }
    }

    // ---- calls ----

    /// Move the top `n` values into the outgoing area and spill the rest.
    fn call_args(&mut self, n: usize) {
        let base = self.stack.len() - n;
        for i in 0..n {
            let v = self.stack[base + i];
            self.store_val(v, base + i, SP, 8 * i as i64);
        }
        for _ in 0..n {
            let (v, _) = self.pop();
            self.free_loc(v.loc);
        }
        self.spill_all();
    }

    fn call_results(&mut self, results: &[ValType]) {
        for (j, &t) in results.iter().enumerate() {
            let r = self.alloc_for(t);
            self.a.ldst_off(load_kind(t), r, SP, 8 * j as i64, T1);
            self.push_reg(t, r);
        }
    }

    /// Call through the `VmFuncRef` in x9.
    fn call_funcref(&mut self) {
        self.a
            .ldst_off(Mem::LdrX, T0, XREF, FUNCREF_CODE as i64, T1);
        self.a.mov(true, XCALLER, VMCTX);
        self.a
            .ldst_off(Mem::LdrX, VMCTX, XREF, FUNCREF_VMCTX as i64, T1);
        self.a.blr(T0);
        self.a.ldst_off(Mem::LdrX, VMCTX, SP, self.out, T1);
    }

    /// Call a runtime helper with integer arguments taken from the listed sources.
    /// Everything must already be spilled.
    fn call_shim(&mut self, addr: u64, args: &[ShimArg]) {
        for (i, a) in args.iter().enumerate() {
            let r = i as u8;
            match *a {
                ShimArg::Vmctx => self.a.mov(true, r, VMCTX),
                ShimArg::Slot(d, t) => match self.stack[d].loc {
                    // Constants are not written to their slots by `spill_all`.
                    Loc::Imm(x) => {
                        self.a
                            .mov_imm(true, r, if is64(t) { x } else { x & 0xFFFF_FFFF })
                    }
                    _ => self.a.ldst_off(load_kind(t), r, SP, self.slot_off(d), T1),
                },
                ShimArg::Imm(v) => self.a.mov_imm(true, r, v),
            }
        }
        self.a.mov_imm(true, T0, addr);
        self.a.blr(T0);
    }

    /// After a helper returning a trap code in w0: trap if nonzero.
    fn check_w0(&mut self) {
        self.a.cbnz(false, 0, self.cx.trap_exit);
    }

    // ---- memory ----

    /// Compute the address of a `size`-byte access at `addr + offset`, trapping when out of
    /// bounds. Returns the register holding the in-memory index (relative to x27).
    fn mem_index(&mut self, ra: u8, offset: u32, size: u32) -> u8 {
        let idx = if offset == 0 {
            ra
        } else if Asm::addsub_imm_ok(offset as u64) {
            self.a.add_imm(true, T0, ra, offset);
            T0
        } else {
            self.a.mov_imm(true, T0, offset as u64);
            self.a.add(true, T0, ra, T0);
            T0
        };
        self.a.add_imm(true, T1, idx, size);
        self.a.cmp(true, T1, MEMSIZE);
        let oob = self.trap(TrapCode::MemoryOutOfBounds);
        self.a.b_cond(Cond::Hi, oob);
        idx
    }

    fn load(&mut self, op: LoadOp, ma: MemArg) {
        let pv = self.pop();
        let ra = self.gpr(pv);
        let size = 1u32 << op.width_log2();
        let idx = self.mem_index(ra, ma.offset, size);
        use LoadOp::*;
        let (k, ty) = match op {
            I32Load => (Mem::LdrW, ValType::I32),
            I64Load => (Mem::LdrX, ValType::I64),
            F32Load => (Mem::LdrS, ValType::F32),
            F64Load => (Mem::LdrD, ValType::F64),
            I32Load8S => (Mem::LdrSbW, ValType::I32),
            I32Load8U => (Mem::LdrB, ValType::I32),
            I32Load16S => (Mem::LdrShW, ValType::I32),
            I32Load16U => (Mem::LdrH, ValType::I32),
            I64Load8S => (Mem::LdrSbX, ValType::I64),
            I64Load8U => (Mem::LdrB, ValType::I64),
            I64Load16S => (Mem::LdrShX, ValType::I64),
            I64Load16U => (Mem::LdrH, ValType::I64),
            I64Load32S => (Mem::LdrSwX, ValType::I64),
            I64Load32U => (Mem::LdrW, ValType::I64),
        };
        if is_float(ty) {
            let rd = self.alloc(true);
            self.a.ldst_regoff(k, rd, MEMBASE, idx, false);
            self.free_loc(Loc::Gpr(ra));
            self.push_reg(ty, rd);
        } else {
            self.a.ldst_regoff(k, ra, MEMBASE, idx, false);
            self.push_reg(ty, ra);
        }
    }

    fn store(&mut self, op: StoreOp, ma: MemArg) {
        let pv = self.pop();
        let rv = self.reg(pv);
        let pa = self.pop();
        let ra = self.gpr(pa);
        let size = 1u32 << op.width_log2();
        let idx = self.mem_index(ra, ma.offset, size);
        use StoreOp::*;
        let k = match op {
            I32Store | I64Store32 => Mem::StrW,
            I64Store => Mem::StrX,
            F32Store => Mem::StrS,
            F64Store => Mem::StrD,
            I32Store8 | I64Store8 => Mem::StrB,
            I32Store16 | I64Store16 => Mem::StrH,
        };
        self.a.ldst_regoff(k, rv, MEMBASE, idx, false);
        self.free_loc(pv.0.loc_after(rv));
        self.free_loc(Loc::Gpr(ra));
    }

    // ---- numeric ----

    fn const_fold(&mut self, op: NumOp) -> bool {
        let (params, rty) = op.signature();
        let n = params.len();
        let len = self.stack.len();
        let imms: Vec<u64> = self.stack[len - n..]
            .iter()
            .filter_map(|v| {
                if let Loc::Imm(x) = v.loc {
                    Some(x)
                } else {
                    None
                }
            })
            .collect();
        if imms.len() != n {
            return false;
        }
        let r = crate::num::eval(op, imms[0], if n == 2 { imms[1] } else { 0 });
        match r {
            Ok(v) => {
                self.stack.truncate(len - n);
                self.push(rty, Loc::Imm(v));
                true
            }
            Err(_) => false,
        }
    }

    fn int_cmp(&mut self, sf: bool, cc: Cond) {
        let pb = self.pop();
        let pa = self.pop();
        let ra = self.gpr(pa);
        match pb.0.loc {
            Loc::Imm(x) if (if sf { x } else { x as u32 as u64 }) < 4096 => {
                self.a.cmp_imm(sf, ra, x as u32);
            }
            _ => {
                let rb = self.gpr(pb);
                self.a.cmp(sf, ra, rb);
                self.free_loc(Loc::Gpr(rb));
            }
        }
        self.free_loc(Loc::Gpr(ra));
        self.push(ValType::I32, Loc::Flags(cc));
    }

    fn float_cmp(&mut self, dbl: bool, cc: Cond) {
        let pb = self.pop();
        let rb = self.fpr(pb);
        let pa = self.pop();
        let ra = self.fpr(pa);
        self.a.fcmp(dbl, ra, rb);
        self.free_loc(Loc::Fpr(ra));
        self.free_loc(Loc::Fpr(rb));
        self.push(ValType::I32, Loc::Flags(cc));
    }

    fn int_binop(&mut self, sf: bool, op: IntOp) {
        let ty = if sf { ValType::I64 } else { ValType::I32 };
        let pb = self.pop();
        let pa = self.pop();
        let bits: u64 = if sf { 64 } else { 32 };
        let imm = if let Loc::Imm(x) = pb.0.loc {
            Some(if sf { x } else { x & 0xFFFF_FFFF })
        } else {
            None
        };
        // Immediate forms.
        if let Some(x) = imm {
            let done = match op {
                IntOp::Add | IntOp::Sub if Asm::addsub_imm_ok(x) => {
                    let ra = self.gpr(pa);
                    if op == IntOp::Add {
                        self.a.add_imm(sf, ra, ra, x as u32);
                    } else {
                        self.a.sub_imm(sf, ra, ra, x as u32);
                    }
                    Some(ra)
                }
                IntOp::And | IntOp::Or | IntOp::Xor if logical_imm(x, bits as u32).is_some() => {
                    let enc = logical_imm(x, bits as u32).unwrap();
                    let ra = self.gpr(pa);
                    let opc = match op {
                        IntOp::And => 0,
                        IntOp::Or => 1,
                        _ => 2,
                    };
                    self.a.logical_imm(opc, sf, ra, ra, enc);
                    Some(ra)
                }
                IntOp::Shl | IntOp::ShrS | IntOp::ShrU | IntOp::Rotr | IntOp::Rotl => {
                    let ra = self.gpr(pa);
                    let s = (x % bits) as u32;
                    match op {
                        IntOp::Shl => self.a.lsl_imm(sf, ra, ra, s),
                        IntOp::ShrS => self.a.asr_imm(sf, ra, ra, s),
                        IntOp::ShrU => self.a.lsr_imm(sf, ra, ra, s),
                        IntOp::Rotr => self.a.ror_imm(sf, ra, ra, s),
                        _ => self.a.ror_imm(sf, ra, ra, (bits as u32 - s) % bits as u32),
                    }
                    Some(ra)
                }
                _ => None,
            };
            if let Some(r) = done {
                self.push_reg(ty, r);
                return;
            }
        }
        let rb = self.gpr(pb);
        let ra = self.gpr(pa);
        match op {
            IntOp::Add => self.a.add(sf, ra, ra, rb),
            IntOp::Sub => self.a.sub(sf, ra, ra, rb),
            IntOp::Mul => self.a.mul(sf, ra, ra, rb),
            IntOp::And => self.a.and(sf, ra, ra, rb),
            IntOp::Or => self.a.orr(sf, ra, ra, rb),
            IntOp::Xor => self.a.eor(sf, ra, ra, rb),
            IntOp::Shl => self.a.lslv(sf, ra, ra, rb),
            IntOp::ShrS => self.a.asrv(sf, ra, ra, rb),
            IntOp::ShrU => self.a.lsrv(sf, ra, ra, rb),
            IntOp::Rotr => self.a.rorv(sf, ra, ra, rb),
            IntOp::Rotl => {
                self.a.neg(sf, T0, rb);
                self.a.rorv(sf, ra, ra, T0);
            }
            IntOp::DivS | IntOp::DivU | IntOp::RemS | IntOp::RemU => {
                let z = self.trap(TrapCode::IntegerDivideByZero);
                self.a.cbz(sf, rb, z);
                if op == IntOp::DivS {
                    // INT_MIN / -1 overflows.
                    let ok = self.a.new_label();
                    self.a.cmn_imm(sf, rb, 1);
                    self.a.b_cond(Cond::Ne, ok);
                    self.a.cmp_imm(sf, ra, 1);
                    let ov = self.trap(TrapCode::IntegerOverflow);
                    self.a.b_cond(Cond::Vs, ov);
                    self.a.bind(ok);
                }
                match op {
                    IntOp::DivS => self.a.sdiv(sf, ra, ra, rb),
                    IntOp::DivU => self.a.udiv(sf, ra, ra, rb),
                    IntOp::RemS => {
                        self.a.sdiv(sf, T0, ra, rb);
                        self.a.msub(sf, ra, T0, rb, ra);
                    }
                    _ => {
                        self.a.udiv(sf, T0, ra, rb);
                        self.a.msub(sf, ra, T0, rb, ra);
                    }
                }
            }
        }
        self.free_loc(Loc::Gpr(rb));
        self.push_reg(ty, ra);
    }

    fn int_unop(&mut self, sf: bool, op: NumOp) {
        let ty = if sf { ValType::I64 } else { ValType::I32 };
        let pa = self.pop();
        let ra = self.gpr(pa);
        use NumOp::*;
        match op {
            I32Clz | I64Clz => self.a.clz(sf, ra, ra),
            I32Ctz | I64Ctz => {
                self.a.rbit(sf, ra, ra);
                self.a.clz(sf, ra, ra);
            }
            I32Popcnt | I64Popcnt => {
                self.a.fmov_from_gpr(sf, F0, ra);
                self.a.cnt8b(F0, F0);
                self.a.addv8b(F0, F0);
                self.a.fmov_to_gpr(false, ra, F0);
            }
            I32Extend8S => self.a.sxt(false, ra, ra, 8),
            I32Extend16S => self.a.sxt(false, ra, ra, 16),
            I64Extend8S => self.a.sxt(true, ra, ra, 8),
            I64Extend16S => self.a.sxt(true, ra, ra, 16),
            I64Extend32S => self.a.sxt(true, ra, ra, 32),
            _ => unreachable!(),
        }
        self.push_reg(ty, ra);
    }

    fn float_binop(&mut self, dbl: bool, op: u32) {
        let ty = if dbl { ValType::F64 } else { ValType::F32 };
        let pb = self.pop();
        let rb = self.fpr(pb);
        let pa = self.pop();
        let ra = self.fpr(pa);
        self.a.fp2(dbl, op, ra, ra, rb);
        self.free_loc(Loc::Fpr(rb));
        self.push_reg(ty, ra);
    }

    fn copysign(&mut self, dbl: bool) {
        let ty = if dbl { ValType::F64 } else { ValType::F32 };
        let pb = self.pop();
        let rb = self.fpr(pb);
        let pa = self.pop();
        let ra = self.fpr(pa);
        if dbl {
            self.a.movz(true, T0, 0x8000, 48);
        } else {
            self.a.movz(false, T0, 0x8000, 16);
        }
        self.a.fmov_from_gpr(dbl, F0, T0);
        self.a.bit8b(ra, rb, F0);
        self.free_loc(Loc::Fpr(rb));
        self.push_reg(ty, ra);
    }

    fn float_unop(&mut self, dbl: bool, f: fn(&mut Asm, bool, u8, u8)) {
        let ty = if dbl { ValType::F64 } else { ValType::F32 };
        let pa = self.pop();
        let ra = self.fpr(pa);
        f(self.a, dbl, ra, ra);
        self.push_reg(ty, ra);
    }

    /// Trapping float-to-int truncation; `lo`/`hi` are exclusive bounds.
    fn trunc_checked(&mut self, dbl: bool, sf: bool, signed: bool, lo: f64, hi: f64) {
        let ty = if sf { ValType::I64 } else { ValType::I32 };
        let pa = self.pop();
        let rf = self.fpr(pa);
        self.a.fcmp(dbl, rf, rf);
        let inv = self.trap(TrapCode::InvalidConversionToInteger);
        self.a.b_cond(Cond::Vs, inv);
        let ov = self.trap(TrapCode::IntegerOverflow);
        let bits = |x: f64| {
            if dbl {
                x.to_bits()
            } else {
                (x as f32).to_bits() as u64
            }
        };
        self.a.mov_imm(dbl, T0, bits(hi));
        self.a.fmov_from_gpr(dbl, F0, T0);
        self.a.fcmp(dbl, rf, F0);
        self.a.b_cond(Cond::Ge, ov);
        self.a.mov_imm(dbl, T0, bits(lo));
        self.a.fmov_from_gpr(dbl, F0, T0);
        self.a.fcmp(dbl, rf, F0);
        self.a.b_cond(Cond::Ls, ov);
        let rd = self.alloc(false);
        if signed {
            self.a.fcvtzs(sf, dbl, rd, rf);
        } else {
            self.a.fcvtzu(sf, dbl, rd, rf);
        }
        self.free_loc(Loc::Fpr(rf));
        self.push_reg(ty, rd);
    }

    fn convert(&mut self, op: NumOp) {
        use NumOp::*;
        let (params, rty) = op.signature();
        let src = params[0];
        let pa = self.pop();
        // Pure re-typing.
        match op {
            I64ExtendI32U => {
                // i32 values are kept zero-extended.
                let r = self.gpr(pa);
                self.push_reg(rty, r);
                return;
            }
            I32WrapI64 => {
                let r = self.gpr(pa);
                self.a.mov(false, r, r);
                self.push_reg(rty, r);
                return;
            }
            I64ExtendI32S => {
                let r = self.gpr(pa);
                self.a.sxt(true, r, r, 32);
                self.push_reg(rty, r);
                return;
            }
            _ => {}
        }
        if let Loc::Imm(x) = pa.0.loc
            && matches!(
                op,
                I32ReinterpretF32 | I64ReinterpretF64 | F32ReinterpretI32 | F64ReinterpretI64
            )
        {
            self.push(rty, Loc::Imm(x));
            return;
        }
        let sdbl = src == ValType::F64 || src == ValType::I64;
        let rdbl = rty == ValType::F64 || rty == ValType::I64;
        if is_float(src) {
            let rf = self.fpr(pa);
            if is_float(rty) {
                // promote / demote
                self.a.fcvt(rty == ValType::F64, rf, rf);
                self.push_reg(rty, rf);
                return;
            }
            let rd = self.alloc(false);
            match op {
                I32ReinterpretF32 | I64ReinterpretF64 => self.a.fmov_to_gpr(sdbl, rd, rf),
                I32TruncSatF32S | I32TruncSatF64S | I64TruncSatF32S | I64TruncSatF64S => {
                    self.a.fcvtzs(rdbl, sdbl, rd, rf)
                }
                _ => self.a.fcvtzu(rdbl, sdbl, rd, rf),
            }
            self.free_loc(Loc::Fpr(rf));
            self.push_reg(rty, rd);
        } else {
            let ri = self.gpr(pa);
            let rd = self.alloc(true);
            match op {
                F32ReinterpretI32 | F64ReinterpretI64 => self.a.fmov_from_gpr(rdbl, rd, ri),
                F32ConvertI32S | F32ConvertI64S | F64ConvertI32S | F64ConvertI64S => {
                    self.a.scvtf(sdbl, rdbl, rd, ri)
                }
                _ => self.a.ucvtf(sdbl, rdbl, rd, ri),
            }
            self.free_loc(Loc::Gpr(ri));
            self.push_reg(rty, rd);
        }
    }

    fn numeric(&mut self, op: NumOp) {
        use NumOp::*;
        if self.const_fold(op) {
            return;
        }
        match op {
            I32Eqz | I64Eqz => {
                let pa = self.pop();
                if let Loc::Flags(c) = pa.0.loc {
                    self.push(ValType::I32, Loc::Flags(c.invert()));
                    return;
                }
                let r = self.gpr(pa);
                self.a.cmp_imm(op == I64Eqz, r, 0);
                self.free_loc(Loc::Gpr(r));
                self.push(ValType::I32, Loc::Flags(Cond::Eq));
            }
            I32Eq => self.int_cmp(false, Cond::Eq),
            I32Ne => self.int_cmp(false, Cond::Ne),
            I32LtS => self.int_cmp(false, Cond::Lt),
            I32LtU => self.int_cmp(false, Cond::Lo),
            I32GtS => self.int_cmp(false, Cond::Gt),
            I32GtU => self.int_cmp(false, Cond::Hi),
            I32LeS => self.int_cmp(false, Cond::Le),
            I32LeU => self.int_cmp(false, Cond::Ls),
            I32GeS => self.int_cmp(false, Cond::Ge),
            I32GeU => self.int_cmp(false, Cond::Hs),
            I64Eq => self.int_cmp(true, Cond::Eq),
            I64Ne => self.int_cmp(true, Cond::Ne),
            I64LtS => self.int_cmp(true, Cond::Lt),
            I64LtU => self.int_cmp(true, Cond::Lo),
            I64GtS => self.int_cmp(true, Cond::Gt),
            I64GtU => self.int_cmp(true, Cond::Hi),
            I64LeS => self.int_cmp(true, Cond::Le),
            I64LeU => self.int_cmp(true, Cond::Ls),
            I64GeS => self.int_cmp(true, Cond::Ge),
            I64GeU => self.int_cmp(true, Cond::Hs),
            // Unordered (NaN) makes every one of these false except `ne`.
            F32Eq => self.float_cmp(false, Cond::Eq),
            F32Ne => self.float_cmp(false, Cond::Ne),
            F32Lt => self.float_cmp(false, Cond::Mi),
            F32Gt => self.float_cmp(false, Cond::Gt),
            F32Le => self.float_cmp(false, Cond::Ls),
            F32Ge => self.float_cmp(false, Cond::Ge),
            F64Eq => self.float_cmp(true, Cond::Eq),
            F64Ne => self.float_cmp(true, Cond::Ne),
            F64Lt => self.float_cmp(true, Cond::Mi),
            F64Gt => self.float_cmp(true, Cond::Gt),
            F64Le => self.float_cmp(true, Cond::Ls),
            F64Ge => self.float_cmp(true, Cond::Ge),
            I32Clz | I32Ctz | I32Popcnt | I32Extend8S | I32Extend16S => self.int_unop(false, op),
            I64Clz | I64Ctz | I64Popcnt | I64Extend8S | I64Extend16S | I64Extend32S => {
                self.int_unop(true, op)
            }
            I32Add => self.int_binop(false, IntOp::Add),
            I32Sub => self.int_binop(false, IntOp::Sub),
            I32Mul => self.int_binop(false, IntOp::Mul),
            I32DivS => self.int_binop(false, IntOp::DivS),
            I32DivU => self.int_binop(false, IntOp::DivU),
            I32RemS => self.int_binop(false, IntOp::RemS),
            I32RemU => self.int_binop(false, IntOp::RemU),
            I32And => self.int_binop(false, IntOp::And),
            I32Or => self.int_binop(false, IntOp::Or),
            I32Xor => self.int_binop(false, IntOp::Xor),
            I32Shl => self.int_binop(false, IntOp::Shl),
            I32ShrS => self.int_binop(false, IntOp::ShrS),
            I32ShrU => self.int_binop(false, IntOp::ShrU),
            I32Rotl => self.int_binop(false, IntOp::Rotl),
            I32Rotr => self.int_binop(false, IntOp::Rotr),
            I64Add => self.int_binop(true, IntOp::Add),
            I64Sub => self.int_binop(true, IntOp::Sub),
            I64Mul => self.int_binop(true, IntOp::Mul),
            I64DivS => self.int_binop(true, IntOp::DivS),
            I64DivU => self.int_binop(true, IntOp::DivU),
            I64RemS => self.int_binop(true, IntOp::RemS),
            I64RemU => self.int_binop(true, IntOp::RemU),
            I64And => self.int_binop(true, IntOp::And),
            I64Or => self.int_binop(true, IntOp::Or),
            I64Xor => self.int_binop(true, IntOp::Xor),
            I64Shl => self.int_binop(true, IntOp::Shl),
            I64ShrS => self.int_binop(true, IntOp::ShrS),
            I64ShrU => self.int_binop(true, IntOp::ShrU),
            I64Rotl => self.int_binop(true, IntOp::Rotl),
            I64Rotr => self.int_binop(true, IntOp::Rotr),
            F32Abs => self.float_unop(false, Asm::fabs),
            F32Neg => self.float_unop(false, Asm::fneg),
            F32Ceil => self.float_unop(false, Asm::frintp),
            F32Floor => self.float_unop(false, Asm::frintm),
            F32Trunc => self.float_unop(false, Asm::frintz),
            F32Nearest => self.float_unop(false, Asm::frintn),
            F32Sqrt => self.float_unop(false, Asm::fsqrt),
            F64Abs => self.float_unop(true, Asm::fabs),
            F64Neg => self.float_unop(true, Asm::fneg),
            F64Ceil => self.float_unop(true, Asm::frintp),
            F64Floor => self.float_unop(true, Asm::frintm),
            F64Trunc => self.float_unop(true, Asm::frintz),
            F64Nearest => self.float_unop(true, Asm::frintn),
            F64Sqrt => self.float_unop(true, Asm::fsqrt),
            F32Add => self.float_binop(false, 2),
            F32Sub => self.float_binop(false, 3),
            F32Mul => self.float_binop(false, 0),
            F32Div => self.float_binop(false, 1),
            F32Max => self.float_binop(false, 4),
            F32Min => self.float_binop(false, 5),
            F32Copysign => self.copysign(false),
            F64Add => self.float_binop(true, 2),
            F64Sub => self.float_binop(true, 3),
            F64Mul => self.float_binop(true, 0),
            F64Div => self.float_binop(true, 1),
            F64Max => self.float_binop(true, 4),
            F64Min => self.float_binop(true, 5),
            F64Copysign => self.copysign(true),
            I32TruncF32S => self.trunc_checked(false, false, true, -2147483904.0, 2147483648.0),
            I32TruncF32U => self.trunc_checked(false, false, false, -1.0, 4294967296.0),
            I32TruncF64S => self.trunc_checked(true, false, true, -2147483649.0, 2147483648.0),
            I32TruncF64U => self.trunc_checked(true, false, false, -1.0, 4294967296.0),
            I64TruncF32S => self.trunc_checked(
                false,
                true,
                true,
                -9223373136366403584.0,
                9223372036854775808.0,
            ),
            I64TruncF32U => self.trunc_checked(false, true, false, -1.0, 18446744073709551616.0),
            I64TruncF64S => self.trunc_checked(
                true,
                true,
                true,
                -9223372036854777856.0,
                9223372036854775808.0,
            ),
            I64TruncF64U => self.trunc_checked(true, true, false, -1.0, 18446744073709551616.0),
            _ => self.convert(op),
        }
    }

    // ---- the main dispatch ----

    fn op(&mut self, op: Op) -> Result<(), String> {
        if self.dead {
            match op {
                Op::Block(_) | Op::Loop(_) | Op::If(_) => {
                    self.dead_depth += 1;
                    return Ok(());
                }
                Op::End if self.dead_depth > 0 => {
                    self.dead_depth -= 1;
                    return Ok(());
                }
                Op::Else | Op::End if self.dead_depth == 0 => {}
                _ => return Ok(()),
            }
        }
        let consumes_flags = matches!(
            op,
            Op::BrIf(_)
                | Op::If(_)
                | Op::Select
                | Op::SelectT(_)
                | Op::Num(NumOp::I32Eqz)
                | Op::Drop
        );
        if !consumes_flags {
            self.flush_flags();
        }
        if !matches!(op, Op::End | Op::Else) {
            self.charge();
        }
        match op {
            Op::Unreachable => {
                let l = self.trap(TrapCode::Unreachable);
                self.a.b(l);
                self.set_dead();
            }
            Op::Nop => {}
            Op::Block(bt) | Op::Loop(bt) => {
                let (p, r) = self.block_sig(bt);
                self.spill_all();
                let n = self.stack.len();
                for i in n - p.len()..n {
                    self.to_slot(i);
                }
                let label = self.a.new_label();
                let kind = if matches!(op, Op::Loop(_)) {
                    Kind::Loop
                } else {
                    Kind::Block
                };
                if kind == Kind::Loop {
                    self.label_here(label);
                }
                self.ctrls.push(Ctrl {
                    kind,
                    height: self.stack.len() - p.len(),
                    params: p,
                    results: r,
                    label,
                    else_label: None,
                    branched: false,
                });
            }
            Op::If(bt) => {
                let (p, r) = self.block_sig(bt);
                let c = self.pop_cond();
                self.spill_all();
                let n = self.stack.len();
                for i in n - p.len()..n {
                    self.to_slot(i);
                }
                let else_l = self.a.new_label();
                self.branch_on(&c, false, else_l);
                self.free_cond(c);
                let label = self.a.new_label();
                self.ctrls.push(Ctrl {
                    kind: Kind::If,
                    height: self.stack.len() - p.len(),
                    params: p,
                    results: r,
                    label,
                    else_label: Some(else_l),
                    branched: false,
                });
            }
            Op::Else => {
                let fi = self.ctrls.len() - 1;
                if !self.dead {
                    let n = self.ctrls[fi].results.len();
                    let len = self.stack.len();
                    for i in len - n..len {
                        self.to_slot(i);
                    }
                    self.jump(fi);
                }
                let h = self.ctrls[fi].height;
                while self.stack.len() > h {
                    let (v, _) = self.pop();
                    self.free_loc(v.loc);
                }
                let el = self.ctrls[fi].else_label.take().unwrap();
                self.label_here(el);
                self.ctrls[fi].kind = Kind::Else;
                for t in self.ctrls[fi].params.clone() {
                    self.push(t, Loc::Slot);
                }
                self.dead = false;
            }
            Op::End => {
                let fi = self.ctrls.len() - 1;
                let fallthrough = !self.dead;
                if fallthrough {
                    let n = self.ctrls[fi].results.len();
                    let len = self.stack.len();
                    for i in len - n..len {
                        self.to_slot(i);
                    }
                }
                if fi == 0 {
                    // End of the function.
                    if fallthrough {
                        self.branch_moves(0);
                        let n = self.results.len();
                        for _ in 0..n {
                            let (v, _) = self.pop();
                            self.free_loc(v.loc);
                        }
                        self.a.b(self.epilogue);
                    }
                    self.ctrls.pop();
                    return Ok(());
                }
                let c = self.ctrls.pop().unwrap();
                let mut live = fallthrough || c.branched;
                if let Some(el) = c.else_label {
                    // `if` without `else`: params flow through unchanged.
                    self.label_here(el);
                    live = true;
                }
                if c.kind != Kind::Loop {
                    self.label_here(c.label);
                }
                while self.stack.len() > c.height {
                    let (v, _) = self.pop();
                    self.free_loc(v.loc);
                }
                for &t in &c.results {
                    self.push(t, Loc::Slot);
                }
                self.dead = !live;
            }
            Op::Br(l) => {
                let fi = self.frame_index(l);
                self.branch_moves(fi);
                self.jump(fi);
                self.set_dead();
            }
            Op::BrIf(l) => {
                let fi = self.frame_index(l);
                let c = self.pop_cond();
                if self.needs_moves(fi) {
                    let skip = self.a.new_label();
                    self.branch_on(&c, false, skip);
                    self.branch_moves(fi);
                    self.jump(fi);
                    self.a.bind(skip);
                } else if fi == 0 {
                    self.branch_on(&c, true, self.epilogue);
                } else {
                    let l = self.ctrls[fi].label;
                    if self.ctrls[fi].kind != Kind::Loop {
                        self.ctrls[fi].branched = true;
                    }
                    self.branch_on(&c, true, l);
                }
                self.free_cond(c);
            }
            Op::BrTable { targets, default } => {
                let pv = self.pop();
                let ri = self.gpr(pv);
                let n = targets.len() as u64;
                let def_l = self.a.new_label();
                if n < 4096 {
                    self.a.cmp_imm(false, ri, n as u32);
                } else {
                    self.a.mov_imm(false, T0, n);
                    self.a.cmp(false, ri, T0);
                }
                self.a.b_cond(Cond::Hs, def_l);
                self.a.adr(T0, 12);
                self.a.add_lsl(true, T0, T0, ri, 2);
                self.a.br(T0);
                let mut stubs: Vec<(Label, usize)> = Vec::new();
                for &t in targets.iter() {
                    let fi = self.frame_index(t);
                    if self.needs_moves(fi) {
                        let s = self.a.new_label();
                        stubs.push((s, fi));
                        self.a.b(s);
                    } else {
                        self.jump(fi);
                    }
                }
                self.a.bind(def_l);
                let dfi = self.frame_index(default);
                self.branch_moves(dfi);
                self.jump(dfi);
                for (s, fi) in stubs {
                    self.a.bind(s);
                    self.branch_moves(fi);
                    self.jump(fi);
                }
                self.free_loc(Loc::Gpr(ri));
                self.set_dead();
            }
            Op::Return => {
                self.branch_moves(0);
                self.a.b(self.epilogue);
                self.set_dead();
            }
            Op::Call(f) => {
                let m = self.cx.m;
                let ft = m.func_type(f).clone();
                self.call_args(ft.params.len());
                if f >= m.num_imported_funcs {
                    let l = self.cx.entries[(f - m.num_imported_funcs) as usize];
                    self.a.bl(l);
                } else {
                    self.a.ldst_off(Mem::LdrX, T0, VMCTX, CTX_FUNCS as i64, T1);
                    self.a.ldst_off(Mem::LdrX, XREF, T0, 8 * f as i64, T1);
                    self.call_funcref();
                }
                self.reload_memsize();
                self.call_results(&ft.results);
            }
            Op::CallIndirect { ty, table } => {
                let ft = self.cx.m.types[ty as usize].clone();
                let pi = self.pop();
                // Keep the index in a register the argument moves cannot touch.
                let ri = self.gpr(pi);
                self.a.mov(true, XCALLER, ri);
                self.free_loc(Loc::Gpr(ri));
                self.call_args(ft.params.len());
                let ri = XCALLER;
                self.a.ldst_off(Mem::LdrX, T0, VMCTX, CTX_TABLES as i64, T1);
                self.a.ldst_off(Mem::LdrX, T0, T0, 8 * table as i64, T1);
                self.a.ldst_off(Mem::LdrX, T1, T0, TABLE_LEN as i64, T1);
                self.a.cmp(true, ri, T1);
                let undef = self.trap(TrapCode::UndefinedElement);
                self.a.b_cond(Cond::Hs, undef);
                self.a.ldst_off(Mem::LdrX, T0, T0, TABLE_ELEMS as i64, T1);
                self.a.ldst_regoff(Mem::LdrX, XREF, T0, ri, true);
                let ok = self.a.new_label();
                self.a.cbnz(true, XREF, ok);
                self.a.ldst_off(Mem::StrX, ri, RT, RT_TRAP_ARG as i64, T1);
                self.a
                    .movz(false, 0, TrapCode::UninitializedElement.to_raw() as u16, 0);
                self.a.b(self.cx.trap_exit);
                self.a.bind(ok);
                self.a
                    .ldst_off(Mem::LdrW, T0, XREF, FUNCREF_TYPE_ID as i64, T1);
                self.a
                    .ldst_off(Mem::LdrX, T1, VMCTX, CTX_TYPE_IDS as i64, T1);
                self.a.ldst_off(Mem::LdrW, T1, T1, 4 * ty as i64, T1);
                self.a.cmp(false, T0, T1);
                let mism = self.trap(TrapCode::IndirectCallTypeMismatch);
                self.a.b_cond(Cond::Ne, mism);
                self.call_funcref();
                self.reload_memsize();
                self.call_results(&ft.results);
            }
            Op::Drop => {
                let (v, _) = self.pop();
                self.free_loc(v.loc);
            }
            Op::Select | Op::SelectT(_) => {
                let c = self.pop_cond();
                let cc = match c {
                    Ok(cc) => cc,
                    Err(r) => {
                        self.a.cmp_imm(false, r, 0);
                        self.free_loc(Loc::Gpr(r));
                        Cond::Ne
                    }
                };
                let pb = self.pop();
                let pa = self.pop();
                let ty = pa.0.ty;
                let rb = self.reg(pb);
                let ra = self.reg(pa);
                if is_float(ty) {
                    self.a.fcsel(ty == ValType::F64, ra, ra, rb, cc);
                    self.free_loc(Loc::Fpr(rb));
                } else {
                    self.a.csel(is64(ty), ra, ra, rb, cc);
                    self.free_loc(Loc::Gpr(rb));
                }
                self.push_reg(ty, ra);
            }
            Op::LocalGet(i) => {
                let ty = self.locals[i as usize];
                let (base, off) = self.local_addr(i);
                let r = self.alloc_for(ty);
                self.a.ldst_off(load_kind(ty), r, base, off, T1);
                self.push_reg(ty, r);
            }
            Op::LocalSet(i) | Op::LocalTee(i) => {
                let (base, off) = self.local_addr(i);
                let pv = self.pop();
                let ty = pv.0.ty;
                if let (Loc::Imm(x), Op::LocalSet(_)) = (pv.0.loc, &op) {
                    self.store_imm(x, base, off);
                } else {
                    let r = self.reg(pv);
                    let k = if is_float(ty) {
                        store_kind(ty)
                    } else {
                        Mem::StrX
                    };
                    self.a.ldst_off(k, r, base, off, T1);
                    if matches!(op, Op::LocalTee(_)) {
                        self.push_reg(ty, r);
                    } else {
                        self.free_loc(if is_float(ty) {
                            Loc::Fpr(r)
                        } else {
                            Loc::Gpr(r)
                        });
                    }
                }
            }
            Op::GlobalGet(g) => {
                let ty = self.cx.m.globals[g as usize].ty;
                self.a
                    .ldst_off(Mem::LdrX, T0, VMCTX, CTX_GLOBALS as i64, T1);
                self.a.ldst_off(Mem::LdrX, T0, T0, 8 * g as i64, T1);
                let r = self.alloc_for(ty);
                self.a.ldst_off(load_kind(ty), r, T0, 0, T1);
                self.push_reg(ty, r);
            }
            Op::GlobalSet(g) => {
                let pv = self.pop();
                let ty = pv.0.ty;
                let r = self.reg(pv);
                self.a
                    .ldst_off(Mem::LdrX, T0, VMCTX, CTX_GLOBALS as i64, T1);
                self.a.ldst_off(Mem::LdrX, T0, T0, 8 * g as i64, T1);
                let k = if is_float(ty) {
                    store_kind(ty)
                } else {
                    Mem::StrX
                };
                self.a.ldst_off(k, r, T0, 0, T1);
                self.free_loc(if is_float(ty) {
                    Loc::Fpr(r)
                } else {
                    Loc::Gpr(r)
                });
            }
            Op::TableGet(t) => {
                let ty = self.cx.m.tables[t as usize].elem;
                let pi = self.pop();
                let ri = self.gpr(pi);
                self.table_ptr(t);
                self.a.ldst_off(Mem::LdrX, T1, T0, TABLE_LEN as i64, T1);
                self.a.cmp(true, ri, T1);
                let oob = self.trap(TrapCode::TableOutOfBounds);
                self.a.b_cond(Cond::Hs, oob);
                self.a.ldst_off(Mem::LdrX, T0, T0, TABLE_ELEMS as i64, T1);
                self.a.ldst_regoff(Mem::LdrX, ri, T0, ri, true);
                self.push_reg(ty, ri);
            }
            Op::TableSet(t) => {
                let pv = self.pop();
                let rv = self.gpr(pv);
                let pi = self.pop();
                let ri = self.gpr(pi);
                self.table_ptr(t);
                self.a.ldst_off(Mem::LdrX, T1, T0, TABLE_LEN as i64, T1);
                self.a.cmp(true, ri, T1);
                let oob = self.trap(TrapCode::TableOutOfBounds);
                self.a.b_cond(Cond::Hs, oob);
                self.a.ldst_off(Mem::LdrX, T0, T0, TABLE_ELEMS as i64, T1);
                self.a.ldst_regoff(Mem::StrX, rv, T0, ri, true);
                self.free_loc(Loc::Gpr(rv));
                self.free_loc(Loc::Gpr(ri));
            }
            Op::TableSize(t) => {
                self.table_ptr(t);
                let r = self.alloc(false);
                self.a.ldst_off(Mem::LdrW, r, T0, TABLE_LEN as i64, T1);
                self.push_reg(ValType::I32, r);
            }
            Op::Load(lo, ma) => self.load(lo, ma),
            Op::Store(so, ma) => self.store(so, ma),
            Op::MemorySize => {
                let r = self.alloc(false);
                self.a.lsr_imm(true, r, MEMSIZE, 16);
                self.push_reg(ValType::I32, r);
            }
            Op::MemoryGrow => {
                self.spill_all();
                let d = self.stack.len() - 1;
                self.call_shim(
                    self.cx.shims.memory_grow,
                    &[ShimArg::Vmctx, ShimArg::Slot(d, ValType::I32)],
                );
                self.stack.pop();
                // The helper returns a u32: the upper half of x0 is unspecified.
                self.a.mov(false, 0, 0);
                self.reload_memsize();
                self.take_reg(false, 0);
                self.push_reg(ValType::I32, 0);
            }
            Op::I32Const(v) => self.push(ValType::I32, Loc::Imm(v as u32 as u64)),
            Op::I64Const(v) => self.push(ValType::I64, Loc::Imm(v as u64)),
            Op::F32Const(v) => self.push(ValType::F32, Loc::Imm(v as u64)),
            Op::F64Const(v) => self.push(ValType::F64, Loc::Imm(v)),
            Op::Num(n) => self.numeric(n),
            Op::RefNull(t) => self.push(t, Loc::Imm(0)),
            Op::RefIsNull => {
                let pv = self.pop();
                if let Loc::Imm(x) = pv.0.loc {
                    self.push(ValType::I32, Loc::Imm((x == 0) as u64));
                } else {
                    let r = self.gpr(pv);
                    self.a.cmp_imm(true, r, 0);
                    self.free_loc(Loc::Gpr(r));
                    self.push(ValType::I32, Loc::Flags(Cond::Eq));
                }
            }
            Op::RefFunc(f) => {
                let r = self.alloc(false);
                self.a.ldst_off(Mem::LdrX, T0, VMCTX, CTX_FUNCS as i64, T1);
                self.a.ldst_off(Mem::LdrX, r, T0, 8 * f as i64, T1);
                self.push_reg(ValType::FuncRef, r);
            }
            Op::MemoryInit(seg) => self.bulk(self.cx.shims.memory_init, Some(seg as u64), None),
            Op::DataDrop(seg) => {
                self.spill_all();
                self.call_shim(
                    self.cx.shims.data_drop,
                    &[ShimArg::Vmctx, ShimArg::Imm(seg as u64)],
                );
            }
            Op::MemoryCopy => self.bulk(self.cx.shims.memory_copy, None, None),
            Op::MemoryFill => self.bulk(self.cx.shims.memory_fill, None, None),
            Op::TableInit { elem, table } => self.bulk(
                self.cx.shims.table_init,
                Some(table as u64),
                Some(elem as u64),
            ),
            Op::ElemDrop(seg) => {
                self.spill_all();
                self.call_shim(
                    self.cx.shims.elem_drop,
                    &[ShimArg::Vmctx, ShimArg::Imm(seg as u64)],
                );
            }
            Op::TableCopy { dst, src } => {
                self.bulk(self.cx.shims.table_copy, Some(dst as u64), Some(src as u64))
            }
            Op::TableFill(t) => {
                // (i, val, n): val is a 64-bit reference.
                self.spill_all();
                let d = self.stack.len() - 3;
                let et = self.cx.m.tables[t as usize].elem;
                self.call_shim(
                    self.cx.shims.table_fill,
                    &[
                        ShimArg::Vmctx,
                        ShimArg::Imm(t as u64),
                        ShimArg::Slot(d, ValType::I32),
                        ShimArg::Slot(d + 1, et),
                        ShimArg::Slot(d + 2, ValType::I32),
                    ],
                );
                self.stack.truncate(d);
                self.check_w0();
            }
            Op::TableGrow(t) => {
                // (init, delta) -> old size or -1
                self.spill_all();
                let d = self.stack.len() - 2;
                let et = self.cx.m.tables[t as usize].elem;
                self.call_shim(
                    self.cx.shims.table_grow,
                    &[
                        ShimArg::Vmctx,
                        ShimArg::Imm(t as u64),
                        ShimArg::Slot(d, et),
                        ShimArg::Slot(d + 1, ValType::I32),
                    ],
                );
                self.stack.truncate(d);
                self.a.mov(false, 0, 0);
                self.take_reg(false, 0);
                self.push_reg(ValType::I32, 0);
            }
        }
        Ok(())
    }

    fn table_ptr(&mut self, t: u32) {
        self.a.ldst_off(Mem::LdrX, T0, VMCTX, CTX_TABLES as i64, T1);
        self.a.ldst_off(Mem::LdrX, T0, T0, 8 * t as i64, T1);
    }

    /// A bulk operation taking three i32 operands, plus up to two immediates first.
    fn bulk(&mut self, addr: u64, i1: Option<u64>, i2: Option<u64>) {
        self.spill_all();
        let d = self.stack.len() - 3;
        let mut args = vec![ShimArg::Vmctx];
        if let Some(x) = i1 {
            args.push(ShimArg::Imm(x));
        }
        if let Some(x) = i2 {
            args.push(ShimArg::Imm(x));
        }
        for k in 0..3 {
            args.push(ShimArg::Slot(d + k, ValType::I32));
        }
        self.call_shim(addr, &args);
        self.stack.truncate(d);
        self.check_w0();
    }
}

#[derive(Copy, Clone)]
enum ShimArg {
    Vmctx,
    Slot(usize, ValType),
    Imm(u64),
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum IntOp {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    ShrS,
    ShrU,
    Rotl,
    Rotr,
    DivS,
    DivU,
    RemS,
    RemU,
}

impl Val {
    /// The location of this value's register after it was materialised into `r`.
    fn loc_after(&self, r: u8) -> Loc {
        if is_float(self.ty) {
            Loc::Fpr(r)
        } else {
            Loc::Gpr(r)
        }
    }
}
