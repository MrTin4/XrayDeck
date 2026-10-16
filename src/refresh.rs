use std::time::{Duration, Instant};

/// Missed ticks are skipped, never queued. Manual refresh restarts the interval.
#[derive(Default)]
pub struct RefreshSchedule {
    interval: Option<Duration>,
    next: Option<Instant>,
}

impl RefreshSchedule {
    pub fn configure(&mut self, enabled: bool, seconds: u64, now: Instant) {
        self.interval = enabled.then(|| Duration::from_secs(seconds));
        self.restart(now);
    }

    pub fn restart(&mut self, now: Instant) {
        self.next = self.interval.map(|interval| now + interval);
    }

    /// Round up so that 0 is only shown once the actual deadline is reached.
    pub fn remaining_seconds(&self, now: Instant) -> Option<u64> {
        self.next.map(|next| {
            let remaining = next.saturating_duration_since(now);
            remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)
        })
    }

    pub fn poll(&mut self, now: Instant, blocked: bool) -> bool {
        if self.next.is_none_or(|next| now < next) {
            return false;
        }
        self.restart(now);
        !blocked
    }

    /// Keep a due check pending while busy, without queuing missed intervals.
    pub fn poll_when_idle(&mut self, now: Instant, blocked: bool) -> bool {
        !blocked && self.poll(now, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hourly_checks_defer_while_busy_and_resume_once_without_catching_up() {
        let now = Instant::now();
        let mut schedule = RefreshSchedule::default();
        schedule.configure(true, 3600, now);
        assert!(!schedule.poll_when_idle(now + Duration::from_secs(3599), false));
        assert!(!schedule.poll_when_idle(now + Duration::from_secs(3600), true));
        assert!(!schedule.poll_when_idle(now + Duration::from_secs(10800), true));
        assert!(schedule.poll_when_idle(now + Duration::from_secs(10801), false));
        assert!(!schedule.poll_when_idle(now + Duration::from_secs(10801), false));
        assert!(!schedule.poll_when_idle(now + Duration::from_secs(14400), false));
        assert!(schedule.poll_when_idle(now + Duration::from_secs(14401), false));
        schedule.configure(true, 120 * 60, now + Duration::from_secs(14402));
        assert!(!schedule.poll_when_idle(now + Duration::from_secs(18002), false));
        assert!(schedule.poll_when_idle(now + Duration::from_secs(21602), false));
    }

    #[test]
    fn countdown_uses_the_deadline_and_rounds_up_without_underflow() {
        let now = Instant::now();
        let mut schedule = RefreshSchedule::default();
        assert_eq!(schedule.remaining_seconds(now), None);
        schedule.configure(true, 30, now);
        assert_eq!(schedule.remaining_seconds(now), Some(30));
        assert_eq!(
            schedule.remaining_seconds(now + Duration::from_millis(1)),
            Some(30)
        );
        assert_eq!(
            schedule.remaining_seconds(now + Duration::from_secs(1)),
            Some(29)
        );
        assert_eq!(
            schedule.remaining_seconds(now + Duration::from_millis(29999)),
            Some(1)
        );
        assert_eq!(
            schedule.remaining_seconds(now + Duration::from_secs(30)),
            Some(0)
        );
        assert_eq!(
            schedule.remaining_seconds(now + Duration::from_secs(100)),
            Some(0)
        );
        assert!(schedule.poll(now + Duration::from_secs(100), false));
        assert_eq!(
            schedule.remaining_seconds(now + Duration::from_secs(100)),
            Some(30)
        );
    }

    #[test]
    fn countdown_tracks_manual_reset_interval_changes_skips_and_disabling() {
        let now = Instant::now();
        let mut schedule = RefreshSchedule::default();
        schedule.configure(true, 30, now);
        schedule.restart(now + Duration::from_secs(20));
        assert_eq!(
            schedule.remaining_seconds(now + Duration::from_secs(20)),
            Some(30)
        );
        schedule.configure(true, 5, now + Duration::from_secs(21));
        assert_eq!(
            schedule.remaining_seconds(now + Duration::from_secs(21)),
            Some(5)
        );
        assert!(!schedule.poll(now + Duration::from_secs(26), true));
        assert_eq!(
            schedule.remaining_seconds(now + Duration::from_secs(26)),
            Some(5)
        );
        schedule.configure(false, 5, now + Duration::from_secs(27));
        assert_eq!(
            schedule.remaining_seconds(now + Duration::from_secs(27)),
            None
        );
    }

    #[test]
    fn configuration_enables_changes_and_disables_the_schedule() {
        let now = Instant::now();
        let mut schedule = RefreshSchedule::default();
        assert!(!schedule.poll(now, false));
        schedule.configure(true, 30, now);
        assert!(!schedule.poll(now + Duration::from_secs(29), false));
        assert!(schedule.poll(now + Duration::from_secs(30), false));
        schedule.configure(true, 5, now + Duration::from_secs(31));
        assert!(!schedule.poll(now + Duration::from_secs(35), false));
        assert!(schedule.poll(now + Duration::from_secs(36), false));
        schedule.configure(false, 5, now);
        assert!(!schedule.poll(now + Duration::from_secs(1000), false));
    }

    #[test]
    fn busy_and_delayed_ticks_do_not_queue_catch_up_requests() {
        let now = Instant::now();
        let mut schedule = RefreshSchedule::default();
        schedule.configure(true, 5, now);
        assert!(!schedule.poll(now + Duration::from_secs(5), true));
        assert!(!schedule.poll(now + Duration::from_secs(6), false));
        assert!(schedule.poll(now + Duration::from_secs(100), false));
        assert!(!schedule.poll(now + Duration::from_secs(100), false));
        assert!(schedule.poll(now + Duration::from_secs(105), false));
    }

    #[test]
    fn manual_refresh_postpones_the_next_automatic_refresh() {
        let now = Instant::now();
        let mut schedule = RefreshSchedule::default();
        schedule.configure(true, 30, now);
        schedule.restart(now + Duration::from_secs(29));
        assert!(!schedule.poll(now + Duration::from_secs(30), false));
        assert!(schedule.poll(now + Duration::from_secs(59), false));
    }
}
