//! Times the sound analyzer on synthetic audio, to check the live path keeps
//! up with real time: standalone windows every `WINDOW_HOP_SAMPLES` (as
//! training reads clips) and the live `Stream` every `voice::HOP_FRAMES`.
//!
//!     cargo run --release --features sound --example bench_sound

use std::time::Instant;

use omatuber::sound::{Analyzer, SAMPLE_RATE, Stream, WINDOW_HOP_SAMPLES, WINDOW_SAMPLES};
use omatuber::voice::HOP_FRAMES;

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

/// Fastest of 9 runs of `run`, which returns how many windows it analysed.
/// The minimum, not the median: on a busy desktop the other runs measure
/// mostly what else the machine was doing.
fn ms_per_window(mut run: impl FnMut() -> usize) -> f64 {
    let mut per_run: Vec<f64> = (0..9)
        .map(|_| {
            let start = Instant::now();
            let windows = run();
            start.elapsed().as_secs_f64() * 1e3 / windows as f64
        })
        .collect();
    per_run.sort_by(f64::total_cmp);
    per_run[0]
}

fn report(name: &str, hop_samples: usize, ms: f64) {
    let hop_ms = hop_samples as f64 * 1e3 / SAMPLE_RATE as f64;
    println!(
        "{name}: hop {hop_ms:.0} ms, {ms:.3} ms per window, {:.2}% of one core",
        ms / hop_ms * 100.0
    );
}

fn main() {
    let audio = signal(60);
    let mut analyzer = Analyzer::new();
    // Warm up allocations and caches.
    analyzer.window(&audio[..WINDOW_SAMPLES], -90.0);
    let windows = (audio.len() - WINDOW_SAMPLES) / WINDOW_HOP_SAMPLES + 1;
    let ms = ms_per_window(|| {
        for i in 0..windows {
            let s = i * WINDOW_HOP_SAMPLES;
            std::hint::black_box(analyzer.window(&audio[s..s + WINDOW_SAMPLES], -90.0));
        }
        windows
    });
    report("standalone", WINDOW_HOP_SAMPLES, ms);

    let mut hop = 0;
    let ms = ms_per_window(|| {
        let mut stream = Stream::new(HOP_FRAMES, -90.0);
        hop = stream.hop_samples();
        let mut windows = 0;
        // 10 ms chunks, as PipeWire delivers them.
        for chunk in audio.chunks(160) {
            stream.extend(chunk);
            while let Some(w) = stream.next_window() {
                std::hint::black_box(w);
                windows += 1;
            }
        }
        windows
    });
    report("live stream", hop, ms);
}
