//! Teaching sounds: example clips per class, training with an honest
//! evaluation, and swapping in the new model only when accepted.
//!
//! Examples are 16 kHz mono WAV files in `<data>/sounds/<class>/NNN.wav` and
//! never leave the computer. The model is `<data>/bayes.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::sound::{
    Analyzer, BACKGROUND, Features, Model, SAMPLE_RATE, SILENCE_DB, leave_one_out, rms_db,
};

/// Below this, an evaluation says little and the class is flagged.
pub const MIN_CLIPS: usize = 3;
/// A reaction class recognised in fewer of its own clips is flagged as
/// unreliable; the plan suggests a manual trigger for it.
pub const MIN_RECALL: f32 = 0.6;

pub fn sounds_dir(data: &Path) -> PathBuf {
    data.join("sounds")
}

pub fn model_path(data: &Path) -> PathBuf {
    data.join("bayes.json")
}

/// Class names become directory names: letters, digits, `-` and `_` only.
pub fn valid_class(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

/// Clip files per class, sorted; classes without clips are left out.
pub fn clips(data: &Path) -> BTreeMap<String, Vec<PathBuf>> {
    let mut out = BTreeMap::new();
    let Ok(entries) = fs::read_dir(sounds_dir(data)) else {
        return out;
    };
    for class in entries.filter_map(Result::ok).filter(|e| e.path().is_dir()) {
        let mut files: Vec<PathBuf> = fs::read_dir(class.path())
            .into_iter()
            .flatten()
            .filter_map(|e| Some(e.ok()?.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "wav"))
            .collect();
        files.sort();
        if !files.is_empty() {
            out.insert(class.file_name().to_string_lossy().into_owned(), files);
        }
    }
    out
}

/// Saves one example; refuses a clip with nothing above the silence gate,
/// since it would teach nothing.
pub fn save_clip(data: &Path, class: &str, samples: &[f32]) -> Result<PathBuf, String> {
    if !valid_class(class) {
        return Err(format!(
            "nome de som inválido: {class:?} (use letras, números, - ou _)"
        ));
    }
    if Analyzer::new().clip(samples, SILENCE_DB).is_empty() {
        return Err(format!(
            "baixo demais ({:.0} dB); nada foi salvo",
            rms_db(samples)
        ));
    }
    let dir = sounds_dir(data).join(class);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let next = fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok()?.path().file_stem()?.to_str()?.parse::<usize>().ok())
        .max()
        .map_or(1, |n| n + 1);
    let path = dir.join(format!("{next:03}.wav"));
    write_wav(&path, samples)?;
    Ok(path)
}

/// Deletes the most recent example of a class, e.g. a botched take.
pub fn remove_last(data: &Path, class: &str) -> Result<(), String> {
    let last = clips(data).remove(class).and_then(|mut f| f.pop());
    match last {
        Some(path) => fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display())),
        None => Ok(()),
    }
}

pub fn write_wav(path: &Path, samples: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE as u32,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())
}

pub fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
    let mut r = hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = r.spec();
    if spec.sample_rate != SAMPLE_RATE as u32 || spec.channels != 1 {
        return Err(format!(
            "{}: need 16 kHz mono, got {} Hz × {} channels; convert with \
             `ffmpeg -i in -ar 16000 -ac 1 out.wav`",
            path.display(),
            spec.sample_rate,
            spec.channels
        ));
    }
    match spec.sample_format {
        hound::SampleFormat::Float => r
            .samples::<f32>()
            .map(|s| s.map_err(|e| e.to_string()))
            .collect(),
        hound::SampleFormat::Int => {
            let scale = (1u32 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale).map_err(|e| e.to_string()))
                .collect()
        }
    }
}

/// Outcome of training, shown before the person decides to use the model.
#[derive(Clone, Debug)]
pub struct Report {
    pub classes: Vec<String>,
    pub clips: Vec<usize>,
    /// Clips of each class classified as that class, leaving the clip out of
    /// training each time.
    pub correct: Vec<usize>,
    pub warnings: Vec<String>,
    /// Reaction classes too unreliable to fire automatically.
    pub weak: Vec<String>,
    pub model: Model,
}

