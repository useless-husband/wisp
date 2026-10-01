//! File system calls in a preopened directory (given as the first argument).
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};

fn main() {
    let root = std::env::args().nth(1).expect("directory argument");
    let p = |s: &str| format!("{root}/{s}");
    fs::create_dir_all(p("work/nested/deep")).unwrap();
    fs::write(p("work/a.txt"), b"hello file system\n").unwrap();
    let mut f = fs::OpenOptions::new().append(true).open(p("work/a.txt")).unwrap();
    f.write_all(b"appended line\n").unwrap();
    drop(f);
    println!("a.txt: {:?}", fs::read_to_string(p("work/a.txt")).unwrap());

    let mut f = fs::File::open(p("work/a.txt")).unwrap();
    f.seek(SeekFrom::Start(6)).unwrap();
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf).unwrap();
    println!("bytes 6..10: {:?}, position {}", std::str::from_utf8(&buf).unwrap(), f.stream_position().unwrap());
    println!("len {}", f.metadata().unwrap().len());

    let big: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
    fs::write(p("work/nested/big.bin"), &big).unwrap();
    let back = fs::read(p("work/nested/big.bin")).unwrap();
    println!("big.bin: {} bytes, equal {}", back.len(), back == big);

    for i in 0..5 {
        fs::write(p(&format!("work/nested/deep/f{i}.txt")), format!("{i}")).unwrap();
    }
    let mut names: Vec<String> =
        fs::read_dir(p("work/nested/deep")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    println!("deep: {names:?}");

    fs::rename(p("work/a.txt"), p("work/b.txt")).unwrap();
    println!("a.txt exists {}, b.txt exists {}", fs::metadata(p("work/a.txt")).is_ok(), fs::metadata(p("work/b.txt")).is_ok());
    let md = fs::metadata(p("work/nested")).unwrap();
    println!("nested is_dir {}", md.is_dir());
    fs::write(p("work/trunc.txt"), b"0123456789").unwrap();
    let f = fs::OpenOptions::new().write(true).open(p("work/trunc.txt")).unwrap();
    f.set_len(3).unwrap();
    drop(f);
    println!("trunc.txt: {:?}", fs::read_to_string(p("work/trunc.txt")).unwrap());
    match fs::read(p("work/missing.txt")) {
        Ok(_) => println!("missing.txt: unexpectedly present"),
        Err(e) => println!("missing.txt: {:?}", e.kind()),
    }
    match fs::create_dir(p("work/nested")) {
        Ok(_) => println!("create existing dir: ok?"),
        Err(e) => println!("create existing dir: {:?}", e.kind()),
    }
    fs::remove_dir_all(p("work")).unwrap();
    println!("cleaned up: {}", fs::metadata(p("work")).is_err());
}
