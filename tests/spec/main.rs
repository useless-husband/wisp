//! Runs the official WebAssembly spec test suite (`test/core/*.wast`) against wisp.
//!
//! The suite is fetched at test time from github.com/WebAssembly/spec at a pinned commit
//! (the `wg-2.0` tag). The `wast` crate parses the script text format and encodes text
//! modules to binary; everything else (decoding, validation, linking, execution and the
//! comparison of results) is wisp.
//!
//! Environment variables:
//! * `WISP_SPEC_DIR`     use an existing `test/core` directory instead of fetching
//! * `WISP_SPEC_FILTER`  only run files whose name contains this string
//! * `WISP_SPEC_REPORT`  write a Markdown report to this path
//! * `WISP_SPEC_VERBOSE` print every failure

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use wast::core::{AbstractHeapType, HeapType, NanPattern, WastArgCore, WastRetCore};
use wast::parser::{self, ParseBuffer};
use wast::token::Id;
use wast::{QuoteWatTest, Wast, WastArg, WastDirective, WastExecute, WastInvoke, WastRet};
use wisp::*;

const SPEC_COMMIT: &str = "fffc6e12fa454e475455a7b58d3b5dc343980c10"; // tag wg-2.0

#[derive(Default, Clone)]
struct Stats {
    passed: usize,
    failed: Vec<String>,
    skipped: Vec<String>,
    /// Rejections that matched in kind but not in message text.
    msg_mismatch: Vec<String>,
}

struct Runner {
    engine: Engine,
    store: Store<()>,
    linker: Linker<()>,
    named: HashMap<String, Instance>,
    current: Option<Instance>,
    stats: Stats,
    file: String,
    verbose: bool,
}

