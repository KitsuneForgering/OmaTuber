//! Sound-type probe: records labelled examples from the microphone through
//! PipeWire, trains a naive Bayes classifier on them, and shows what it hears
//! live.
//!
//!     sound-probe levels [seconds]
//!     sound-probe record <class> [clips] [seconds]
//!     sound-probe train
//!     sound-probe live [min_probability] [confirm_windows]
//!     sound-probe classify <file.wav>
//!
//! Examples are kept as 16 kHz mono WAV in `$OMATUBER_DATA/sounds/<class>/`
//! (default `$XDG_DATA_HOME/omatuber`, else `~/.local/share/omatuber`), so the
//! YAMNet comparison can read the same clips; the model goes to `bayes.json`
//! beside them. Nothing leaves the computer. `OMATUBER_SOURCE` picks a
//! PipeWire node (see `wpctl status`); unset means the default source.
//!
//! Suggested classes: fala, risada, susto, grito, suspiro and outros (typing,
//! clicks, music). `fala` and `outros` are background: they compete in the
//! classifier but never fire a reaction.

use std::env;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use omatuber::sound::{
    Analyzer, BACKGROUND, Detector, Model, SAMPLE_RATE, SILENCE_DB, WINDOW_HOP_SAMPLES,
    WINDOW_SAMPLES, argmax, data_dir, rms_db,
};
use omatuber::{capture, training};

const USAGE: &str = "usage: sound-probe levels [seconds] | record <class> [clips] [seconds] | train | \
                     live [min_probability] [confirm_windows] | classify <file.wav>";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["levels"] => levels(10.0),
        ["levels", secs] => parse(secs).and_then(levels),
        ["record", class] => record(class, 8, 2.0),
        ["record", class, clips] => parse(clips).and_then(|n| record(class, n, 2.0)),
        ["record", class, clips, secs] => {
            parse(clips).and_then(|n| parse(secs).and_then(|s| record(class, n, s)))
        }
        ["train"] => train(),
        ["live"] => live(0.7, 2),
        ["live", p] => parse(p).and_then(|p| live(p, 2)),
        ["live", p, n] => parse(p).and_then(|p| parse(n).and_then(|n| live(p, n))),
        ["classify", file] => classify(Path::new(file)),
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse<T: std::str::FromStr>(s: &str) -> Result<T, String> {
    s.parse().map_err(|_| format!("invalid number: {s}"))
}

fn open_mic() -> Result<Receiver<Vec<f32>>, String> {
    let rx = capture::start(env::var("OMATUBER_SOURCE").ok())?;
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(_) => Ok(rx),
        Err(_) => {
            Err("no audio from PipeWire after 3 s; check OMATUBER_SOURCE and `wpctl status`".into())
        }
    }
}

fn drain(rx: &Receiver<Vec<f32>>) {
    while rx.try_recv().is_ok() {}
}

/// Collects `n` samples, or fails if the stream stalls.
fn take(rx: &Receiver<Vec<f32>>, n: usize) -> Result<Vec<f32>, String> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let chunk = rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "audio stopped arriving from PipeWire".to_owned())?;
        out.extend(chunk);
    }
    out.truncate(n);
    Ok(out)
}

fn bar(p: f32, width: usize) -> String {
    let n = (p.clamp(0.0, 1.0) * width as f32).round() as usize;
    format!("{}{}", "█".repeat(n), "·".repeat(width - n))
}

fn levels(secs: f32) -> Result<(), String> {
    let rx = open_mic()?;
    println!("level every 0.25 s for {secs} s; silence gate is {SILENCE_DB} dBFS");
    let start = Instant::now();
    while start.elapsed().as_secs_f32() < secs {
        let block = take(&rx, WINDOW_HOP_SAMPLES)?;
        let db = rms_db(&block);
        let width = ((db + 70.0) / 70.0 * 40.0).clamp(0.0, 40.0) as usize;
        println!(
            "{:6.2}s {:6.1} dB {}",
            start.elapsed().as_secs_f32(),
            db,
            "█".repeat(width)
        );
    }
    Ok(())
}

