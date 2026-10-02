//! Real programs compiled with `rustc --target wasm32-wasip1`, run under wisp with both
//! execution strategies and compared with the same programs compiled natively.
//!
//! Needs the `wasm32-wasip1` Rust target. Without it the tests print a notice and pass,
//! unless `WISP_REQUIRE_PROGRAMS=1` is set (CI sets it).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::OnceLock;
use wisp::wasi::{Output, WasiCtx, WasiCtxBuilder};
use wisp::*;

fn programs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/programs")
}

/// Build the guest programs once: returns (wasm dir, native dir), or None if unavailable.
fn built() -> Option<&'static (PathBuf, PathBuf)> {
    static B: OnceLock<Option<(PathBuf, PathBuf)>> = OnceLock::new();
    B.get_or_init(|| {
        let dir = programs_dir();
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let build = |target: Option<&str>| {
            let mut c = Command::new(&cargo);
            c.current_dir(&dir)
                .args(["build", "--release", "-j4", "--quiet"]);
            if let Some(t) = target {
                c.args(["--target", t]);
            }
            c.status().is_ok_and(|s| s.success())
        };
        if !build(Some("wasm32-wasip1")) {
            if std::env::var("WISP_REQUIRE_PROGRAMS").is_ok() {
                panic!("could not build the wasm32-wasip1 test programs");
            }
            eprintln!("note: wasm32-wasip1 target unavailable, skipping program tests");
            return None;
        }
        assert!(build(None), "native build of the test programs failed");
        let t = dir.join("target");
        Some((t.join("wasm32-wasip1/release"), t.join("release")))
    })
    .as_ref()
}

struct Run {
    stdout: String,
    status: i32,
}

fn run_wisp(name: &str, args: &[&str], dirs: &[(&Path, &str)], strategy: Strategy) -> Run {
    let (wasm_dir, _) = built().unwrap();
    let bytes = std::fs::read(wasm_dir.join(format!("{name}.wasm"))).unwrap();
    let engine = Engine::new(Config::default().strategy(strategy));
    let module = Module::new(&engine, &bytes).unwrap();
    let out = Rc::new(RefCell::new(Vec::new()));
    let mut b = WasiCtxBuilder::new()
        .arg(name)
        .args(args.iter().copied())
        .env("WISP_TEST", "1")
        .stdout(Output::Buffer(out.clone()))
        .stderr(Output::Discard)
        .stdin_bytes(b"line one\nline two\n".to_vec());
    for (host, guest) in dirs {
        b = b.preopen_dir(host, *guest).unwrap();
    }
    let mut store: Store<WasiCtx> = Store::new(&engine, b.build());
    let mut linker = Linker::new();
    wisp::wasi::add_to_linker(&mut linker, &mut store);
    let inst = linker.instantiate(&mut store, &module).unwrap();
    let start = inst.get_func(&store, "_start").unwrap();
    let status = match start.call(&mut store, &[]) {
        Ok(_) => 0,
        Err(e) => match e.trap().and_then(|t| t.exit_status()) {
            Some(c) => c,
            None => panic!("{name} {args:?} ({strategy:?}) failed: {e}"),
        },
    };
    let stdout = String::from_utf8(out.borrow().clone()).unwrap();
    Run { stdout, status }
}

fn run_native(name: &str, args: &[&str]) -> Run {
    let (_, native_dir) = built().unwrap();
    let o = Command::new(native_dir.join(name))
        .args(args)
        .env_clear()
        .env("WISP_TEST", "1")
        .stdin(std::process::Stdio::piped())
        .output()
        .unwrap();
    Run {
        stdout: String::from_utf8(o.stdout).unwrap(),
        status: o.status.code().unwrap_or(-1),
    }
}

fn strategies() -> Vec<Strategy> {
    let mut v = vec![Strategy::Interpreter];
    if Strategy::default() == Strategy::Compiler {
        v.push(Strategy::Compiler);
    }
    v
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wisp-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn cpu_programs_match_native() {
    if built().is_none() {
        return;
    }
    let cases: &[(&str, &[&str])] = &[
        ("primes", &["300000"]),
        ("raytrace", &["64"]),
        ("compress", &["300000"]),
        ("json", &["2000"]),
        ("matmul", &["48"]),
        ("fib", &["22"]),
        ("regex", &["100000"]),
    ];
    for (name, args) in cases {
        let native = run_native(name, args);
        assert_eq!(native.status, 0);
        for s in strategies() {
            let w = run_wisp(name, args, &[], s);
            assert_eq!(w.stdout, native.stdout, "{name} {args:?} under {s:?}");
            assert_eq!(w.status, 0);
        }
    }
}

#[test]
fn args_env_and_exit_status() {
    if built().is_none() {
        return;
    }
    for s in strategies() {
        let w = run_wisp("hello", &["one", "two"], &[], s);
        assert_eq!(
            w.stdout,
            "hello, wasm! args=[\"one\", \"two\"]\nenv WISP_TEST=1\n"
        );
        let w = run_wisp("hello", &["--exit", "7"], &[], s);
        assert_eq!(w.status, 7);
        let w = run_wisp("hello", &["--stdin"], &[], s);
        assert!(
            w.stdout.contains("stdin: 18 bytes, 2 lines"),
            "{}",
            w.stdout
        );
    }
}

#[test]
fn file_io_matches_native() {
    if built().is_none() {
        return;
    }
    let nd = scratch("fileio-native");
    let native = run_native("fileio", &[nd.to_str().unwrap()]);
    assert_eq!(native.status, 0, "{}", native.stdout);
    for s in strategies() {
        let wd = scratch(&format!("fileio-{s:?}"));
        let w = run_wisp("fileio", &["/data"], &[(&wd, "/data")], s);
        assert_eq!(w.stdout, native.stdout, "{s:?}");
        assert_eq!(
            std::fs::read_dir(&wd).unwrap().count(),
            0,
            "program cleans up after itself"
        );
        std::fs::remove_dir_all(wd).unwrap();
    }
    std::fs::remove_dir_all(nd).unwrap();
}

#[test]
fn sandbox_cannot_be_escaped() {
    if built().is_none() {
        return;
    }
    use std::os::unix::fs::symlink;
    for s in strategies() {
        let base = scratch(&format!("sandbox-{s:?}"));
        let boxd = base.join("box");
        std::fs::create_dir_all(boxd.join("sub")).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        std::fs::write(base.join("outside/secret.txt"), "TOP SECRET").unwrap();
        std::fs::write(boxd.join("inside.txt"), "hi").unwrap();
        symlink("inside.txt", boxd.join("link_in")).unwrap();
        symlink("../outside", boxd.join("escape")).unwrap();
        symlink("/etc", boxd.join("abs")).unwrap();
        symlink("../../outside/secret.txt", boxd.join("sub/up")).unwrap();
        symlink("loop", boxd.join("loop")).unwrap();
        let w = run_wisp("sandbox", &["/sandbox"], &[(&boxd, "/sandbox")], s);
        let lines: Vec<&str> = w.stdout.lines().collect();
        assert_eq!(lines.len(), 12, "{}", w.stdout);
        for l in &lines[..3] {
            assert!(l.ends_with("READ \"hi\""), "{l}");
        }
        for l in &lines[3..] {
            assert!(l.contains("denied"), "escape not blocked under {s:?}: {l}");
        }
        assert!(!w.stdout.contains("TOP SECRET"));
        assert!(!base.join("outside/planted.txt").exists());
        assert!(!base.join("made-outside").exists());
        std::fs::remove_dir_all(base).unwrap();
    }
}
