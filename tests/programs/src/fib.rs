//! Recursive Fibonacci: call-heavy.
#[inline(never)]
fn fib(n: u32) -> u64 {
    if n < 2 { n as u64 } else { fib(n - 1) + fib(n - 2) }
}

fn main() {
    let n: u32 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(32);
    println!("fib({n}) = {}", fib(std::hint::black_box(n)));
}
