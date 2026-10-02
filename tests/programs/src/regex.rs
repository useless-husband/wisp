//! Regular-expression search with the `regex` crate (large module, Unicode tables).
use regex::Regex;

fn main() {
    let size: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(2 << 20);
    let words = ["alpha", "beta", "gamma", "delta", "user42", "x-ray", "café", "naïve", "2026-10-02", "id=17"];
    let mut seed = 99u64;
    let mut text = String::with_capacity(size + 32);
    while text.len() < size {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        text.push_str(words[((seed >> 40) as usize) % words.len()]);
        text.push(if (seed >> 20) % 11 == 0 { '\n' } else { ' ' });
    }
    let pats = [r"\b\w+\d+\b", r"\d{4}-\d{2}-\d{2}", r"(?i)ALPHA|GAMMA", r"\p{L}+é\p{L}*", r"id=(\d+)"];
    for p in pats {
        let re = Regex::new(p).unwrap();
        let n = re.find_iter(&text).count();
        println!("{p}: {n} matches");
    }
}
