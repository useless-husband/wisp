//! The store: owner of every function, table, memory, global and instance.

use super::values::*;
use super::vm::*;
use crate::config::Config;
use crate::error::{Error, Result, Trap, TrapCode};
use crate::module::ModuleInner;
use crate::types::*;
use std::any::Any;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// Signature of a host function as stored: the store, the calling instance (if called from
/// wasm), arguments and results.
pub(crate) type HostFn =
    Rc<dyn Fn(&mut StoreInner, Option<u32>, &[Val], &mut [Val]) -> Result<(), Trap>>;

pub(crate) enum FuncKind {
    Wasm { instance: u32 },
    Host(HostFn),
}

pub(crate) struct FuncInst {
    pub ty: FuncType,
    pub kind: FuncKind,
    pub vmref: Box<VmFuncRef>,
}

/// A global's storage cell. `value` is first so `*mut GlobalCell` is a `*mut u64`.
#[repr(C)]
pub(crate) struct GlobalCell {
    pub value: u64,
    pub ty: GlobalType,
}

pub(crate) struct InstanceData {
    pub module: Arc<ModuleInner>,
    pub vmctx: Box<VmCtx>,
    pub funcs: Vec<u32>,
    pub tables: Vec<u32>,
    pub memories: Vec<u32>,
    pub globals: Vec<u32>,
    // Arrays the VmCtx points into; never resized after creation.
    _func_ptrs: Vec<*const VmFuncRef>,
    _global_ptrs: Vec<*mut u64>,
    _table_ptrs: Vec<*mut VmTable>,
    _type_ids: Vec<u32>,
    /// Element segment contents; emptied by `elem.drop`.
    pub elems: Vec<Vec<u64>>,
    /// Data segment byte ranges in the module; emptied by `data.drop`.
    pub datas: Vec<(usize, usize)>,
    pub exports: HashMap<String, Extern>,
}

pub struct StoreInner {
    pub(crate) config: Config,
    pub(crate) funcs: Vec<FuncInst>,
    pub(crate) tables: Vec<Box<VmTable>>,
    pub(crate) memories: Vec<Box<VmMemory>>,
    pub(crate) globals: Vec<Box<GlobalCell>>,
    pub(crate) instances: Vec<InstanceData>,
    types: Vec<FuncType>,
    type_map: HashMap<FuncType, u32>,
    pub(crate) runtime: Box<VmRuntime>,
    /// The interpreter's value stack and the first free slot of it.
    pub(crate) istack: Vec<u64>,
    pub(crate) istack_top: usize,
    /// Current wasm call depth (interpreted frames plus nested entries).
    pub(crate) depth: u32,
    pub(crate) max_depth: u32,
    /// Native calls in progress (host -> wasm -> host ...), for the compiled-code stack limit.
    pub(crate) native_entries: u32,
    pub(crate) data: Box<dyn Any>,
    /// A trap raised inside a host call made from compiled code, delivered on unwind.
    pub(crate) pending_trap: Option<Trap>,
}

impl StoreInner {
    pub(crate) fn new(config: Config, data: Box<dyn Any>) -> Box<StoreInner> {
        let slots = config.interp_stack_slots;
        let max_depth = config.max_call_depth;
        let mut s = Box::new(StoreInner {
            config,
            funcs: Vec::new(),
            tables: Vec::new(),
            memories: Vec::new(),
            globals: Vec::new(),
            instances: Vec::new(),
            types: Vec::new(),
            type_map: HashMap::new(),
            runtime: Box::new(VmRuntime {
                stack_limit: 0,
                fuel: i64::MAX,
                entry_sp: 0,
                trap: 0,
                _pad: 0,
                store: std::ptr::null_mut(),
            }),
            istack: vec![0u64; slots],
            istack_top: 0,
            depth: 0,
            max_depth,
            native_entries: 0,
            data,
            pending_trap: None,
        });
        let p: *mut StoreInner = &mut *s;
        s.runtime.store = p;
        s
    }

