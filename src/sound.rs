//! Sound-type detection: per-window audio features and a Gaussian naive Bayes
//! classifier trained on the user's own recordings.
//!
//! Input is 16 kHz mono `f32`. Frames follow YAMNet's STFT (25 ms periodic
//! Hann window, 10 ms hop, 512-point FFT) so both can be compared on the same
//! clips. A window of 0.5 s, taken every 0.25 s, becomes one feature vector;
//! windows quieter than a silence gate are skipped, not classified.

use std::collections::VecDeque;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use serde::{Deserialize, Serialize};

pub const SAMPLE_RATE: usize = 16_000;
const FRAME: usize = 400;
const HOP: usize = 160;
const FFT_LEN: usize = 512;
const BINS: usize = FFT_LEN / 2 + 1;
const MEL_BANDS: usize = 24;
const MEL_MIN_HZ: f32 = 125.0;
const MEL_MAX_HZ: f32 = 7500.0;
const WINDOW_FRAMES: usize = 50;
/// Samples that make one window.
pub const WINDOW_SAMPLES: usize = (WINDOW_FRAMES - 1) * HOP + FRAME;
/// Samples between the starts of consecutive windows.
pub const WINDOW_HOP_SAMPLES: usize = WINDOW_FRAMES / 2 * HOP;
/// Pitch search range, as autocorrelation lags: 500 Hz down to 70 Hz.
const PITCH_LAGS: std::ops::RangeInclusive<usize> = 32..=228;
/// Envelope rhythm range, in frames: 10 Hz down to 4 Hz (laughter is ~4–6 Hz).
const RHYTHM_LAGS: std::ops::RangeInclusive<usize> = 10..=25;
/// Default silence gate: window RMS below this is not classified.
pub const SILENCE_DB: f32 = -50.0;

pub const N_FEATURES: usize = MEL_BANDS + 12;

/// Classes that compete in the classifier but never fire a reaction: plain
/// speech and everyday noise (typing, clicks, music).
pub const BACKGROUND: [&str; 2] = ["fala", "outros"];

/// Where examples and the model live: `$OMATUBER_DATA`, else
/// `$XDG_DATA_HOME/omatuber`, else `~/.local/share/omatuber`.
pub fn data_dir() -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;
    if let Some(d) = std::env::var_os("OMATUBER_DATA") {
        return Ok(PathBuf::from(d));
    }
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d).join("omatuber"));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/share/omatuber"))
}