impl Report {
    pub fn right_total(&self) -> (usize, usize) {
        (self.correct.iter().sum(), self.clips.iter().sum())
    }
}

/// Trains on every saved example and evaluates leaving one clip out.
pub fn train(data: &Path) -> Result<Report, String> {
    let by_class = clips(data);
    if by_class.len() < 2 {
        return Err("grave pelo menos dois sons diferentes para treinar".to_owned());
    }
    // Leaving one clip out of a class with a single clip leaves it empty, so
    // evaluation (and the honest report) needs two per class.
    let short: Vec<&str> = by_class
        .iter()
        .filter(|(_, f)| f.len() < 2)
        .map(|(c, _)| c.as_str())
        .collect();
    if !short.is_empty() {
        return Err(format!(
            "grave pelo menos 2 exemplos de cada som; faltam em: {}",
            short.join(", ")
        ));
    }
    let classes: Vec<String> = by_class.keys().cloned().collect();
    let mut analyzer = Analyzer::new();
    let mut per_clip: Vec<(usize, Vec<Features>)> = Vec::new();
    for (c, files) in by_class.values().enumerate() {
        for f in files {
            per_clip.push((c, analyzer.clip(&read_wav(f)?, SILENCE_DB)));
        }
    }
    let confusion = leave_one_out(&classes, &per_clip)?;
    let clips: Vec<usize> = by_class.values().map(Vec::len).collect();
    let correct: Vec<usize> = (0..classes.len()).map(|i| confusion[i][i]).collect();

    let mut warnings = Vec::new();
    let mut weak = Vec::new();
    for (i, class) in classes.iter().enumerate() {
        if clips[i] < MIN_CLIPS {
            warnings.push(format!(
                "{class}: só {} exemplo(s); grave pelo menos {MIN_CLIPS}",
                clips[i]
            ));
        } else if !BACKGROUND.contains(&class.as_str())
            && (correct[i] as f32) < MIN_RECALL * clips[i] as f32
        {
            warnings.push(format!(
                "{class}: reconhecido em {} de {} exemplos; prefira um atalho para esse sentimento",
                correct[i], clips[i]
            ));
            weak.push(class.clone());
        }
    }
    for (bg, what) in [
        ("outros", "ruídos comuns, como teclado, cliques e música"),
        ("fala", "sua voz falando normalmente"),
    ] {
        if !by_class.contains_key(bg) {
            warnings.push(format!(
                "Grave também \"{bg}\" ({what}) para evitar disparos falsos"
            ));
        }
    }

    let all: Vec<(usize, Features)> = per_clip
        .iter()
        .flat_map(|(c, ws)| ws.iter().map(move |w| (*c, *w)))
        .collect();
    let model = Model::train(classes.clone(), &all)?;
    Ok(Report {
        classes,
        clips,
        correct,
        warnings,
        weak,
        model,
    })
}

/// Makes `model` the one live detection uses.
pub fn accept(data: &Path, model: &Model) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(model).map_err(|e| e.to_string())?;
    crate::prefs::write_atomic(&model_path(data), &json).map_err(|e| e.to_string())
}

