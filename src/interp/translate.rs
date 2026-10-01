//! Single-pass translation of validated function bodies into interpreter bytecode.
//!
//! The translator mirrors the operand stack at compile time. Each entry records where its
//! value currently is: in its own canonical slot (`nlocals + depth`), still in a local
//! (`local.get` is free), or a constant not yet written anywhere. Values are copied into
//! canonical slots only when something needs them there: a call argument, a branch value,
//! a control-flow boundary, or a `local.set` to a local that a pending entry still refers to.
//!
//! Lazy local references never cross a block, loop or if boundary (they are written to their
//! canonical slots on entry), so every pending reference is used within straight-line code
//! where its local cannot change behind its back.

use super::bytecode::*;
use crate::binary::module::ModuleData;
use crate::binary::ops::{BlockType, LoadOp, NumOp, Op, OpReader, StoreOp};
use crate::binary::reader::Reader;
use crate::error::{Result, TrapCode};
use crate::num;
use crate::types::ValType;
use crate::validate::FuncInfo;

#[derive(Copy, Clone, Debug, PartialEq)]
enum Src {
    Slot(Slot),
    Const(u64),
}

#[derive(Copy, Clone, Debug)]
struct Entry {
    src: Src,
    ty: ValType,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Kind {
    Block,
    Loop,
    If,
    Else,
    Func,
}

#[derive(Debug)]
struct Frame {
    kind: Kind,
    height: usize,
    params: Vec<ValType>,
    results: Vec<ValType>,
    /// Loop header.
    start: u32,
    /// Forward branches to patch with the end of this frame.
    patches: Vec<usize>,
    /// For `if`: the branch to the `else` arm (None if never taken).
    else_patch: Option<usize>,
    /// For `if`: whether there was an `else`.
    had_else: bool,
}

/// How a branch condition can be tested.
enum Cond {
    Cmp(Cmp, Slot, Slot),
    CmpImm(Cmp, Slot, u32),
    Nez(Slot),
    Eqz(Slot),
    Const(bool),
}

impl Cond {
    fn negate(self) -> Cond {
        match self {
            Cond::Cmp(c, a, b) => Cond::Cmp(c.negate(), a, b),
            Cond::CmpImm(c, a, i) => Cond::CmpImm(c.negate(), a, i),
            Cond::Nez(s) => Cond::Eqz(s),
            Cond::Eqz(s) => Cond::Nez(s),
            Cond::Const(b) => Cond::Const(!b),
        }
    }
}

pub struct Translator<'a> {
    m: &'a ModuleData,
    code: Vec<Instr>,
    locals: Vec<ValType>,
    nlocals: u32,
    nresults: usize,
    stack: Vec<Entry>,
    frames: Vec<Frame>,
    dead: bool,
    dead_depth: u32,
    last_def: Option<(usize, Slot)>,
    fuel: bool,
    fuel_at: Option<usize>,
}

fn is_commutative(op: NumOp) -> bool {
    use NumOp::*;
    matches!(
        op,
        I32Add
            | I32Mul
            | I32And
            | I32Or
            | I32Xor
            | I32Eq
            | I32Ne
            | I64Add
            | I64Mul
            | I64And
            | I64Or
            | I64Xor
            | I64Eq
            | I64Ne
    )
}

/// `a op b == b mirror(op) a` for comparisons.
fn mirror(op: NumOp) -> Option<NumOp> {
    use NumOp::*;
    Some(match op {
        I32LtS => I32GtS,
        I32GtS => I32LtS,
        I32LtU => I32GtU,
        I32GtU => I32LtU,
        I32LeS => I32GeS,
        I32GeS => I32LeS,
        I32LeU => I32GeU,
        I32GeU => I32LeU,
        I64LtS => I64GtS,
        I64GtS => I64LtS,
        I64LtU => I64GtU,
        I64GtU => I64LtU,
        I64LeS => I64GeS,
        I64GeS => I64LeS,
        I64LeU => I64GeU,
        I64GeU => I64LeU,
        _ if is_commutative(op) => op,
        _ => return None,
    })
}