fn spec_dir() -> PathBuf {
    if let Ok(d) = std::env::var("WISP_SPEC_DIR") {
        return PathBuf::from(d);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("spec");
    let dir = root
        .join(format!("spec-{SPEC_COMMIT}"))
        .join("test")
        .join("core");
    if dir.join("i32.wast").exists() {
        return dir;
    }
    std::fs::create_dir_all(&root).unwrap();
    let url = format!("https://github.com/WebAssembly/spec/archive/{SPEC_COMMIT}.tar.gz");
    eprintln!("fetching {url}");
    let tarball = root.join("spec.tar.gz");
    let ok = Command::new("curl")
        .args(["-sSfL", "-o"])
        .arg(&tarball)
        .arg(&url)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        panic!("could not download the spec test suite (set WISP_SPEC_DIR to a local copy)");
    }
    let ok = Command::new("tar")
        .arg("xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&root)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "could not extract the spec test suite");
    dir
}

fn spectest(store: &mut Store<()>, linker: &mut Linker<()>) {
    let print = |params: Vec<ValType>| FuncType::new(params, vec![]);
    let noop = |_: Caller<'_, ()>, _: &[Val], _: &mut [Val]| Ok(());
    linker.func(store, "spectest", "print", print(vec![]), noop);
    linker.func(
        store,
        "spectest",
        "print_i32",
        print(vec![ValType::I32]),
        noop,
    );
    linker.func(
        store,
        "spectest",
        "print_i64",
        print(vec![ValType::I64]),
        noop,
    );
    linker.func(
        store,
        "spectest",
        "print_f32",
        print(vec![ValType::F32]),
        noop,
    );
    linker.func(
        store,
        "spectest",
        "print_f64",
        print(vec![ValType::F64]),
        noop,
    );
    linker.func(
        store,
        "spectest",
        "print_i32_f32",
        print(vec![ValType::I32, ValType::F32]),
        noop,
    );
    linker.func(
        store,
        "spectest",
        "print_f64_f64",
        print(vec![ValType::F64, ValType::F64]),
        noop,
    );
    let g = |store: &mut Store<()>, ty, v| {
        Global::new(store, GlobalType { ty, mutable: false }, v).unwrap()
    };
    let gi32 = g(store, ValType::I32, Val::I32(666));
    let gi64 = g(store, ValType::I64, Val::I64(666));
    let gf32 = g(store, ValType::F32, Val::f32(666.6));
    let gf64 = g(store, ValType::F64, Val::f64(666.6));
    linker.define("spectest", "global_i32", Extern::Global(gi32));
    linker.define("spectest", "global_i64", Extern::Global(gi64));
    linker.define("spectest", "global_f32", Extern::Global(gf32));
    linker.define("spectest", "global_f64", Extern::Global(gf64));
    let tt = TableType {
        elem: ValType::FuncRef,
        limits: Limits {
            min: 10,
            max: Some(20),
        },
    };
    let t = Table::new(store, tt, Val::FuncRef(None)).unwrap();
    linker.define("spectest", "table", Extern::Table(t));
    let m = Memory::new(
        store,
        MemoryType {
            limits: Limits {
                min: 1,
                max: Some(2),
            },
        },
    )
    .unwrap();
    linker.define("spectest", "memory", Extern::Memory(m));
}

fn arg_val(a: &WastArg) -> Option<Val> {
    let WastArg::Core(c) = a else { return None };
    Some(match c {
        WastArgCore::I32(v) => Val::I32(*v),
        WastArgCore::I64(v) => Val::I64(*v),
        WastArgCore::F32(v) => Val::F32(v.bits),
        WastArgCore::F64(v) => Val::F64(v.bits),
        WastArgCore::RefNull(HeapType::Abstract {
            ty: AbstractHeapType::Func,
            ..
        }) => Val::FuncRef(None),
        WastArgCore::RefNull(HeapType::Abstract {
            ty: AbstractHeapType::Extern,
            ..
        }) => Val::ExternRef(None),
        WastArgCore::RefExtern(n) => Val::ExternRef(Some(*n)),
        _ => return None,
    })
}

fn f32_matches(p: &NanPattern<wast::token::F32>, bits: u32) -> bool {
    let is_nan = bits & 0x7F80_0000 == 0x7F80_0000 && bits & 0x007F_FFFF != 0;
    match p {
        NanPattern::CanonicalNan => bits & 0x7FFF_FFFF == 0x7FC0_0000,
        NanPattern::ArithmeticNan => is_nan && bits & 0x0040_0000 != 0,
        NanPattern::Value(v) => v.bits == bits,
    }
}

fn f64_matches(p: &NanPattern<wast::token::F64>, bits: u64) -> bool {
    let exp = 0x7FF0_0000_0000_0000u64;
    let is_nan = bits & exp == exp && bits & 0x000F_FFFF_FFFF_FFFF != 0;
    match p {
        NanPattern::CanonicalNan => bits & 0x7FFF_FFFF_FFFF_FFFF == 0x7FF8_0000_0000_0000,
        NanPattern::ArithmeticNan => is_nan && bits & 0x0008_0000_0000_0000 != 0,
        NanPattern::Value(v) => v.bits == bits,
    }
}

fn ret_matches(r: &WastRetCore, v: &Val) -> bool {
    match (r, v) {
        (WastRetCore::I32(a), Val::I32(b)) => a == b,
        (WastRetCore::I64(a), Val::I64(b)) => a == b,
        (WastRetCore::F32(p), Val::F32(b)) => f32_matches(p, *b),
        (WastRetCore::F64(p), Val::F64(b)) => f64_matches(p, *b),
        (WastRetCore::RefNull(_), Val::FuncRef(None) | Val::ExternRef(None)) => true,
        (WastRetCore::RefExtern(None), Val::ExternRef(Some(_))) => true,
        (WastRetCore::RefExtern(Some(n)), Val::ExternRef(Some(m))) => n == m,
        (WastRetCore::RefFunc(_), Val::FuncRef(Some(_))) => true,
        (WastRetCore::Either(alts), v) => alts.iter().any(|a| ret_matches(a, v)),
        _ => false,
    }
}

impl Runner {
    fn new(strategy: Strategy, file: &str) -> Runner {
        let engine = Engine::new(Config::default().strategy(strategy));
        let mut store = Store::new(&engine, ());
        let mut linker = Linker::new();
        spectest(&mut store, &mut linker);
        Runner {
            engine,
            store,
            linker,
            named: HashMap::new(),
            current: None,
            stats: Stats::default(),
            file: file.to_string(),
            verbose: std::env::var("WISP_SPEC_VERBOSE").is_ok(),
        }
    }

    fn pass(&mut self) {
        self.stats.passed += 1;
    }

    fn fail(&mut self, line: usize, msg: String) {
        let m = format!("{}:{line}: {msg}", self.file);
        if self.verbose {
            eprintln!("FAIL {m}");
        }
        self.stats.failed.push(m);
    }

    fn skip(&mut self, line: usize, msg: &str) {
        self.stats
            .skipped
            .push(format!("{}:{line}: {msg}", self.file));
    }

    fn instantiate(&mut self, bytes: &[u8]) -> Result<Instance> {
        let module = Module::new(&self.engine, bytes)?;
        self.linker.instantiate(&mut self.store, &module)
    }

    fn instance(&self, id: Option<Id>) -> Option<Instance> {
        match id {
            Some(id) => self.named.get(id.name()).copied(),
            None => self.current,
        }
    }

    fn invoke(&mut self, inv: &WastInvoke) -> std::result::Result<Result<Vec<Val>>, String> {
        let inst = self.instance(inv.module).ok_or("no such module")?;
        let f = inst
            .get_func(&self.store, inv.name)
            .ok_or_else(|| format!("no export {}", inv.name))?;
        let mut args = Vec::new();
        for a in &inv.args {
            args.push(arg_val(a).ok_or("unsupported argument")?);
        }
        Ok(f.call(&mut self.store, &args))
    }

    fn execute(&mut self, exec: &mut WastExecute) -> std::result::Result<Result<Vec<Val>>, String> {
        match exec {
            WastExecute::Invoke(inv) => self.invoke(inv),
            WastExecute::Wat(w) => {
                let bytes = w.encode().map_err(|e| e.to_string())?;
                Ok(self.instantiate(&bytes).map(|_| vec![]))
            }
            WastExecute::Get { module, global, .. } => {
                let inst = self.instance(*module).ok_or("no such module")?;
                let g = inst
                    .get_global(&self.store, global)
                    .ok_or("no such global")?;
                Ok(Ok(vec![g.get(&self.store)]))
            }
        }
    }

    fn directive(&mut self, line: usize, d: WastDirective) {
        match d {
            WastDirective::Module(mut qw) => {
                let name = qw.name().map(|n| n.name().to_string());
                let bytes = match qw.encode() {
                    Ok(b) => b,
                    Err(e) => return self.skip(line, &format!("text encoding failed: {e}")),
                };
                match self.instantiate(&bytes) {
                    Ok(i) => {
                        self.current = Some(i);
                        if let Some(n) = name {
                            self.named.insert(n, i);
                        }
                        self.pass();
                    }
                    Err(e) => {
                        self.current = None;
                        self.fail(line, format!("module failed: {e}"));
                    }
                }
            }
            WastDirective::Register { name, module, .. } => match self.instance(module) {
                Some(i) => {
                    self.linker.instance(&self.store, name, i);
                }
                None => self.fail(line, "register: no module".into()),
            },
            WastDirective::Invoke(inv) => match self.invoke(&inv) {
                Ok(Ok(_)) => self.pass(),
                Ok(Err(e)) => self.fail(line, format!("invoke {} failed: {e}", inv.name)),
                Err(e) => self.skip(line, &e),
            },
            WastDirective::AssertReturn {
                mut exec, results, ..
            } => match self.execute(&mut exec) {
                Ok(Ok(vals)) => {
                    let ok = vals.len() == results.len()
                        && results
                            .iter()
                            .zip(&vals)
                            .all(|(r, v)| matches!(r, WastRet::Core(c) if ret_matches(c, v)));
                    if ok {
                        self.pass();
                    } else {
                        let got: Vec<String> = vals.iter().map(|v| v.to_string()).collect();
                        self.fail(line, format!("wrong result: got [{}]", got.join(", ")));
                    }
                }
                Ok(Err(e)) => self.fail(line, format!("expected values, got error: {e}")),
                Err(e) => self.skip(line, &e),
            },
            WastDirective::AssertTrap {
                mut exec, message, ..
            } => match self.execute(&mut exec) {
                Ok(Ok(_)) => self.fail(line, format!("expected trap \"{message}\", got success")),
                Ok(Err(Error::Trap(t))) => {
                    if t.to_string().contains(message) {
                        self.pass();
                    } else {
                        self.fail(line, format!("expected trap \"{message}\", got \"{t}\""));
                    }
                }
                Ok(Err(e)) => self.fail(line, format!("expected trap \"{message}\", got {e}")),
                Err(e) => self.skip(line, &e),
            },
            WastDirective::AssertExhaustion { call, message, .. } => match self.invoke(&call) {
                Ok(Err(Error::Trap(t))) if t.code == TrapCode::StackExhausted => self.pass(),
                Ok(r) => self.fail(line, format!("expected \"{message}\", got {r:?}")),
                Err(e) => self.skip(line, &e),
            },
            WastDirective::AssertMalformed {
                module, message, ..
            }
            | WastDirective::AssertInvalid {
                module, message, ..
            } => {
                let malformed_expected = message_is_malformed(message);
                let mut qw = module;
                let bytes = match qw.to_test() {
                    Ok(QuoteWatTest::Binary(b)) => b,
                    Ok(QuoteWatTest::Text(_)) => {
                        // Text-format errors are the text parser's business, not the runtime's.
                        return self.skip(line, "text format (module quote)");
                    }
                    Err(e) => return self.skip(line, &format!("text encoding failed: {e}")),
                };
                match Module::new(&self.engine, &bytes) {
                    Ok(_) => self.fail(
                        line,
                        format!("expected rejection \"{message}\", module accepted"),
                    ),
                    Err(e) => {
                        let kind_ok = matches!(e, Error::Malformed { .. }) == malformed_expected
                            || matches!(e, Error::Unsupported(_));
                        if !e.message().contains(message) || !kind_ok {
                            self.stats.msg_mismatch.push(format!(
                                "{}:{line}: expected \"{message}\", got \"{e}\"",
                                self.file
                            ));
                        }
                        self.pass();
                    }
                }
            }
            WastDirective::AssertUnlinkable {
                mut module,
                message,
                ..
            } => {
                let bytes = match module.encode() {
                    Ok(b) => b,
                    Err(e) => return self.skip(line, &format!("text encoding failed: {e}")),
                };
                match self.instantiate(&bytes) {
                    Ok(_) => self.fail(line, format!("expected link failure \"{message}\"")),
                    Err(Error::Link(m)) => {
                        let expect = if message.contains("unknown import") {
                            "unknown import"
                        } else {
                            "incompatible import"
                        };
                        if !m.contains(expect) {
                            self.stats.msg_mismatch.push(format!(
                                "{}:{line}: expected \"{message}\", got \"{m}\"",
                                self.file
                            ));
                        }
                        self.pass();
                    }
                    Err(e) => self.fail(line, format!("expected link failure, got {e}")),
                }
            }
            other => {
                let _ = &other;
                self.skip(line, "directive not used by the 2.0 core suite")
            }
        }
    }
}

/// Messages the reference interpreter reports from its decoder (as opposed to validation).
fn message_is_malformed(m: &str) -> bool {
    [
        "magic header",
        "unknown binary version",
        "integer",
        "unexpected end",
        "malformed",
        "section size mismatch",
        "too many locals",
        "inconsistent lengths",
        "data count section required",
        "zero byte expected",
        "illegal opcode",
        "length out of bounds",
        "unexpected content after last section",
        "END opcode expected",
    ]
    .iter()
    .any(|p| m.contains(p))
}

fn run_file(path: &Path, strategy: Strategy) -> Stats {
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    let text = std::fs::read_to_string(path).unwrap();
    let mut lexer = wast::lexer::Lexer::new(&text);
    lexer.allow_confusing_unicode(true);
    let buf = match ParseBuffer::new_with_lexer(lexer) {
        Ok(b) => b,
        Err(e) => {
            let mut s = Stats::default();
            s.failed.push(format!("{name}: lex error {e}"));
            return s;
        }
    };
    let wast: Wast = match parser::parse(&buf) {
        Ok(w) => w,
        Err(e) => {
            let mut s = Stats::default();
            s.failed.push(format!("{name}: parse error {e}"));
            return s;
        }
    };
    let mut r = Runner::new(strategy, &name);
    for d in wast.directives {
        let (line, _) = d.span().linecol_in(&text);
        let res =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.directive(line + 1, d)));
        if res.is_err() {
            r.fail(line + 1, "PANIC".into());
            // A panic may leave the store inconsistent; start fresh for the rest of the file.
            r = Runner {
                stats: std::mem::take(&mut r.stats),
                ..Runner::new(strategy, &name)
            };
        }
    }
    r.stats
}

