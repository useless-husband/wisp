//! Module structure and section decoding.

use super::ops::{Op, OpReader};
use super::reader::Reader;
use crate::error::{Error, Result};
use crate::types::*;
use std::collections::HashMap;

/// Engine limit on the number of locals of one function (same limit as wasmparser).
pub const MAX_LOCALS: u64 = 50_000;

#[derive(Clone, Debug)]
pub struct Import {
    pub module: String,
    pub name: String,
    pub desc: ImportDesc,
}

#[derive(Clone, Debug)]
pub enum ImportDesc {
    Func(u32),
    Table(TableType),
    Memory(MemoryType),
    Global(GlobalType),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ExternKind {
    Func,
    Table,
    Memory,
    Global,
}

#[derive(Clone, Debug)]
pub struct Export {
    pub name: String,
    pub kind: ExternKind,
    pub index: u32,
}

/// A constant expression: decoded instructions without the final `end`.
#[derive(Clone, Debug)]
pub struct ConstExpr {
    pub ops: Vec<Op>,
    pub offset: usize,
}

#[derive(Clone, Debug)]
pub enum ElemMode {
    Passive,
    Active { table: u32, offset: ConstExpr },
    Declarative,
}

#[derive(Clone, Debug)]
pub struct ElemSegment {
    pub ty: ValType,
    pub mode: ElemMode,
    pub items: Vec<ConstExpr>,
    pub offset: usize,
}

#[derive(Clone, Debug)]
pub enum DataMode {
    Passive,
    Active { memory: u32, offset: ConstExpr },
}

#[derive(Clone, Debug)]
pub struct DataSegment {
    pub mode: DataMode,
    /// Byte range of the payload inside `ModuleData::bytes`.
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug)]
pub struct FuncBody {
    /// Non-parameter locals, expanded.
    pub locals: Vec<ValType>,
    /// Instruction bytes (up to and including the final `end`).
    pub code_start: usize,
    pub code_end: usize,
}

/// A decoded (not yet validated) module.
#[derive(Clone, Debug, Default)]
pub struct ModuleData {
    pub bytes: Vec<u8>,
    pub types: Vec<FuncType>,
    pub imports: Vec<Import>,
    /// Type index of every function, imports first.
    pub funcs: Vec<u32>,
    pub tables: Vec<TableType>,
    pub memories: Vec<MemoryType>,
    pub globals: Vec<GlobalType>,
    pub num_imported_funcs: u32,
    pub num_imported_tables: u32,
    pub num_imported_memories: u32,
    pub num_imported_globals: u32,
    /// Initialisers of the defined globals.
    pub global_inits: Vec<ConstExpr>,
    pub exports: Vec<Export>,
    pub start: Option<u32>,
    pub elems: Vec<ElemSegment>,
    pub datas: Vec<DataSegment>,
    pub data_count: Option<u32>,
    pub bodies: Vec<FuncBody>,
    /// Function names from the `name` custom section, if present and well formed.
    pub func_names: HashMap<u32, String>,
}

impl ModuleData {
    pub fn func_type(&self, func: u32) -> &FuncType {
        &self.types[self.funcs[func as usize] as usize]
    }

    pub fn num_defined_funcs(&self) -> u32 {
        self.funcs.len() as u32 - self.num_imported_funcs
    }
}

const SEC_CUSTOM: u8 = 0;
const SEC_TYPE: u8 = 1;
const SEC_IMPORT: u8 = 2;
const SEC_FUNCTION: u8 = 3;
const SEC_TABLE: u8 = 4;
const SEC_MEMORY: u8 = 5;
const SEC_GLOBAL: u8 = 6;
const SEC_EXPORT: u8 = 7;
const SEC_START: u8 = 8;
const SEC_ELEMENT: u8 = 9;
const SEC_CODE: u8 = 10;
const SEC_DATA: u8 = 11;
const SEC_DATACOUNT: u8 = 12;

/// Position of each known section in the required order.
fn section_rank(id: u8) -> u8 {
    match id {
        SEC_TYPE => 1,
        SEC_IMPORT => 2,
        SEC_FUNCTION => 3,
        SEC_TABLE => 4,
        SEC_MEMORY => 5,
        SEC_GLOBAL => 6,
        SEC_EXPORT => 7,
        SEC_START => 8,
        SEC_ELEMENT => 9,
        SEC_DATACOUNT => 10,
        SEC_CODE => 11,
        SEC_DATA => 12,
        _ => 0,
    }
}

/// A `vec` length. Callers must not trust it for allocation sizes: use [`cap`].
fn vec_len(r: &mut Reader, _min_elem_size: usize) -> Result<usize> {
    Ok(r.u32()? as usize)
}

/// A safe initial capacity for a vector of `n` elements read from `r`.
fn cap(n: usize, r: &Reader) -> usize {
    n.min(r.remaining())
}

