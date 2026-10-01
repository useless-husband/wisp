//! Sieve of Eratosthenes: memory-bound integer work.
fn main() {
    let n: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(10_000_000);
    let mut composite = vec![false; n + 1];
    let mut count = 0u64;
    let mut sum = 0u64;
    let mut i = 2;
    while i <= n {
        if !composite[i] {
            count += 1;
            sum = sum.wrapping_add(i as u64);
            // `i * i` would overflow a 32-bit usize on wasm32 for large i.
            let mut j = if i <= n / i { i * i } else { n + 1 };
            while j <= n {
                composite[j] = true;
                j += i;
            }
        }
        i += 1;
    }
    println!("primes below {n}: {count}, sum {sum}");
}