fn main() {
    let dir = spec_dir();
    let filter = std::env::var("WISP_SPEC_FILTER").unwrap_or_default();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "wast"))
        .filter(|p| p.file_name().unwrap().to_string_lossy().contains(&filter))
        .collect();
    files.sort();
    let mut strategies = vec![Strategy::Interpreter];
    if Strategy::default() == Strategy::Compiler {
        strategies.push(Strategy::Compiler);
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(2);
    let mut results: BTreeMap<String, Vec<Stats>> = BTreeMap::new();
    let jobs: Vec<(PathBuf, usize, Strategy)> = files
        .iter()
        .flat_map(|f| {
            strategies
                .iter()
                .enumerate()
                .map(move |(i, s)| (f.clone(), i, *s))
        })
        .collect();
    let jobs = std::sync::Mutex::new(jobs.into_iter());
    let out = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|sc| {
        for _ in 0..threads {
            sc.spawn(|| {
                loop {
                    let job = jobs.lock().unwrap().next();
                    let Some((path, i, s)) = job else { break };
                    let p2 = path.clone();
                    let stats = std::thread::Builder::new()
                        .stack_size(256 << 20)
                        .spawn(move || run_file(&p2, s))
                        .unwrap()
                        .join()
                        .unwrap_or_else(|_| Stats {
                            failed: vec!["thread panicked".into()],
                            ..Default::default()
                        });
                    out.lock().unwrap().push((
                        path.file_name().unwrap().to_string_lossy().to_string(),
                        i,
                        stats,
                    ));
                }
            });
        }
    });
    for (name, i, s) in out.into_inner().unwrap() {
        let v = results
            .entry(name)
            .or_insert_with(|| vec![Stats::default(); strategies.len()]);
        v[i] = s;
    }
    report(&results, &strategies);
}

