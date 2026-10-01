//! The public embedding API: stores, host functions, linking and access to instances.

use super::store::{HostFn, StoreInner};
use super::values::*;
use crate::config::Engine;
use crate::error::{Error, Result, Trap};
use crate::module::Module;
use crate::types::*;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;

/// Owns all runtime objects. `T` is user data reachable from host functions.
pub struct Store<T> {
    pub(crate) inner: Box<StoreInner>,
    _data: PhantomData<T>,
}

impl<T: 'static> Store<T> {
    pub fn new(engine: &Engine, data: T) -> Store<T> {
        Store { inner: StoreInner::new(engine.config.clone(), Box::new(data)), _data: PhantomData }
    }

    pub fn data(&self) -> &T {
        self.inner.data.downcast_ref::<T>().expect("store data type")
    }

    pub fn data_mut(&mut self) -> &mut T {
        self.inner.data.downcast_mut::<T>().expect("store data type")
    }

    /// Set the remaining fuel (only metered if the engine was built with `fuel(true)`).
    pub fn set_fuel(&mut self, fuel: u64) {
        self.inner.runtime.fuel = fuel.min(i64::MAX as u64) as i64;
    }

    pub fn fuel(&self) -> u64 {
        self.inner.runtime.fuel.max(0) as u64
    }
}

/// The context passed to host functions.
pub struct Caller<'a, T> {
    pub(crate) store: &'a mut StoreInner,
    pub(crate) instance: Option<u32>,
    _data: PhantomData<T>,
}

impl<'a, T: 'static> Caller<'a, T> {
    pub fn data(&self) -> &T {
        self.store.data.downcast_ref::<T>().expect("store data type")
    }

    pub fn data_mut(&mut self) -> &mut T {
        self.store.data.downcast_mut::<T>().expect("store data type")
    }

    /// An export of the calling instance.
    pub fn get_export(&self, name: &str) -> Option<Extern> {
        self.store.export(Instance(self.instance?), name)
    }

    /// The calling instance's memory (its export named "memory", or its memory 0).
    pub fn memory(&self) -> Option<Memory> {
        self.store.instance_memory(self.instance?)
    }

    /// The calling instance's memory bytes together with the store data.
    pub fn memory_and_data(&mut self) -> (&mut [u8], &mut T) {
        let mem: &mut [u8] = match self.memory() {
            Some(m) => self.store.memories[m.0 as usize].as_mut_slice(),
            None => &mut [],
        };
        let data = self.store.data.downcast_mut::<T>().expect("store data type");
        (mem, data)
    }

    /// Call another function (re-entering wasm from a host function is allowed).
    pub fn call(&mut self, f: Func, args: &[Val]) -> Result<Vec<Val>> {
        self.store.invoke(f, args)
    }
}

impl Func {
    /// Define a host function.
    pub fn new<T: 'static>(
        store: &mut Store<T>,
        ty: FuncType,
        f: impl Fn(Caller<'_, T>, &[Val], &mut [Val]) -> Result<(), Trap> + 'static,
    ) -> Func {
        let wrapped: HostFn = Rc::new(move |s: &mut StoreInner, inst, args, results| {
            f(Caller { store: s, instance: inst, _data: PhantomData }, args, results)
        });
        store.inner.alloc_host_func(ty, wrapped)
    }

    /// Call the function.
    pub fn call<T: 'static>(&self, store: &mut Store<T>, args: &[Val]) -> Result<Vec<Val>> {
        store.inner.invoke(*self, args)
    }

    pub fn ty<T>(&self, store: &Store<T>) -> FuncType {
        store.inner.funcs[self.0 as usize].ty.clone()
    }
}

impl Memory {
    pub fn new<T>(store: &mut Store<T>, ty: MemoryType) -> Result<Memory> {
        store.inner.alloc_memory(ty)
    }

    pub fn data<'a, T>(&self, store: &'a Store<T>) -> &'a [u8] {
        store.inner.memories[self.0 as usize].as_slice()
    }

    pub fn data_mut<'a, T>(&self, store: &'a mut Store<T>) -> &'a mut [u8] {
        store.inner.memories[self.0 as usize].as_mut_slice()
    }

    /// Size in pages.
    pub fn size<T>(&self, store: &Store<T>) -> u32 {
        store.inner.memories[self.0 as usize].pages()
    }

    /// Grow by `delta` pages; returns the old size.
    pub fn grow<T>(&self, store: &mut Store<T>, delta: u32) -> Result<u32> {
        let r = store.inner.memories[self.0 as usize].grow(delta);
        if r < 0 { Err(Error::Api("memory.grow failed".into())) } else { Ok(r as u32) }
    }

    pub fn read<T>(&self, store: &Store<T>, offset: usize, buf: &mut [u8]) -> Result<()> {
        let d = self.data(store);
        let end = offset.checked_add(buf.len()).filter(|&e| e <= d.len());
        let end = end.ok_or_else(|| Error::Api("out of bounds memory read".into()))?;
        buf.copy_from_slice(&d[offset..end]);
        Ok(())
    }

    pub fn write<T>(&self, store: &mut Store<T>, offset: usize, buf: &[u8]) -> Result<()> {
        let d = self.data_mut(store);
        let end = offset.checked_add(buf.len()).filter(|&e| e <= d.len());
        let end = end.ok_or_else(|| Error::Api("out of bounds memory write".into()))?;
        d[offset..end].copy_from_slice(buf);
        Ok(())
    }
}

