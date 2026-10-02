//! Compiled modules.

use crate::binary::module::{ExternKind, ImportDesc, ModuleData};
use crate::config::{Engine, Strategy};
use crate::error::Result;
use crate::interp::bytecode::InterpModule;
use crate::interp::translate::Translator;
use crate::runtime::vm::{KIND_COMPILED, KIND_INTERP};
use crate::types::*;
use crate::validate::validate;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) struct ModuleInner {
    pub data: ModuleData,
    pub interp: InterpModule,
    pub jit: Option<crate::jit::CompiledModule>,
    pub stats: CompileStats,
}

// The raw pointers inside compiled code are immutable after compilation.
unsafe impl Send for ModuleInner {}
unsafe impl Sync for ModuleInner {}

impl ModuleInner {
    /// Entry point and kind of defined function `def`.
    pub(crate) fn func_entry(&self, def: u32) -> (*const u8, u32) {
        if let Some(j) = &self.jit
            && let Some(p) = j.entry(def)
        {
            return (p, KIND_COMPILED);
        }
        (crate::jit::interp_trampoline(), KIND_INTERP)
    }
}

/// What compilation did and how long it took.
#[derive(Clone, Debug, Default)]
pub struct CompileStats {
    pub decode: Duration,
    pub validate: Duration,
    pub translate: Duration,
    pub compile: Duration,
    /// Defined functions compiled to machine code.
    pub compiled_funcs: u32,
    /// Defined functions left to the interpreter, with the reason for the first few.
    pub interpreted_funcs: u32,
    pub fallback_reasons: Vec<String>,
    /// Bytes of machine code generated.
    pub code_bytes: usize,
}

/// A decoded, validated and compiled module, ready to instantiate.
#[derive(Clone)]
pub struct Module {
    pub(crate) inner: Arc<ModuleInner>,
}

impl Module {
    /// Decode, validate and compile a binary module.
    pub fn new(engine: &Engine, bytes: &[u8]) -> Result<Module> {
        let t0 = Instant::now();
        let data = ModuleData::decode(bytes)?;
        let t1 = Instant::now();
        let info = validate(&data)?;
        let t2 = Instant::now();
        let mut stats = CompileStats {
            decode: t1 - t0,
            validate: t2 - t1,
            ..Default::default()
        };
        let ndef = data.num_defined_funcs() as usize;
        let mut interp = InterpModule {
            funcs: (0..ndef).map(|_| None).collect(),
            type_params: data.types.iter().map(|t| t.params.len() as u32).collect(),
        };
        let fuel = engine.config.fuel;
        let mut jit = None;
        if engine.config.strategy == Strategy::Compiler && crate::jit::available() {
            let t = Instant::now();
            let c = crate::jit::compile_module(&data, &info, fuel);
            stats.compile = t.elapsed();
            stats.code_bytes = c.code_size();
            jit = Some(c);
        }
        let t = Instant::now();
        #[allow(clippy::needless_range_loop)]
        for def in 0..ndef {
            let compiled = jit.as_ref().is_some_and(|j| j.entry(def as u32).is_some());
            if compiled {
                stats.compiled_funcs += 1;
            } else {
                if let Some(j) = &jit
                    && stats.fallback_reasons.len() < 8
                    && let Some(r) = j.fallback_reason(def as u32)
                {
                    stats.fallback_reasons.push(format!(
                        "func {}: {r}",
                        def as u32 + data.num_imported_funcs
                    ));
                }
                stats.interpreted_funcs += 1;
                interp.funcs[def] =
                    Some(Translator::translate(&data, def as u32, &info[def], fuel)?);
            }
        }
        stats.translate = t.elapsed();
        Ok(Module {
            inner: Arc::new(ModuleInner {
                data,
                interp,
                jit,
                stats,
            }),
        })
    }

    /// Only decode and validate.
    pub fn validate(bytes: &[u8]) -> Result<()> {
        let data = ModuleData::decode(bytes)?;
        validate(&data)?;
        Ok(())
    }

    pub fn stats(&self) -> &CompileStats {
        &self.inner.stats
    }

    /// The module's imports: (module, name, type).
    pub fn imports(&self) -> Vec<(String, String, ExternType)> {
        let d = &self.inner.data;
        d.imports
            .iter()
            .map(|i| {
                let t = match &i.desc {
                    ImportDesc::Func(t) => ExternType::Func(d.types[*t as usize].clone()),
                    ImportDesc::Table(t) => ExternType::Table(*t),
                    ImportDesc::Memory(t) => ExternType::Memory(*t),
                    ImportDesc::Global(t) => ExternType::Global(*t),
                };
                (i.module.clone(), i.name.clone(), t)
            })
            .collect()
    }

    /// The module's exports: (name, type).
    pub fn exports(&self) -> Vec<(String, ExternType)> {
        let d = &self.inner.data;
        d.exports
            .iter()
            .map(|e| {
                let t = match e.kind {
                    ExternKind::Func => ExternType::Func(d.func_type(e.index).clone()),
                    ExternKind::Table => ExternType::Table(d.tables[e.index as usize]),
                    ExternKind::Memory => ExternType::Memory(d.memories[e.index as usize]),
                    ExternKind::Global => ExternType::Global(d.globals[e.index as usize]),
                };
                (e.name.clone(), t)
            })
            .collect()
    }

    /// Number of functions (imported + defined).
    pub fn num_funcs(&self) -> usize {
        self.inner.data.funcs.len()
    }

    /// Size of the binary in bytes.
    pub fn size(&self) -> usize {
        self.inner.data.bytes.len()
    }
}
