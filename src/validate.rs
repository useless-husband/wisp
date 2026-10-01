//! Validation of modules and function bodies (WebAssembly 2.0, without SIMD).
//!
//! Function bodies are checked with the algorithm from the specification's appendix: an
//! operand stack of possibly-unknown types and a stack of control frames.

use crate::binary::module::*;
use crate::binary::ops::{BlockType, MemArg, Op, OpReader};
use crate::binary::reader::Reader;
use crate::error::{Error, Result};
use crate::types::*;
use std::collections::HashSet;

/// Facts the validator learns about a function that later passes reuse.
#[derive(Clone, Debug, Default)]
pub struct FuncInfo {
    /// Maximum operand stack height, in values.
    pub max_height: u32,
}

pub fn validate(m: &ModuleData) -> Result<Vec<FuncInfo>> {
    let v = ModuleValidator::new(m);
    match v.run() {
        Ok(info) => Ok(info),
        Err(e @ Error::Invalid { .. }) => {
            // The reference interpreter decodes everything before validating, so a malformed
            // body later in the module takes precedence over an earlier validation error.
            if let Err(me @ Error::Malformed { .. }) = m.check_bodies_decode() {
                return Err(me);
            }
            Err(e)
        }
        Err(e) => Err(e),
    }
}

struct ModuleValidator<'m> {
    m: &'m ModuleData,
    /// Functions that may be referenced by `ref.func` in code.
    refs: HashSet<u32>,
}

fn inv<T>(offset: usize, msg: impl Into<String>) -> Result<T> {
    Err(Error::invalid(offset, msg))
}

impl<'m> ModuleValidator<'m> {
    fn new(m: &'m ModuleData) -> Self {
        ModuleValidator {
            m,
            refs: HashSet::new(),
        }
    }