fn limits(r: &mut Reader) -> Result<Limits> {
    let start = r.pos;
    let flags = r.u8()?;
    if flags & 0x80 != 0 {
        // The flags are a one-byte LEB128 in the reference decoder.
        return Err(Error::malformed(start, "integer representation too long"));
    }
    match flags {
        0x00 => Ok(Limits {
            min: r.u32()?,
            max: None,
        }),
        0x01 => {
            let min = r.u32()?;
            let max = r.u32()?;
            Ok(Limits {
                min,
                max: Some(max),
            })
        }
        // 0x02/0x03 are shared memories (threads proposal), 0x04+ memory64.
        _ => Err(Error::malformed(start, "integer too large")),
    }
}

fn table_type(r: &mut Reader) -> Result<TableType> {
    let elem = r.ref_type()?;
    Ok(TableType {
        elem,
        limits: limits(r)?,
    })
}

fn global_type(r: &mut Reader) -> Result<GlobalType> {
    let ty = r.val_type()?;
    let start = r.pos;
    let mutable = match r.u8()? {
        0 => false,
        1 => true,
        _ => return Err(Error::malformed(start, "malformed mutability")),
    };
    Ok(GlobalType { ty, mutable })
}

fn const_expr(r: &mut Reader) -> Result<ConstExpr> {
    let offset = r.pos;
    let mut ops = Vec::new();
    // Like the reference decoder, read past the section end and let the size check catch it.
    let mut or = OpReader::new(r.unbounded());
    loop {
        let op = or.read()?;
        if op == Op::End {
            break;
        }
        ops.push(op);
    }
    r.pos = or.pos();
    Ok(ConstExpr { ops, offset })
}

fn func_index_expr(r: &mut Reader) -> Result<ConstExpr> {
    let offset = r.pos;
    let idx = r.u32()?;
    Ok(ConstExpr {
        ops: vec![Op::RefFunc(idx)],
        offset,
    })
}

impl ModuleData {
    /// Decode a binary module. Function bodies are split out but not decoded.
    pub fn decode(bytes: &[u8]) -> Result<ModuleData> {
        let mut m = ModuleData {
            bytes: bytes.to_vec(),
            ..Default::default()
        };
        match m.decode_sections() {
            Ok(()) => Ok(m),
            Err(e) => {
                // The reference decoder decodes function bodies as it goes, so an encoding
                // error inside a body is reported before anything that follows it.
                m.check_bodies_decode()?;
                Err(e)
            }
        }
    }

    /// Decode every collected function body's instruction encoding (no type checking).
    pub fn check_bodies_decode(&self) -> Result<()> {
        for body in &self.bodies {
            let r = Reader::new(&self.bytes).sub(
                body.code_start,
                self.bytes.len(),
                "unexpected end of section or function",
            );
            let mut ops = OpReader::new(r);
            let mut kinds: Vec<bool> = vec![false]; // whether each open block is an `if`
            while !kinds.is_empty() {
                if ops.pos() > body.code_end {
                    return Err(Error::malformed(ops.pos(), "section size mismatch"));
                }
                match ops.read()? {
                    Op::Block(_) | Op::Loop(_) => kinds.push(false),
                    Op::If(_) => kinds.push(true),
                    Op::Else if !kinds.last().copied().unwrap_or(false) => {
                        return Err(Error::malformed(ops.pos(), "END opcode expected"));
                    }
                    Op::Else => *kinds.last_mut().unwrap() = false,
                    Op::End => {
                        kinds.pop();
                    }
                    Op::MemoryInit(_) | Op::DataDrop(_) if self.data_count.is_none() => {
                        return Err(Error::malformed(ops.pos(), "data count section required"));
                    }
                    _ => {}
                }
            }
            if ops.pos() != body.code_end {
                return Err(Error::malformed(ops.pos(), "section size mismatch"));
            }
        }
        Ok(())
    }