/// Records `length` from the microphone, after letting the stream settle.
/// Blocks; call it off the UI thread.
pub fn record(source: Option<String>, length: Duration) -> Result<Vec<f32>, String> {
    let rx = crate::capture::start(source)?;
    let want = (length.as_secs_f64() * SAMPLE_RATE as f64) as usize;
    let settle = SAMPLE_RATE / 5; // the first 200 ms often hold a click
    let mut out = Vec::with_capacity(want + settle);
    while out.len() < want + settle {
        let chunk = rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "o PipeWire parou de enviar áudio".to_owned())?;
        out.extend(chunk);
    }
    Ok(out[settle..settle + want].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32, hz: f32) -> Vec<f32> {
        let n = (secs * SAMPLE_RATE as f32) as usize;
        (0..n)
            .map(|i| 0.3 * (i as f32 * hz * std::f32::consts::TAU / SAMPLE_RATE as f32).sin())
            .collect()
    }

    fn noise(secs: f32, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..(secs * SAMPLE_RATE as f32) as usize)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s as f32 / u32::MAX as f32 - 0.5) * 0.6
            })
            .collect()
    }

    fn data(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("omatuber-train-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn saves_numbered_clips_and_refuses_silence_and_bad_names() {
        let d = data("save");
        let a = save_clip(&d, "risada", &tone(1.0, 300.0)).unwrap();
        let b = save_clip(&d, "risada", &tone(1.0, 320.0)).unwrap();
        assert_eq!(a.file_name().unwrap(), "001.wav");
        assert_eq!(b.file_name().unwrap(), "002.wav");
        assert!(
            save_clip(&d, "risada", &vec![0.0; SAMPLE_RATE])
                .unwrap_err()
                .contains("baixo demais")
        );
        assert!(save_clip(&d, "../x", &tone(1.0, 300.0)).is_err());
        remove_last(&d, "risada").unwrap();
        assert_eq!(clips(&d)["risada"].len(), 1);
        let back = read_wav(&a).unwrap();
        assert_eq!(back.len(), SAMPLE_RATE);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn trains_evaluates_and_warns_about_missing_background() {
        let d = data("train");
        for i in 0..3 {
            save_clip(&d, "risada", &tone(1.0, 300.0 + 10.0 * i as f32)).unwrap();
            save_clip(&d, "outros", &noise(1.0, 7 + i)).unwrap();
        }
        let r = train(&d).unwrap();
        assert_eq!(r.classes, ["outros", "risada"]);
        assert_eq!(r.clips, [3, 3]);
        assert_eq!(r.right_total(), (6, 6), "tones vs noise separate cleanly");
        assert!(r.weak.is_empty());
        assert!(
            r.warnings.iter().any(|w| w.contains("\"fala\"")),
            "{:?}",
            r.warnings
        );
        assert!(!r.warnings.iter().any(|w| w.contains("\"outros\"")));

        accept(&d, &r.model).unwrap();
        let saved = Model::from_json(&fs::read_to_string(model_path(&d)).unwrap()).unwrap();
        assert_eq!(saved.classes, r.model.classes);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn few_examples_and_one_class_are_reported() {
        let d = data("few");
        save_clip(&d, "risada", &tone(1.0, 300.0)).unwrap();
        assert!(train(&d).unwrap_err().contains("dois sons"));
        save_clip(&d, "outros", &noise(1.0, 3)).unwrap();
        save_clip(&d, "outros", &noise(1.0, 4)).unwrap();
        let err = train(&d).unwrap_err();
        assert!(err.contains("faltam em: risada"), "{err}");
        save_clip(&d, "risada", &tone(1.0, 310.0)).unwrap();
        let r = train(&d).unwrap();
        assert!(
            r.warnings.iter().any(|w| w.starts_with("risada: só 2")),
            "{:?}",
            r.warnings
        );
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn indistinguishable_reaction_class_is_marked_weak() {
        let d = data("weak");
        // "susto" recorded as the same noise as "outros": nothing to learn.
        for i in 0..4 {
            save_clip(&d, "outros", &noise(1.0, 100 + i)).unwrap();
            save_clip(&d, "susto", &noise(1.0, 200 + i)).unwrap();
        }
        let r = train(&d).unwrap();
        // Observed: susto recognised in 0 of 4 clips, outros in 2 of 4.
        assert_eq!(r.weak, ["susto"]);
        assert!(
            r.warnings.iter().any(|w| w.contains("prefira um atalho")),
            "{:?}",
            r.warnings
        );
        assert!(
            !r.weak.contains(&"outros".to_owned()),
            "background classes are never flagged"
        );
        fs::remove_dir_all(d).unwrap();
    }
}