    pub(crate) fn canonical_type(&mut self, t: &FuncType) -> u32 {
        if let Some(&id) = self.type_map.get(t) {
            return id;
        }
        let id = self.types.len() as u32;
        self.types.push(t.clone());
        self.type_map.insert(t.clone(), id);
        id
    }

    // ---- allocation ----

    pub(crate) fn alloc_host_func(&mut self, ty: FuncType, f: HostFn) -> Func {
        let type_id = self.canonical_type(&ty);
        let addr = self.funcs.len() as u32;
        let vmref = Box::new(VmFuncRef {
            code: crate::jit::host_trampoline(),
            vmctx: std::ptr::null_mut(),
            type_id,
            func_addr: addr,
            def_index: 0,
            kind: KIND_HOST,
        });
        self.funcs.push(FuncInst {
            ty,
            kind: FuncKind::Host(f),
            vmref,
        });
        Func(addr)
    }

    pub(crate) fn alloc_table(&mut self, ty: TableType, init: u64) -> Table {
        self.tables.push(VmTable::new(ty, init));
        Table(self.tables.len() as u32 - 1)
    }

    pub(crate) fn alloc_memory(&mut self, ty: MemoryType) -> Result<Memory> {
        let m = VmMemory::new(ty).map_err(Error::Link)?;
        self.memories.push(m);
        Ok(Memory(self.memories.len() as u32 - 1))
    }

    pub(crate) fn alloc_global(&mut self, ty: GlobalType, value: u64) -> Global {
        self.globals.push(Box::new(GlobalCell { value, ty }));
        Global(self.globals.len() as u32 - 1)
    }

    // ---- value conversion ----

    pub(crate) fn val_to_raw(&self, v: &Val) -> u64 {
        match *v {
            Val::I32(x) => x as u32 as u64,
            Val::I64(x) => x as u64,
            Val::F32(x) => x as u64,
            Val::F64(x) => x,
            Val::FuncRef(None) | Val::ExternRef(None) => 0,
            Val::FuncRef(Some(f)) => &*self.funcs[f.0 as usize].vmref as *const VmFuncRef as u64,
            Val::ExternRef(Some(x)) => x as u64 + 1,
        }
    }

    pub(crate) fn raw_to_val(&self, raw: u64, ty: ValType) -> Val {
        match ty {
            ValType::I32 => Val::I32(raw as u32 as i32),
            ValType::I64 => Val::I64(raw as i64),
            ValType::F32 => Val::F32(raw as u32),
            ValType::F64 => Val::F64(raw),
            ValType::FuncRef => {
                if raw == 0 {
                    Val::FuncRef(None)
                } else {
                    let fr = raw as *const VmFuncRef;
                    Val::FuncRef(Some(Func(unsafe { (*fr).func_addr })))
                }
            }
            ValType::ExternRef => {
                if raw == 0 {
                    Val::ExternRef(None)
                } else {
                    Val::ExternRef(Some((raw - 1) as u32))
                }
            }
            ValType::V128 => unreachable!("v128 is rejected at validation"),
        }
    }

    // ---- calls ----

    /// Call a function from the host.
    pub(crate) fn invoke(&mut self, f: Func, args: &[Val]) -> Result<Vec<Val>> {
        let fi = self
            .funcs
            .get(f.0 as usize)
            .ok_or_else(|| Error::Api("unknown function".into()))?;
        let ty = fi.ty.clone();
        if args.len() != ty.params.len() {
            return Err(Error::Api(format!(
                "function expects {} arguments, got {}",
                ty.params.len(),
                args.len()
            )));
        }
        for (a, t) in args.iter().zip(ty.params.iter()) {
            if a.ty() != *t {
                return Err(Error::Api(format!(
                    "argument type mismatch: expected {t}, got {}",
                    a.ty()
                )));
            }
        }
        let n = ty.params.len().max(ty.results.len());
        let mut raw = vec![0u64; n];
        for (i, a) in args.iter().enumerate() {
            raw[i] = self.val_to_raw(a);
        }
        let p: *mut StoreInner = self;
        self.runtime.store = p;
        let fr: *const VmFuncRef = &*self.funcs[f.0 as usize].vmref;
        unsafe { self.call_raw(fr, raw.as_mut_ptr(), None) }.map_err(Error::Trap)?;
        Ok(ty
            .results
            .iter()
            .enumerate()
            .map(|(i, t)| self.raw_to_val(raw[i], *t))
            .collect())
    }