/// Decompose a compare instruction into (cmp, lhs, rhs).
fn as_cmp(i: &Instr) -> Option<Cond> {
    use Instr::*;
    Some(match *i {
        I32Eq { a, b, .. } => Cond::Cmp(Cmp::Eq, a, b),
        I32Ne { a, b, .. } => Cond::Cmp(Cmp::Ne, a, b),
        I32LtS { a, b, .. } => Cond::Cmp(Cmp::LtS, a, b),
        I32LtU { a, b, .. } => Cond::Cmp(Cmp::LtU, a, b),
        I32GtS { a, b, .. } => Cond::Cmp(Cmp::GtS, a, b),
        I32GtU { a, b, .. } => Cond::Cmp(Cmp::GtU, a, b),
        I32LeS { a, b, .. } => Cond::Cmp(Cmp::LeS, a, b),
        I32LeU { a, b, .. } => Cond::Cmp(Cmp::LeU, a, b),
        I32GeS { a, b, .. } => Cond::Cmp(Cmp::GeS, a, b),
        I32GeU { a, b, .. } => Cond::Cmp(Cmp::GeU, a, b),
        I32EqImm { a, imm, .. } => Cond::CmpImm(Cmp::Eq, a, imm),
        I32NeImm { a, imm, .. } => Cond::CmpImm(Cmp::Ne, a, imm),
        I32LtSImm { a, imm, .. } => Cond::CmpImm(Cmp::LtS, a, imm),
        I32LtUImm { a, imm, .. } => Cond::CmpImm(Cmp::LtU, a, imm),
        I32GtSImm { a, imm, .. } => Cond::CmpImm(Cmp::GtS, a, imm),
        I32GtUImm { a, imm, .. } => Cond::CmpImm(Cmp::GtU, a, imm),
        I32LeSImm { a, imm, .. } => Cond::CmpImm(Cmp::LeS, a, imm),
        I32LeUImm { a, imm, .. } => Cond::CmpImm(Cmp::LeU, a, imm),
        I32GeSImm { a, imm, .. } => Cond::CmpImm(Cmp::GeS, a, imm),
        I32GeUImm { a, imm, .. } => Cond::CmpImm(Cmp::GeU, a, imm),
        I32Eqz { a, .. } => Cond::Eqz(a),
        _ => return None,
    })
}

fn load_instr(op: LoadOp, d: Slot, a: Slot, off: u32) -> Instr {
    use LoadOp::*;
    match op {
        I32Load | F32Load => Instr::LoadI32 { d, a, off },
        I64Load | F64Load => Instr::LoadI64 { d, a, off },
        I32Load8S => Instr::LoadI32S8 { d, a, off },
        I32Load8U => Instr::LoadI32U8 { d, a, off },
        I32Load16S => Instr::LoadI32S16 { d, a, off },
        I32Load16U => Instr::LoadI32U16 { d, a, off },
        I64Load8S => Instr::LoadI64S8 { d, a, off },
        I64Load8U => Instr::LoadI64U8 { d, a, off },
        I64Load16S => Instr::LoadI64S16 { d, a, off },
        I64Load16U => Instr::LoadI64U16 { d, a, off },
        I64Load32S => Instr::LoadI64S32 { d, a, off },
        I64Load32U => Instr::LoadI64U32 { d, a, off },
    }
}

fn store_instr(op: StoreOp, a: Slot, v: Slot, off: u32) -> Instr {
    match op.width_log2() {
        0 => Instr::Store8 { a, v, off },
        1 => Instr::Store16 { a, v, off },
        2 => Instr::Store32 { a, v, off },
        _ => Instr::Store64 { a, v, off },
    }
}

