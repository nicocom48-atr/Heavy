use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::time::sleep;

/// Thread-safe token-bucket speed limiter for dynamic bandwidth regulation in real time.
#[derive(Clone, Debug)]
pub struct SpeedLimiter {
    /// Configured maximum bandwidth in bytes per second (0 = unlimited).
    limit_bps: Arc<AtomicU64>,
    state: Arc<Mutex<LimiterState>>,
}

#[derive(Debug)]
struct LimiterState {
    last_check: Instant,
    available_tokens: f64,
}

impl Default for SpeedLimiter {
    fn default() -> Self {
        Self::new(0)
    }
}

impl SpeedLimiter {
    /// Create a new limiter with the given limit in bytes per second (0 = unlimited).
    pub fn new(limit_bps: u64) -> Self {
        Self {
            limit_bps: Arc::new(AtomicU64::new(limit_bps)),
            state: Arc::new(Mutex::new(LimiterState {
                last_check: Instant::now(),
                available_tokens: 0.0,
            })),
        }
    }

    /// Set limit in Megabytes per second (MB/s). 0 = unlimited.
    pub fn set_limit_mbps(&self, mbps: u64) {
        self.set_limit_bps(mbps.saturating_mul(1024 * 1024));
    }

    /// Set limit in Bytes per second (B/s). 0 = unlimited.
    pub fn set_limit_bps(&self, bps: u64) {
        self.limit_bps.store(bps, Ordering::Relaxed);
    }

    /// Get current limit in MB/s (0 = unlimited).
    pub fn get_limit_mbps(&self) -> u64 {
        self.limit_bps.load(Ordering::Relaxed) / (1024 * 1024)
    }

    /// Get current limit in B/s (0 = unlimited).
    pub fn get_limit_bps(&self) -> u64 {
        self.limit_bps.load(Ordering::Relaxed)
    }

    /// Check if speed is currently restricted.
    pub fn is_limited(&self) -> bool {
        self.limit_bps.load(Ordering::Relaxed) > 0
    }

    /// Throttle the current async task if necessary to conform to the rate limit.
    pub async fn consume(&self, bytes: usize) {
        let limit = self.limit_bps.load(Ordering::Relaxed);
        if limit == 0 || bytes == 0 {
            return;
        }

        let wait_duration = {
            let mut state = self.state.lock();
            let now = Instant::now();
            let elapsed = now.saturating_duration_since(state.last_check).as_secs_f64();
            state.last_check = now;

            // Replenish tokens based on elapsed time, capped at 1 second of bandwidth or 8MB burst
            let max_burst = (limit as f64).max(8.0 * 1024.0 * 1024.0);
            state.available_tokens =
                (state.available_tokens + elapsed * (limit as f64)).min(max_burst);

            if state.available_tokens >= bytes as f64 {
                state.available_tokens -= bytes as f64;
                None
            } else {
                let deficit = bytes as f64 - state.available_tokens;
                state.available_tokens = 0.0;
                let wait_secs = deficit / (limit as f64);
                // Advance last_check so sleep time is accounted for
                state.last_check = now + std::time::Duration::from_secs_f64(wait_secs);
                Some(std::time::Duration::from_secs_f64(wait_secs))
            }
        };

        if let Some(dur) = wait_duration {
            sleep(dur).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_unlimited_does_not_sleep() {
        let limiter = SpeedLimiter::default();
        let start = Instant::now();
        limiter.consume(10 * 1024 * 1024).await;
        assert!(start.elapsed().as_millis() < 50);
    }

    #[tokio::test]
    async fn test_rate_limiting_enforcement() {
        let limiter = SpeedLimiter::new(1024 * 1024); // 1 MB/s
        let start = Instant::now();
        // Consume 512 KB
        limiter.consume(512 * 1024).await;
        // Consume another 512 KB (total 1 MB -> should take ~0.5 - 1.0s)
        limiter.consume(512 * 1024).await;
        let elapsed = start.elapsed().as_millis();
        assert!(elapsed >= 350, "Elapsed was only {} ms", elapsed);
    }

    #[tokio::test]
    async fn test_dynamic_limit_update() {
        let limiter = SpeedLimiter::new(1024 * 1024);
        assert_eq!(limiter.get_limit_mbps(), 1);
        limiter.set_limit_mbps(50);
        assert_eq!(limiter.get_limit_mbps(), 50);
        assert_eq!(limiter.get_limit_bps(), 50 * 1024 * 1024);
        limiter.set_limit_mbps(0);
        assert_eq!(limiter.is_limited(), false);
    }
}

