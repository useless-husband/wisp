//! Arguments, environment, stdin, stdout/stderr and exit status.
use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!("hello, wasm! args={args:?}");
    let mut vars: Vec<(String, String)> = std::env::vars().filter(|(k, _)| k.starts_with("WISP_")).collect();
    vars.sort();
    for (k, v) in vars {
        println!("env {k}={v}");
    }
    if args.first().map(String::as_str) == Some("--stdin") {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).unwrap();
        println!("stdin: {} bytes, {} lines", s.len(), s.lines().count());
    }
    eprintln!("this goes to stderr");
    if let Some(pos) = args.iter().position(|a| a == "--exit") {
        let code: i32 = args[pos + 1].parse().unwrap();
        std::process::exit(code);
    }
}