    fn decode_sections(&mut self) -> Result<()> {
        let m = self;
        let bytes: &[u8] = &m.bytes.clone();
        let mut r = Reader::new(bytes);
        if r.bytes(4)? != b"\0asm" {
            return Err(Error::malformed(0, "magic header not detected"));
        }
        if r.u32_fixed()? != 1 {
            return Err(Error::malformed(4, "unknown binary version"));
        }
        let mut last_rank = 0u8;
        let mut func_count: Option<usize> = None;
        let mut code_count: Option<usize> = None;
        let mut data_seen = false;
        while !r.eof() {
            let id_pos = r.pos;
            let id = r.u8()?;
            let size = r.u32()? as usize;
            if size > r.remaining() {
                return Err(Error::malformed(r.pos, "length out of bounds"));
            }
            let end = r.pos + size;
            let mut s = r.sub(r.pos, end, "unexpected end of section or function");
            if id > SEC_DATACOUNT {
                return Err(Error::malformed(id_pos, "malformed section id"));
            }
            if id != SEC_CUSTOM {
                let rank = section_rank(id);
                if rank <= last_rank {
                    return Err(Error::malformed(
                        id_pos,
                        "unexpected content after last section",
                    ));
                }
                last_rank = rank;
            }
            match id {
                SEC_CUSTOM => m.custom_section(&mut s)?,
                SEC_TYPE => {
                    let n = vec_len(&mut s, 3)?;
                    for _ in 0..n {
                        let p = s.pos;
                        if s.u8()? != 0x60 {
                            return Err(Error::malformed(p, "integer representation too long"));
                        }
                        let np = vec_len(&mut s, 1)?;
                        let mut params = Vec::with_capacity(cap(np, &s));
                        for _ in 0..np {
                            params.push(s.val_type()?);
                        }
                        let nr = vec_len(&mut s, 1)?;
                        let mut results = Vec::with_capacity(cap(nr, &s));
                        for _ in 0..nr {
                            results.push(s.val_type()?);
                        }
                        m.types.push(FuncType::new(params, results));
                    }
                }
                SEC_IMPORT => {
                    let n = vec_len(&mut s, 4)?;
                    for _ in 0..n {
                        let module = s.name()?;
                        let name = s.name()?;
                        let kp = s.pos;
                        let desc = match s.u8()? {
                            0x00 => ImportDesc::Func(s.u32()?),
                            0x01 => ImportDesc::Table(table_type(&mut s)?),
                            0x02 => ImportDesc::Memory(MemoryType {
                                limits: limits(&mut s)?,
                            }),
                            0x03 => ImportDesc::Global(global_type(&mut s)?),
                            _ => return Err(Error::malformed(kp, "malformed import kind")),
                        };
                        match &desc {
                            ImportDesc::Func(t) => {
                                m.funcs.push(*t);
                                m.num_imported_funcs += 1;
                            }
                            ImportDesc::Table(t) => {
                                m.tables.push(*t);
                                m.num_imported_tables += 1;
                            }
                            ImportDesc::Memory(t) => {
                                m.memories.push(*t);
                                m.num_imported_memories += 1;
                            }
                            ImportDesc::Global(t) => {
                                m.globals.push(*t);
                                m.num_imported_globals += 1;
                            }
                        }
                        m.imports.push(Import { module, name, desc });
                    }
                }
                SEC_FUNCTION => {
                    let n = vec_len(&mut s, 1)?;
                    for _ in 0..n {
                        m.funcs.push(s.u32()?);
                    }
                    func_count = Some(n);
                }
                SEC_TABLE => {
                    let n = vec_len(&mut s, 3)?;
                    for _ in 0..n {
                        m.tables.push(table_type(&mut s)?);
                    }
                }
                SEC_MEMORY => {
                    let n = vec_len(&mut s, 2)?;
                    for _ in 0..n {
                        m.memories.push(MemoryType {
                            limits: limits(&mut s)?,
                        });
                    }
                }
                SEC_GLOBAL => {
                    let n = vec_len(&mut s, 3)?;
                    for _ in 0..n {
                        let gt = global_type(&mut s)?;
                        let init = const_expr(&mut s)?;
                        m.globals.push(gt);
                        m.global_inits.push(init);
                    }
                }
                SEC_EXPORT => {
                    let n = vec_len(&mut s, 3)?;
                    for _ in 0..n {
                        let name = s.name()?;
                        let kp = s.pos;
                        let kind = match s.u8()? {
                            0 => ExternKind::Func,
                            1 => ExternKind::Table,
                            2 => ExternKind::Memory,
                            3 => ExternKind::Global,
                            _ => return Err(Error::malformed(kp, "malformed export kind")),
                        };
                        let index = s.u32()?;
                        m.exports.push(Export { name, kind, index });
                    }
                }
                SEC_START => m.start = Some(s.u32()?),
                SEC_ELEMENT => {
                    let n = vec_len(&mut s, 1)?;
                    for _ in 0..n {
                        let seg = elem_segment(&mut s)?;
                        m.elems.push(seg);
                    }
                }
                SEC_DATACOUNT => m.data_count = Some(s.u32()?),
                SEC_CODE => {
                    let n = vec_len(&mut s, 1)?;
                    code_count = Some(n);
                    for _ in 0..n {
                        let size = s.u32()? as usize;
                        if size > s.remaining() {
                            return Err(Error::malformed(
                                s.pos,
                                "unexpected end of section or function",
                            ));
                        }
                        let body_end = s.pos + size;
                        let mut b = s.sub(s.pos, body_end, "unexpected end of section or function");
                        let groups = vec_len(&mut b, 2)?;
                        let mut locals = Vec::new();
                        let mut total: u64 = 0;
                        for _ in 0..groups {
                            let count = b.u32()? as u64;
                            total += count;
                            if total > MAX_LOCALS {
                                return Err(Error::malformed(b.pos, "too many locals"));
                            }
                            let t = b.val_type()?;
                            locals.extend(std::iter::repeat_n(t, count as usize));
                        }
                        m.bodies.push(FuncBody {
                            locals,
                            code_start: b.pos,
                            code_end: body_end,
                        });
                        s.pos = body_end;
                    }
                }
                SEC_DATA => {
                    data_seen = true;
                    let n = vec_len(&mut s, 1)?;
                    if let Some(c) = m.data_count
                        && c as usize != n
                    {
                        return Err(Error::malformed(
                            s.pos,
                            "data count and data section have inconsistent lengths",
                        ));
                    }
                    for _ in 0..n {
                        let fp = s.pos;
                        let flags = s.u32()?;
                        let mode = match flags {
                            0 => DataMode::Active {
                                memory: 0,
                                offset: const_expr(&mut s)?,
                            },
                            1 => DataMode::Passive,
                            2 => {
                                let memory = s.u32()?;
                                DataMode::Active {
                                    memory,
                                    offset: const_expr(&mut s)?,
                                }
                            }
                            _ => return Err(Error::malformed(fp, "malformed data segment flags")),
                        };
                        let len = s.u32()? as usize;
                        if len > s.remaining() {
                            return Err(Error::malformed(
                                s.pos,
                                "unexpected end of section or function",
                            ));
                        }
                        let start = s.pos;
                        s.pos += len;
                        m.datas.push(DataSegment {
                            mode,
                            start,
                            end: start + len,
                        });
                    }
                }
                _ => unreachable!(),
            }
            if s.pos != end {
                return Err(Error::malformed(s.pos, "section size mismatch"));
            }
            r.pos = end;
        }
        if code_count.unwrap_or(0) != func_count.unwrap_or(0) {
            return Err(Error::malformed(
                r.pos,
                "function and code section have inconsistent lengths",
            ));
        }
        if m.data_count.unwrap_or(0) != 0 && !data_seen {
            return Err(Error::malformed(
                r.pos,
                "data count and data section have inconsistent lengths",
            ));
        }
        Ok(())
    }