/// The trained model, as `sound-probe train` saves it.
pub fn load_model() -> Result<Model, String> {
    let path = data_dir()?.join("bayes.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Model::from_json(&text)
}
pub type Features = [f32; N_FEATURES];

struct Frame {
    log_mel: [f32; MEL_BANDS],
    db: f32,
    zcr: f32,
    centroid_khz: f32,
    flatness: f32,
    pitch_hz: Option<f32>,
}

pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    hann: Vec<f32>,
    mel: Vec<[f32; BINS]>,
    buf: Vec<Complex32>,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    pub fn new() -> Self {
        let hann = (0..FRAME)
            .map(|n| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / FRAME as f32).cos())
            .collect();
        Self {
            fft: FftPlanner::new().plan_fft_forward(FFT_LEN),
            hann,
            mel: mel_matrix(),
            buf: vec![Complex32::default(); FFT_LEN],
        }
    }

    /// Feature vectors for every window of a clip, skipping silent ones.
    pub fn clip(&mut self, samples: &[f32], silence_db: f32) -> Vec<Features> {
        if samples.len() < WINDOW_SAMPLES {
            return Vec::new();
        }
        (0..=(samples.len() - WINDOW_SAMPLES) / WINDOW_HOP_SAMPLES)
            .filter_map(|i| {
                let start = i * WINDOW_HOP_SAMPLES;
                self.window(&samples[start..start + WINDOW_SAMPLES], silence_db)
            })
            .collect()
    }

    /// Features of one window of `WINDOW_SAMPLES`, or `None` when its RMS is
    /// below `silence_db`.
    pub fn window(&mut self, samples: &[f32], silence_db: f32) -> Option<Features> {
        assert_eq!(samples.len(), WINDOW_SAMPLES);
        if rms_db(samples) < silence_db {
            return None;
        }
        let frames: Vec<Frame> = (0..WINDOW_FRAMES)
            .map(|i| self.frame(&samples[i * HOP..i * HOP + FRAME]))
            .collect();
        Some(summarize(&frames))
    }

    fn frame(&mut self, x: &[f32]) -> Frame {
        for (i, c) in self.buf.iter_mut().enumerate() {
            let v = if i < FRAME { x[i] * self.hann[i] } else { 0.0 };
            *c = Complex32::new(v, 0.0);
        }
        self.fft.process(&mut self.buf);
        let mag: Vec<f32> = self.buf[..BINS].iter().map(|c| c.norm()).collect();

        let mut log_mel = [0.0; MEL_BANDS];
        for (band, weights) in log_mel.iter_mut().zip(&self.mel) {
            let m: f32 = weights.iter().zip(&mag).map(|(w, m)| w * m).sum();
            *band = (m + 1e-3).ln();
        }

        let power: Vec<f32> = mag.iter().map(|m| m * m).collect();
        let total: f32 = power[1..].iter().sum::<f32>() + 1e-12;
        let centroid_hz = power[1..]
            .iter()
            .enumerate()
            .map(|(k, p)| bin_hz(k + 1) * p)
            .sum::<f32>()
            / total;
        let log_mean = power[1..].iter().map(|p| (p + 1e-12).ln()).sum::<f32>() / (BINS - 1) as f32;
        let flatness = log_mean.exp() / (total / (BINS - 1) as f32);

        let crossings = x
            .windows(2)
            .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
            .count();
        Frame {
            log_mel,
            db: rms_db(x),
            zcr: crossings as f32 / FRAME as f32,
            centroid_khz: centroid_hz / 1000.0,
            flatness,
            pitch_hz: pitch(x),
        }
    }
}

/// Windows over a continuous stream, `hop_frames` frames (10 ms each) apart.
///
/// Each frame is analysed once and reused by every window that contains it,
/// so a short hop costs little more than the 0.25 s one: per second of audio
/// at most 100 frames are analysed, plus one cheap summary per window. Frames
/// that only fall in silent windows are never analysed. A window's features
/// are exactly what `Analyzer::window` gives for the same samples.
pub struct Stream {
    analyzer: Analyzer,
    hop_frames: usize,
    silence_db: f32,
    /// Samples from the start of the next window on.
    buf: Vec<f32>,
    /// Absolute frame index where the next window starts.
    next: usize,
    /// Analysed frames, contiguous, starting at absolute index `first`.
    frames: VecDeque<Frame>,
    first: usize,
}

impl Stream {
    pub fn new(hop_frames: usize, silence_db: f32) -> Self {
        assert!((1..=WINDOW_FRAMES).contains(&hop_frames));
        Self {
            analyzer: Analyzer::new(),
            hop_frames,
            silence_db,
            buf: Vec::with_capacity(2 * WINDOW_SAMPLES),
            next: 0,
            frames: VecDeque::with_capacity(WINDOW_FRAMES),
            first: 0,
        }
    }

    /// Samples between the starts of consecutive windows.
    pub fn hop_samples(&self) -> usize {
        self.hop_frames * HOP
    }

    pub fn extend(&mut self, samples: &[f32]) {
        self.buf.extend_from_slice(samples);
    }

    /// Samples received but not yet behind a finished window.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// Forgets all audio; the next samples start a new stream. For gaps,
    /// such as audio dropped to catch up.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.frames.clear();
        self.next = 0;
        self.first = 0;
    }

    /// The next complete window, if enough samples arrived: `Some(None)` for
    /// silence, `Some(Some(features))` otherwise.
    pub fn next_window(&mut self) -> Option<Option<Features>> {
        if self.buf.len() < WINDOW_SAMPLES {
            return None;
        }
        let window = &self.buf[..WINDOW_SAMPLES];
        let features = (rms_db(window) >= self.silence_db).then(|| {
            // Keep the frames this window shares with earlier ones (windows
            // only move forward, so `next >= first`).
            let keep = (self.first + self.frames.len()).saturating_sub(self.next);
            self.frames.drain(..self.frames.len() - keep);
            self.first = self.next;
            for i in self.frames.len()..WINDOW_FRAMES {
                let frame = self.analyzer.frame(&window[i * HOP..i * HOP + FRAME]);
                self.frames.push_back(frame);
            }
            summarize(self.frames.make_contiguous())
        });
        self.next += self.hop_frames;
        self.buf.drain(..self.hop_samples());
        Some(features)
    }
}