impl<'a> Translator<'a> {
    pub fn translate(
        m: &'a ModuleData,
        def_index: u32,
        info: &FuncInfo,
        fuel: bool,
    ) -> Result<InterpFunc> {
        let func = m.num_imported_funcs + def_index;
        let ft = m.func_type(func);
        let body = &m.bodies[def_index as usize];
        let nparams = ft.params.len() as u32;
        let nlocals = nparams + body.locals.len() as u32;
        let mut locals = ft.params.to_vec();
        locals.extend_from_slice(&body.locals);
        let mut t = Translator {
            m,
            code: Vec::new(),
            locals,
            nlocals,
            nresults: ft.results.len(),
            stack: Vec::new(),
            frames: Vec::new(),
            dead: false,
            dead_depth: 0,
            last_def: None,
            fuel,
            fuel_at: None,
        };
        t.frames.push(Frame {
            kind: Kind::Func,
            height: 0,
            params: vec![],
            results: ft.results.to_vec(),
            start: 0,
            patches: vec![],
            else_patch: None,
            had_else: false,
        });
        let r = Reader::new(&m.bytes).sub(body.code_start, body.code_end, "unexpected end");
        let mut ops = OpReader::new(r);
        while !t.frames.is_empty() {
            let op = ops.read()?;
            t.op(op);
        }
        Ok(InterpFunc {
            code: t.code.into_boxed_slice(),
            nparams,
            nresults: ft.results.len() as u32,
            nlocals: body.locals.len() as u32,
            frame_size: nlocals + info.max_height,
        })
    }

    fn canon(&self, depth: usize) -> Slot {
        self.nlocals + depth as u32
    }

    fn emit(&mut self, i: Instr) -> usize {
        self.code.push(i);
        self.last_def = None;
        self.code.len() - 1
    }

    fn emit_def(&mut self, i: Instr, d: Slot) {
        self.code.push(i);
        self.last_def = Some((self.code.len() - 1, d));
    }

    /// A position other code may jump to.
    fn label(&mut self) {
        self.last_def = None;
        self.fuel_at = None;
    }

    fn charge(&mut self) {
        if !self.fuel {
            return;
        }
        match self.fuel_at {
            Some(i) => {
                if let Instr::Fuel { n } = &mut self.code[i] {
                    *n += 1;
                }
            }
            None => {
                self.code.push(Instr::Fuel { n: 1 });
                self.fuel_at = Some(self.code.len() - 1);
                self.last_def = None;
            }
        }
    }

    fn push(&mut self, src: Src, ty: ValType) {
        self.stack.push(Entry { src, ty });
    }

    fn push_canon(&mut self, ty: ValType) -> Slot {
        let s = self.canon(self.stack.len());
        self.push(Src::Slot(s), ty);
        s
    }

    fn pop(&mut self) -> Entry {
        self.stack.pop().expect("validated stack")
    }

    fn emit_const(&mut self, d: Slot, v: u64, ty: ValType) {
        if matches!(ty, ValType::I32 | ValType::F32) {
            self.emit(Instr::Const32 { d, v: v as u32 });
        } else {
            self.emit(Instr::Const64 { d, v });
        }
    }

    /// Write entry `idx` into its canonical slot.
    fn materialize(&mut self, idx: usize) {
        let d = self.canon(idx);
        let e = self.stack[idx];
        match e.src {
            Src::Slot(s) if s == d => {}
            Src::Slot(s) => {
                self.emit(Instr::Copy { d, s });
            }
            Src::Const(v) => self.emit_const(d, v, e.ty),
        }
        self.stack[idx].src = Src::Slot(d);
    }

    fn materialize_top(&mut self, n: usize) {
        let len = self.stack.len();
        for i in len - n..len {
            self.materialize(i);
        }
    }

    /// Write every entry that still refers to a local into its canonical slot.
    fn spill_locals(&mut self) {
        for i in 0..self.stack.len() {
            if let Src::Slot(s) = self.stack[i].src
                && s < self.nlocals
            {
                self.materialize(i);
            }
        }
    }

    /// The slot holding a just-popped entry that was at depth `depth`, writing constants out.
    fn operand(&mut self, e: Entry, depth: usize) -> Slot {
        match e.src {
            Src::Slot(s) => s,
            Src::Const(v) => {
                let d = self.canon(depth);
                self.emit_const(d, v, e.ty);
                d
            }
        }
    }