    /// Call through a function reference with arguments/results in `args`.
    ///
    /// # Safety
    /// `args` must have room for `max(params, results)` values.
    pub(crate) unsafe fn call_raw(
        &mut self,
        fr: *const VmFuncRef,
        args: *mut u64,
        caller: Option<u32>,
    ) -> Result<(), Trap> {
        unsafe {
            match (*fr).kind {
                KIND_INTERP => {
                    let ty = &self.funcs[(*fr).func_addr as usize].ty;
                    let (np, nr) = (ty.params.len(), ty.results.len());
                    let base = self.istack_top;
                    if base + np.max(nr) > self.istack.len() {
                        return Err(TrapCode::StackExhausted.into());
                    }
                    let fp = self.istack.as_mut_ptr().add(base);
                    std::ptr::copy_nonoverlapping(args, fp, np);
                    let store: *mut StoreInner = self;
                    crate::interp::exec::execute(store, fr, fp)?;
                    std::ptr::copy_nonoverlapping(fp, args, nr);
                    Ok(())
                }
                KIND_HOST => self.call_host((*fr).func_addr, args, caller),
                _ => crate::jit::call_compiled(self, fr, args),
            }
        }
    }

    /// A call from the interpreter to a host or compiled function. `top` is the first value
    /// stack slot not used by the calling frame.
    ///
    /// # Safety
    /// As for [`call_raw`](Self::call_raw).
    pub(crate) unsafe fn call_out(
        &mut self,
        fr: *const VmFuncRef,
        args: *mut u64,
        caller: *mut VmCtx,
        top: usize,
    ) -> Result<(), Trap> {
        let saved = self.istack_top;
        self.istack_top = top;
        let caller = if caller.is_null() {
            None
        } else {
            Some(unsafe { (*caller).instance })
        };
        let r = unsafe { self.call_raw(fr, args, caller) };
        self.istack_top = saved;
        r
    }

    /// # Safety
    /// `args` must have room for `max(params, results)` values.
    pub(crate) unsafe fn call_host(
        &mut self,
        addr: u32,
        args: *mut u64,
        caller: Option<u32>,
    ) -> Result<(), Trap> {
        let fi = &self.funcs[addr as usize];
        let FuncKind::Host(f) = &fi.kind else {
            unreachable!()
        };
        let f = f.clone();
        let ty = fi.ty.clone();
        let params: Vec<Val> = ty
            .params
            .iter()
            .enumerate()
            .map(|(i, t)| self.raw_to_val(unsafe { *args.add(i) }, *t))
            .collect();
        let mut results: Vec<Val> = ty.results.iter().map(|t| Val::default_for(*t)).collect();
        if self.depth >= self.max_depth {
            return Err(TrapCode::StackExhausted.into());
        }
        self.depth += 1;
        let r = f(self, caller, &params, &mut results);
        self.depth -= 1;
        r?;
        for (i, (v, t)) in results.iter().zip(ty.results.iter()).enumerate() {
            if v.ty() != *t {
                return Err(Trap::host(format!(
                    "host function returned {} where {t} was expected",
                    v.ty()
                )));
            }
            unsafe { *args.add(i) = self.val_to_raw(v) };
        }
        Ok(())
    }

