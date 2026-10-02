//! The embedding API, exercised with both execution strategies.

use std::cell::Cell;
use std::rc::Rc;
use wisp::*;

fn strategies() -> Vec<Strategy> {
    let mut v = vec![Strategy::Interpreter];
    if Strategy::default() == Strategy::Compiler {
        v.push(Strategy::Compiler);
    }
    v
}

fn wasm(text: &str) -> Vec<u8> {
    wat::parse_str(text).unwrap()
}

#[test]
fn host_functions_memory_and_reentry() {
    let src = wasm(
        r#"(module
            (import "env" "log" (func $log (param i32 i32)))
            (import "env" "twice" (func $twice (param i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 16) "hello")
            (func (export "square") (param i32) (result i32) local.get 0 local.get 0 i32.mul)
            (func (export "run") (result i32)
                i32.const 16 i32.const 5 call $log
                i32.const 21 call $twice))"#,
    );
    for s in strategies() {
        let engine = Engine::new(Config::default().strategy(s));
        let module = Module::new(&engine, &src).unwrap();
        let mut store = Store::new(&engine, Vec::<String>::new());
        let mut linker = Linker::new();
        let i32t = ValType::I32;
        linker.func(
            &mut store,
            "env",
            "log",
            FuncType::new([i32t, i32t], []),
            |mut c: Caller<'_, Vec<String>>, a, _| {
                let (p, n) = (a[0].i32().unwrap() as usize, a[1].i32().unwrap() as usize);
                let (mem, data) = c.memory_and_data();
                data.push(String::from_utf8(mem[p..p + n].to_vec()).unwrap());
                Ok(())
            },
        );
        // A host function that calls back into wasm.
        linker.func(
            &mut store,
            "env",
            "twice",
            FuncType::new([i32t], [i32t]),
            |mut c: Caller<'_, Vec<String>>, a, r| {
                let sq = c.get_export("square").unwrap().into_func().unwrap();
                let v = c.call(sq, &[a[0]]).map_err(|e| Trap::host(e.to_string()))?;
                r[0] = Val::I32(v[0].i32().unwrap() * 2);
                Ok(())
            },
        );
        let inst = linker.instantiate(&mut store, &module).unwrap();
        let run = inst.get_func(&store, "run").unwrap();
        assert_eq!(
            run.call(&mut store, &[]).unwrap(),
            vec![Val::I32(882)],
            "{s:?}"
        );
        assert_eq!(store.data(), &vec!["hello".to_string()]);
        let mem = inst.get_memory(&store, "memory").unwrap();
        mem.write(&mut store, 100, b"xyz").unwrap();
        let mut buf = [0u8; 3];
        mem.read(&store, 100, &mut buf).unwrap();
        assert_eq!(&buf, b"xyz");
        assert!(mem.read(&store, 65535, &mut buf).is_err());
    }
}

#[test]
fn traps_are_errors_and_the_store_stays_usable() {
    let src = wasm(
        r#"(module
            (memory 1)
            (func (export "div") (param i32 i32) (result i32) local.get 0 local.get 1 i32.div_s)
            (func (export "oob") (result i32) i32.const 65533 i32.load)
            (func $deep (export "deep") (param i32) (result i32)
                local.get 0 i32.const 1 i32.add call $deep)
            (func (export "ok") (result i32) i32.const 7))"#,
    );
    for s in strategies() {
        let engine = Engine::new(Config::default().strategy(s));
        let module = Module::new(&engine, &src).unwrap();
        let mut store = Store::new(&engine, ());
        let inst = Instance::new(&mut store, &module, &[]).unwrap();
        let call = |store: &mut Store<()>, name: &str, args: &[Val]| {
            inst.get_func(store, name).unwrap().call(store, args)
        };
        let e = call(&mut store, "div", &[Val::I32(1), Val::I32(0)]).unwrap_err();
        assert_eq!(
            e.trap().unwrap().code,
            TrapCode::IntegerDivideByZero,
            "{s:?}"
        );
        let e = call(&mut store, "div", &[Val::I32(i32::MIN), Val::I32(-1)]).unwrap_err();
        assert_eq!(e.trap().unwrap().code, TrapCode::IntegerOverflow);
        let e = call(&mut store, "oob", &[]).unwrap_err();
        assert_eq!(e.trap().unwrap().code, TrapCode::MemoryOutOfBounds);
        let e = call(&mut store, "deep", &[Val::I32(0)]).unwrap_err();
        assert_eq!(e.trap().unwrap().code, TrapCode::StackExhausted);
        // After every trap the store keeps working.
        assert_eq!(call(&mut store, "ok", &[]).unwrap(), vec![Val::I32(7)]);
        // Wrong argument types are API errors, not traps.
        assert!(matches!(
            call(&mut store, "div", &[Val::I64(1), Val::I32(1)]),
            Err(Error::Api(_))
        ));
    }
}