    fn block_sig(&self, bt: BlockType) -> (Vec<ValType>, Vec<ValType>) {
        match bt {
            BlockType::Empty => (vec![], vec![]),
            BlockType::Value(t) => (vec![], vec![t]),
            BlockType::Func(i) => {
                let t = &self.m.types[i as usize];
                (t.params.to_vec(), t.results.to_vec())
            }
        }
    }

    fn push_frame(&mut self, kind: Kind, params: Vec<ValType>, results: Vec<ValType>) {
        let height = self.stack.len() - params.len();
        self.frames.push(Frame {
            kind,
            height,
            params,
            results,
            start: self.code.len() as u32,
            patches: vec![],
            else_patch: None,
            had_else: false,
        });
    }

    fn frame_at(&self, depth: u32) -> usize {
        self.frames.len() - 1 - depth as usize
    }

    /// Turn a popped condition into a branch test, fusing a just-emitted comparison.
    fn take_cond(&mut self, e: Entry) -> Cond {
        match e.src {
            Src::Const(v) => Cond::Const(v as u32 != 0),
            Src::Slot(s) => {
                if let Some((idx, d)) = self.last_def
                    && d == s
                    && idx + 1 == self.code.len()
                    && let Some(c) = as_cmp(&self.code[idx])
                {
                    self.code.pop();
                    self.last_def = None;
                    return c;
                }
                Cond::Nez(s)
            }
        }
    }

    /// Emit a branch taken when `cond` holds. Returns the instruction to patch, if any.
    fn emit_cond_branch(&mut self, cond: Cond, t: u32) -> Option<usize> {
        let i = match cond {
            Cond::Cmp(c, a, b) => Instr::br_cmp(c, a, b, t),
            Cond::CmpImm(c, a, imm) => Instr::br_cmp_imm(c, a, imm, t),
            Cond::Nez(s) => Instr::BrIfNez { c: s, t },
            Cond::Eqz(s) => Instr::BrIfEqz { c: s, t },
            Cond::Const(true) => Instr::Br { t },
            Cond::Const(false) => return None,
        };
        Some(self.emit(i))
    }

    fn label_arity(&self, fi: usize) -> usize {
        let f = &self.frames[fi];
        if f.kind == Kind::Loop {
            f.params.len()
        } else {
            f.results.len()
        }
    }

    /// Whether branching to frame `fi` needs value copies.
    fn branch_needs_copies(&self, fi: usize) -> bool {
        let n = self.label_arity(fi);
        let base = self.stack.len() - n;
        let h = self.frames[fi].height;
        (0..n).any(|i| self.stack[base + i].src != Src::Slot(self.canon(h + i)))
    }

    /// Copy the branch values into the target's slots (ascending order is safe, see module docs).
    fn emit_branch_copies(&mut self, fi: usize) {
        let n = self.label_arity(fi);
        let base = self.stack.len() - n;
        let h = self.frames[fi].height;
        for i in 0..n {
            let d = self.canon(h + i);
            let e = self.stack[base + i];
            match e.src {
                Src::Slot(s) if s == d => {}
                Src::Slot(s) => {
                    self.emit(Instr::Copy { d, s });
                }
                Src::Const(v) => self.emit_const(d, v, e.ty),
            }
        }
    }

    /// Emit an unconditional jump to frame `fi` (values already in place).
    fn emit_jump(&mut self, fi: usize) {
        if self.frames[fi].kind == Kind::Loop {
            let t = self.frames[fi].start;
            self.emit(Instr::Br { t });
        } else {
            let idx = self.emit(Instr::Br { t: 0 });
            self.frames[fi].patches.push(idx);
        }
    }