fn bin_hz(k: usize) -> f32 {
    k as f32 * SAMPLE_RATE as f32 / FFT_LEN as f32
}

pub fn rms_db(x: &[f32]) -> f32 {
    let ms = x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32;
    10.0 * (ms + 1e-10).log10()
}

fn hz_to_mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

/// Triangular mel filters on the HTK scale, as `tf.signal.linear_to_mel_weight_matrix`
/// builds them: the DC bin gets no weight.
fn mel_matrix() -> Vec<[f32; BINS]> {
    let lo = hz_to_mel(MEL_MIN_HZ);
    let hi = hz_to_mel(MEL_MAX_HZ);
    let edges: Vec<f32> = (0..MEL_BANDS + 2)
        .map(|i| lo + (hi - lo) * i as f32 / (MEL_BANDS + 1) as f32)
        .collect();
    (0..MEL_BANDS)
        .map(|b| {
            let (l, c, u) = (edges[b], edges[b + 1], edges[b + 2]);
            let mut row = [0.0; BINS];
            for (k, w) in row.iter_mut().enumerate().skip(1) {
                let m = hz_to_mel(bin_hz(k));
                *w = ((m - l) / (c - l)).min((u - m) / (u - c)).max(0.0);
            }
            row
        })
        .collect()
}

/// Fundamental frequency by normalized autocorrelation, if the frame is
/// clearly periodic within the voice range.
fn pitch(x: &[f32]) -> Option<f32> {
    let mean = x.iter().sum::<f32>() / x.len() as f32;
    let x: Vec<f32> = x.iter().map(|v| v - mean).collect();
    // Energy of every prefix, so each lag's two energies are a subtraction
    // instead of two more passes over the frame (the lag loop dominates the
    // whole analysis).
    let mut prefix = Vec::with_capacity(x.len() + 1);
    prefix.push(0.0_f64);
    for v in &x {
        prefix.push(prefix[prefix.len() - 1] + f64::from(v * v));
    }
    let n = x.len();
    let r: Vec<f32> = PITCH_LAGS
        .map(|lag| {
            let (a, b) = (&x[..n - lag], &x[lag..]);
            let num: f32 = a.iter().zip(b).map(|(p, q)| p * q).sum();
            let den = ((prefix[n - lag] * (prefix[n] - prefix[lag])).sqrt()) as f32;
            if den > 1e-9 { num / den } else { 0.0 }
        })
        .collect();
    let best = r.iter().copied().fold(0.0, f32::max);
    if best <= 0.5 {
        return None;
    }
    // Multiples of the period correlate almost as well; take the shortest
    // lag close to the best one to avoid reporting an octave too low.
    let i = r.iter().position(|&v| v >= 0.9 * best)?;
    let i = (i..r.len())
        .take_while(|&j| j == i || r[j] >= r[j - 1])
        .last()?;
    Some(SAMPLE_RATE as f32 / (PITCH_LAGS.start() + i) as f32)
}

fn mean_std(v: impl Iterator<Item = f32> + Clone) -> (f32, f32) {
    let n = v.clone().count().max(1) as f32;
    let mean = v.clone().sum::<f32>() / n;
    let var = v.map(|x| (x - mean).powi(2)).sum::<f32>() / n;
    (mean, var.sqrt())
}