    // ---- segment operations (slow paths shared by both engines) ----

    pub(crate) fn instance_of(&mut self, vmctx: *mut VmCtx) -> &mut InstanceData {
        let i = unsafe { (*vmctx).instance };
        &mut self.instances[i as usize]
    }

    pub(crate) fn memory_init(
        &mut self,
        vmctx: *mut VmCtx,
        seg: u32,
        dst: u32,
        src: u32,
        n: u32,
    ) -> Result<(), TrapCode> {
        let inst = self.instance_of(vmctx);
        let (s, e) = inst.datas[seg as usize];
        let module = inst.module.clone();
        let data = &module.data.bytes[s..e];
        let mem = unsafe { &mut *(*vmctx).memory };
        let (src, dst, n) = (src as u64, dst as u64, n as u64);
        if src + n > data.len() as u64 || dst + n > mem.size {
            return Err(TrapCode::MemoryOutOfBounds);
        }
        mem.as_mut_slice()[dst as usize..(dst + n) as usize]
            .copy_from_slice(&data[src as usize..(src + n) as usize]);
        Ok(())
    }

    pub(crate) fn data_drop(&mut self, vmctx: *mut VmCtx, seg: u32) {
        self.instance_of(vmctx).datas[seg as usize] = (0, 0);
    }

    pub(crate) fn table_init(
        &mut self,
        vmctx: *mut VmCtx,
        table: u32,
        seg: u32,
        dst: u32,
        src: u32,
        n: u32,
    ) -> Result<(), TrapCode> {
        let tab = unsafe { &mut **(*vmctx).tables.add(table as usize) };
        let inst = self.instance_of(vmctx);
        let elems = &inst.elems[seg as usize];
        let (src, dst, n) = (src as u64, dst as u64, n as u64);
        if src + n > elems.len() as u64 || dst + n > tab.size() as u64 {
            return Err(TrapCode::TableOutOfBounds);
        }
        tab.slice_mut()[dst as usize..(dst + n) as usize]
            .copy_from_slice(&elems[src as usize..(src + n) as usize]);
        Ok(())
    }

    pub(crate) fn elem_drop(&mut self, vmctx: *mut VmCtx, seg: u32) {
        self.instance_of(vmctx).elems[seg as usize] = Vec::new();
    }

    // ---- instance access ----

    pub(crate) fn export(&self, inst: Instance, name: &str) -> Option<Extern> {
        self.instances
            .get(inst.0 as usize)?
            .exports
            .get(name)
            .copied()
    }

    pub(crate) fn instance_memory(&self, inst: u32) -> Option<Memory> {
        let d = self.instances.get(inst as usize)?;
        if let Some(Extern::Memory(m)) = d.exports.get("memory") {
            return Some(*m);
        }
        d.memories.first().map(|&m| Memory(m))
    }

    pub(crate) fn extern_type(&self, e: Extern) -> ExternType {
        match e {
            Extern::Func(f) => ExternType::Func(self.funcs[f.0 as usize].ty.clone()),
            Extern::Table(t) => ExternType::Table(self.tables[t.0 as usize].ty),
            Extern::Memory(m) => ExternType::Memory(self.memories[m.0 as usize].current_type()),
            Extern::Global(g) => ExternType::Global(self.globals[g.0 as usize].ty),
        }
    }

    // ---- instantiation ----

