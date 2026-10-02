#!/usr/bin/env python3
"""End-to-end benchmarks: wisp (compiler and interpreter) against native code, wasmtime and
wasmer, on the guest programs in tests/programs.

Method: every (program, runtime) pair is run REPEAT times sequentially; the table reports the
median wall-clock time of the whole process (start-up, compilation and execution). Every
run's stdout is compared with the native build's; a mismatch aborts the benchmark.

Usage: bench/run.py [--repeat N] [--out FILE]
Runtimes are found through $WASMTIME / $WASMER or on $PATH; missing ones are skipped.
"""
import argparse, datetime, os, platform, shutil, statistics, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROGS = os.path.join(ROOT, "tests", "programs")
WASM = os.path.join(PROGS, "target", "wasm32-wasip1", "release")
NATIVE = os.path.join(PROGS, "target", "release")
WISP = os.path.join(ROOT, "target", "release", "wisp")

WORKLOADS = [
    ("primes", "20000000", "sieve of Eratosthenes, memory-bound integer loops"),
    ("fib", "34", "recursive Fibonacci, call-heavy"),
    ("matmul", "300", "f64 matrix multiply"),
    ("raytrace", "400", "ray tracer, f64 arithmetic and sqrt"),
    ("compress", "8000000", "miniz_oxide DEFLATE round trip"),
    ("json", "60000", "serde_json generate/parse/serialise"),
    ("regex", "4000000", "regex crate search, 1.3 MB module"),
]


def find(env, name):
    p = os.environ.get(env) or shutil.which(name)
    return p if p and os.path.exists(p) else None


def runtimes():
    rts = [("native", None), ("wisp (compiler)", [WISP, "run", "--jit"]), ("wisp (interpreter)", [WISP, "run", "--interp"])]
    wt = find("WASMTIME", "wasmtime")
    if wt:
        rts.append(("wasmtime (Cranelift)", [wt, "run", "-C", "cache=n"]))
        rts.append(("wasmtime (Winch)", [wt, "run", "-C", "cache=n", "-C", "compiler=winch"]))
    wm = find("WASMER", "wasmer")
    if wm:
        rts.append(("wasmer (Cranelift)", [wm, "run", "--disable-cache", "--cranelift"]))
        rts.append(("wasmer (Singlepass)", [wm, "run", "--disable-cache", "--singlepass"]))
    return rts


def version(cmd):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=30).stdout.strip().splitlines()[0]
    except Exception:
        return "?"


def cmdline(rt, prog, arg):
    name, base = rt
    if base is None:
        return [os.path.join(NATIVE, prog), arg]
    wasm = os.path.join(WASM, prog + ".wasm")
    if base[0] == WISP:
        return base + [wasm, arg]
    return base + [wasm, "--", arg] if "wasmer" in name else base + [wasm, arg]