    fn custom_section(&mut self, s: &mut Reader) -> Result<()> {
        let name = s.name()?;
        if name == "name" {
            // Best effort: a malformed name section is ignored, as the spec requires.
            let mut sub = s.sub(s.pos, s.end, "unexpected end");
            let _ = self.name_section(&mut sub);
        }
        s.pos = s.end;
        Ok(())
    }

    fn name_section(&mut self, s: &mut Reader) -> Result<()> {
        while !s.eof() {
            let id = s.u8()?;
            let size = s.u32()? as usize;
            if size > s.remaining() {
                return s.err("bad name subsection");
            }
            let end = s.pos + size;
            if id == 1 {
                let mut t = s.sub(s.pos, end, "unexpected end");
                let n = t.u32()?;
                for _ in 0..n {
                    let idx = t.u32()?;
                    let name = t.name()?;
                    self.func_names.insert(idx, name);
                }
            }
            s.pos = end;
        }
        Ok(())
    }
}

fn elem_kind(r: &mut Reader) -> Result<ValType> {
    let p = r.pos;
    match r.u8()? {
        0x00 => Ok(ValType::FuncRef),
        _ => Err(Error::malformed(p, "malformed element kind")),
    }
}

fn elem_segment(s: &mut Reader) -> Result<ElemSegment> {
    let offset = s.pos;
    let flags = s.u32()?;
    if flags > 7 {
        return Err(Error::malformed(offset, "malformed elements segment kind"));
    }
    let passive_or_decl = flags & 1 != 0;
    let explicit_table = flags & 2 != 0;
    let uses_exprs = flags & 4 != 0;
    let mode = if !passive_or_decl {
        let table = if explicit_table { s.u32()? } else { 0 };
        ElemMode::Active {
            table,
            offset: const_expr(s)?,
        }
    } else if explicit_table {
        ElemMode::Declarative
    } else {
        ElemMode::Passive
    };
    // flags 0 and 4 have no element kind / type byte.
    let ty = if flags == 0 || flags == 4 {
        ValType::FuncRef
    } else if uses_exprs {
        s.ref_type()?
    } else {
        elem_kind(s)?
    };
    let n = vec_len(s, 1)?;
    let mut items = Vec::with_capacity(cap(n, s));
    for _ in 0..n {
        items.push(if uses_exprs {
            const_expr(s)?
        } else {
            func_index_expr(s)?
        });
    }
    Ok(ElemSegment {
        ty,
        mode,
        items,
        offset,
    })
}
