//! Tries to reach files outside its preopened directory (the first argument).
//! The host test creates: <dir>/inside.txt, <dir>/sub/, <dir>/link_in -> inside.txt,
//! <dir>/escape -> ../outside (a sibling directory holding secret.txt), <dir>/abs -> /etc,
//! <dir>/sub/up -> ../../outside/secret.txt, and <dir>/loop -> loop.
use std::fs;

fn attempt(what: &str, r: std::io::Result<String>) {
    match r {
        Ok(s) => println!("{what}: READ {:?}", s.trim()),
        Err(e) => println!("{what}: denied ({:?})", e.kind()),
    }
}

fn main() {
    let d = std::env::args().nth(1).expect("directory argument");
    attempt("inside.txt", fs::read_to_string(format!("{d}/inside.txt")));
    attempt("sub/../inside.txt", fs::read_to_string(format!("{d}/sub/../inside.txt")));
    attempt("link_in (symlink inside)", fs::read_to_string(format!("{d}/link_in")));
    attempt("../outside/secret.txt", fs::read_to_string(format!("{d}/../outside/secret.txt")));
    attempt("sub/../../outside/secret.txt", fs::read_to_string(format!("{d}/sub/../../outside/secret.txt")));
    attempt("escape/secret.txt (dir symlink)", fs::read_to_string(format!("{d}/escape/secret.txt")));
    attempt("sub/up (file symlink with ..)", fs::read_to_string(format!("{d}/sub/up")));
    attempt("abs/hosts (absolute symlink)", fs::read_to_string(format!("{d}/abs/hosts")));
    attempt("/etc/hosts (no preopen)", fs::read_to_string("/etc/hosts"));
    attempt("loop (symlink loop)", fs::read_to_string(format!("{d}/loop")));
    let w = fs::write(format!("{d}/escape/planted.txt"), "x").map(|_| String::from("wrote"));
    attempt("write escape/planted.txt", w);
    let w = fs::create_dir(format!("{d}/../made-outside")).map(|_| String::from("created"));
    attempt("mkdir ../made-outside", w);
}
