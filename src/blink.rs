//! Eye blinks: closes the eyes for a moment at random intervals.
//!
//! Decided here and sent to every page, so the preview and OBS blink at the
//! same instant; a page choosing its own random moments would drift apart.
//! Without blink images nothing changes on screen.

use std::time::Duration;

use crate::server::Outputs;

/// Range between blinks; people blink every few seconds, unevenly.
pub const MIN_GAP: Duration = Duration::from_millis(2500);
pub const MAX_GAP: Duration = Duration::from_millis(6000);
/// How long the eyes stay closed.
pub const CLOSED: Duration = Duration::from_millis(150);

/// Small xorshift generator; blinks need variety, not quality randomness.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Time until the next blink, uniform in `MIN_GAP..=MAX_GAP`.
    pub fn gap(&mut self) -> Duration {
        let span = (MAX_GAP - MIN_GAP).as_millis() as u64;
        MIN_GAP + Duration::from_millis(self.next() % (span + 1))
    }
}

/// Runs forever on the current tokio runtime; `notify` is called on every
/// change, e.g. to repaint the preview.
pub fn spawn(outputs: Outputs, notify: impl Fn() + Send + 'static) {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64);
    tokio::spawn(async move {
        let mut rng = Rng::new(seed);
        loop {
            tokio::time::sleep(rng.gap()).await;
            outputs.set_blink(true);
            notify();
            tokio::time::sleep(CLOSED).await;
            outputs.set_blink(false);
            notify();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_stay_in_range_and_vary() {
        let mut rng = Rng::new(42);
        let gaps: Vec<Duration> = (0..1000).map(|_| rng.gap()).collect();
        assert!(gaps.iter().all(|g| (MIN_GAP..=MAX_GAP).contains(g)));
        let distinct: std::collections::BTreeSet<_> = gaps.iter().collect();
        assert!(distinct.len() > 500, "{} distinct gaps", distinct.len());
    }

    #[tokio::test(start_paused = true)]
    async fn blinks_close_then_open_on_schedule() {
        let outputs = Outputs::default();
        spawn(outputs.clone(), || {});
        assert!(!outputs.blinking());
        // Some time within the maximum gap the eyes close…
        let mut closed_at = None;
        for step in 0..=MAX_GAP.as_millis() as u64 / 10 + 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if outputs.blinking() {
                closed_at = Some(step);
                break;
            }
        }
        assert!(closed_at.is_some(), "never blinked");
        // …and open again after CLOSED.
        tokio::time::sleep(CLOSED + Duration::from_millis(10)).await;
        assert!(!outputs.blinking());
    }
}
