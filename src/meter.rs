//! Interpretation of obs-websocket `InputVolumeMeters` levels.
//!
//! Each channel carries three linear multipliers, computed from the last audio
//! block OBS processed before the 50 ms tick (obs-websocket
//! `src/utils/Obs_VolumeMeter.cpp`, `GetMeterData`):
//!
//! - `[0]` RMS × fader volume, zero when muted
//! - `[1]` sample peak × fader volume, zero when muted
//! - `[2]` sample peak before fader and mute
//!
//! The mouth must follow what the stream hears, so only the post-fader values
//! are usable; `[2]` would animate a muted microphone.

/// Which post-fader value drives the mouth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelKind {
    Rms,
    Peak,
}

/// Loudest channel's post-fader level, in dBFS. Empty input means silence.
pub fn level_db(channels: &[[f32; 3]], kind: LevelKind) -> f32 {
    let index = match kind {
        LevelKind::Rms => 0,
        LevelKind::Peak => 1,
    };
    let loudest = channels
        .iter()
        .map(|c| c[index])
        .filter(|v| v.is_finite())
        .fold(0.0_f32, f32::max);
    mul_to_db(loudest)
}

/// Converts a linear amplitude multiplier to decibels; zero becomes -inf.
pub fn mul_to_db(mul: f32) -> f32 {
    20.0 * mul.log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_is_zero_db() {
        assert_eq!(mul_to_db(1.0), 0.0);
    }

    #[test]
    fn half_amplitude_is_about_minus_six_db() {
        assert!((mul_to_db(0.5) + 6.0206).abs() < 1e-3);
    }

    #[test]
    fn zero_is_negative_infinity() {
        assert_eq!(mul_to_db(0.0), f32::NEG_INFINITY);
    }

    #[test]
    fn picks_loudest_channel() {
        let ch = [[0.01, 0.1, 0.1], [0.1, 0.5, 0.5]];
        assert!((level_db(&ch, LevelKind::Rms) - mul_to_db(0.1)).abs() < 1e-6);
        assert!((level_db(&ch, LevelKind::Peak) - mul_to_db(0.5)).abs() < 1e-6);
    }

    #[test]
    fn ignores_pre_fader_peak_of_muted_source() {
        let muted = [[0.0, 0.0, 0.8]];
        assert_eq!(level_db(&muted, LevelKind::Rms), f32::NEG_INFINITY);
        assert_eq!(level_db(&muted, LevelKind::Peak), f32::NEG_INFINITY);
    }

    #[test]
    fn no_channels_is_silence() {
        assert_eq!(level_db(&[], LevelKind::Rms), f32::NEG_INFINITY);
    }

    #[test]
    fn non_finite_values_are_ignored() {
        let ch = [[f32::NAN, f32::INFINITY, 1.0], [0.1, 0.1, 0.1]];
        assert!((level_db(&ch, LevelKind::Rms) - mul_to_db(0.1)).abs() < 1e-6);
        assert!((level_db(&ch, LevelKind::Peak) - mul_to_db(0.1)).abs() < 1e-6);
    }
}
