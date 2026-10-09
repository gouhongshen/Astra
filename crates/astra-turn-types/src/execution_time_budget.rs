//! Clock-free projections of one admitted execution deadline.

use std::time::Duration;

/// Provider synthesis is bounded, but a short run must not reserve all its time.
pub const FINAL_SYNTHESIS_TIME_CAP: Duration = Duration::from_secs(30);

/// Compute once at admission, not again from each round's remaining time.
pub fn final_synthesis_reserve(available: Duration) -> Duration {
    FINAL_SYNTHESIS_TIME_CAP.min(available / 2)
}

/// Both windows are observed from the same clock instant. The authority that
/// owns their absolute cutoffs lives outside this value projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionTimeRemaining {
    pub work_remaining: Duration,
    pub total_remaining: Duration,
}

impl ExecutionTimeRemaining {
    /// Provider dispatch is whole-second granular; a fraction cannot admit work.
    pub fn has_work(self) -> bool {
        self.work_remaining.as_secs() != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_retains_work_without_exceeding_synthesis_cap() {
        for milliseconds in [
            0, 1, 999, 1_000, 1_999, 2_000, 59_999, 60_000, 60_001, 120_000,
        ] {
            let available = Duration::from_millis(milliseconds);
            let final_time = final_synthesis_reserve(available);
            let work = available - final_time;
            assert_eq!(work + final_time, available);
            assert!(final_time <= FINAL_SYNTHESIS_TIME_CAP && final_time <= work);
            assert_eq!(
                ExecutionTimeRemaining {
                    work_remaining: work,
                    total_remaining: available
                }
                .has_work(),
                milliseconds >= 2_000,
            );
        }
    }
}
