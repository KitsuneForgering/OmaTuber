//! Decides whether the avatar's mouth is open from a stream of audio levels.
//!
//! Time is passed in by the caller so the logic can be tested without a clock.

use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mouth {
    Closed,
    Open,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GateParams {
    /// Level at or above which a closed mouth opens, in dBFS.
    pub open_db: f32,
    /// Level below which an open mouth may close, in dBFS. Must not exceed `open_db`.
    pub close_db: f32,
    /// How long the level must stay below `close_db` before the mouth closes.
    pub hold: Duration,
    /// Without a sample for this long, the source is treated as lost and the mouth closes.
    pub stale_after: Duration,
}

impl Default for GateParams {
    /// Starting values, not yet tuned against real recordings (plan, step 4).
    fn default() -> Self {
        Self {
            open_db: -30.0,
            close_db: -38.0,
            hold: Duration::from_millis(150),
            // OBS emits meters every 50 ms and zeroes levels after 300 ms without audio.
            stale_after: Duration::from_millis(300),
        }
    }
}

#[derive(Debug)]
pub struct Gate {
    params: GateParams,
    mouth: Mouth,
    last_loud: Duration,
    last_sample: Option<Duration>,
}

impl Gate {
    pub fn new(params: GateParams) -> Self {
        debug_assert!(params.close_db <= params.open_db);
        Self {
            params,
            mouth: Mouth::Closed,
            last_loud: Duration::ZERO,
            last_sample: None,
        }
    }

    pub fn mouth(&self) -> Mouth {
        self.mouth
    }

    pub fn params(&self) -> GateParams {
        self.params
    }

    pub fn set_params(&mut self, params: GateParams) {
        debug_assert!(params.close_db <= params.open_db);
        self.params = params;
    }

    /// Feeds a level sample taken at `now` (time since any fixed origin).
    pub fn on_level(&mut self, now: Duration, level_db: f32) -> Mouth {
        self.last_sample = Some(now);
        // NaN compares false everywhere, so it counts as quiet.
        match self.mouth {
            Mouth::Closed if level_db >= self.params.open_db => {
                self.mouth = Mouth::Open;
                self.last_loud = now;
            }
            Mouth::Open if level_db >= self.params.close_db => self.last_loud = now,
            Mouth::Open if now.saturating_sub(self.last_loud) >= self.params.hold => {
                self.mouth = Mouth::Closed;
            }
            _ => {}
        }
        self.mouth
    }

    /// Advances time without a sample for the selected source, e.g. when OBS
    /// stopped reporting it or the connection dropped.
    pub fn on_tick(&mut self, now: Duration) -> Mouth {
        if self.is_stale(now) {
            self.mouth = Mouth::Closed;
        }
        self.mouth
    }

    /// True when no sample arrived within `stale_after` of `now`.
    pub fn is_stale(&self, now: Duration) -> bool {
        self.last_sample
            .is_none_or(|t| now.saturating_sub(t) >= self.params.stale_after)
    }

    /// Forces the closed state, e.g. on disconnect.
    pub fn reset(&mut self) {
        self.mouth = Mouth::Closed;
        self.last_sample = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(50);

    fn gate() -> Gate {
        Gate::new(GateParams::default())
    }

    /// Feeds one sample per 50 ms tick starting at `start`; returns the states.
    fn feed(g: &mut Gate, start: Duration, levels: &[f32]) -> Vec<Mouth> {
        levels
            .iter()
            .enumerate()
            .map(|(i, &db)| g.on_level(start + TICK * i as u32, db))
            .collect()
    }

    #[test]
    fn silence_stays_closed() {
        let mut g = gate();
        let states = feed(&mut g, Duration::ZERO, &[f32::NEG_INFINITY; 600]);
        assert!(states.iter().all(|&m| m == Mouth::Closed));
    }

    #[test]
    fn opens_on_first_loud_sample() {
        let mut g = gate();
        let states = feed(&mut g, Duration::ZERO, &[-60.0, -60.0, -20.0]);
        assert_eq!(states, [Mouth::Closed, Mouth::Closed, Mouth::Open]);
    }

    #[test]
    fn exactly_at_open_threshold_opens() {
        let mut g = gate();
        assert_eq!(g.on_level(Duration::ZERO, -30.0), Mouth::Open);
    }

    #[test]
    fn level_between_thresholds_does_not_open() {
        let mut g = gate();
        let states = feed(&mut g, Duration::ZERO, &[-34.0; 40]);
        assert!(states.iter().all(|&m| m == Mouth::Closed));
    }

    #[test]
    fn oscillation_between_thresholds_keeps_mouth_open() {
        // Level wobbling around -34 dB after a loud onset must not flap.
        let mut g = gate();
        let mut levels = vec![-20.0];
        levels.extend([-31.0, -36.0, -33.0, -37.5, -32.0].repeat(10));
        let states = feed(&mut g, Duration::ZERO, &levels);
        assert!(states.iter().all(|&m| m == Mouth::Open));
    }

    #[test]
    fn short_dip_below_close_is_held() {
        let mut g = gate();
        // Two quiet ticks = 100 ms < 150 ms hold.
        let states = feed(&mut g, Duration::ZERO, &[-20.0, -60.0, -60.0, -20.0]);
        assert!(states.iter().all(|&m| m == Mouth::Open));
    }

    #[test]
    fn closes_after_hold_elapses() {
        let mut g = gate();
        let states = feed(&mut g, Duration::ZERO, &[-20.0, -60.0, -60.0, -60.0, -60.0]);
        // Loud at 0 ms; quiet samples at 50, 100, 150 ms: closes at 150 ms.
        assert_eq!(
            states,
            [
                Mouth::Open,
                Mouth::Open,
                Mouth::Open,
                Mouth::Closed,
                Mouth::Closed
            ]
        );
    }

    #[test]
    fn muted_source_reports_zero_and_closes() {
        // OBS sends 0.0 multipliers for a muted source, i.e. -inf dB.
        let mut g = gate();
        g.on_level(Duration::ZERO, -20.0);
        let states = feed(&mut g, TICK, &[f32::NEG_INFINITY; 4]);
        assert_eq!(*states.last().unwrap(), Mouth::Closed);
    }

    #[test]
    fn nan_level_counts_as_quiet() {
        let mut g = gate();
        assert_eq!(g.on_level(Duration::ZERO, f32::NAN), Mouth::Closed);
        g.on_level(TICK, -20.0);
        let states = feed(&mut g, TICK * 2, &[f32::NAN; 3]);
        assert_eq!(*states.last().unwrap(), Mouth::Closed);
    }

    #[test]
    fn lost_source_closes_after_stale_timeout() {
        let mut g = gate();
        g.on_level(Duration::ZERO, -20.0);
        assert_eq!(g.on_tick(Duration::from_millis(250)), Mouth::Open);
        assert_eq!(g.on_tick(Duration::from_millis(300)), Mouth::Closed);
    }

    #[test]
    fn tick_before_any_sample_is_closed() {
        let mut g = gate();
        assert_eq!(g.on_tick(Duration::from_secs(5)), Mouth::Closed);
    }

    #[test]
    fn reset_closes_and_forgets_last_sample() {
        let mut g = gate();
        g.on_level(Duration::ZERO, -20.0);
        g.reset();
        assert_eq!(g.mouth(), Mouth::Closed);
        assert_eq!(g.on_tick(TICK), Mouth::Closed);
    }

    #[test]
    fn non_monotonic_time_does_not_panic_or_close_early() {
        let mut g = gate();
        g.on_level(Duration::from_secs(1), -20.0);
        assert_eq!(g.on_level(Duration::from_millis(900), -60.0), Mouth::Open);
    }
}
