//! Live sound-type detection for reactions: microphone through PipeWire →
//! features → naive Bayes → confirmed class → `reactions::Command::Sound`.
//!
//! Audio is analysed in memory and dropped; nothing is recorded or sent
//! anywhere. Detection starts off and runs only while the person enables it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

use crate::reactions::Command;
use crate::sound::{
    BACKGROUND, Detector, Model, SAMPLE_RATE, SILENCE_DB, Stream, WINDOW_SAMPLES, argmax,
};

/// Windows are taken every 50 ms (5 frames). Frames are shared between
/// windows, so this costs less CPU than the 0.25 s hop used for training,
/// and a sound is noticed up to 200 ms sooner.
pub const HOP_FRAMES: usize = 5;
/// The plan's confirmation: the same class with p ≥ 0.7 in consecutive
/// windows, here spanning 0.25 s (6 windows 50 ms apart), the same time the
/// original 2 windows 0.25 s apart covered.
pub const MIN_P: f32 = 0.7;
pub const CONFIRM_SPAN: Duration = Duration::from_millis(250);
/// If analysis falls behind by more than this, the oldest audio is dropped so
/// a reaction never fires long after the sound (real-time over completeness).
pub const MAX_BACKLOG: Duration = Duration::from_millis(500);

/// Turns a stream of samples into fired classes.
pub struct Listener {
    model: Model,
    stream: Stream,
    detector: Detector,
    pub stats: Stats,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Most likely class in the last analysed window and its probability;
    /// `None` for silence.
    pub heard: Option<(String, f32)>,
    /// Slowest analysis of one window so far, and the latest.
    pub worst_window: Duration,
    pub last_window: Duration,
    /// Audio discarded because analysis fell behind.
    pub dropped: Duration,
}

impl Listener {
    pub fn new(model: Model) -> Self {
        Self::with_hop(model, HOP_FRAMES)
    }

    fn with_hop(model: Model, hop_frames: usize) -> Self {
        let stream = Stream::new(hop_frames, SILENCE_DB);
        let confirm = backlog_samples(CONFIRM_SPAN) / stream.hop_samples() + 1;
        let detector = Detector::new(&model.classes, &BACKGROUND, MIN_P, confirm);
        Self {
            model,
            stream,
            detector,
            stats: Stats::default(),
        }
    }

    pub fn classes(&self) -> &[String] {
        &self.model.classes
    }

    /// Adds captured samples; returns the classes that fired, in order.
    pub fn push(&mut self, samples: &[f32]) -> Vec<String> {
        let max = WINDOW_SAMPLES + backlog_samples(MAX_BACKLOG);
        let samples = if self.stream.pending() + samples.len() > max {
            // Too far behind: keep only the newest audio, as a new stream.
            let keep = samples.len().min(max);
            let drop = self.stream.pending() + samples.len() - keep;
            self.stats.dropped += Duration::from_secs_f64(drop as f64 / SAMPLE_RATE as f64);
            self.stream.reset();
            &samples[samples.len() - keep..]
        } else {
            samples
        };
        self.stream.extend(samples);
        let mut fired = Vec::new();
        loop {
            let started = Instant::now();
            let Some(features) = self.stream.next_window() else {
                break;
            };
            let post = features.map(|f| self.model.posteriors(&f));
            self.stats.heard = post.as_ref().map(|p| {
                let c = argmax(p);
                (self.model.classes[c].clone(), p[c])
            });
            if let Some(c) = self.detector.update(post.as_deref()) {
                fired.push(self.model.classes[c].clone());
            }
            self.stats.last_window = started.elapsed();
            self.stats.worst_window = self.stats.worst_window.max(self.stats.last_window);
        }
        fired
    }
}

fn backlog_samples(d: Duration) -> usize {
    (d.as_secs_f64() * SAMPLE_RATE as f64) as usize
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub running: bool,
    pub error: Option<String>,
    /// Classes that can fire a reaction (background classes left out).
    pub classes: Vec<String>,
    pub stats: Stats,
}

/// Dropping the handle stops capture.
pub struct Handle {
    stop: Arc<AtomicBool>,
    pub status: watch::Receiver<Status>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Classes a sentiment can be bound to: the model's, minus background ones.
pub fn reaction_classes(model: &Model) -> Vec<String> {
    model
        .classes
        .iter()
        .filter(|c| !BACKGROUND.contains(&c.as_str()))
        .cloned()
        .collect()
}

/// Starts listening on `source` (PipeWire node; `None` = default input).
pub fn start(
    model: Model,
    source: Option<String>,
    reactions: mpsc::UnboundedSender<Command>,
    notify: impl Fn() + Send + 'static,
) -> Handle {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, status) = watch::channel(Status {
        running: false,
        error: None,
        classes: reaction_classes(&model),
        stats: Stats::default(),
    });
    let flag = stop.clone();
    std::thread::Builder::new()
        .name("omatuber-voice".into())
        .spawn(move || {
            let result = listen(model, source, &flag, &reactions, &tx, &notify);
            tx.send_modify(|s| {
                s.running = false;
                s.error = result.err();
            });
            notify();
        })
        .expect("spawning a thread");
    Handle { stop, status }
}