fn report(results: &BTreeMap<String, Vec<Stats>>, strategies: &[Strategy]) {
    let label = |s: &Strategy| match s {
        Strategy::Interpreter => "interpreter",
        Strategy::Compiler => "compiler",
    };
    let mut md = String::new();
    writeln!(md, "# Spec test results\n").unwrap();
    writeln!(
        md,
        "WebAssembly/spec `test/core/*.wast` at tag `wg-2.0` (commit `{SPEC_COMMIT}`)."
    )
    .unwrap();
    writeln!(
        md,
        "Generated by `cargo test --release --test spec` with `WISP_SPEC_REPORT` set.\n"
    )
    .unwrap();
    write!(md, "| file |").unwrap();
    for s in strategies {
        write!(md, " {} pass | fail |", label(s)).unwrap();
    }
    writeln!(md, " skipped |").unwrap();
    write!(md, "|---|").unwrap();
    for _ in strategies {
        write!(md, "---:|---:|").unwrap();
    }
    writeln!(md, "---:|").unwrap();
    let mut totals = vec![(0usize, 0usize, 0usize, 0usize); strategies.len()];
    for (name, v) in results {
        write!(md, "| {name} |").unwrap();
        for (i, s) in v.iter().enumerate() {
            write!(md, " {} | {} |", s.passed, s.failed.len()).unwrap();
            totals[i].0 += s.passed;
            totals[i].1 += s.failed.len();
            totals[i].2 += s.skipped.len();
            totals[i].3 += s.msg_mismatch.len();
        }
        writeln!(md, " {} |", v[0].skipped.len()).unwrap();
    }
    write!(md, "| **total** |").unwrap();
    for t in &totals {
        write!(md, " **{}** | **{}** |", t.0, t.1).unwrap();
    }
    writeln!(md, " {} |", totals[0].2).unwrap();
    for (i, s) in strategies.iter().enumerate() {
        writeln!(md, "\n## Failures ({})\n", label(s)).unwrap();
        let mut any = false;
        for v in results.values() {
            for f in &v[i].failed {
                writeln!(md, "- {f}").unwrap();
                any = true;
            }
        }
        if !any {
            writeln!(md, "None.").unwrap();
        }
    }
    writeln!(md, "\n## Skipped directives\n").unwrap();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for v in results.values() {
        for s in &v[0].skipped {
            let r = s.splitn(3, ':').nth(2).unwrap_or("").trim().to_string();
            *reasons.entry(r).or_default() += 1;
        }
    }
    for (r, n) in &reasons {
        writeln!(md, "- {n} x {r}").unwrap();
    }
    writeln!(
        md,
        "\n## Rejections with a different message or error class ({})\n",
        totals[0].3
    )
    .unwrap();
    writeln!(
        md,
        "These modules were correctly rejected, but the error text or class (malformed vs invalid)\n\
         differs from the reference interpreter's. They count as passes above.\n"
    )
    .unwrap();
    for v in results.values() {
        for m in &v[0].msg_mismatch {
            writeln!(md, "- {m}").unwrap();
        }
    }
    println!("{}", md.split("\n## Failures").next().unwrap());
    for (i, s) in strategies.iter().enumerate() {
        println!(
            "{}: {} passed, {} failed",
            label(s),
            totals[i].0,
            totals[i].1
        );
    }
    if let Ok(p) = std::env::var("WISP_SPEC_REPORT") {
        std::fs::write(&p, &md).unwrap();
        println!("report written to {p}");
    }
    let failed: usize = totals.iter().map(|t| t.1).sum();
    if failed > 0 {
        if std::env::var("WISP_SPEC_VERBOSE").is_err() {
            for (i, s) in strategies.iter().enumerate() {
                for v in results.values() {
                    for f in v[i].failed.iter().take(5) {
                        eprintln!("[{}] {f}", label(s));
                    }
                }
            }
        }
        std::process::exit(1);
    }
}
