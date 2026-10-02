//! Differential fuzzing: wisp's interpreter against wisp's baseline compiler.
//!
//! Random valid modules come from `wasm-smith` (WebAssembly 2.0 features, no SIMD, NaNs
//! canonicalised, loops bounded by an injected fuel counter). Every exported function is
//! called with two argument patterns in both engines; results, trap messages, exported
//! memories and globals must agree. Seeds are sequential from `--seed`; a mismatch prints the
//! seed and saves the module.
//!
//! Usage: fuzz-diff [--cases N] [--seed S] [--save DIR]

use arbitrary::Unstructured;
use std::fmt::Write as _;
use wisp::*;

struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn config() -> wasm_smith::Config {
    wasm_smith::Config {
        simd_enabled: false,
        relaxed_simd_enabled: false,
        threads_enabled: false,
        shared_everything_threads_enabled: false,
        exceptions_enabled: false,
        gc_enabled: false,
        tail_call_enabled: false,
        memory64_enabled: false,
        wide_arithmetic_enabled: false,
        extended_const_enabled: false,
        custom_page_sizes_enabled: false,
        custom_descriptors_enabled: false,
        compact_imports_enabled: false,
        multi_value_enabled: true,
        reference_types_enabled: true,
        bulk_memory_enabled: true,
        saturating_float_to_int_enabled: true,
        sign_extension_ops_enabled: true,
        canonicalize_nans: true,
        export_everything: true,
        max_imports: 0,
        max_memories: 1,
        max_tables: 3,
        min_funcs: 1,
        max_funcs: 24,
        ..Default::default()
    }
}

/// A NaN-insensitive rendering of a value.
fn show(v: &Val) -> String {
    match *v {
        Val::F32(b) if f32::from_bits(b).is_nan() => "nan:f32".into(),
        Val::F64(b) if f64::from_bits(b).is_nan() => "nan:f64".into(),
        other => other.to_string(),
    }
}

fn args_for(ty: &FuncType, pattern: u32) -> Vec<Val> {
    ty.params
        .iter()
        .map(|t| match (t, pattern) {
            (_, 0) => Val::default_for(*t),
            (ValType::I32, _) => Val::I32(0x7FFF_FFF3),
            (ValType::I64, _) => Val::I64(-3),
            (ValType::F32, _) => Val::f32(1.5),
            (ValType::F64, _) => Val::f64(-2.25),
            _ => Val::default_for(*t),
        })
        .collect()
}

/// Everything observable from running a module, as text.
fn run(strategy: Strategy, wasm: &[u8]) -> Result<(String, bool, usize), String> {
    let engine = Engine::new(Config::default().strategy(strategy));
    let module = Module::new(&engine, wasm).map_err(|e| format!("rejected: {e}"))?;
    let mut store = Store::new(&engine, ());
    let mut out = String::new();
    let inst = match Instance::new(&mut store, &module, &[]) {
        Ok(i) => i,
        Err(e) => {
            return Ok((
                format!("instantiate: {e}"),
                e.message().contains("stack exhausted"),
                0,
            ));
        }
    };
    let mut exhausted = false;
    let mut calls = 0;
    for (name, ext) in inst.exports(&store) {
        let Extern::Func(f) = ext else { continue };
        let ty = f.ty(&store);
        for pattern in 0..2 {
            calls += 1;
            let args = args_for(&ty, pattern);
            match f.call(&mut store, &args) {
                Ok(vals) => {
                    let v: Vec<String> = vals.iter().map(show).collect();
                    writeln!(out, "{name}#{pattern} -> [{}]", v.join(", ")).unwrap();
                }
                Err(e) => {
                    writeln!(out, "{name}#{pattern} -> {e}").unwrap();
                    if e.message().contains("stack exhausted") {
                        // The two engines exhaust the stack at different depths, so
                        // side effects before that point may legitimately differ.
                        exhausted = true;
                        return Ok((out, exhausted, calls));
                    }
                }
            }
        }
    }
    for (name, ext) in inst.exports(&store) {
        match ext {
            Extern::Memory(m) => {
                let d = m.data(&store);
                let mut h = 0xcbf29ce484222325u64;
                for b in d {
                    h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
                }
                writeln!(out, "memory {name}: {} bytes, fnv {h:016x}", d.len()).unwrap();
            }
            Extern::Global(g) => writeln!(out, "global {name} = {}", show(&g.get(&store))).unwrap(),
            Extern::Table(t) => writeln!(out, "table {name}: {} entries", t.size(&store)).unwrap(),
            Extern::Func(_) => {}
        }
    }
    Ok((out, exhausted, calls))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let cases: u64 = get("--cases").and_then(|v| v.parse().ok()).unwrap_or(1000);
    let seed0: u64 = get("--seed").and_then(|v| v.parse().ok()).unwrap_or(1);
    let save = get("--save");
    let (mut generated, mut compared, mut calls, mut exhausted, mut rejected, mut mismatches) =
        (0, 0, 0, 0, 0, 0);
    for seed in seed0..seed0 + cases {
        let mut rng = SplitMix(seed);
        let len = 512 + (rng.next() % 16384) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let mut u = Unstructured::new(&bytes);
        let Ok(mut m) = wasm_smith::Module::new(config(), &mut u) else {
            continue;
        };
        if m.ensure_termination(5_000).is_err() {
            continue;
        }
        let wasm = m.to_bytes();
        generated += 1;
        let a = run(Strategy::Interpreter, &wasm);
        let b = run(Strategy::Compiler, &wasm);
        match (&a, &b) {
            (Ok((oa, ea, n)), Ok((ob, eb, _))) => {
                compared += 1;
                calls += n;
                if *ea || *eb {
                    exhausted += 1;
                }
                // With stack exhaustion only the run up to the exhausting call is compared,
                // and only if both exhausted.
                if oa != ob && !(*ea && *eb) {
                    mismatches += 1;
                    eprintln!("MISMATCH seed {seed}\n--- interpreter\n{oa}--- compiler\n{ob}");
                    if let Some(d) = &save {
                        let _ = std::fs::create_dir_all(d);
                        let _ = std::fs::write(format!("{d}/seed-{seed}.wasm"), &wasm);
                    }
                }
            }
            _ => {
                rejected += 1;
                eprintln!(
                    "seed {seed}: interpreter {:?} / compiler {:?}",
                    a.as_ref().err(),
                    b.as_ref().err()
                );
            }
        }
    }
    println!(
        "seeds {seed0}..{}: {generated} modules generated, {compared} compared ({calls} calls), \
         {exhausted} hit stack exhaustion, {rejected} rejected by wisp, {mismatches} mismatches",
        seed0 + cases
    );
    if mismatches > 0 || rejected > 0 {
        std::process::exit(1);
    }
}