#[test]
fn fuel_bounds_execution() {
    let src = wasm(r#"(module (func (export "spin") (loop (br 0))))"#);
    for s in strategies() {
        let engine = Engine::new(Config::default().strategy(s).fuel(true));
        let module = Module::new(&engine, &src).unwrap();
        let mut store = Store::new(&engine, ());
        store.set_fuel(100_000);
        let inst = Instance::new(&mut store, &module, &[]).unwrap();
        let e = inst
            .get_func(&store, "spin")
            .unwrap()
            .call(&mut store, &[])
            .unwrap_err();
        assert_eq!(e.trap().unwrap().code, TrapCode::OutOfFuel, "{s:?}");
        assert_eq!(store.fuel(), 0);
    }
}

#[test]
fn instances_of_different_engines_link() {
    // A compiled module and an interpreted one calling each other through imports and a
    // shared table, both directions.
    let lib = wasm(
        r#"(module
            (table (export "t") 2 funcref)
            (func $inc (export "inc") (param i32) (result i32) local.get 0 i32.const 1 i32.add)
            (elem (i32.const 0) $inc))"#,
    );
    let app = wasm(
        r#"(module
            (import "lib" "inc" (func $inc (param i32) (result i32)))
            (import "lib" "t" (table 2 funcref))
            (type $t (func (param i32) (result i32)))
            (func $dbl (param i32) (result i32) local.get 0 i32.const 2 i32.mul)
            (elem (i32.const 1) $dbl)
            (func (export "go") (param i32) (result i32)
                local.get 0 call $inc
                i32.const 1 call_indirect (type $t)
                i32.const 0 call_indirect (type $t)))"#,
    );
    for (s1, s2) in [
        (Strategy::Interpreter, Strategy::Compiler),
        (Strategy::Compiler, Strategy::Interpreter),
    ] {
        let e1 = Engine::new(Config::default().strategy(s1));
        let e2 = Engine::new(Config::default().strategy(s2));
        let m1 = Module::new(&e1, &lib).unwrap();
        let m2 = Module::new(&e2, &app).unwrap();
        let mut store = Store::new(&e1, ());
        let mut linker = Linker::new();
        let l = linker.instantiate(&mut store, &m1).unwrap();
        linker.instance(&store, "lib", l);
        let a = linker.instantiate(&mut store, &m2).unwrap();
        let go = a.get_func(&store, "go").unwrap();
        // ((5 + 1) * 2) + 1
        assert_eq!(
            go.call(&mut store, &[Val::I32(5)]).unwrap(),
            vec![Val::I32(13)]
        );
    }
}

#[test]
fn host_traps_and_exit_codes_propagate_through_compiled_frames() {
    let src = wasm(
        r#"(module
            (import "env" "fail" (func $fail (param i32)))
            (func $inner (param i32) local.get 0 call $fail)
            (func (export "outer") (param i32) local.get 0 call $inner))"#,
    );
    for s in strategies() {
        let engine = Engine::new(Config::default().strategy(s));
        let module = Module::new(&engine, &src).unwrap();
        let mut store = Store::new(&engine, ());
        let calls = Rc::new(Cell::new(0));
        let c2 = calls.clone();
        let mut linker = Linker::new();
        linker.func(
            &mut store,
            "env",
            "fail",
            FuncType::new([ValType::I32], []),
            move |_: Caller<'_, ()>, a, _| {
                c2.set(c2.get() + 1);
                Err(Trap::new(TrapCode::Exit(a[0].i32().unwrap())))
            },
        );
        let inst = linker.instantiate(&mut store, &module).unwrap();
        let outer = inst.get_func(&store, "outer").unwrap();
        let e = outer.call(&mut store, &[Val::I32(3)]).unwrap_err();
        assert_eq!(e.trap().unwrap().exit_status(), Some(3), "{s:?}");
        let e = outer.call(&mut store, &[Val::I32(4)]).unwrap_err();
        assert_eq!(e.trap().unwrap().exit_status(), Some(4));
        assert_eq!(calls.get(), 2);
    }
}

#[test]
fn compile_stats_report_coverage() {
    let src = wasm(r#"(module (func (export "f") (result i32) i32.const 1))"#);
    let m = Module::new(&Engine::default(), &src).unwrap();
    let s = m.stats();
    assert_eq!(s.compiled_funcs + s.interpreted_funcs, 1);
    if Strategy::default() == Strategy::Compiler {
        assert_eq!(s.compiled_funcs, 1);
        assert!(s.code_bytes > 0);
    }
}

#[test]
fn cross_instance_calls_use_the_callee_memory() {
    // Regression: compiled code called another instance's compiled function through a
    // function reference with the caller's memory base/size still in the pinned registers.
    let lib = wasm(
        r#"(module
            (memory 1) (data (i32.const 0) "\2a")
            (table (export "t") 1 funcref) (elem (i32.const 0) $peek)
            (func $peek (export "peek") (result i32) i32.const 0 i32.load8_u))"#,
    );
    let app = wasm(
        r#"(module
            (import "lib" "peek" (func $peek (result i32)))
            (import "lib" "t" (table 1 funcref))
            (type $t (func (result i32)))
            (memory 1) (data (i32.const 0) "\07")
            (func (export "direct") (result i32) call $peek i32.const 0 i32.load8_u i32.add)
            (func (export "indirect") (result i32)
                i32.const 0 call_indirect (type $t) i32.const 0 i32.load8_u i32.add))"#,
    );
    for s in strategies() {
        let engine = Engine::new(Config::default().strategy(s));
        let (m1, m2) = (
            Module::new(&engine, &lib).unwrap(),
            Module::new(&engine, &app).unwrap(),
        );
        let mut store = Store::new(&engine, ());
        let mut linker = Linker::new();
        let l = linker.instantiate(&mut store, &m1).unwrap();
        linker.instance(&store, "lib", l);
        let a = linker.instantiate(&mut store, &m2).unwrap();
        for name in ["direct", "indirect"] {
            let f = a.get_func(&store, name).unwrap();
            // 42 from the library's memory + 7 from the app's own memory afterwards.
            assert_eq!(
                f.call(&mut store, &[]).unwrap(),
                vec![Val::I32(49)],
                "{name} under {s:?}"
            );
        }
    }
}
