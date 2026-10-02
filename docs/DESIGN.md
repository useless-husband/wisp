# wisp design

This document describes how wisp is put together, the problems that took the most care, and
the alternatives that were considered and rejected.

## Layout

| path | role |
|---|---|
| `src/binary/` | byte reader with LEB128, instruction decoder (`ops.rs`), section decoder (`module.rs`) |
| `src/validate.rs` | module and function validation (spec appendix algorithm) |
| `src/num.rs` | numeric semantics on raw 64-bit slots, shared by the interpreter and constant folding |
| `src/interp/` | bytecode definition, translator, execution loop |
| `src/jit/` | AArch64 encoder, executable memory, compiler, trampolines and runtime helpers |
| `src/runtime/` | store, instances, `#[repr(C)]` VM structures, embedding API |
| `src/wasi/` | WASI preview 1 and the capability path resolver |
| `src/main.rs` | the `wisp` CLI |
| `tests/spec/` | `.wast` script runner for the official suite |
| `tests/programs/` | guest programs compiled to `wasm32-wasip1` |
| `tools/fuzz-diff/` | interpreter-versus-compiler differential fuzzer |

The only runtime dependency is the `libc` crate (system call bindings).

## Decoding and validation

The decoder keeps function bodies as byte ranges; the validator, the interpreter's translator
and the compiler each decode instructions on the fly with `OpReader`. Validation follows the
appendix of the specification literally: a stack of possibly-unknown operand types and a stack of
control frames, with the unknown type surviving `br_table`'s re-push so that unreachable code
types the way the reference interpreter does.

Matching the reference interpreter's *error messages* turned out to be a design question rather
than a cosmetic one. The reference decoder reads integers and instructions from the whole input
and only checks a section's declared size afterwards, and it decodes function bodies while
reading the code section. wisp reproduces that order: LEB128 reads may run past a section end
(the size check reports it), function bodies are decoded past their declared end on the error
path only, function/code count consistency is checked at the end of the module, and if a
section-level error follows the code section, body encoding errors are reported first. With these
rules every `assert_malformed` (binary) and `assert_invalid` module in the suite is rejected with
the reference message and error class; the runner reports any difference.

The validator also records, per function, the maximum operand stack height and the largest
argument/result count of any call. Both engines use these to lay out frames before generating code.

## Values and runtime structures

Every value is a 64-bit slot: `i32`/`f32` in the low half (upper half unspecified — every
consumer masks or uses 32-bit operations), `i64`/`f64` in all 64 bits, references as pointers
(`funcref` is a `*const VmFuncRef`, null is 0; `externref` is a host id plus one). Floats are
carried as bits everywhere, so NaN payloads survive copies, globals and the API.

Compiled code, the interpreter and the host share `#[repr(C)]` structures whose field offsets
are part of the code generator's contract and are checked by a unit test:

- `VmCtx` per instance: memory, runtime, arrays of function references, global cells, tables and
  canonical type ids.
- `VmFuncRef` per function: code pointer, callee `VmCtx`, canonical type id, store index and a
  kind (interpreted, compiled, host). `call_indirect` compares type ids as integers.
- `VmMemory`: base and byte size. The whole maximum size (up to 4 GiB) is reserved with
  `mmap(PROT_NONE)` and pages are made accessible as the memory grows, so the base never moves and
  both engines can cache it.
- `VmRuntime` per store: native stack limit, fuel, the entry stack pointer used to unwind traps,
  and a pointer back to the store for helpers.

Function types are interned per store, so structural type equality is an integer comparison.
Instantiation follows the spec order, including the 2.0 rule that a trap during element or data
segment initialisation leaves earlier writes (and the partly initialised instance) in place.

## The interpreter

### Why slots instead of a stack machine

A direct stack interpreter spends most of its time moving values between the operand stack and
locals. Because the operand stack height is static, the operand at depth *d* can be given the
fixed slot `nlocals + d`, and every instruction can name its operands and result by slot. The
translator keeps a compile-time mirror of the stack where each entry is either *in its own slot*,
*still in a local* or *a constant*:

- `local.get` pushes a reference to the local — no instruction.
- A binary operator whose right operand is a constant uses an immediate form; a constant left
  operand of a commutative or comparison operator is swapped (with the mirrored comparison).
- Pure operators on constants are folded at translation time.
- `local.set x` right after the instruction that produced the value rewrites that instruction's
  destination to `x`.
- A comparison immediately consumed by `br_if`/`if` becomes one fused compare-and-branch.

The hazard of lazy local references is aliasing: `local.get 0; ...; local.set 0` must not change
the earlier value. `local.set x` therefore first writes every pending reference to `x` into its
own slot. Control flow makes this subtle — a copy made on one arm of an `if` is not made on the
other — so pending references are materialised before every `block`, `loop` and `if`; inside
straight-line code they are always safe.

Branch values move to the target's slots in ascending order; since sources are never below their
destinations, no parallel-move resolution is needed. `return` copies results to slots `0..n`
after first materialising any result that still refers to a lower local.

### Calls

The callee's frame starts at the caller's first argument slot, so arguments are already in place
and results end up exactly where the caller's stack expects them. A call pushes a small frame
record on a heap vector; wasm-to-wasm calls never recurse on the native stack. The value stack is
a fixed-size allocation (2M slots by default) and both its size and the call depth (100,000) are
checked on every call.

`Instr` is a 16-byte enum (checked by a test); the loop is a single `match`, which rustc compiles
to a jump table. Fuel, when enabled, is a separate instruction charged once per straight-line
block, so it costs nothing when disabled.

Rejected: tail-call threaded code (needs `become`, unstable in Rust); a separate operand stack
with push/pop (measured slower in other engines and generates more instructions); register
allocation of slots into fewer slots (not worth the translation time for an interpreter).

## The baseline compiler

### Abstract stack and register cache

The compiler walks each validated body once. Every operand stack entry is in one of five places:
a general register, a vector register, its spill slot, the condition flags, or a constant. An
operator pops its operands (so their registers become its own, and cannot be chosen for
spilling while it runs), materialises what it needs, reuses the left operand's register for the
result, and pushes it. When the 20 general (`x0–x8`, `x11–x15`, `x19–x24`) or 30 vector registers
run out, the deepest register-held entry is spilled.

Comparisons push a *flags* entry. If the next operator is `br_if`, `if`, `select` or `i32.eqz`, it
uses the condition code directly (`cmp; b.lt`, `cmp; csel`, inverted condition); any other
operator first materialises it with `cset`. Starting a fuel-charging sequence also materialises
it, because `subs` would clobber the flags.

At every block, loop and `if` entry everything is spilled, so each label has one canonical state
(all entries in their slots; constants stay constants because they need no storage). A branch
then only has to store the values it carries into the target's slots. This is the main source of
slowness compared with an optimising compiler and the main source of simplicity: there is no
merge-state reconciliation.

Rejected: keeping locals in registers (Liftoff does; it needs merge reconciliation); a linear-scan
allocator (that is a second pass and a different kind of compiler).

### Frame and calling convention

```text
x29 + 16 + 8i   parameter i / result i (the caller's outgoing area)
x29             saved x29, x30
...             non-parameter locals
...             one slot per operand stack depth
sp + OUT        saved VmCtx
sp + 0          outgoing area: 8 * max(params, results) over all calls
```

All sizes are known before code generation (from the validator), so slots are addressed with
`[sp, #imm]` and parameters with `[x29, #imm]`, one instruction each in all but huge frames.
The prologue checks `sp - frame` against the runtime's stack limit and traps with "call stack
exhausted" before touching the new frame.

`x28` holds the instance context, `x27`/`x26` the memory base and byte size, `x25` the runtime.
These are callee-saved in the platform ABI, so Rust helpers preserve them. Calls within a module
are direct `bl` to the callee's entry. Calls through a function reference (imports,
`call_indirect`) load the code pointer and the callee's `VmCtx` from the `VmFuncRef`, put the
reference in `x9` and the caller's context in `x10`, and restore `x28` from the frame afterwards.
Host functions and interpreted functions share one *slow trampoline* that passes `x9`, the
argument area and `x10` to Rust. After any call the memory size register is reloaded, because the
callee may have grown memory.