impl Global {
    pub fn new<T>(store: &mut Store<T>, ty: GlobalType, v: Val) -> Result<Global> {
        if v.ty() != ty.ty {
            return Err(Error::Api("global initial value has the wrong type".into()));
        }
        let raw = store.inner.val_to_raw(&v);
        Ok(store.inner.alloc_global(ty, raw))
    }

    pub fn get<T>(&self, store: &Store<T>) -> Val {
        let g = &store.inner.globals[self.0 as usize];
        store.inner.raw_to_val(g.value, g.ty.ty)
    }

    pub fn set<T>(&self, store: &mut Store<T>, v: Val) -> Result<()> {
        let ty = store.inner.globals[self.0 as usize].ty;
        if !ty.mutable || v.ty() != ty.ty {
            return Err(Error::Api("cannot set global: immutable or wrong type".into()));
        }
        let raw = store.inner.val_to_raw(&v);
        store.inner.globals[self.0 as usize].value = raw;
        Ok(())
    }

    pub fn ty<T>(&self, store: &Store<T>) -> GlobalType {
        store.inner.globals[self.0 as usize].ty
    }
}

impl Table {
    pub fn new<T>(store: &mut Store<T>, ty: TableType, init: Val) -> Result<Table> {
        if init.ty() != ty.elem {
            return Err(Error::Api("table initial value has the wrong type".into()));
        }
        let raw = store.inner.val_to_raw(&init);
        Ok(store.inner.alloc_table(ty, raw))
    }

    pub fn size<T>(&self, store: &Store<T>) -> u32 {
        store.inner.tables[self.0 as usize].size()
    }

    pub fn get<T>(&self, store: &Store<T>, i: u32) -> Option<Val> {
        let t = &store.inner.tables[self.0 as usize];
        let raw = t.get(i)?;
        Some(store.inner.raw_to_val(raw, t.ty.elem))
    }

    pub fn set<T>(&self, store: &mut Store<T>, i: u32, v: Val) -> Result<()> {
        let raw = store.inner.val_to_raw(&v);
        if !store.inner.tables[self.0 as usize].set(i, raw) {
            return Err(Error::Api("table index out of bounds".into()));
        }
        Ok(())
    }
}

impl Instance {
    /// Instantiate with imports given in the module's import order.
    pub fn new<T>(store: &mut Store<T>, module: &Module, imports: &[Extern]) -> Result<Instance> {
        store.inner.instantiate(&module.inner, imports)
    }

    pub fn get_export<T>(&self, store: &Store<T>, name: &str) -> Option<Extern> {
        store.inner.export(*self, name)
    }

    pub fn get_func<T>(&self, store: &Store<T>, name: &str) -> Option<Func> {
        self.get_export(store, name)?.into_func()
    }

    pub fn get_memory<T>(&self, store: &Store<T>, name: &str) -> Option<Memory> {
        self.get_export(store, name)?.into_memory()
    }

    pub fn get_global<T>(&self, store: &Store<T>, name: &str) -> Option<Global> {
        self.get_export(store, name)?.into_global()
    }

    pub fn get_table<T>(&self, store: &Store<T>, name: &str) -> Option<Table> {
        self.get_export(store, name)?.into_table()
    }

    /// All exports, sorted by name.
    pub fn exports<T>(&self, store: &Store<T>) -> Vec<(String, Extern)> {
        let mut v: Vec<(String, Extern)> =
            store.inner.instances[self.0 as usize].exports.iter().map(|(k, v)| (k.clone(), *v)).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

/// Resolves imports by (module, name).
pub struct Linker<T> {
    defs: HashMap<(String, String), Extern>,
    _data: PhantomData<T>,
}

impl<T: 'static> Default for Linker<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: 'static> Linker<T> {
    pub fn new() -> Self {
        Linker { defs: HashMap::new(), _data: PhantomData }
    }

    pub fn define(&mut self, module: &str, name: &str, ext: Extern) -> &mut Self {
        self.defs.insert((module.to_string(), name.to_string()), ext);
        self
    }

    /// Define a host function.
    pub fn func(
        &mut self,
        store: &mut Store<T>,
        module: &str,
        name: &str,
        ty: FuncType,
        f: impl Fn(Caller<'_, T>, &[Val], &mut [Val]) -> Result<(), Trap> + 'static,
    ) -> &mut Self {
        let func = Func::new(store, ty, f);
        self.define(module, name, Extern::Func(func))
    }

    /// Make every export of `instance` importable under module name `module`.
    pub fn instance(&mut self, store: &Store<T>, module: &str, instance: Instance) -> &mut Self {
        for (name, ext) in instance.exports(store) {
            self.define(module, &name, ext);
        }
        self
    }

    pub fn get(&self, module: &str, name: &str) -> Option<Extern> {
        self.defs.get(&(module.to_string(), name.to_string())).copied()
    }

    /// Resolve the module's imports and instantiate it.
    pub fn instantiate(&self, store: &mut Store<T>, module: &Module) -> Result<Instance> {
        let mut imports = Vec::new();
        for (m, n, _) in module.imports() {
            match self.get(&m, &n) {
                Some(e) => imports.push(e),
                None => return Err(Error::Link(format!("unknown import \"{m}\" \"{n}\""))),
            }
        }
        Instance::new(store, module, &imports)
    }
}