    fn emit_return(&mut self) {
        let n = self.nresults;
        let base = self.stack.len() - n;
        // A result taken from a lower local would be clobbered by an earlier copy.
        for i in 0..n {
            if let Src::Slot(s) = self.stack[base + i].src
                && (s as usize) < i
            {
                self.materialize(base + i);
            }
        }
        for i in 0..n {
            let d = i as Slot;
            let e = self.stack[base + i];
            match e.src {
                Src::Slot(s) if s == d => {}
                Src::Slot(s) => {
                    self.emit(Instr::Copy { d, s });
                }
                Src::Const(v) => self.emit_const(d, v, e.ty),
            }
        }
        self.emit(Instr::Return);
    }

    fn set_dead(&mut self) {
        let h = self.frames.last().unwrap().height;
        self.stack.truncate(h);
        self.dead = true;
    }

    fn set_local(&mut self, i: u32, e: Entry) {
        let mut copies = false;
        for k in 0..self.stack.len() {
            if self.stack[k].src == Src::Slot(i) {
                self.materialize(k);
                copies = true;
            }
        }
        let depth = self.stack.len();
        match e.src {
            Src::Slot(s) if s == i => {}
            Src::Slot(s) => {
                if !copies
                    && s == self.canon(depth)
                    && let Some((idx, d)) = self.last_def
                    && d == s
                    && idx + 1 == self.code.len()
                    && self.code[idx].retarget(i)
                {
                    self.last_def = None;
                    return;
                }
                self.emit(Instr::Copy { d: i, s });
            }
            Src::Const(v) => self.emit_const(i, v, e.ty),
        }
    }

    fn numeric(&mut self, op: NumOp) {
        let (params, rty) = op.signature();
        if params.len() == 1 {
            let a = self.pop();
            let depth = self.stack.len();
            if let Src::Const(v) = a.src
                && let Ok(r) = num::eval(op, v, 0)
            {
                self.push(Src::Const(r), rty);
                return;
            }
            let sa = self.operand(a, depth);
            let d = self.canon(depth);
            self.emit_def(Instr::num(op, d, sa, 0), d);
            self.push(Src::Slot(d), rty);
            return;
        }
        let b = self.pop();
        let a = self.pop();
        let depth = self.stack.len();
        let d = self.canon(depth);
        if let (Src::Const(x), Src::Const(y)) = (a.src, b.src)
            && let Ok(r) = num::eval(op, x, y)
        {
            self.push(Src::Const(r), rty);
            return;
        }
        let imm_of = |v: u64, ty: ValType| -> Option<u32> {
            match ty {
                ValType::I32 => Some(v as u32),
                ValType::I64 if (v as i64) >= i32::MIN as i64 && (v as i64) <= i32::MAX as i64 => {
                    Some(v as u32)
                }
                _ => None,
            }
        };
        if let Src::Const(v) = b.src
            && let Some(imm) = imm_of(v, b.ty)
            && Instr::num_imm(op, 0, 0, 0).is_some()
        {
            let sa = self.operand(a, depth);
            self.emit_def(Instr::num_imm(op, d, sa, imm).unwrap(), d);
            self.push(Src::Slot(d), rty);
            return;
        }
        if let Src::Const(v) = a.src
            && let Some(imm) = imm_of(v, a.ty)
            && let Some(mop) = mirror(op)
            && Instr::num_imm(mop, 0, 0, 0).is_some()
        {
            let sb = self.operand(b, depth + 1);
            self.emit_def(Instr::num_imm(mop, d, sb, imm).unwrap(), d);
            self.push(Src::Slot(d), rty);
            return;
        }
        let sa = self.operand(a, depth);
        let sb = self.operand(b, depth + 1);
        self.emit_def(Instr::num(op, d, sa, sb), d);
        self.push(Src::Slot(d), rty);
    }

