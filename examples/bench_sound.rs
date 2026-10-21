//! Times the sound analyzer on synthetic audio, to check the live path keeps
//! up with real time. One window is analysed every `WINDOW_HOP_SAMPLES`.
//!
//!     cargo run --release --features sound --example bench_sound

use std::time::Instant;

use omatuber::sound::{Analyzer, SAMPLE_RATE, WINDOW_HOP_SAMPLES, WINDOW_SAMPLES};

/// Voiced-like signal: 180 Hz sawtooth with a 5 Hz amplitude wobble, plus
/// deterministic noise so pitch search and FFT do realistic work.
fn signal(seconds: usize) -> Vec<f32> {
    let mut seed = 0x9e37_79b9_u32;
    (0..seconds * SAMPLE_RATE)
        .map(|n| {
            let t = n as f32 / SAMPLE_RATE as f32;
            let saw = 2.0 * (t * 180.0).fract() - 1.0;
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed as f32 / u32::MAX as f32) - 0.5;
            0.3 * saw * (0.6 + 0.4 * (t * 5.0 * std::f32::consts::TAU).sin()) + 0.05 * noise
        })
        .collect()
}

fn main() {
    let audio = signal(60);
    let mut analyzer = Analyzer::new();
    let windows = (audio.len() - WINDOW_SAMPLES) / WINDOW_HOP_SAMPLES + 1;
    // Warm up allocations and caches.
    analyzer.window(&audio[..WINDOW_SAMPLES], -90.0);

    let mut per_run = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        for i in 0..windows {
            let s = i * WINDOW_HOP_SAMPLES;
            std::hint::black_box(analyzer.window(&audio[s..s + WINDOW_SAMPLES], -90.0));
        }
        per_run.push(start.elapsed().as_secs_f64() * 1e3 / windows as f64);
    }
    per_run.sort_by(f64::total_cmp);
    let hop_ms = WINDOW_HOP_SAMPLES as f64 * 1e3 / SAMPLE_RATE as f64;
    let median = per_run[per_run.len() / 2];
    println!("windows per run: {windows}, hop {hop_ms:.0} ms");
    println!("ms per window: min {:.3}, median {median:.3}, max {:.3}", per_run[0], per_run[4]);
    println!("CPU share of one core at real time: {:.2}%", median / hop_ms * 100.0);
}