fn summarize(frames: &[Frame]) -> Features {
    let mut f = [0.0; N_FEATURES];

    // Spectral shape: mean log-mel per band, relative to the window's mean so
    // that loudness is carried separately.
    let n = frames.len() as f32;
    let mut bands = [0.0; MEL_BANDS];
    for fr in frames {
        for (b, v) in bands.iter_mut().zip(fr.log_mel) {
            *b += v / n;
        }
    }
    let overall = bands.iter().sum::<f32>() / MEL_BANDS as f32;
    for (dst, b) in f.iter_mut().zip(bands) {
        *dst = b - overall;
    }

    let db = frames.iter().map(|fr| fr.db);
    let (db_mean, db_std) = mean_std(db.clone());
    let db_max = db.clone().fold(f32::MIN, f32::max);
    let db_min = db.fold(f32::MAX, f32::min);
    let onset = frames
        .windows(2)
        .map(|w| w[1].db - w[0].db)
        .fold(0.0_f32, f32::max);
    let (zcr_mean, zcr_std) = mean_std(frames.iter().map(|fr| fr.zcr));
    let (cen_mean, cen_std) = mean_std(frames.iter().map(|fr| fr.centroid_khz));
    let (flat_mean, _) = mean_std(frames.iter().map(|fr| fr.flatness));
    let voiced: Vec<f32> = frames.iter().filter_map(|fr| fr.pitch_hz).collect();
    let pitch_log2 = if voiced.is_empty() {
        7.0
    } else {
        voiced.iter().map(|p| p.log2()).sum::<f32>() / voiced.len() as f32
    };

    let rest = [
        db_mean,
        db_std,
        db_max - db_min,
        onset,
        zcr_mean,
        zcr_std,
        cen_mean,
        cen_std,
        flat_mean,
        rhythm(frames),
        voiced.len() as f32 / n,
        pitch_log2,
    ];
    f[MEL_BANDS..].copy_from_slice(&rest);
    f
}

/// How strongly the loudness envelope repeats at 4–10 Hz: the peak of its
/// normalized autocorrelation in that range.
fn rhythm(frames: &[Frame]) -> f32 {
    let (mean, _) = mean_std(frames.iter().map(|fr| fr.db));
    let e: Vec<f32> = frames.iter().map(|fr| fr.db - mean).collect();
    let energy: f32 = e.iter().map(|v| v * v).sum();
    if energy < 1e-6 {
        return 0.0;
    }
    RHYTHM_LAGS
        .map(|lag| e.iter().zip(&e[lag..]).map(|(a, b)| a * b).sum::<f32>() / energy)
        .fold(0.0, f32::max)
}

// ------------------------------------------------------------------- classifier

/// Gaussian naive Bayes over `Features`, with equal class priors.
///
/// The features are correlated (neighbouring mel bands especially), which
/// makes plain naive Bayes overconfident. Log-likelihoods are divided by
/// `temperature` before turning them into probabilities.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub classes: Vec<String>,
    pub temperature: f32,
    n_features: usize,
    means: Vec<Vec<f32>>,
    vars: Vec<Vec<f32>>,
}

/// Share of each feature's overall variance added to every class variance,
/// so a feature that barely varies in a few examples cannot dominate.
const VAR_SMOOTHING: f32 = 0.05;
pub const DEFAULT_TEMPERATURE: f32 = 6.0;

impl Model {
    /// Trains from `(class index, features)` pairs. Every class needs at least
    /// two examples.
    pub fn train(classes: Vec<String>, examples: &[(usize, Features)]) -> Result<Model, String> {
        let k = classes.len();
        let mut counts = vec![0usize; k];
        let mut means = vec![vec![0.0; N_FEATURES]; k];
        for (c, x) in examples {
            counts[*c] += 1;
            for (m, v) in means[*c].iter_mut().zip(x) {
                *m += v;
            }
        }
        if let Some(c) = (0..k).find(|&c| counts[c] < 2) {
            return Err(format!(
                "class \"{}\" has {} non-silent windows; record more",
                classes[c], counts[c]
            ));
        }
        for (m, n) in means.iter_mut().zip(&counts) {
            m.iter_mut().for_each(|v| *v /= *n as f32);
        }
        let all = examples.len() as f32;
        let global_mean: Vec<f32> = (0..N_FEATURES)
            .map(|j| examples.iter().map(|(_, x)| x[j]).sum::<f32>() / all)
            .collect();
        let global_var: Vec<f32> = (0..N_FEATURES)
            .map(|j| {
                examples
                    .iter()
                    .map(|(_, x)| (x[j] - global_mean[j]).powi(2))
                    .sum::<f32>()
                    / all
            })
            .collect();
        let mut vars = vec![vec![0.0; N_FEATURES]; k];
        for (c, x) in examples {
            for j in 0..N_FEATURES {
                vars[*c][j] += (x[j] - means[*c][j]).powi(2) / counts[*c] as f32;
            }
        }
        for row in &mut vars {
            for (v, g) in row.iter_mut().zip(&global_var) {
                *v += VAR_SMOOTHING * g + 1e-6;
            }
        }
        Ok(Model {
            classes,
            temperature: DEFAULT_TEMPERATURE,
            n_features: N_FEATURES,
            means,
            vars,
        })
    }

