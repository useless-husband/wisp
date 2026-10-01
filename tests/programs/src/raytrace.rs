//! A small Whitted-style ray tracer: spheres, a checkered plane, shadows and reflections.
//! Uses only +, -, *, / and sqrt so the output is bit-identical on every platform.
#[derive(Clone, Copy)]
struct V(f64, f64, f64);

impl V {
    fn add(self, o: V) -> V { V(self.0 + o.0, self.1 + o.1, self.2 + o.2) }
    fn sub(self, o: V) -> V { V(self.0 - o.0, self.1 - o.1, self.2 - o.2) }
    fn mul(self, s: f64) -> V { V(self.0 * s, self.1 * s, self.2 * s) }
    fn dot(self, o: V) -> f64 { self.0 * o.0 + self.1 * o.1 + self.2 * o.2 }
    fn norm(self) -> V { self.mul(1.0 / self.dot(self).sqrt()) }
    fn hadamard(self, o: V) -> V { V(self.0 * o.0, self.1 * o.1, self.2 * o.2) }
}

struct Sphere { c: V, r: f64, color: V, refl: f64 }

fn pow16(x: f64) -> f64 { let x2 = x * x; let x4 = x2 * x2; let x8 = x4 * x4; x8 * x8 }

fn hit(spheres: &[Sphere], o: V, d: V) -> Option<(f64, usize)> {
    let mut best: Option<(f64, usize)> = None;
    for (i, s) in spheres.iter().enumerate() {
        let oc = o.sub(s.c);
        let b = oc.dot(d);
        let c = oc.dot(oc) - s.r * s.r;
        let disc = b * b - c;
        if disc > 0.0 {
            let t = -b - disc.sqrt();
            if t > 1e-6 && best.is_none_or(|(bt, _)| t < bt) {
                best = Some((t, i));
            }
        }
    }
    // Plane y = -1 as index usize::MAX.
    if d.1 < -1e-9 {
        let t = (-1.0 - o.1) / d.1;
        if t > 1e-6 && best.is_none_or(|(bt, _)| t < bt) {
            best = Some((t, usize::MAX));
        }
    }
    best
}

fn trace(spheres: &[Sphere], o: V, d: V, depth: u32) -> V {
    let light = V(-0.4, 0.9, -0.3).norm();
    let Some((t, i)) = hit(spheres, o, d) else {
        let k = 0.5 * (d.1 + 1.0);
        return V(1.0, 1.0, 1.0).mul(1.0 - k).add(V(0.5, 0.7, 1.0).mul(k));
    };
    let p = o.add(d.mul(t));
    let (n, color, refl) = if i == usize::MAX {
        let check = ((p.0.floor() + p.2.floor()) as i64).rem_euclid(2) == 0;
        (V(0.0, 1.0, 0.0), if check { V(0.9, 0.9, 0.9) } else { V(0.2, 0.2, 0.2) }, 0.3)
    } else {
        let s = &spheres[i];
        (p.sub(s.c).mul(1.0 / s.r), s.color, s.refl)
    };
    let shadow = hit(spheres, p.add(n.mul(1e-4)), light).is_some();
    let diff = if shadow { 0.0 } else { n.dot(light).max(0.0) };
    let h = light.sub(d).norm();
    let spec = if shadow { 0.0 } else { pow16(n.dot(h).max(0.0)) };
    let mut c = color.mul(0.1 + 0.9 * diff).add(V(1.0, 1.0, 1.0).mul(0.5 * spec));
    if depth < 4 && refl > 0.0 {
        let r = d.sub(n.mul(2.0 * d.dot(n)));
        let rc = trace(spheres, p.add(n.mul(1e-4)), r.norm(), depth + 1);
        c = c.mul(1.0 - refl).add(rc.hadamard(V(1.0, 1.0, 1.0)).mul(refl));
    }
    c
}

fn main() {
    let w: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(320);
    let h = w * 3 / 4;
    let spheres = [
        Sphere { c: V(0.0, 0.0, 4.0), r: 1.0, color: V(0.9, 0.2, 0.2), refl: 0.4 },
        Sphere { c: V(-2.1, -0.2, 5.0), r: 0.8, color: V(0.2, 0.9, 0.3), refl: 0.2 },
        Sphere { c: V(2.0, 0.3, 5.5), r: 1.2, color: V(0.3, 0.4, 0.95), refl: 0.6 },
        Sphere { c: V(0.6, -0.7, 2.6), r: 0.3, color: V(0.95, 0.85, 0.2), refl: 0.1 },
    ];
    let mut fnv = 0xcbf29ce484222325u64;
    let mut sum = 0u64;
    for y in 0..h {
        for x in 0..w {
            let mut acc = V(0.0, 0.0, 0.0);
            for s in 0..4 {
                let jx = (s % 2) as f64 * 0.5 + 0.25;
                let jy = (s / 2) as f64 * 0.5 + 0.25;
                let u = ((x as f64 + jx) / w as f64 - 0.5) * 2.0 * (w as f64 / h as f64);
                let v = (0.5 - (y as f64 + jy) / h as f64) * 2.0;
                acc = acc.add(trace(&spheres, V(0.0, 0.3, 0.0), V(u, v, 1.6).norm(), 0));
            }
            let c = acc.mul(0.25);
            for ch in [c.0, c.1, c.2] {
                let b = (ch.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                fnv = (fnv ^ b as u64).wrapping_mul(0x100000001b3);
                sum += b as u64;
            }
        }
    }
    println!("{w}x{h}: fnv {fnv:016x}, mean {:.6}", sum as f64 / (w * h * 3) as f64);
}
