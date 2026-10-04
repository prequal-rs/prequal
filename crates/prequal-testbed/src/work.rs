use std::{hint::black_box, time::Instant};

/// Pure CPU work: a dependent multiply/rotate chain the optimiser can't elide or vectorise.
pub fn burn(iterations: u64) -> u64 {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for i in 0..iterations {
        x = (x.rotate_left(5) ^ i).wrapping_mul(0x0100_0000_01B3);
    }
    black_box(x)
}

/// Iterations of [`burn`] per microsecond on one uncontended core, measured once at startup.
pub fn calibrate() -> f64 {
    burn(1_000_000);
    let iterations = 20_000_000;
    let start = Instant::now();
    burn(iterations);
    iterations as f64 / start.elapsed().as_micros().max(1) as f64
}