fn listen(
    model: Model,
    source: Option<String>,
    stop: &AtomicBool,
    reactions: &mpsc::UnboundedSender<Command>,
    status: &watch::Sender<Status>,
    notify: &impl Fn(),
) -> Result<(), String> {
    let rx = crate::capture::start(source)?;
    let mut listener = Listener::new(model);
    status.send_modify(|s| s.running = true);
    notify();
    let mut last_report = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let chunk = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(chunk) => chunk,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return Err("a captura do PipeWire parou".to_owned());
            }
        };
        for class in listener.push(&chunk) {
            let _ = reactions.send(Command::Sound(class));
        }
        // The UI only needs a few updates per second.
        if last_report.elapsed() >= Duration::from_millis(250) {
            last_report = Instant::now();
            status.send_modify(|s| s.stats = listener.stats.clone());
            notify();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sound::Analyzer;

    /// Deterministic noise in [-amp, amp].
    fn noise(n: usize, amp: f32, seed: &mut u32) -> Vec<f32> {
        (0..n)
            .map(|_| {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 17;
                *seed ^= *seed << 5;
                (*seed as f32 / u32::MAX as f32 * 2.0 - 1.0) * amp
            })
            .collect()
    }

    fn tone(n: usize, hz: f32) -> Vec<f32> {
        (0..n)
            .map(|i| 0.3 * (i as f32 * hz * std::f32::consts::TAU / SAMPLE_RATE as f32).sin())
            .collect()
    }

    /// Model with a "risada" class (a 220 Hz tone stands in for it) and the
    /// "outros" background class (noise). Synthetic, only to drive the code.
    fn model() -> Model {
        let mut analyzer = Analyzer::new();
        let mut seed = 7;
        let mut examples = Vec::new();
        for f in analyzer.clip(&tone(SAMPLE_RATE * 3, 220.0), SILENCE_DB) {
            examples.push((0, f));
        }
        for f in analyzer.clip(&noise(SAMPLE_RATE * 3, 0.3, &mut seed), SILENCE_DB) {
            examples.push((1, f));
        }
        Model::train(vec!["risada".to_owned(), "outros".to_owned()], &examples).unwrap()
    }

    #[test]
    fn fires_once_for_a_sustained_trained_sound_and_never_for_background() {
        let mut l = Listener::new(model());
        let mut seed = 3;
        let fired = l.push(&noise(SAMPLE_RATE * 2, 0.3, &mut seed));
        assert!(fired.is_empty(), "background class never fires");
        // Fed in 10 ms chunks, as PipeWire would.
        let sound = tone(SAMPLE_RATE * 2, 220.0);
        let fired: Vec<String> = sound.chunks(160).flat_map(|c| l.push(c)).collect();
        assert_eq!(fired, ["risada"], "fires once per continuous sound");
        assert_eq!(
            l.stats.heard.as_ref().map(|(c, _)| c.as_str()),
            Some("risada")
        );
    }

    /// Seconds from the start of a trained sound to the reaction, for each
    /// onset 0–240 ms after a whole second of background noise (where the
    /// windows fall relative to the sound changes the delay).
    fn latencies(hop_frames: usize) -> Vec<f64> {
        let model = model();
        (0..25)
            .map(|offset| {
                let mut l = Listener::with_hop(model.clone(), hop_frames);
                let mut seed = 11;
                for c in noise(SAMPLE_RATE * 2 + offset * 160, 0.3, &mut seed).chunks(160) {
                    l.push(c);
                }
                let sound = tone(SAMPLE_RATE * 3, 220.0);
                let at = sound
                    .chunks(160)
                    .enumerate()
                    .find_map(|(i, c)| (!l.push(c).is_empty()).then_some((i + 1) * 160))
                    .expect("fires");
                at as f64 / SAMPLE_RATE as f64
            })
            .collect()
    }

    /// The plan estimated 0.5–1 s; this pins the design's actual figures and
    /// that the 50 ms hop is never slower than the 0.25 s one.
    #[test]
    fn detection_latency_from_sound_onset() {
        let stats = |v: &[f64]| {
            let mean = v.iter().sum::<f64>() / v.len() as f64;
            (mean, v.iter().copied().fold(0.0, f64::max))
        };
        let (old_mean, old_max) = stats(&latencies(25));
        let (mean, max) = stats(&latencies(HOP_FRAMES));
        eprintln!(
            "detection latency, 0.25 s hop: mean {old_mean:.3} s, max {old_max:.3} s; \
             {HOP_FRAMES}-frame hop: mean {mean:.3} s, max {max:.3} s"
        );
        assert!((0.5..=1.0).contains(&max), "{max}");
        assert!(mean < old_mean && max <= old_max);
    }

    #[test]
    fn silence_is_not_classified() {
        let mut l = Listener::new(model());
        assert!(l.push(&vec![0.0; SAMPLE_RATE * 2]).is_empty());
        assert_eq!(l.stats.heard, None);
    }

    #[test]
    fn falling_behind_drops_old_audio_to_stay_real_time() {
        let mut l = Listener::new(model());
        // 5 s arriving at once, e.g. after the thread was descheduled.
        l.push(&vec![0.0; SAMPLE_RATE * 5]);
        let kept = WINDOW_SAMPLES + backlog_samples(MAX_BACKLOG);
        let dropped_s = l.stats.dropped.as_secs_f64();
        assert!(
            dropped_s > 4.0 - 1e-3 - kept as f64 / SAMPLE_RATE as f64,
            "dropped {dropped_s}"
        );
        assert!(
            l.stream.pending() < WINDOW_SAMPLES,
            "everything left was analysed"
        );
    }

    #[test]
    fn background_classes_are_not_offered_for_binding() {
        assert_eq!(reaction_classes(&model()), ["risada"]);
    }
}