    pub fn from_json(text: &str) -> Result<Model, String> {
        let m: Model = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if m.n_features != N_FEATURES {
            return Err(format!(
                "model has {} features, this build computes {N_FEATURES}; train again",
                m.n_features
            ));
        }
        Ok(m)
    }

    fn log_likelihoods(&self, x: &Features) -> Vec<f32> {
        self.means
            .iter()
            .zip(&self.vars)
            .map(|(mean, var)| {
                let ll: f32 = x
                    .iter()
                    .zip(mean)
                    .zip(var)
                    .map(|((x, m), v)| {
                        -0.5 * ((x - m).powi(2) / v + (2.0 * std::f32::consts::PI * v).ln())
                    })
                    .sum();
                ll / self.temperature
            })
            .collect()
    }

    /// Class probabilities for one window.
    pub fn posteriors(&self, x: &Features) -> Vec<f32> {
        softmax(&self.log_likelihoods(x))
    }

    /// Class probabilities for several windows of one sound, combined.
    pub fn posteriors_many(&self, xs: &[Features]) -> Option<Vec<f32>> {
        let mut sum = vec![0.0; self.classes.len()];
        for x in xs {
            for (s, l) in sum.iter_mut().zip(self.log_likelihoods(x)) {
                *s += l;
            }
        }
        (!xs.is_empty()).then(|| softmax(&sum))
    }
}

fn softmax(logits: &[f32]) -> Vec<f32> {
    let max = logits.iter().copied().fold(f32::MIN, f32::max);
    let exp: Vec<f32> = logits.iter().map(|l| (l - max).exp()).collect();
    let total: f32 = exp.iter().sum();
    exp.iter().map(|e| e / total).collect()
}

pub fn argmax(p: &[f32]) -> usize {
    p.iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map_or(0, |(i, _)| i)
}

/// Leave-one-clip-out evaluation: each clip is classified by a model trained
/// on all the others. Returns `confusion[true][predicted]`; clips with no
/// non-silent window are left out.
pub fn leave_one_out(
    classes: &[String],
    clips: &[(usize, Vec<Features>)],
) -> Result<Vec<Vec<usize>>, String> {
    let k = classes.len();
    let mut confusion = vec![vec![0; k]; k];
    for (i, (truth, windows)) in clips.iter().enumerate() {
        let train: Vec<(usize, Features)> = clips
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .flat_map(|(_, (c, ws))| ws.iter().map(move |w| (*c, *w)))
            .collect();
        let model = Model::train(classes.to_vec(), &train)?;
        if let Some(p) = model.posteriors_many(windows) {
            confusion[*truth][argmax(&p)] += 1;
        }
    }
    Ok(confusion)
}

/// Turns per-window probabilities into events: a class fires once after it
/// has been the most likely one, above `min_p`, for `confirm` windows in a row.
/// Background classes (speech, other noises) never fire.
pub struct Detector {
    pub min_p: f32,
    pub confirm: usize,
    background: Vec<bool>,
    current: Option<usize>,
    run: usize,
}

impl Detector {
    pub fn new(classes: &[String], background: &[&str], min_p: f32, confirm: usize) -> Self {
        Self {
            min_p,
            confirm: confirm.max(1),
            background: classes
                .iter()
                .map(|c| background.contains(&c.as_str()))
                .collect(),
            current: None,
            run: 0,
        }
    }