    pub(crate) fn instantiate(
        &mut self,
        module: &Arc<ModuleInner>,
        imports: &[Extern],
    ) -> Result<Instance> {
        use crate::binary::module::{DataMode, ElemMode, ImportDesc};
        let m = &module.data;
        if imports.len() != m.imports.len() {
            return Err(Error::Link(format!(
                "expected {} imports, got {}",
                m.imports.len(),
                imports.len()
            )));
        }
        let mut funcs = Vec::new();
        let mut tables = Vec::new();
        let mut memories = Vec::new();
        let mut globals = Vec::new();
        for (imp, ext) in m.imports.iter().zip(imports) {
            let bad = || {
                Error::Link(format!(
                    "incompatible import type for \"{}\" \"{}\"",
                    imp.module, imp.name
                ))
            };
            match (&imp.desc, ext) {
                (ImportDesc::Func(t), Extern::Func(f)) => {
                    let want = &m.types[*t as usize];
                    let have = self.funcs.get(f.0 as usize).ok_or_else(bad)?;
                    if &have.ty != want {
                        return Err(bad());
                    }
                    funcs.push(f.0);
                }
                (ImportDesc::Table(t), Extern::Table(x)) => {
                    let have = self.tables.get(x.0 as usize).ok_or_else(bad)?.ty;
                    if have.elem != t.elem || !have.limits.matches(&t.limits) {
                        return Err(bad());
                    }
                    tables.push(x.0);
                }
                (ImportDesc::Memory(t), Extern::Memory(x)) => {
                    let have = self
                        .memories
                        .get(x.0 as usize)
                        .ok_or_else(bad)?
                        .current_type();
                    if !have.limits.matches(&t.limits) {
                        return Err(bad());
                    }
                    memories.push(x.0);
                }
                (ImportDesc::Global(t), Extern::Global(x)) => {
                    let have = self.globals.get(x.0 as usize).ok_or_else(bad)?.ty;
                    if have != *t {
                        return Err(bad());
                    }
                    globals.push(x.0);
                }
                _ => return Err(bad()),
            }
        }

        let inst_idx = self.instances.len() as u32;
        let type_ids: Vec<u32> = m.types.iter().map(|t| self.canonical_type(t)).collect();
        // Defined functions.
        for (def, &tidx) in m.funcs[m.num_imported_funcs as usize..].iter().enumerate() {
            let ty = m.types[tidx as usize].clone();
            let (code, kind) = module.func_entry(def as u32);
            let addr = self.funcs.len() as u32;
            let vmref = Box::new(VmFuncRef {
                code,
                vmctx: std::ptr::null_mut(),
                type_id: type_ids[tidx as usize],
                func_addr: addr,
                def_index: def as u32,
                kind,
            });
            self.funcs.push(FuncInst {
                ty,
                kind: FuncKind::Wasm { instance: inst_idx },
                vmref,
            });
            funcs.push(addr);
        }
        for t in &m.tables[m.num_imported_tables as usize..] {
            tables.push(self.alloc_table(*t, 0).0);
        }
        for t in &m.memories[m.num_imported_memories as usize..] {
            memories.push(self.alloc_memory(*t)?.0);
        }
        let func_ptrs: Vec<*const VmFuncRef> = funcs
            .iter()
            .map(|&f| &*self.funcs[f as usize].vmref as *const VmFuncRef)
            .collect();
        // Globals: initialisers may read imported globals and take function references.
        for (i, init) in m.global_inits.iter().enumerate() {
            let ty = m.globals[m.num_imported_globals as usize + i];
            let v = self.eval_const(&init.ops, &globals, &func_ptrs);
            globals.push(self.alloc_global(ty, v).0);
        }
        let mut table_ptrs: Vec<*mut VmTable> = tables
            .iter()
            .map(|&t| &mut *self.tables[t as usize] as *mut VmTable)
            .collect();
        let mut global_ptrs: Vec<*mut u64> = globals
            .iter()
            .map(|&g| &mut self.globals[g as usize].value as *mut u64)
            .collect();
        let memory_ptr: *mut VmMemory = match memories.first() {
            Some(&mi) => &mut *self.memories[mi as usize],
            None => std::ptr::null_mut(),
        };
        let mut vmctx = Box::new(VmCtx {
            memory: memory_ptr,
            runtime: &mut *self.runtime,
            funcs: func_ptrs.as_ptr(),
            globals: global_ptrs.as_mut_ptr(),
            tables: table_ptrs.as_mut_ptr(),
            type_ids: type_ids.as_ptr(),
            interp: &module.interp as *const _ as *const u8,
            instance: inst_idx,
            _pad: 0,
        });
        let vmctx_ptr: *mut VmCtx = &mut *vmctx;
        for &f in &funcs[m.num_imported_funcs as usize..] {
            self.funcs[f as usize].vmref.vmctx = vmctx_ptr;
        }
        let mut exports = HashMap::new();
        for e in &m.exports {
            use crate::binary::module::ExternKind;
            let x = match e.kind {
                ExternKind::Func => Extern::Func(Func(funcs[e.index as usize])),
                ExternKind::Table => Extern::Table(Table(tables[e.index as usize])),
                ExternKind::Memory => Extern::Memory(Memory(memories[e.index as usize])),
                ExternKind::Global => Extern::Global(Global(globals[e.index as usize])),
            };
            exports.insert(e.name.clone(), x);
        }
        let elems: Vec<Vec<u64>> = m
            .elems
            .iter()
            .map(|seg| {
                seg.items
                    .iter()
                    .map(|it| self.eval_const(&it.ops, &globals, &func_ptrs))
                    .collect()
            })
            .collect();
        let datas: Vec<(usize, usize)> = m.datas.iter().map(|d| (d.start, d.end)).collect();
        self.instances.push(InstanceData {
            module: module.clone(),
            vmctx,
            funcs,
            tables,
            memories,
            globals,
            _func_ptrs: func_ptrs,
            _global_ptrs: global_ptrs,
            _table_ptrs: table_ptrs,
            _type_ids: type_ids,
            elems,
            datas,
            exports,
        });

        // Active segments, in order; a trap leaves earlier writes in place (spec 2.0).
        let globals = self.instances[inst_idx as usize].globals.clone();
        for (i, seg) in m.elems.iter().enumerate() {
            match &seg.mode {
                ElemMode::Active { table, offset } => {
                    let off = self.eval_const(&offset.ops, &globals, &[]) as u32;
                    let n = seg.items.len() as u32;
                    self.table_init(vmctx_ptr, *table, i as u32, off, 0, n)?;
                    self.elem_drop(vmctx_ptr, i as u32);
                }
                ElemMode::Declarative => self.elem_drop(vmctx_ptr, i as u32),
                ElemMode::Passive => {}
            }
        }
        for (i, d) in m.datas.iter().enumerate() {
            if let DataMode::Active { offset, .. } = &d.mode {
                let off = self.eval_const(&offset.ops, &globals, &[]) as u32;
                let n = (d.end - d.start) as u32;
                self.memory_init(vmctx_ptr, i as u32, off, 0, n)?;
                self.data_drop(vmctx_ptr, i as u32);
            }
        }
        if let Some(start) = m.start {
            let f = Func(self.instances[inst_idx as usize].funcs[start as usize]);
            self.invoke(f, &[])?;
        }
        Ok(Instance(inst_idx))
    }

    /// Evaluate a validated constant expression to a raw value.
    fn eval_const(
        &self,
        ops: &[crate::binary::ops::Op],
        globals: &[u32],
        funcs: &[*const VmFuncRef],
    ) -> u64 {
        use crate::binary::ops::Op;
        let mut stack: Vec<u64> = Vec::with_capacity(2);
        for op in ops {
            let v = match op {
                Op::I32Const(v) => *v as u32 as u64,
                Op::I64Const(v) => *v as u64,
                Op::F32Const(v) => *v as u64,
                Op::F64Const(v) => *v,
                Op::RefNull(_) => 0,
                Op::RefFunc(f) => funcs[*f as usize] as u64,
                Op::GlobalGet(g) => self.globals[globals[*g as usize] as usize].value,
                _ => unreachable!("validated constant expression"),
            };
            stack.push(v);
        }
        stack.pop().unwrap_or(0)
    }
}
