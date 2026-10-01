//! Dense f64 matrix multiplication: floating point and memory access.
fn main() {
    let n: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(256);
    let mut seed = 12345u64;
    let mut rnd = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) as f64) / (1u64 << 31) as f64 - 0.5
    };
    let a: Vec<f64> = (0..n * n).map(|_| rnd()).collect();
    let b: Vec<f64> = (0..n * n).map(|_| rnd()).collect();
    let mut c = vec![0.0f64; n * n];
    for i in 0..n {
        for k in 0..n {
            let aik = a[i * n + k];
            for j in 0..n {
                c[i * n + j] += aik * b[k * n + j];
            }
        }
    }
    let trace: f64 = (0..n).map(|i| c[i * n + i]).sum();
    let sum: f64 = c.iter().sum();
    println!("{n}x{n}: trace {trace:.9} sum {sum:.9}");
}