fn record(class: &str, clips: usize, secs: f32) -> Result<(), String> {
    if !training::valid_class(class) {
        return Err(format!("class must be letters, digits, - or _: {class:?}"));
    }
    let data = data_dir()?;
    let rx = open_mic()?;
    let n = (secs * SAMPLE_RATE as f32) as usize;
    println!(
        "recording {clips} clips of {secs} s for \"{class}\" into {}",
        training::sounds_dir(&data).join(class).display()
    );
    for i in 1..=clips {
        for count in (1..=3).rev() {
            print!("\rclip {i}/{clips}: {count}… ");
            let _ = std::io::stdout().flush();
            thread::sleep(Duration::from_secs(1));
        }
        drain(&rx);
        print!("\rclip {i}/{clips}: NOW   ");
        let _ = std::io::stdout().flush();
        let samples = take(&rx, n)?;
        match training::save_clip(&data, class, &samples) {
            Ok(path) => println!(
                "\rclip {i}/{clips}: saved {} ({:.0} dB)",
                path.file_name().unwrap().to_string_lossy(),
                rms_db(&samples)
            ),
            Err(e) => println!("\rclip {i}/{clips}: {e}"),
        }
    }
    Ok(())
}

fn train() -> Result<(), String> {
    let data = data_dir()?;
    let report = training::train(&data)?;
    println!("leave-one-clip-out, per class (recognised / clips):");
    for (i, class) in report.classes.iter().enumerate() {
        println!(
            "{:>10}: {:3} / {:3}",
            truncate(class, 10),
            report.correct[i],
            report.clips[i]
        );
    }
    let (right, total) = report.right_total();
    println!(
        "accuracy: {right}/{total} clips ({:.0}%)",
        100.0 * right as f32 / total.max(1) as f32
    );
    for w in &report.warnings {
        println!("warning: {w}");
    }
    training::accept(&data, &report.model)?;
    println!("model saved to {}", training::model_path(&data).display());
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn load_model() -> Result<Model, String> {
    omatuber::sound::load_model().map_err(|e| format!("{e}; run `train` first"))
}

fn top(model: &Model, p: &[f32], n: usize) -> String {
    let mut idx: Vec<usize> = (0..p.len()).collect();
    idx.sort_by(|a, b| p[*b].total_cmp(&p[*a]));
    idx.iter()
        .take(n)
        .map(|&i| format!("{} {:.2}", model.classes[i], p[i]))
        .collect::<Vec<_>>()
        .join("  ")
}

fn live(min_p: f32, confirm: usize) -> Result<(), String> {
    let model = load_model()?;
    let rx = open_mic()?;
    let mut analyzer = Analyzer::new();
    let mut detector = Detector::new(&model.classes, &BACKGROUND, min_p, confirm);
    println!(
        "classes: {} · fires at p ≥ {min_p} for {confirm} window(s) · Ctrl+C to stop",
        model.classes.join(", ")
    );
    let start = Instant::now();
    let mut buf: Vec<f32> = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(chunk) => buf.extend(chunk),
            Err(RecvTimeoutError::Timeout) => {
                eprintln!("no audio for 2 s");
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return Err("PipeWire capture stopped".into()),
        }
        while buf.len() >= WINDOW_SAMPLES {
            let window = &buf[..WINDOW_SAMPLES];
            let t = start.elapsed().as_secs_f32();
            let db = rms_db(window);
            let post = analyzer
                .window(window, SILENCE_DB)
                .map(|f| model.posteriors(&f));
            match &post {
                None => println!("{t:7.2}s {db:6.1} dB  (silence)"),
                Some(p) => {
                    let best = argmax(p);
                    println!(
                        "{t:7.2}s {db:6.1} dB  {:>8} {} {:.2} | {}",
                        truncate(&model.classes[best], 8),
                        bar(p[best], 12),
                        p[best],
                        top(&model, p, 3)
                    );
                }
            }
            if let Some(c) = detector.update(post.as_deref()) {
                println!("          >>> {} <<<", model.classes[c].to_uppercase());
            }
            buf.drain(..WINDOW_HOP_SAMPLES);
        }
    }
}

fn classify(file: &Path) -> Result<(), String> {
    let model = load_model()?;
    let samples = training::read_wav(file)?;
    let mut analyzer = Analyzer::new();
    if samples.len() < WINDOW_SAMPLES {
        return Err("file is shorter than one 0.5 s window".into());
    }
    let mut kept = Vec::new();
    for i in 0..=(samples.len() - WINDOW_SAMPLES) / WINDOW_HOP_SAMPLES {
        let start = i * WINDOW_HOP_SAMPLES;
        let t = start as f32 / SAMPLE_RATE as f32;
        match analyzer.window(&samples[start..start + WINDOW_SAMPLES], SILENCE_DB) {
            None => println!("{t:6.2}s  (silence)"),
            Some(f) => {
                println!("{t:6.2}s  {}", top(&model, &model.posteriors(&f), 3));
                kept.push(f);
            }
        }
    }
    match model.posteriors_many(&kept) {
        Some(p) => println!("whole file: {}", top(&model, &p, 3)),
        None => println!("whole file: silence"),
    }
    Ok(())
}