### Bounds checks

Each access computes `index = addr + offset` in 64 bits, checks `index + size <= x26`, and loads
from `[x27, index]`. That is three extra instructions per access. The alternative — reserving
8 GiB per memory, relying on guard pages and turning `SIGSEGV` into traps — removes them, but
needs a process-wide signal handler that cooperates with other handlers, safe unwinding from a
signal context, and per-thread state. Explicit checks were chosen for the first version because
they are simple, portable and obviously correct. The benchmarks suggest the cost: the sieve,
which is dominated by memory accesses, runs 2.5× slower than under wasmtime (which uses guard
pages, but also optimises), against about 1× for the call-heavy `fib`.

### Traps

Compiled frames own nothing, so a trap can discard them all. The entry trampoline saves the
callee-saved registers, pushes the previous `entry_sp`, and records its own `sp` in the runtime.
Trap sites branch to a per-function stub that loads the trap code into `w0` and jumps to a
module-local exit sequence: reset `sp` to `entry_sp`, restore registers, return the code. Rust
converts it to a `Trap`. Helpers report traps by returning a code (or by leaving a full `Trap` with
data, such as a host error or a WASI exit status, in the store and returning a "pending" code).
Entries nest: a host function that calls back into wasm gets its own entry, and its exit restores
the outer `entry_sp`. Panics inside host functions called from compiled code are caught at the
helper boundary and become traps.

Every conditional trap branch targets the function's own stubs, keeping it within the ±1 MiB reach
of AArch64 conditional branches; functions too large for that are left to the interpreter.

### Encoder

`jit/a64.rs` encodes the instruction forms the compiler uses, including bitmask immediates
(found by searching for the element size and rotation) and shortest `movz`/`movn`/`movk`/`orr`
constant sequences. A test assembles the same instructions with clang, extracts `__text` from the
Mach-O object and compares all words.

### Executable memory

Code is copied into an `mmap(MAP_JIT)` region while the thread's JIT write protection is off
(`pthread_jit_write_protect_np(0)`), protection is turned back on, and the instruction cache is
invalidated. The pages are never writable and executable for the same thread at once.

## WASI and the sandbox

The guest can only name paths relative to a directory descriptor it was given. The resolver
(`wasi/sandbox.rs`) walks a path one component at a time with `openat(O_NOFOLLOW|O_DIRECTORY)`
and `readlinkat`, keeping a stack of directories it opened:

- `..` pops the stack; popping the preopened root fails with `ENOTCAPABLE`.
- A symlink's target is read and spliced into the remaining path; absolute targets fail, and the
  same `..` rule applies to the rest, so `escape -> ../outside` is caught. More than 40
  expansions is `ELOOP`.
- The final component is returned unresolved, with the directory holding it, and the operation
  uses an `*at` call with `O_NOFOLLOW`/`AT_SYMLINK_NOFOLLOW`.

The kernel never follows a link or a `..` on wisp's behalf. If an intermediate directory is
replaced by a symlink between the check and the open, `O_NOFOLLOW` makes the open fail instead
of escaping. This is the approach of cap-std; Linux's `openat2(RESOLVE_BENEATH)` would do the
same in one call but is not available on macOS.

## Testing strategy

- Unit tests next to the code: LEB128 edge cases, the numeric table, NaN and zero-sign rules,
  trap bounds of float-to-int conversion, VM structure offsets, memory growth, the sandbox
  resolver, instruction size, bitmask immediates, and the encoder against clang.
- The official spec suite, run with both engines.
- The embedding API (`tests/api.rs`), including host re-entry, traps through compiled frames and
  linking interpreted and compiled instances in both directions.
- Real programs against native output, and sandbox escapes (`tests/programs.rs`).
- Differential fuzzing with wasm-smith (`tools/fuzz-diff`).
