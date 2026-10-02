# Changelog

## 0.1.0 — unreleased

First version.

- Binary decoder and validator for WebAssembly 2.0 without SIMD, with the reference
  interpreter's error messages.
- Interpreter over slot-addressed bytecode (lazy locals, immediates, fused compare-and-branch,
  constant folding, frames laid over argument slots).
- Single-pass AArch64 baseline compiler for macOS: own encoder, MAP_JIT memory, register cache
  with flags and constants, explicit bounds checks, trap unwinding through the entry trampoline,
  calls to other instances, the interpreter and host functions.
- Embedding API: `Engine`, `Module`, `Store`, `Instance`, `Func`, `Memory`, `Global`, `Table`,
  `Linker`, `Caller`; fuel metering.
- WASI preview 1 with a capability-based path resolver; `wisp` CLI.
- Evidence: spec suite runner (27,416 assertions pass with both engines), guest programs compared
  with native builds, sandbox escape tests, encoder checked against clang, wasm-smith
  differential fuzzer, benchmarks against native, wasmtime and wasmer.
