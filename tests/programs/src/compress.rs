//! DEFLATE round trip with miniz_oxide on generated text.
use miniz_oxide::deflate::compress_to_vec;
use miniz_oxide::inflate::decompress_to_vec;

fn main() {
    let size: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(4 << 20);
    let words = ["wasm", "runtime", "compiler", "the", "of", "stack", "memory", "table", "function", "a",
                 "branch", "loop", "value", "trap", "module", "and", "baseline", "register", "aarch64", "jit"];
    let mut seed = 42u64;
    let mut data = Vec::with_capacity(size + 16);
    while data.len() < size {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let w = words[((seed >> 40) as usize) % words.len()];
        data.extend_from_slice(w.as_bytes());
        data.push(if (seed >> 20) % 13 == 0 { b'\n' } else { b' ' });
    }
    data.truncate(size);
    let packed = compress_to_vec(&data, 6);
    let unpacked = decompress_to_vec(&packed).expect("inflate");
    assert_eq!(unpacked, data, "round trip");
    let mut h = 0xcbf29ce484222325u64;
    for b in &packed {
        h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
    }
    println!("{} -> {} bytes, round trip ok, fnv {h:016x}", data.len(), packed.len());
}
