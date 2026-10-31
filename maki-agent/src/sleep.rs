use std::time::{Duration, Instant, SystemTime};

pub const DEFAULT_SLEEP_THRESHOLD: Duration = Duration::from_secs(3);
pub const WAKE_GRACE_DURATION: Duration = Duration::from_secs(15);
const CLOCK_DRIFT_TOLERANCE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct SleepDetector {
    last_wall: SystemTime,
    last_mono: Instant,
    threshold: Duration,
}

impl Default for SleepDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl SleepDetector {
    pub fn new() -> Self {
        Self::with_threshold(DEFAULT_SLEEP_THRESHOLD)
    }

    pub fn with_threshold(threshold: Duration) -> Self {
        Self {
            last_wall: SystemTime::now(),
            last_mono: Instant::now(),
            threshold,
        }
    }

    pub fn check(&mut self) -> Option<Duration> {
        let now_wall = SystemTime::now();
        let now_mono = Instant::now();
        let wall_delta = now_wall.duration_since(self.last_wall).unwrap_or(Duration::ZERO);
        let mono_delta = now_mono.saturating_duration_since(self.last_mono);
        self.last_wall = now_wall;
        self.last_mono = now_mono;

        if wall_delta >= self.threshold {
            let sleep_duration = if wall_delta > mono_delta + CLOCK_DRIFT_TOLERANCE {
                wall_delta.saturating_sub(mono_delta)
            } else {
                wall_delta
            };
            Some(sleep_duration)
        } else {
            None
        }
    }
}

pub async fn watch_for_sleep(poll_interval: Duration, threshold: Duration) -> Duration {
    let mut detector = SleepDetector::with_threshold(threshold);
    loop {
        smol::Timer::after(poll_interval).await;
        if let Some(slept) = detector.check() {
            return slept;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_intervals_do_not_trigger_sleep() {
        let mut detector = SleepDetector::with_threshold(Duration::from_secs(3));
        assert!(detector.check().is_none());
    }

    #[test]
    fn simulated_wall_jump_triggers_sleep() {
        let mut detector = SleepDetector::with_threshold(Duration::from_secs(3));
        detector.last_wall -= Duration::from_secs(60);
        let slept = detector.check();
        assert!(slept.is_some());
        assert!(slept.unwrap() >= Duration::from_secs(50));
    }
}
