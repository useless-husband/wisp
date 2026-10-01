//! The `wisp` command line.

use std::process::ExitCode;
use std::time::Instant;
use wisp::wasi::{WasiCtx, WasiCtxBuilder};
use wisp::*;

const USAGE: &str = "\
usage: wisp run [options] <module.wasm> [args...]
       wisp validate <module.wasm>
       wisp inspect <module.wasm>
       wisp compile [--interp] <module.wasm>

run options:
  --dir HOST[::GUEST]  give the guest access to directory HOST (seen as GUEST)
  --env NAME=VALUE     set an environment variable for the guest
  --interp             use the interpreter (default: the AArch64 compiler where available)
  --jit                use the baseline compiler
  --fuel N             stop after about N wasm instructions
  --invoke NAME        call export NAME instead of _start; remaining args are its i32/i64/f32/f64 arguments
  --stats              print compile statistics to stderr
";

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("wisp: {msg}");
    ExitCode::from(1)
}

fn read(path: &str) -> Result<Vec<u8>, ExitCode> {
    std::fs::read(path).map_err(|e| fail(format!("{path}: {e}")))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("validate") if args.len() == 2 => {
            let bytes = match read(&args[1]) {
                Ok(b) => b,
                Err(c) => return c,
            };
            match Module::validate(&bytes) {
                Ok(()) => {
                    println!("{}: valid", args[1]);
                    ExitCode::SUCCESS
                }
                Err(e) => fail(format!("{}: {e}", args[1])),
            }
        }
        Some("inspect") if args.len() == 2 => inspect(&args[1]),
        Some("compile") => compile(&args[1..]),
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("--version" | "version") => {
            println!("wisp {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn print_stats(m: &Module) {
    let s = m.stats();
    eprintln!(
        "wisp: {} bytes, decode {:?}, validate {:?}, compile {:?} ({} funcs, {} bytes of code), translate {:?} ({} funcs interpreted)",
        m.size(),
        s.decode,
        s.validate,
        s.compile,
        s.compiled_funcs,
        s.code_bytes,
        s.translate,
        s.interpreted_funcs
    );
    for r in &s.fallback_reasons {
        eprintln!("wisp:   interpreted: {r}");
    }
}

fn compile(args: &[String]) -> ExitCode {
    let (strategy, path) = match args {
        [p] => (Strategy::default(), p),
        [f, p] if f == "--interp" => (Strategy::Interpreter, p),
        _ => return fail("usage: wisp compile [--interp] <module.wasm>"),
    };
    let bytes = match read(path) {
        Ok(b) => b,
        Err(c) => return c,
    };
    let engine = Engine::new(Config::default().strategy(strategy));
    let t = Instant::now();
    match Module::new(&engine, &bytes) {
        Ok(m) => {
            let el = t.elapsed();
            print_stats(&m);
            println!(
                "{path}: {:.1} ms total, {:.1} MB/s",
                el.as_secs_f64() * 1e3,
                bytes.len() as f64 / 1e6 / el.as_secs_f64()
            );
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("{path}: {e}")),
    }
}

fn inspect(path: &str) -> ExitCode {
    let bytes = match read(path) {
        Ok(b) => b,
        Err(c) => return c,
    };
    let engine = Engine::new(Config::default().strategy(Strategy::Interpreter));
    let m = match Module::new(&engine, &bytes) {
        Ok(m) => m,
        Err(e) => return fail(format!("{path}: {e}")),
    };
    println!("{path}: {} bytes, {} functions", m.size(), m.num_funcs());
    for (module, name, ty) in m.imports() {
        println!("  import {module}.{name}: {ty:?}");
    }
    for (name, ty) in m.exports() {
        println!("  export {name}: {ty:?}");
    }
    ExitCode::SUCCESS
}

fn parse_arg(s: &str, ty: ValType) -> Option<Val> {
    Some(match ty {
        ValType::I32 => Val::I32(s.parse::<i64>().ok()? as i32),
        ValType::I64 => Val::I64(s.parse().ok()?),
        ValType::F32 => Val::f32(s.parse().ok()?),
        ValType::F64 => Val::f64(s.parse().ok()?),
        _ => return None,
    })
}

fn run(args: &[String]) -> ExitCode {
    let mut wasi = WasiCtxBuilder::new();
    let mut config = Config::default();
    let mut fuel: Option<u64> = None;
    let mut invoke: Option<String> = None;
    let mut stats = false;
    let mut i = 0;
    while i < args.len() && args[i].starts_with("--") {
        let flag = args[i].as_str();
        let mut value = || {
            i += 1;
            args.get(i).cloned()
        };
        match flag {
            "--dir" => {
                let Some(v) = value() else {
                    return fail("--dir needs a value");
                };
                let (host, guest) = match v.split_once("::") {
                    Some((h, g)) => (h.to_string(), g.to_string()),
                    None => (v.clone(), v.clone()),
                };
                wasi = match wasi.preopen_dir(&host, guest) {
                    Ok(w) => w,
                    Err(e) => return fail(format!("--dir {host}: {e}")),
                };
            }
            "--env" => {
                let Some(v) = value() else {
                    return fail("--env needs a value");
                };
                let Some((k, val)) = v.split_once('=') else {
                    return fail("--env expects NAME=VALUE");
                };
                wasi = wasi.env(k, val);
            }
            "--interp" => config.strategy = Strategy::Interpreter,
            "--jit" => config.strategy = Strategy::Compiler,
            "--fuel" => {
                let Some(v) = value().and_then(|v| v.parse().ok()) else {
                    return fail("--fuel needs a number");
                };
                fuel = Some(v);
                config.fuel = true;
            }
            "--invoke" => invoke = value(),
            "--stats" => stats = true,
            "--" => {
                i += 1;
                break;
            }
            _ => return fail(format!("unknown option {flag}")),
        }
        i += 1;
    }
    let Some(path) = args.get(i) else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let guest_args = &args[i + 1..];
    let bytes = match read(path) {
        Ok(b) => b,
        Err(c) => return c,
    };
    let engine = Engine::new(config);
    let module = match Module::new(&engine, &bytes) {
        Ok(m) => m,
        Err(e) => return fail(format!("{path}: {e}")),
    };
    if stats {
        print_stats(&module);
    }
    let name = std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ctx = wasi.arg(name).args(guest_args.iter().cloned()).build();
    let mut store: Store<WasiCtx> = Store::new(&engine, ctx);
    if let Some(f) = fuel {
        store.set_fuel(f);
    }
    let mut linker = Linker::new();
    wisp::wasi::add_to_linker(&mut linker, &mut store);
    let instance = match linker.instantiate(&mut store, &module) {
        Ok(i) => i,
        Err(e) => return report(e, &store),
    };
    let result = match &invoke {
        Some(name) => {
            let Some(f) = instance.get_func(&store, name) else {
                return fail(format!("no exported function {name}"));
            };
            let ty = f.ty(&store);
            let mut vals = Vec::new();
            for (a, t) in guest_args.iter().zip(ty.params.iter()) {
                match parse_arg(a, *t) {
                    Some(v) => vals.push(v),
                    None => return fail(format!("cannot parse {a} as {t}")),
                }
            }
            if vals.len() != ty.params.len() {
                return fail(format!("{name} expects {} arguments", ty.params.len()));
            }
            f.call(&mut store, &vals).map(|r| {
                for v in r {
                    println!("{v}");
                }
            })
        }
        None => match instance.get_func(&store, "_start") {
            Some(f) => f.call(&mut store, &[]).map(|_| ()),
            None => return fail("module has no _start export (use --invoke)"),
        },
    };
    store.data().flush();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => report(e, &store),
    }
}

fn report(e: Error, store: &Store<WasiCtx>) -> ExitCode {
    store.data().flush();
    if let Some(code) = e.trap().and_then(|t| t.exit_status()) {
        return ExitCode::from(code as u8);
    }
    eprintln!("wisp: {e}");
    // Same convention as other runtimes: a trap exits like an abort.
    ExitCode::from(if e.trap().is_some() { 134 } else { 1 })
}