    fn check_type_idx(&self, idx: u32, off: usize) -> Result<&'m FuncType> {
        match self.m.types.get(idx as usize) {
            Some(t) => Ok(t),
            None => inv(off, format!("unknown type {idx}")),
        }
    }

    fn check_limits(&self, l: &Limits, bound: u64, what: &str, off: usize) -> Result<()> {
        if l.min as u64 > bound || l.max.is_some_and(|x| x as u64 > bound) {
            return inv(off, what.to_string());
        }
        if let Some(max) = l.max
            && l.min > max
        {
            return inv(off, "size minimum must not be greater than maximum");
        }
        Ok(())
    }

    fn run(mut self) -> Result<Vec<FuncInfo>> {
        let m = self.m;
        for t in &m.types {
            if t.params
                .iter()
                .chain(t.results.iter())
                .any(|&v| v == ValType::V128)
            {
                return Err(Error::Unsupported(
                    "v128 values (SIMD) are not implemented".into(),
                ));
            }
        }
        for imp in &m.imports {
            match &imp.desc {
                ImportDesc::Func(t) => {
                    self.check_type_idx(*t, 0)?;
                }
                ImportDesc::Table(t) => {
                    self.check_limits(&t.limits, u32::MAX as u64, "table size", 0)?
                }
                ImportDesc::Memory(t) => self.check_limits(
                    &t.limits,
                    MAX_PAGES as u64,
                    "memory size must be at most 65536 pages (4GiB)",
                    0,
                )?,
                ImportDesc::Global(g) => {
                    if g.ty == ValType::V128 {
                        return Err(Error::Unsupported(
                            "v128 globals (SIMD) are not implemented".into(),
                        ));
                    }
                }
            }
        }
        for &t in &m.funcs[m.num_imported_funcs as usize..] {
            self.check_type_idx(t, 0)?;
        }
        for t in &m.tables[m.num_imported_tables as usize..] {
            self.check_limits(&t.limits, u32::MAX as u64, "table size", 0)?;
        }
        if m.memories.len() > 1 {
            return inv(0, "multiple memories");
        }
        for t in &m.memories[m.num_imported_memories as usize..] {
            self.check_limits(
                &t.limits,
                MAX_PAGES as u64,
                "memory size must be at most 65536 pages (4GiB)",
                0,
            )?;
        }
        // Function references declared outside function bodies (C.refs).
        let mut note_refs = |e: &ConstExpr, refs: &mut HashSet<u32>| {
            for op in &e.ops {
                if let Op::RefFunc(f) = op {
                    refs.insert(*f);
                }
            }
        };
        let mut refs = HashSet::new();
        for g in &m.global_inits {
            note_refs(g, &mut refs);
        }
        for seg in &m.elems {
            for it in &seg.items {
                note_refs(it, &mut refs);
            }
        }
        for e in &m.exports {
            if e.kind == ExternKind::Func {
                refs.insert(e.index);
            }
        }
        self.refs = refs;

        let ng = m.num_imported_globals as usize;
        for (i, init) in m.global_inits.iter().enumerate() {
            let gt = m.globals[ng + i];
            if gt.ty == ValType::V128 {
                return Err(Error::Unsupported(
                    "v128 globals (SIMD) are not implemented".into(),
                ));
            }
            self.const_expr(init, gt.ty)?;
        }
        let mut names = HashSet::new();
        for e in &m.exports {
            let (count, what) = match e.kind {
                ExternKind::Func => (m.funcs.len(), "function"),
                ExternKind::Table => (m.tables.len(), "table"),
                ExternKind::Memory => (m.memories.len(), "memory"),
                ExternKind::Global => (m.globals.len(), "global"),
            };
            if e.index as usize >= count {
                return inv(0, format!("unknown {what} {}", e.index));
            }
            if !names.insert(e.name.as_str()) {
                return inv(0, "duplicate export name");
            }
        }
        if let Some(s) = m.start {
            if s as usize >= m.funcs.len() {
                return inv(0, format!("unknown function {s}"));
            }
            let t = m.func_type(s);
            if !t.params.is_empty() || !t.results.is_empty() {
                return inv(0, "start function must have type [] -> []");
            }
        }
        for seg in &m.elems {
            if let ElemMode::Active { table, offset } = &seg.mode {
                let Some(tt) = m.tables.get(*table as usize) else {
                    return inv(seg.offset, format!("unknown table {table}"));
                };
                self.const_expr(offset, ValType::I32)?;
                if tt.elem != seg.ty {
                    return inv(seg.offset, "type mismatch");
                }
            }
            for it in &seg.items {
                self.const_expr(it, seg.ty)?;
            }
        }
        for d in &m.datas {
            if let DataMode::Active { memory, offset } = &d.mode {
                if *memory as usize >= m.memories.len() {
                    return inv(offset.offset, format!("unknown memory {memory}"));
                }
                self.const_expr(offset, ValType::I32)?;
            }
        }
        let mut infos = Vec::with_capacity(m.bodies.len());
        for (i, body) in m.bodies.iter().enumerate() {
            let func = m.num_imported_funcs + i as u32;
            let mut fv = FuncValidator::new(&self, func, body)?;
            fv.run()?;
            infos.push(FuncInfo {
                max_height: fv.max_height as u32,
            });
        }
        Ok(infos)
    }

    fn const_expr(&self, e: &ConstExpr, want: ValType) -> Result<()> {
        let m = self.m;
        let mut stack: Vec<ValType> = Vec::new();
        for op in &e.ops {
            let t = match op {
                Op::I32Const(_) => ValType::I32,
                Op::I64Const(_) => ValType::I64,
                Op::F32Const(_) => ValType::F32,
                Op::F64Const(_) => ValType::F64,
                Op::RefNull(t) => *t,
                Op::RefFunc(f) => {
                    if *f as usize >= m.funcs.len() {
                        return inv(e.offset, format!("unknown function {f}"));
                    }
                    ValType::FuncRef
                }
                Op::GlobalGet(g) => {
                    // Only imported globals are visible to constant expressions in 2.0.
                    if *g >= m.num_imported_globals {
                        return inv(e.offset, format!("unknown global {g}"));
                    }
                    let gt = m.globals[*g as usize];
                    if gt.mutable {
                        return inv(e.offset, "constant expression required");
                    }
                    gt.ty
                }
                _ => return inv(e.offset, "constant expression required"),
            };
            stack.push(t);
        }
        if stack.len() != 1 || stack[0] != want {
            return inv(e.offset, "type mismatch");
        }
        Ok(())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Kind {
    Block,
    Loop,
    If,
    Else,
    Func,
}

struct Ctrl {
    kind: Kind,
    start: Vec<ValType>,
    end: Vec<ValType>,
    height: usize,
    unreachable: bool,
}

/// `None` is the unknown type produced by popping in unreachable code.
type MaybeType = Option<ValType>;

struct FuncValidator<'a, 'm> {
    mv: &'a ModuleValidator<'m>,
    ops: OpReader<'m>,
    locals: Vec<ValType>,
    results: Vec<ValType>,
    vals: Vec<MaybeType>,
    ctrls: Vec<Ctrl>,
    max_height: usize,
    /// Start of the instruction being validated, for error offsets.
    at: usize,
    body_end: usize,
}

impl<'a, 'm> FuncValidator<'a, 'm> {
    fn new(mv: &'a ModuleValidator<'m>, func: u32, body: &'m FuncBody) -> Result<Self> {
        let m = mv.m;
        let ft = m.func_type(func);
        let mut locals: Vec<ValType> = ft.params.to_vec();
        locals.extend_from_slice(&body.locals);
        if locals.contains(&ValType::V128) {
            return Err(Error::Unsupported(
                "v128 locals (SIMD) are not implemented".into(),
            ));
        }
        // Decode past the declared body end like the reference decoder; `run` checks the size.
        let r = Reader::new(&m.bytes).sub(
            body.code_start,
            m.bytes.len(),
            "unexpected end of section or function",
        );
        Ok(FuncValidator {
            body_end: body.code_end,
            mv,
            ops: OpReader::new(r),
            locals,
            results: ft.results.to_vec(),
            vals: Vec::new(),
            ctrls: Vec::new(),
            max_height: 0,
            at: body.code_start,
        })
    }

    fn err<T>(&self, msg: impl Into<String>) -> Result<T> {
        Err(Error::invalid(self.at, msg))
    }

    fn push(&mut self, t: MaybeType) {
        self.vals.push(t);
        if self.vals.len() > self.max_height {
            self.max_height = self.vals.len();
        }
    }

    fn push_t(&mut self, t: ValType) {
        self.push(Some(t));
    }

    fn pop(&mut self) -> Result<MaybeType> {
        let c = self.ctrls.last().unwrap();
        if self.vals.len() == c.height {
            if c.unreachable {
                return Ok(None);
            }
            return self.err("type mismatch");
        }
        Ok(self.vals.pop().unwrap())
    }

    fn pop_expect(&mut self, want: ValType) -> Result<MaybeType> {
        let got = self.pop()?;
        match got {
            Some(t) if t != want => self.err("type mismatch"),
            _ => Ok(got),
        }
    }

    fn pop_vals(&mut self, types: &[ValType]) -> Result<Vec<MaybeType>> {
        let mut out = vec![None; types.len()];
        for (i, &t) in types.iter().enumerate().rev() {
            out[i] = self.pop_expect(t)?;
        }
        Ok(out)
    }

    fn push_vals(&mut self, types: &[ValType]) {
        for &t in types {
            self.push_t(t);
        }
    }

    fn push_ctrl(&mut self, kind: Kind, start: Vec<ValType>, end: Vec<ValType>) {
        let height = self.vals.len();
        self.push_vals(&start);
        self.ctrls.push(Ctrl {
            kind,
            start,
            end,
            height,
            unreachable: false,
        });
    }

    fn pop_ctrl(&mut self) -> Result<Ctrl> {
        let Some(c) = self.ctrls.last() else {
            return self.err("unexpected end");
        };
        let end = c.end.clone();
        let height = c.height;
        self.pop_vals(&end)?;
        if self.vals.len() != height {
            return self.err("type mismatch");
        }
        Ok(self.ctrls.pop().unwrap())
    }

    fn label_types(&self, depth: u32) -> Result<Vec<ValType>> {
        let n = self.ctrls.len();
        if depth as usize >= n {
            return self.err(format!("unknown label {depth}"));
        }
        let c = &self.ctrls[n - 1 - depth as usize];
        Ok(if c.kind == Kind::Loop {
            c.start.clone()
        } else {
            c.end.clone()
        })
    }

    fn set_unreachable(&mut self) {
        let c = self.ctrls.last_mut().unwrap();
        self.vals.truncate(c.height);
        c.unreachable = true;
    }

    fn block_sig(&self, bt: BlockType) -> Result<(Vec<ValType>, Vec<ValType>)> {
        Ok(match bt {
            BlockType::Empty => (vec![], vec![]),
            BlockType::Value(t) => {
                if t == ValType::V128 {
                    return Err(Error::Unsupported(
                        "v128 blocks (SIMD) are not implemented".into(),
                    ));
                }
                (vec![], vec![t])
            }
            BlockType::Func(i) => {
                let t = self.mv.check_type_idx(i, self.at)?;
                (t.params.to_vec(), t.results.to_vec())
            }
        })
    }

    fn local(&self, i: u32) -> Result<ValType> {
        match self.locals.get(i as usize) {
            Some(t) => Ok(*t),
            None => self.err(format!("unknown local {i}")),
        }
    }

    fn global(&self, i: u32) -> Result<GlobalType> {
        match self.mv.m.globals.get(i as usize) {
            Some(g) => Ok(*g),
            None => self.err(format!("unknown global {i}")),
        }
    }

    fn table(&self, i: u32) -> Result<TableType> {
        match self.mv.m.tables.get(i as usize) {
            Some(t) => Ok(*t),
            None => self.err(format!("unknown table {i}")),
        }
    }

    fn memory(&self) -> Result<()> {
        if self.mv.m.memories.is_empty() {
            return self.err("unknown memory 0");
        }
        Ok(())
    }

    fn memarg(&self, ma: MemArg, width_log2: u32) -> Result<()> {
        self.memory()?;
        if ma.align > width_log2 {
            return self.err("alignment must not be larger than natural");
        }
        Ok(())
    }

    fn data_idx(&self, i: u32) -> Result<()> {
        match self.mv.m.data_count {
            None => Err(Error::malformed(self.at, "data count section required")),
            Some(n) if i >= n => self.err(format!("unknown data segment {i}")),
            _ => Ok(()),
        }
    }

    fn elem(&self, i: u32) -> Result<ValType> {
        match self.mv.m.elems.get(i as usize) {
            Some(s) => Ok(s.ty),
            None => self.err(format!("unknown elem segment {i}")),
        }
    }

    fn run(&mut self) -> Result<()> {
        let results = self.results.clone();
        self.ctrls.push(Ctrl {
            kind: Kind::Func,
            start: vec![],
            end: results,
            height: 0,
            unreachable: false,
        });
        while !self.ctrls.is_empty() {
            self.at = self.ops.pos();
            if self.at > self.body_end {
                return Err(Error::malformed(self.at, "section size mismatch"));
            }
            let op = self.ops.read()?;
            self.op(op)?;
        }
        if self.ops.pos() != self.body_end {
            return Err(Error::malformed(self.ops.pos(), "section size mismatch"));
        }
        Ok(())
    }

    fn op(&mut self, op: Op) -> Result<()> {
        use ValType::*;
        match op {
            Op::Unreachable => self.set_unreachable(),
            Op::Nop => {}
            Op::Block(bt) | Op::Loop(bt) => {
                let (s, e) = self.block_sig(bt)?;
                self.pop_vals(&s)?;
                let kind = if matches!(op, Op::Block(_)) {
                    Kind::Block
                } else {
                    Kind::Loop
                };
                self.push_ctrl(kind, s, e);
            }
            Op::If(bt) => {
                let (s, e) = self.block_sig(bt)?;
                self.pop_expect(I32)?;
                self.pop_vals(&s)?;
                self.push_ctrl(Kind::If, s, e);
            }
            Op::Else => {
                if self.ctrls.last().unwrap().kind != Kind::If {
                    return Err(Error::malformed(self.at, "END opcode expected"));
                }
                let c = self.pop_ctrl()?;
                self.push_ctrl(Kind::Else, c.start, c.end);
            }
            Op::End => {
                let c = self.pop_ctrl()?;
                // An `if` without `else` must leave its parameters unchanged.
                if c.kind == Kind::If && c.start != c.end {
                    return self.err("type mismatch");
                }
                self.push_vals(&c.end);
            }
            Op::Br(l) => {
                let t = self.label_types(l)?;
                self.pop_vals(&t)?;
                self.set_unreachable();
            }
            Op::BrIf(l) => {
                self.pop_expect(I32)?;
                let t = self.label_types(l)?;
                self.pop_vals(&t)?;
                self.push_vals(&t);
            }
            Op::BrTable { targets, default } => {
                self.pop_expect(I32)?;
                let dt = self.label_types(default)?;
                let arity = dt.len();
                for &l in targets.iter() {
                    let t = self.label_types(l)?;
                    if t.len() != arity {
                        return self.err("type mismatch");
                    }
                    let popped = self.pop_vals(&t)?;
                    for v in popped {
                        self.push(v);
                    }
                }
                self.pop_vals(&dt)?;
                self.set_unreachable();
            }
            Op::Return => {
                let r = self.results.clone();
                self.pop_vals(&r)?;
                self.set_unreachable();
            }
            Op::Call(f) => {
                let m = self.mv.m;
                if f as usize >= m.funcs.len() {
                    return self.err(format!("unknown function {f}"));
                }
                let t = m.func_type(f).clone();
                self.pop_vals(&t.params)?;
                self.push_vals(&t.results);
            }
            Op::CallIndirect { ty, table } => {
                let tt = self.table(table)?;
                if tt.elem != FuncRef {
                    return self.err("type mismatch");
                }
                let t = self.mv.check_type_idx(ty, self.at)?.clone();
                self.pop_expect(I32)?;
                self.pop_vals(&t.params)?;
                self.push_vals(&t.results);
            }
            Op::Drop => {
                self.pop()?;
            }
            Op::Select => {
                self.pop_expect(I32)?;
                let t1 = self.pop()?;
                let t2 = self.pop()?;
                let num = |t: MaybeType| t.is_none_or(|t| !t.is_ref() && t != V128);
                if !num(t1) || !num(t2) {
                    return self.err("type mismatch");
                }
                if let (Some(a), Some(b)) = (t1, t2)
                    && a != b
                {
                    return self.err("type mismatch");
                }
                self.push(t1.or(t2));
            }
            Op::SelectT(t) => {
                self.pop_expect(I32)?;
                self.pop_expect(t)?;
                self.pop_expect(t)?;
                self.push_t(t);
            }
            Op::LocalGet(i) => {
                let t = self.local(i)?;
                self.push_t(t);
            }
            Op::LocalSet(i) => {
                let t = self.local(i)?;
                self.pop_expect(t)?;
            }
            Op::LocalTee(i) => {
                let t = self.local(i)?;
                self.pop_expect(t)?;
                self.push_t(t);
            }
            Op::GlobalGet(i) => {
                let g = self.global(i)?;
                self.push_t(g.ty);
            }
            Op::GlobalSet(i) => {
                let g = self.global(i)?;
                if !g.mutable {
                    return self.err("global is immutable");
                }
                self.pop_expect(g.ty)?;
            }
            Op::TableGet(i) => {
                let t = self.table(i)?;
                self.pop_expect(I32)?;
                self.push_t(t.elem);
            }
            Op::TableSet(i) => {
                let t = self.table(i)?;
                self.pop_expect(t.elem)?;
                self.pop_expect(I32)?;
            }
            Op::Load(lo, ma) => {
                self.memarg(ma, lo.width_log2())?;
                self.pop_expect(I32)?;
                self.push_t(lo.result());
            }
            Op::Store(so, ma) => {
                self.memarg(ma, so.width_log2())?;
                self.pop_expect(so.operand())?;
                self.pop_expect(I32)?;
            }
            Op::MemorySize => {
                self.memory()?;
                self.push_t(I32);
            }
            Op::MemoryGrow => {
                self.memory()?;
                self.pop_expect(I32)?;
                self.push_t(I32);
            }
            Op::I32Const(_) => self.push_t(I32),
            Op::I64Const(_) => self.push_t(I64),
            Op::F32Const(_) => self.push_t(F32),
            Op::F64Const(_) => self.push_t(F64),
            Op::Num(n) => {
                let (params, result) = n.signature();
                self.pop_vals(params)?;
                self.push_t(result);
            }
            Op::RefNull(t) => self.push_t(t),
            Op::RefIsNull => {
                let t = self.pop()?;
                if let Some(t) = t
                    && !t.is_ref()
                {
                    return self.err("type mismatch");
                }
                self.push_t(I32);
            }
            Op::RefFunc(f) => {
                if f as usize >= self.mv.m.funcs.len() {
                    return self.err(format!("unknown function {f}"));
                }
                if !self.mv.refs.contains(&f) {
                    return self.err("undeclared function reference");
                }
                self.push_t(FuncRef);
            }
            Op::MemoryInit(d) => {
                self.memory()?;
                self.data_idx(d)?;
                self.pop_vals(&[I32, I32, I32])?;
            }
            Op::DataDrop(d) => self.data_idx(d)?,
            Op::MemoryCopy | Op::MemoryFill => {
                self.memory()?;
                self.pop_vals(&[I32, I32, I32])?;
            }
            Op::TableInit { elem, table } => {
                let tt = self.table(table)?;
                let et = self.elem(elem)?;
                if tt.elem != et {
                    return self.err("type mismatch");
                }
                self.pop_vals(&[I32, I32, I32])?;
            }
            Op::ElemDrop(e) => {
                self.elem(e)?;
            }
            Op::TableCopy { dst, src } => {
                let d = self.table(dst)?;
                let s = self.table(src)?;
                if d.elem != s.elem {
                    return self.err("type mismatch");
                }
                self.pop_vals(&[I32, I32, I32])?;
            }
            Op::TableGrow(i) => {
                let t = self.table(i)?;
                self.pop_expect(I32)?;
                self.pop_expect(t.elem)?;
                self.push_t(I32);
            }
            Op::TableSize(i) => {
                self.table(i)?;
                self.push_t(I32);
            }
            Op::TableFill(i) => {
                let t = self.table(i)?;
                self.pop_expect(I32)?;
                self.pop_expect(t.elem)?;
                self.pop_expect(I32)?;
            }
        }
        Ok(())
    }
}