    fn op(&mut self, op: Op) {
        if self.dead {
            match op {
                Op::Block(_) | Op::Loop(_) | Op::If(_) => {
                    self.dead_depth += 1;
                    return;
                }
                Op::Else if self.dead_depth == 0 => {}
                Op::End if self.dead_depth == 0 => {}
                Op::End => {
                    self.dead_depth -= 1;
                    return;
                }
                _ => return,
            }
        }
        if !matches!(op, Op::End | Op::Else) {
            self.charge();
        }
        match op {
            Op::Unreachable => {
                self.emit(Instr::Trap {
                    code: TrapCode::Unreachable.to_raw(),
                });
                self.set_dead();
            }
            Op::Nop => {}
            Op::Block(bt) => {
                let (p, r) = self.block_sig(bt);
                self.spill_locals();
                self.materialize_top(p.len());
                self.push_frame(Kind::Block, p, r);
            }
            Op::Loop(bt) => {
                let (p, r) = self.block_sig(bt);
                self.spill_locals();
                self.materialize_top(p.len());
                self.label();
                self.push_frame(Kind::Loop, p, r);
            }
            Op::If(bt) => {
                let (p, r) = self.block_sig(bt);
                let c = self.pop();
                let cond = self.take_cond(c);
                self.spill_locals();
                self.materialize_top(p.len());
                let br = self.emit_cond_branch(cond.negate(), 0);
                self.push_frame(Kind::If, p, r);
                self.frames.last_mut().unwrap().else_patch = br;
            }
            Op::Else => {
                let fi = self.frames.len() - 1;
                if !self.dead {
                    let n = self.frames[fi].results.len();
                    self.materialize_top(n);
                    self.emit_jump(fi);
                }
                let here = self.code.len() as u32;
                if let Some(p) = self.frames[fi].else_patch.take() {
                    self.code[p].set_target(here);
                }
                let f = &mut self.frames[fi];
                f.kind = Kind::Else;
                f.had_else = true;
                let h = f.height;
                let params = f.params.clone();
                self.stack.truncate(h);
                for t in params {
                    self.push_canon(t);
                }
                self.dead = false;
                self.label();
            }
            Op::End => {
                let fi = self.frames.len() - 1;
                if self.frames[fi].kind == Kind::Func {
                    if !self.dead {
                        self.emit_return();
                    }
                    self.frames.pop();
                    return;
                }
                let fallthrough = !self.dead;
                if fallthrough {
                    let n = self.frames[fi].results.len();
                    self.materialize_top(n);
                }
                let f = self.frames.pop().unwrap();
                let here = self.code.len() as u32;
                let mut live = fallthrough || !f.patches.is_empty();
                if let Some(p) = f.else_patch {
                    // `if` without `else`: the false edge carries the params (= results).
                    self.code[p].set_target(here);
                    live = true;
                }
                for &p in &f.patches {
                    self.code[p].set_target(here);
                }
                self.label();
                self.stack.truncate(f.height);
                for &t in &f.results {
                    self.push_canon(t);
                }
                self.dead = !live;
            }
            Op::Br(l) => {
                let fi = self.frame_at(l);
                if self.frames[fi].kind == Kind::Func {
                    self.emit_return();
                } else {
                    self.emit_branch_copies(fi);
                    self.emit_jump(fi);
                }
                self.set_dead();
            }
            Op::BrIf(l) => {
                let fi = self.frame_at(l);
                let c = self.pop();
                let cond = self.take_cond(c);
                if self.frames[fi].kind == Kind::Func || self.branch_needs_copies(fi) {
                    // if cond { copies; jump }
                    let skip = self.emit_cond_branch(cond.negate(), 0);
                    if self.frames[fi].kind == Kind::Func {
                        self.emit_return();
                    } else {
                        self.emit_branch_copies(fi);
                        self.emit_jump(fi);
                    }
                    let here = self.code.len() as u32;
                    if let Some(s) = skip {
                        self.code[s].set_target(here);
                    }
                    self.label();
                } else if self.frames[fi].kind == Kind::Loop {
                    let t = self.frames[fi].start;
                    self.emit_cond_branch(cond, t);
                } else if let Some(idx) = self.emit_cond_branch(cond, 0) {
                    self.frames[fi].patches.push(idx);
                }
            }
            Op::BrTable { targets, default } => {
                let e = self.pop();
                let depth = self.stack.len();
                let idx = self.operand(e, depth);
                let len = targets.len() as u32;
                self.emit(Instr::BrTable { idx, len });
                let table = self.code.len();
                for _ in 0..=len {
                    self.emit(Instr::Br { t: 0 });
                }
                let all: Vec<u32> = targets
                    .iter()
                    .copied()
                    .chain(std::iter::once(default))
                    .collect();
                for (k, &l) in all.iter().enumerate() {
                    let fi = self.frame_at(l);
                    let entry = table + k;
                    if self.frames[fi].kind == Kind::Func || self.branch_needs_copies(fi) {
                        let stub = self.code.len() as u32;
                        self.code[entry].set_target(stub);
                        if self.frames[fi].kind == Kind::Func {
                            self.emit_return();
                        } else {
                            self.emit_branch_copies(fi);
                            self.emit_jump(fi);
                        }
                    } else if self.frames[fi].kind == Kind::Loop {
                        let t = self.frames[fi].start;
                        self.code[entry].set_target(t);
                    } else {
                        self.frames[fi].patches.push(entry);
                    }
                }
                self.set_dead();
            }
            Op::Return => {
                self.emit_return();
                self.set_dead();
            }
            Op::Call(f) => {
                let ft = self.m.func_type(f);
                let (np, results) = (ft.params.len(), ft.results.to_vec());
                self.materialize_top(np);
                let bd = self.stack.len() - np;
                let base = self.canon(bd);
                self.emit(Instr::Call { f, base });
                self.stack.truncate(bd);
                for t in results {
                    self.push_canon(t);
                }
            }
            Op::CallIndirect { ty, table } => {
                let ft = &self.m.types[ty as usize];
                let (np, results) = (ft.params.len(), ft.results.to_vec());
                self.materialize_top(np + 1);
                let bd = self.stack.len() - np - 1;
                let base = self.canon(bd);
                self.emit(Instr::CallIndirect { base, ty, table });
                self.stack.truncate(bd);
                for t in results {
                    self.push_canon(t);
                }
            }
            Op::Drop => {
                self.pop();
            }
            Op::Select | Op::SelectT(_) => {
                let c = self.pop();
                let b = self.pop();
                let a = self.pop();
                if let Src::Const(v) = c.src {
                    self.stack.push(if v as u32 != 0 { a } else { b });
                    return;
                }
                let depth = self.stack.len();
                self.stack.push(a);
                self.materialize(depth);
                self.stack.pop();
                let sb = self.operand(b, depth + 1);
                let sc = self.operand(c, depth + 2);
                let d = self.canon(depth);
                self.emit(Instr::Select { d, b: sb, c: sc });
                self.push(Src::Slot(d), a.ty);
            }
            Op::LocalGet(i) => {
                let ty = self.local_type(i);
                self.push(Src::Slot(i), ty);
            }
            Op::LocalSet(i) => {
                let e = self.pop();
                self.set_local(i, e);
            }
            Op::LocalTee(i) => {
                let e = self.pop();
                self.set_local(i, e);
                self.push(Src::Slot(i), e.ty);
            }
            Op::GlobalGet(g) => {
                let ty = self.m.globals[g as usize].ty;
                let d = self.canon(self.stack.len());
                self.emit_def(Instr::GlobalGet { d, g }, d);
                self.push(Src::Slot(d), ty);
            }
            Op::GlobalSet(g) => {
                let e = self.pop();
                let s = self.operand(e, self.stack.len());
                self.emit(Instr::GlobalSet { s, g });
            }
            Op::TableGet(t) => {
                let ty = self.m.tables[t as usize].elem;
                let e = self.pop();
                let depth = self.stack.len();
                let i = self.operand(e, depth);
                let d = self.canon(depth);
                self.emit_def(Instr::TableGet { d, i, t }, d);
                self.push(Src::Slot(d), ty);
            }
            Op::TableSet(t) => {
                let v = self.pop();
                let i = self.pop();
                let depth = self.stack.len();
                let si = self.operand(i, depth);
                let sv = self.operand(v, depth + 1);
                self.emit(Instr::TableSet { i: si, v: sv, t });
            }
            Op::Load(lo, ma) => {
                let e = self.pop();
                let depth = self.stack.len();
                let a = self.operand(e, depth);
                let d = self.canon(depth);
                self.emit_def(load_instr(lo, d, a, ma.offset), d);
                self.push(Src::Slot(d), lo.result());
            }
            Op::Store(so, ma) => {
                let v = self.pop();
                let a = self.pop();
                let depth = self.stack.len();
                let sa = self.operand(a, depth);
                let sv = self.operand(v, depth + 1);
                self.emit(store_instr(so, sa, sv, ma.offset));
            }
            Op::MemorySize => {
                let d = self.canon(self.stack.len());
                self.emit_def(Instr::MemorySize { d }, d);
                self.push(Src::Slot(d), ValType::I32);
            }
            Op::MemoryGrow => {
                let e = self.pop();
                let depth = self.stack.len();
                let n = self.operand(e, depth);
                let d = self.canon(depth);
                self.emit(Instr::MemoryGrow { d, n });
                self.push(Src::Slot(d), ValType::I32);
            }
            Op::I32Const(v) => self.push(Src::Const(v as u32 as u64), ValType::I32),
            Op::I64Const(v) => self.push(Src::Const(v as u64), ValType::I64),
            Op::F32Const(v) => self.push(Src::Const(v as u64), ValType::F32),
            Op::F64Const(v) => self.push(Src::Const(v), ValType::F64),
            Op::Num(n) => self.numeric(n),
            Op::RefNull(t) => self.push(Src::Const(0), t),
            Op::RefIsNull => {
                let e = self.pop();
                let depth = self.stack.len();
                if let Src::Const(v) = e.src {
                    self.push(Src::Const((v == 0) as u64), ValType::I32);
                    return;
                }
                let a = self.operand(e, depth);
                let d = self.canon(depth);
                self.emit_def(Instr::I64Eqz { d, a }, d);
                self.push(Src::Slot(d), ValType::I32);
            }
            Op::RefFunc(f) => {
                let d = self.canon(self.stack.len());
                self.emit_def(Instr::RefFunc { d, f }, d);
                self.push(Src::Slot(d), ValType::FuncRef);
            }
            Op::MemoryInit(seg) => self.bulk(3, |base| Instr::MemoryInit { base, seg }),
            Op::DataDrop(seg) => {
                self.emit(Instr::DataDrop { seg });
            }
            Op::MemoryCopy => self.bulk(3, |base| Instr::MemoryCopy { base }),
            Op::MemoryFill => self.bulk(3, |base| Instr::MemoryFill { base }),
            Op::TableInit { elem, table } => self.bulk(3, |base| Instr::TableInit {
                base,
                seg: elem,
                t: table,
            }),
            Op::ElemDrop(seg) => {
                self.emit(Instr::ElemDrop { seg });
            }
            Op::TableCopy { dst, src } => self.bulk(3, |base| Instr::TableCopy { base, dst, src }),
            Op::TableGrow(t) => {
                self.materialize_top(2);
                let bd = self.stack.len() - 2;
                let base = self.canon(bd);
                self.emit(Instr::TableGrow { base, t });
                self.stack.truncate(bd);
                self.push_canon(ValType::I32);
            }
            Op::TableSize(t) => {
                let d = self.canon(self.stack.len());
                self.emit_def(Instr::TableSize { d, t }, d);
                self.push(Src::Slot(d), ValType::I32);
            }
            Op::TableFill(t) => self.bulk(3, |base| Instr::TableFill { base, t }),
        }
    }

    /// An instruction taking `n` operands in consecutive canonical slots.
    fn bulk(&mut self, n: usize, f: impl FnOnce(Slot) -> Instr) {
        self.materialize_top(n);
        let bd = self.stack.len() - n;
        let base = self.canon(bd);
        self.emit(f(base));
        self.stack.truncate(bd);
    }

    fn local_type(&self, i: u32) -> ValType {
        self.locals[i as usize]
    }
}