    /// Feeds one window (`None` for silence); returns a class index when it fires.
    pub fn update(&mut self, posteriors: Option<&[f32]>) -> Option<usize> {
        let top = posteriors.and_then(|p| {
            let c = argmax(p);
            (p[c] >= self.min_p && !self.background[c]).then_some(c)
        });
        if top.is_some() && top == self.current {
            self.run += 1;
        } else {
            self.current = top;
            self.run = usize::from(top.is_some());
        }
        (self.run == self.confirm).then_some(self.current).flatten()
    }

    /// The class that fired and is still confirmed window after window, with
    /// how many windows since it fired (1 = the window after firing).
    pub fn holding(&self) -> Option<(usize, usize)> {
        self.current
            .filter(|_| self.run > self.confirm)
            .map(|c| (c, self.run - self.confirm))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic noise in [-1, 1).
    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect()
    }

    fn sawtooth(hz: f32, n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * ((i as f32 * hz / SAMPLE_RATE as f32) % 1.0) - 1.0))
            .collect()
    }

    #[test]
    fn mel_filters_are_triangles_without_dc() {
        let mel = mel_matrix();
        assert_eq!(mel.len(), MEL_BANDS);
        for row in &mel {
            assert_eq!(row[0], 0.0);
            let peak = row.iter().copied().fold(0.0, f32::max);
            assert!(peak > 0.5 && peak <= 1.0, "peak {peak}");
        }
        // Bands rise in frequency.
        let centers: Vec<usize> = mel.iter().map(|r| argmax(r)).collect();
        assert!(centers.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn silence_is_skipped() {
        let mut a = Analyzer::new();
        assert!(a.window(&vec![0.0; WINDOW_SAMPLES], SILENCE_DB).is_none());
        let quiet: Vec<f32> = noise(WINDOW_SAMPLES, 1).iter().map(|v| v * 1e-4).collect();
        assert!(a.window(&quiet, SILENCE_DB).is_none());
    }

    #[test]
    fn voiced_sawtooth_has_pitch_and_noise_does_not() {
        let mut a = Analyzer::new();
        let saw = a
            .window(&sawtooth(200.0, WINDOW_SAMPLES, 0.3), SILENCE_DB)
            .unwrap();
        let voiced = saw[MEL_BANDS + 10];
        let pitch = 2f32.powf(saw[MEL_BANDS + 11]);
        assert!(voiced > 0.9, "voiced {voiced}");
        assert!((pitch - 200.0).abs() < 10.0, "pitch {pitch}");

        let hiss = a.window(&noise(WINDOW_SAMPLES, 7), SILENCE_DB).unwrap();
        assert!(hiss[MEL_BANDS + 10] < 0.1);
        // Noise is flatter and brighter than the sawtooth.
        assert!(hiss[MEL_BANDS + 8] > saw[MEL_BANDS + 8]);
        assert!(hiss[MEL_BANDS + 6] > saw[MEL_BANDS + 6]);
    }

    #[test]
    fn rhythm_sees_five_hertz_bursts() {
        let mut a = Analyzer::new();
        let steady = noise(WINDOW_SAMPLES, 3);
        let bursts: Vec<f32> = steady
            .iter()
            .enumerate()
            // 0.1 s on, 0.1 s off: a 5 Hz envelope.
            .map(|(i, v)| {
                if (i * 10 / SAMPLE_RATE).is_multiple_of(2) {
                    *v
                } else {
                    v * 0.05
                }
            })
            .collect();
        let r_steady = a.window(&steady, SILENCE_DB).unwrap()[MEL_BANDS + 9];
        let r_bursts = a.window(&bursts, SILENCE_DB).unwrap()[MEL_BANDS + 9];
        assert!(
            r_bursts > 0.3 && r_bursts > r_steady + 0.2,
            "{r_bursts} vs {r_steady}"
        );
    }

    #[test]
    fn clip_counts_windows() {
        let mut a = Analyzer::new();
        let two_s = noise(2 * SAMPLE_RATE, 9);
        let n = (2 * SAMPLE_RATE - WINDOW_SAMPLES) / WINDOW_HOP_SAMPLES + 1;
        assert_eq!(a.clip(&two_s, SILENCE_DB).len(), n);
        assert!(a.clip(&two_s[..WINDOW_SAMPLES - 1], SILENCE_DB).is_empty());
    }

    /// The live path must see exactly what training saw, whatever the hop
    /// and however the audio is chunked, across silence and sound.
    #[test]
    fn stream_windows_equal_standalone_windows() {
        let mut audio = sawtooth(180.0, SAMPLE_RATE, 0.3);
        audio.extend(vec![0.0; SAMPLE_RATE]);
        audio.extend(noise(SAMPLE_RATE, 4).iter().map(|v| v * 0.2));
        let mut a = Analyzer::new();
        for hop_frames in [1, 5, 25, WINDOW_FRAMES] {
            let mut s = Stream::new(hop_frames, SILENCE_DB);
            let mut got = Vec::new();
            for chunk in audio.chunks(237) {
                s.extend(chunk);
                while let Some(w) = s.next_window() {
                    got.push(w);
                }
            }
            let hop = hop_frames * HOP;
            let want: Vec<_> = (0..=(audio.len() - WINDOW_SAMPLES) / hop)
                .map(|i| a.window(&audio[i * hop..i * hop + WINDOW_SAMPLES], SILENCE_DB))
                .collect();
            assert_eq!(got, want, "hop of {hop_frames} frames");
            assert!(got.iter().any(Option::is_none) && got.iter().any(Option::is_some));
        }
    }

    fn clips() -> (Vec<String>, Vec<(usize, Vec<Features>)>) {
        let mut a = Analyzer::new();
        let classes = vec!["tom".to_owned(), "chiado".to_owned()];
        let mut clips = Vec::new();
        for i in 0..4 {
            let hz = 150.0 + 20.0 * i as f32;
            clips.push((0, a.clip(&sawtooth(hz, SAMPLE_RATE, 0.3), SILENCE_DB)));
            let hiss: Vec<f32> = noise(SAMPLE_RATE, i + 20).iter().map(|v| v * 0.3).collect();
            clips.push((1, a.clip(&hiss, SILENCE_DB)));
        }
        (classes, clips)
    }

    #[test]
    fn separates_tone_from_hiss() {
        let (classes, clips) = clips();
        let confusion = leave_one_out(&classes, &clips).unwrap();
        assert_eq!(confusion, vec![vec![4, 0], vec![0, 4]]);
    }

    #[test]
    fn posteriors_sum_to_one_and_survive_json() {
        let (classes, clips) = clips();
        let train: Vec<(usize, Features)> = clips
            .iter()
            .flat_map(|(c, ws)| ws.iter().map(move |w| (*c, *w)))
            .collect();
        let model = Model::train(classes, &train).unwrap();
        let p = model.posteriors(&train[0].1);
        assert!((p.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        let back = Model::from_json(&serde_json::to_string(&model).unwrap()).unwrap();
        assert_eq!(back, model);
    }

    #[test]
    fn training_needs_two_examples_per_class() {
        let classes = vec!["a".to_owned(), "b".to_owned()];
        let x = [0.0; N_FEATURES];
        let err = Model::train(classes, &[(0, x), (0, x), (1, x)]).unwrap_err();
        assert!(err.contains("\"b\""), "{err}");
    }

    #[test]
    fn detector_fires_once_after_confirmation() {
        let classes: Vec<String> = ["fala", "risada"].map(String::from).to_vec();
        let mut d = Detector::new(&classes, &["fala"], 0.7, 2);
        let laugh = [0.1, 0.9];
        let speech = [0.9, 0.1];
        assert_eq!(d.update(Some(&laugh)), None);
        assert_eq!(d.update(Some(&laugh)), Some(1));
        assert_eq!(d.holding(), None, "just fired");
        assert_eq!(d.update(Some(&laugh)), None);
        assert_eq!(d.holding(), Some((1, 1)));
        assert_eq!(d.update(Some(&speech)), None);
        assert_eq!(d.holding(), None);
        assert_eq!(d.update(None), None);
        assert_eq!(d.update(Some(&laugh)), None);
        assert_eq!(d.update(Some(&[0.4, 0.6])), None);
    }
}