def timed(cmd, env):
    t = time.perf_counter()
    p = subprocess.run(cmd, capture_output=True, env=env)
    return time.perf_counter() - t, p


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repeat", type=int, default=5)
    ap.add_argument("--out", default=os.path.join(ROOT, "docs", "benchmarks.md"))
    args = ap.parse_args()
    for d in (WASM, NATIVE):
        if not os.path.isdir(d):
            sys.exit("build the guest programs first: (cd tests/programs && cargo build --release "
                     "&& cargo build --release --target wasm32-wasip1)")
    if not os.path.exists(WISP):
        sys.exit("build wisp first: cargo build --release")
    scratch = tempfile.mkdtemp(prefix="wisp-bench-")
    env = dict(os.environ, WASMER_DIR=os.path.join(scratch, "wasmer"), WASMER_CACHE_DIR=os.path.join(scratch, "cache"))
    rts = runtimes()
    results = {}
    for prog, arg, _ in WORKLOADS:
        expect = None
        for rt in rts:
            times = []
            for _ in range(args.repeat):
                dt, p = timed(cmdline(rt, prog, arg), env)
                if p.returncode != 0:
                    sys.exit(f"{rt[0]} failed on {prog}: {p.stderr.decode()[-500:]}")
                if expect is None:
                    expect = p.stdout
                elif p.stdout != expect:
                    sys.exit(f"{rt[0]} printed different output for {prog}")
                times.append(dt)
            results[(prog, rt[0])] = statistics.median(times)
            print(f"{prog:9} {rt[0]:22} {results[(prog, rt[0])]:8.3f} s", flush=True)

    # Compile speed on the largest modules: wall time of a compile-only command.
    compile_rows = []
    for prog in ("regex", "json"):
        wasm = os.path.join(WASM, prog + ".wasm")
        size = os.path.getsize(wasm)
        cmds = [("wisp (compiler)", [WISP, "compile", wasm]), ("wisp (interpreter)", [WISP, "compile", "--interp", wasm])]
        wt = find("WASMTIME", "wasmtime")
        if wt:
            out = os.path.join(scratch, "m.cwasm")
            cmds.append(("wasmtime (Cranelift)", [wt, "compile", "-C", "cache=n", wasm, "-o", out]))
            cmds.append(("wasmtime (Winch)", [wt, "compile", "-C", "cache=n", "-C", "compiler=winch", wasm, "-o", out]))
        wm = find("WASMER", "wasmer")
        if wm:
            out = os.path.join(scratch, "m.wasmu")
            cmds.append(("wasmer (Cranelift)", [wm, "compile", "--cranelift", wasm, "-o", out]))
            cmds.append(("wasmer (Singlepass)", [wm, "compile", "--singlepass", wasm, "-o", out]))
        for name, c in cmds:
            ts = []
            for _ in range(args.repeat):
                dt, p = timed(c, env)
                if p.returncode != 0:
                    sys.exit(f"{name} failed to compile {prog}: {p.stderr.decode()[-300:]}")
                ts.append(dt)
            m = statistics.median(ts)
            compile_rows.append((prog, size, name, m))
            print(f"compile {prog:6} {name:22} {m*1e3:8.1f} ms  {size/1e6/m:6.1f} MB/s", flush=True)
    shutil.rmtree(scratch, ignore_errors=True)

    names = [r[0] for r in rts]
    lines = ["# Benchmarks", ""]
    lines.append(f"Generated by `bench/run.py` on {datetime.date.today()}.")
    cpu = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
    lines.append(f"Machine: {cpu or platform.processor()}, {os.cpu_count()} cores, {platform.system()} "
                 f"{platform.release()}. The machine was shared with other work while measuring.")
    lines.append("")
    lines.append(f"Method: each cell is the median of {args.repeat} sequential runs of the whole process "
                 "(start-up + compilation + execution), wall clock. Every run's output was checked "
                 "against the native build. Guest programs: `tests/programs`, built with "
                 "`cargo build --release --target wasm32-wasip1`; native with the same source and "
                 "profile for the host.")
    lines.append("")
    lines.append("Versions: " + "; ".join(
        [version([WISP, "--version"])]
        + [version([c[0], "--version"]) for n, c in rts if c and c[0] != WISP and n.endswith("(Cranelift)")]))
    lines.append("")
    lines.append("## Run time (seconds, lower is better)")
    lines.append("")
    lines.append("| program | " + " | ".join(names) + " |")
    lines.append("|---|" + "---:|" * len(names))
    for prog, arg, desc in WORKLOADS:
        cells = [f"{results[(prog, n)]:.3f}" for n in names]
        lines.append(f"| {prog} {arg} | " + " | ".join(cells) + " |")
    lines.append("")
    lines.append("## Relative to native (x slower)")
    lines.append("")
    lines.append("| program | " + " | ".join(names[1:]) + " |")
    lines.append("|---|" + "---:|" * (len(names) - 1))
    for prog, arg, _ in WORKLOADS:
        base = results[(prog, "native")]
        lines.append(f"| {prog} | " + " | ".join(f"{results[(prog, n)] / base:.1f}" for n in names[1:]) + " |")
    lines.append("")
    lines.append("Workloads: " + "; ".join(f"`{p}` {d}" for p, _, d in WORKLOADS) + ".")
    lines.append("")
    lines.append("## Compile time (compile-only command, wall clock, median)")
    lines.append("")
    lines.append("| module | size | runtime | time (ms) | MB/s |")
    lines.append("|---|---:|---|---:|---:|")
    for prog, size, name, m in compile_rows:
        lines.append(f"| {prog}.wasm | {size/1e6:.2f} MB | {name} | {m*1e3:.1f} | {size/1e6/m:.1f} |")
    lines.append("")
    lines.append("`wisp compile` decodes, validates and compiles (or translates) in one process; the "
                 "other runtimes' compile commands also write the compiled artefact to disk.")
    with open(args.out, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
