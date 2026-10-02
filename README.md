# wisp

**A compact WebAssembly runtime written from scratch in Rust: binary decoder, validator,
a register-style interpreter, a single-pass baseline compiler that emits AArch64 machine code,
and WASI preview 1 with a capability sandbox.**

[![CI](https://github.com/useless-husband/wisp/actions/workflows/ci.yml/badge.svg)](https://github.com/useless-husband/wisp/actions/workflows/ci.yml)
· [繁體中文說明](README.zh-TW.md) · [Design](docs/DESIGN.md) · [Spec results](docs/spec-results.md) · [Benchmarks](docs/benchmarks.md)

wisp exists to show, in about 12,600 lines of dependency-light Rust, every layer a WebAssembly
engine needs, with results anyone can re-run:

- **Conformance.** All 27,416 executed assertions of the official spec test suite
  (WebAssembly/spec tag `wg-2.0`) pass with **both** the interpreter and the compiler, and every
  rejected module also matches the reference interpreter's error message. The test runner is
  wisp's own; the `wast` crate is used only to parse the script text format.
- **Real programs.** Rust programs compiled with `rustc --target wasm32-wasip1` (using
  `serde_json`, `miniz_oxide`, `regex` and the standard library's file system API) print exactly
  what their native builds print.
- **A real compiler.** The baseline compiler has its own AArch64 encoder (checked against
  clang's assembler), runs from `MAP_JIT` memory, compiles every non-SIMD 2.0 instruction, and
  is 1.5–2.5× slower than wasmtime/Cranelift end to end on six of seven programs (slightly
  faster on `fib`), about the speed of wasmtime's Winch
  baseline compiler, while compiling faster than either.
- **A sandbox that holds.** Guest file access is resolved component by component from
  preopened directory descriptors; tests show `..`, absolute symlinks, symlinks with `..` and
  symlink loops cannot reach outside.

## Demo

```text
$ wisp run raytrace.wasm 320
320x240: fnv 9427c78dcb5b8249, mean 156.337630
$ wisp compile regex.wasm
1355660 bytes, decode 380.167µs, validate 5.063583ms, compile 8.297583ms (1615 funcs, 2226852 bytes of code), translate 917ns (0 funcs interpreted)
regex.wasm: 13.7 ms total, 98.6 MB/s
$ wisp run --dir box::/sandbox sandbox.wasm /sandbox
inside.txt: READ "hi"
../outside/secret.txt: denied (Uncategorized)
escape/secret.txt (dir symlink): denied (Uncategorized)
abs/hosts (absolute symlink): denied (Uncategorized)
loop (symlink loop): denied (FilesystemLoop)
```

(`Uncategorized` is how Rust's standard library reports WASI's `ENOTCAPABLE`.)

```text
usage: wisp run [--dir HOST[::GUEST]] [--env K=V] [--interp|--jit] [--fuel N] [--invoke NAME] <module.wasm> [args...]
       wisp validate | inspect | compile [--interp] <module.wasm>
```

Embedding:

```rust
use wisp::{Engine, Instance, Module, Store, Val};

let engine = Engine::default(); // compiler on Apple Silicon, interpreter elsewhere
let module = Module::new(&engine, &wasm_bytes)?;
let mut store = Store::new(&engine, ());
let instance = Instance::new(&mut store, &module, &[])?;
let add = instance.get_func(&store, "add").unwrap();
assert_eq!(add.call(&mut store, &[Val::I32(2), Val::I32(3)])?, vec![Val::I32(5)]);
```

`Linker` resolves imports by name and defines host functions (`Caller` gives them the store
data and the caller's memory); `wisp::wasi::add_to_linker` adds WASI. Traps come back as
`Err(Error::Trap(..))` and the store stays usable.

## How it works

```text
 .wasm bytes
     │  binary::module  — sections, LEB128 with the spec's overflow rules
     ▼
 ModuleData ──► validate — the spec appendix algorithm; records max stack height per function
     │
     ├──► interp::translate ──► slot-addressed bytecode ──► interp::exec (loop + match)
     │
     └──► jit::compile ──► jit::a64 encoder ──► MAP_JIT memory ──► entry trampoline
                                   ▲
     runtime: Store / Instance / VmCtx (#[repr(C)], shared by both engines and the host)
     wasi: preview 1 over a capability resolver (openat/readlinkat, never kernel-followed links)
```

**Interpreter.** Wasm's operand stack height is static, so the operand at depth *d* always lives
in frame slot `nlocals + d`. The translator turns stack code into three-address instructions over
those slots (16 bytes each), with operands that may name a local directly (`local.get` costs
nothing), immediate forms (`i32.add x, 5`), fused compare-and-branch, constant folding, and
`local.set` folded into the producing instruction. Calls lay the callee frame over the caller's
argument slots, so arguments are never copied and results land where the caller expects them;
wasm-to-wasm calls do not recurse on the native stack.

**Compiler.** One pass per function over an abstract operand stack whose entries are in a
register, in their spill slot, in the condition flags (a comparison not yet materialised, so
`i32.lt_s; br_if` becomes `cmp; b.lt`) or a constant. 20 general and 30 vector registers form the
cache; at block boundaries everything is in its slot, so merges need no reconciliation.
`x28` holds the instance context, `x27`/`x26` the memory base and size, `x25` the runtime.
Memory accesses are bounds-checked explicitly against `x26` (`add; cmp; b.hi`). Traps branch to an
exit sequence that resets `sp` to the value saved by the entry trampoline and returns the trap
code, which Rust turns into an error. Calls within a module are direct `bl`; imports and
`call_indirect` go through function references, which also lead to host functions and to the
interpreter. Details and rejected alternatives: [docs/DESIGN.md](docs/DESIGN.md).

## Results

### Spec tests

`cargo test --release --test spec` fetches WebAssembly/spec at tag `wg-2.0`
(commit `fffc6e12`) and runs all 90 `test/core/*.wast` files with each engine:

| engine | passed | failed | skipped |
|---|---:|---:|---:|
| interpreter | 27,416 | 0 | 581 |
| compiler | 27,416 | 0 | 581 |

The 581 skipped directives are `module quote` assertions, which test a text-format parser
(wisp has none). The `test/core/simd/` directory is not run: SIMD is not implemented.
Per-file counts: [docs/spec-results.md](docs/spec-results.md).

### Real programs and the sandbox

`cargo test --release --test programs` builds `tests/programs` for `wasm32-wasip1` and natively,
runs each under both engines and requires identical output: a sieve, recursive fib, f64 matrix
multiply, a ray tracer, a DEFLATE round trip (`miniz_oxide`), JSON (`serde_json`), regex search
(`regex`, a 1.3 MB module), a file I/O program in a preopened directory, and a sandbox program making
twelve accesses, nine of them escape attempts (all denied, nothing created outside).

### Differential fuzzing

`tools/fuzz-diff` generates modules with `wasm-smith` (2.0 features, no SIMD, NaNs canonicalised,
loops bounded), calls every export in both engines and compares results, traps, memories and
globals. Run: `make fuzz`. Last run (`fuzz-diff --cases 5000 --seed 1`): 5,000 modules, 9,190
export calls, 0 mismatches, 0 modules rejected by wisp. Generated modules are small (about two
calls each), so this complements rather than replaces the spec suite.

### Benchmarks

`python3 bench/run.py` (method: median of 5 sequential runs of the whole process, wall clock,
output checked against native; Apple M5, macOS 27; wasmtime 49.0.1, wasmer 7.5.0 release
binaries). Seconds, lower is better; times include start-up and compilation.

| program | native | wisp compiler | wisp interpreter | wasmtime Cranelift | wasmtime Winch | wasmer Cranelift | wasmer Singlepass |
|---|---:|---:|---:|---:|---:|---:|---:|
| primes 20M | 0.034 | 0.120 | 0.344 | 0.049 | 0.070 | 0.062 | 0.072 |
| fib 34 | 0.010 | 0.022 | 0.117 | 0.024 | 0.031 | 0.038 | 0.028 |
| matmul 300 | 0.005 | 0.056 | 0.312 | 0.028 | 0.049 | 0.041 | 0.538 |
| raytrace 400 | 0.019 | 0.054 | 0.275 | 0.031 | 0.066 | 0.045 | 1.087 |
| compress 8 MB | 0.201 | 0.713 | 3.553 | 0.313 | 0.517 | 0.327 | 0.517 |
| json 60k | 0.115 | 0.263 | 1.783 | 0.141 | 0.325 | 0.149 | 0.271 |
| regex 4 MB | 0.082 | 0.308 | 2.500 | 0.203 | 0.341 | 0.211 | 0.333 |

Where wisp is slower, plainly: against wasmtime/Cranelift it loses everywhere except `fib`
(1.5–2.5×), and it loses to Winch on `primes`, `compress` and `matmul`. The likely costs (not measured
separately) are the explicit bounds check on every memory access (wasmtime uses guard pages), locals that always live
in memory, and spilling everything at every block boundary. The interpreter is 3–8× slower than
the compiler.

Compile time for `regex.wasm` (1.36 MB), compile-only command, wall clock: wisp 16 ms (83 MB/s,
single thread); wasmtime Cranelift 72 ms, Winch 24 ms; wasmer Cranelift 82 ms, Singlepass 32 ms
(the other runtimes compile functions in parallel and write an artefact).
Full tables: [docs/benchmarks.md](docs/benchmarks.md).

## Scope and limitations

- **Implemented:** WebAssembly 2.0 core minus SIMD: multi-value, bulk memory, reference types,
  multiple tables, sign extension, saturating conversions, mutable global import/export. Both
  engines run all of it; the compiler calls Rust helpers for `memory.grow/fill/copy/init`,
  `data.drop` and the table bulk operations.
- **Not implemented:** SIMD (`v128` is rejected with an "unsupported" error), threads, and all
  3.0 proposals (GC, exceptions, tail calls, memory64, multi-memory, extended const).
- **Compiler host:** AArch64 macOS only. Linux/AArch64 code paths exist but are untested and
  disabled; elsewhere the interpreter is used.
- **Compiler quality:** no register allocation across blocks, no local caching, explicit bounds
  checks instead of guard pages; fuel is charged per straight-line block, so a fuel trap may fire
  up to one block early.
- **WASI:** no sockets (`sock_*` return `ENOTSUP`); `poll_oneoff` supports clock subscriptions and
  reports fd subscriptions ready immediately; rights are recorded but only read/write mode is
  enforced; `fd_readdir` rereads the directory on each call.
- **Embedding:** a `Store` is single-threaded; handles (`Func`, `Memory`, ...) are plain indices
  and are not checked against the store they came from; there is no C API header yet.
- Benchmarks are from one machine and one set of inputs; the numbers above include start-up.

## Related work

wisp makes no novelty claim beyond being a compact from-scratch runtime with a baseline AArch64
compiler and transparent results. The closest projects:

- **wasmtime** with **Cranelift** (optimising) and **Winch** (baseline, single pass). Winch is
  the closest analogue to wisp's compiler; wasmtime is far more complete and faster.
- **wasmer**, with Cranelift, LLVM and **Singlepass** back ends.
- **V8 Liftoff** and **SpiderMonkey's baseline Wasm compiler**: single-pass compilers with a
  register cache over the value stack; wisp's compiler follows the same idea in much simpler form.
- **wasmi**: a Rust interpreter whose register-based bytecode is the same family of design as
  wisp's slot-addressed interpreter.
- **wasm3** (threaded-code interpreter), **WAMR** (interpreters plus AOT/JIT), **wazero** (Go,
  interpreter plus compiler).

## Build and test

Requires Rust 1.88+ (developed on 1.98) and, for the program tests, `rustup target add wasm32-wasip1`.

```sh
cargo build --release            # target/release/wisp
make test                        # unit, API, spec suite (both engines), guest programs
make spec-report                 # regenerate docs/spec-results.md
make fuzz                        # differential fuzzing, 2,000 modules
make bench                       # needs wasmtime/wasmer on PATH or $WASMTIME/$WASMER
make lint                        # rustfmt + clippy -D warnings
```

The spec suite is downloaded with `curl` into `target/spec` on first use (or set
`WISP_SPEC_DIR`). MIT licensed.
