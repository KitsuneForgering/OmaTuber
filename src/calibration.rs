//! Guided calibration: suggest mouth thresholds from a few seconds of
//! silence and of normal speech, using only the levels OBS reports (plan:
//! "coleta alguns segundos de silêncio e fala apenas como níveis").

/// Below this many samples (50 ms each) a phase says too little.
pub const MIN_SAMPLES: usize = 20;
/// Speech and noise closer than this cannot be told apart reliably.
pub const MIN_GAP_DB: f32 = 6.0;
/// Level used for silence (OBS reports zero, i.e. −∞ dB, for a muted or
/// silent source).
const FLOOR_DB: f32 = -100.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Suggestion {
    pub open_db: f32,
    pub close_db: f32,
    /// What was measured, for the explanation shown to the person.
    pub noise_db: f32,
    pub speech_db: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    /// A phase had too few samples, e.g. the source stopped reporting.
    TooShort,
    /// Nothing above the noise during the speech phase.
    NoSpeech,
    /// Speech barely louder than the room; the manual control is better.
    TooClose { noise_db: f32, speech_db: f32 },
}

/// `p` in 0..=1 of the finite-mapped levels; `None` for an empty slice.
fn percentile(levels: &[f32], p: f32) -> Option<f32> {
    let mut v: Vec<f32> = levels
        .iter()
        .map(|&l| if l.is_finite() { l } else { FLOOR_DB })
        .collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    let i = ((v.len() - 1) as f32 * p).round() as usize;
    Some(v[i])
}

/// Noise is taken at its 95th percentile (the loud end of the room), speech
/// at its median (robust to the pauses between words). The open threshold
/// sits a third of the way from noise to speech so quieter syllables still
/// open the mouth; the close threshold stays above the noise so the mouth can
/// close.
pub fn suggest(silence: &[f32], speech: &[f32], min_open_db: f32) -> Result<Suggestion, Problem> {
    if silence.len() < MIN_SAMPLES || speech.len() < MIN_SAMPLES {
        return Err(Problem::TooShort);
    }
    let noise_db = percentile(silence, 0.95).expect("non-empty");
    let speech_db = percentile(speech, 0.5).expect("non-empty");
    if speech_db <= FLOOR_DB + 1.0 {
        return Err(Problem::NoSpeech);
    }
    let gap = speech_db - noise_db;
    if gap < MIN_GAP_DB {
        return Err(Problem::TooClose {
            noise_db,
            speech_db,
        });
    }
    let open_db = (noise_db + gap / 3.0).clamp(min_open_db, -3.0).round();
    // The mouth must be able to close: keep the close threshold 3 dB above the
    // room's loud end (but no lower than the controls go), and at least 2 dB
    // under the open threshold.
    let floor = noise_db.max(min_open_db - 8.0) + 3.0;
    let close_db = (open_db - 8.0).max(floor).min(open_db - 2.0).round();
    Ok(Suggestion {
        open_db,
        close_db,
        noise_db,
        speech_db,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN_OPEN: f32 = -52.0;

    fn around(center: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| center + (i % 5) as f32 - 2.0).collect()
    }

    #[test]
    fn quiet_room_and_clear_speech() {
        let s = suggest(&around(-55.0, 60), &around(-20.0, 60), MIN_OPEN).unwrap();
        assert_eq!((s.noise_db, s.speech_db), (-53.0, -20.0));
        // A third of the way from -53 to -20 is -42; close stays above noise.
        assert_eq!(s.open_db, -42.0);
        assert_eq!(s.close_db, -50.0);
        assert!(s.close_db > s.noise_db && s.close_db < s.open_db);
    }

    #[test]
    fn noisy_room_keeps_close_above_noise() {
        let s = suggest(&around(-38.0, 60), &around(-18.0, 60), MIN_OPEN).unwrap();
        assert_eq!(s.noise_db, -36.0);
        assert_eq!(s.open_db, -30.0);
        // open - 8 = -38 would sit inside the noise; raised to noise + 3.
        assert_eq!(s.close_db, -33.0);
    }

    #[test]
    fn pauses_in_speech_do_not_drag_the_estimate_down() {
        let mut speech = around(-20.0, 40);
        speech.extend(vec![f32::NEG_INFINITY; 15]); // breaths between words
        let s = suggest(&around(-60.0, 60), &speech, MIN_OPEN).unwrap();
        assert!((s.speech_db + 20.0).abs() <= 2.0, "{}", s.speech_db);
    }

    #[test]
    fn muted_silence_phase_is_fine() {
        let s = suggest(&vec![f32::NEG_INFINITY; 60], &around(-25.0, 60), MIN_OPEN).unwrap();
        assert_eq!(s.open_db, -52.0, "clamped to the most sensitive allowed");
        assert!(s.close_db < s.open_db);
    }

    #[test]
    fn problems_are_reported_not_guessed() {
        assert_eq!(
            suggest(&around(-50.0, 5), &around(-20.0, 60), MIN_OPEN),
            Err(Problem::TooShort)
        );
        assert_eq!(
            suggest(&around(-50.0, 60), &vec![f32::NEG_INFINITY; 60], MIN_OPEN),
            Err(Problem::NoSpeech)
        );
        assert!(matches!(
            suggest(&around(-30.0, 60), &around(-26.0, 60), MIN_OPEN),
            Err(Problem::TooClose { .. })
        ));
    }
}
