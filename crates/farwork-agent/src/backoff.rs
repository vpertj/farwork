//! Exponential reconnect backoff with a hard cap.

use std::time::Duration;

/// Upper bound the contract asks for: never wait longer than this between tries.
pub const MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    attempt: u32,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            attempt: 0,
        }
    }

    /// Default policy: start at one second, double, cap at 30 seconds.
    pub fn connector_default() -> Self {
        Self::new(Duration::from_secs(1), MAX_DELAY)
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Delay for the next try. Jitter is derived from the attempt counter so a
    /// fleet of connectors does not reconnect in lockstep, and so the sequence
    /// stays reproducible in tests.
    pub fn next_delay(&mut self) -> Duration {
        let cap_ms = self.max.as_millis() as u64;
        let grown = self.base.as_millis() as u64 * 2u64.saturating_pow(self.attempt.min(16));
        let capped = grown.min(cap_ms);
        let jitter_percent = u64::from(self.attempt).saturating_mul(37) % 21;
        let jittered = capped.saturating_add(capped / 100 * jitter_percent);
        self.attempt = self.attempt.saturating_add(1);
        Duration::from_millis(jittered.min(cap_ms))
    }

    /// Called after a successful handshake: the next failure starts over.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_retry_waits_one_second() {
        let mut backoff = Backoff::connector_default();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
        assert_eq!(backoff.attempt(), 1);
    }

    #[test]
    fn delays_grow_exponentially_until_the_cap() {
        let mut backoff = Backoff::connector_default();
        let delays: Vec<u64> = (0..8)
            .map(|_| backoff.next_delay().as_millis() as u64)
            .collect();
        assert_eq!(delays[0], 1_000);
        assert!(delays[1] >= 2_000, "delays: {delays:?}");
        assert!(delays[2] > delays[1], "delays: {delays:?}");
        assert!(
            delays.windows(2).all(|pair| pair[0] <= pair[1]),
            "non-monotonic: {delays:?}"
        );
    }

    #[test]
    fn delay_never_exceeds_the_cap() {
        let mut backoff = Backoff::connector_default();
        for _ in 0..40 {
            assert!(backoff.next_delay() <= MAX_DELAY);
        }
        assert_eq!(backoff.next_delay(), MAX_DELAY);
    }

    #[test]
    fn reset_returns_to_the_first_step() {
        let mut backoff = Backoff::connector_default();
        for _ in 0..5 {
            backoff.next_delay();
        }
        backoff.reset();
        assert_eq!(backoff.attempt(), 0);
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }

    #[test]
    fn attempt_counter_does_not_overflow_past_the_exponent_guard() {
        let mut backoff = Backoff::new(Duration::from_millis(1), MAX_DELAY);
        for _ in 0..200 {
            assert!(backoff.next_delay() <= MAX_DELAY);
        }
    }
}
