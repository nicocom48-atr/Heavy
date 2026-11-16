use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferRole {
    Sender,
    Receiver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionType {
    None,
    DirectLocal,
    DirectInternet,
    Relay,
}

impl ConnectionType {
    pub fn label(&self) -> &'static str {
        match self {
            ConnectionType::None => "Desconectado",
            ConnectionType::DirectLocal => "Conexión directa local (LAN)",
            ConnectionType::DirectInternet => "Conexión directa por Internet (P2P)",
            ConnectionType::Relay => "Conexión cifrada mediante relay",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferStatus {
    Idle,
    Preparing,
    WaitingForPeer,
    Connecting,
    Connected,
    Transferring,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct TransferProgress {
    pub status: TransferStatus,
    pub role: TransferRole,
    pub connection_type: ConnectionType,
    pub pairing_code: Option<String>,
    pub code_expires_in: Option<String>,
    pub file_name: String,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub resume_offset: u64,
    pub network_received_bytes: u64,
    pub bytes_per_sec: f64,
    pub status_text: String,
    pub peer_address: String,
    pub error: Option<String>,
}

impl Default for TransferProgress {
    fn default() -> Self {
        Self {
            status: TransferStatus::Idle,
            role: TransferRole::Sender,
            connection_type: ConnectionType::None,
            pairing_code: None,
            code_expires_in: None,
            file_name: String::new(),
            total_bytes: 0,
            transferred_bytes: 0,
            resume_offset: 0,
            network_received_bytes: 0,
            bytes_per_sec: 0.0,
            status_text: String::new(),
            peer_address: String::new(),
            error: None,
        }
    }
}

/// Time-weighted sliding window speed estimator over a configurable duration (default ~1.5s).
/// Accurately computes continuous throughput while smoothing out discrete block arrival jitter
/// and responding cleanly to pauses by decaying to 0.0 without keeping stale measurements.
#[derive(Debug, Clone)]
pub struct SpeedWindow {
    window_duration: Duration,
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedWindow {
    pub fn new(window_duration: Duration) -> Self {
        Self {
            window_duration,
            samples: VecDeque::with_capacity(64),
        }
    }

    /// Reset all samples (e.g. when beginning a transfer or after a reconnection attempt).
    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// Record a cumulative bytes sample at the specified instant.
    pub fn record(&mut self, now: Instant, total_bytes: u64) {
        self.samples.push_back((now, total_bytes));
        self.prune(now);
    }

    fn prune(&mut self, now: Instant) {
        while self.samples.len() > 2 {
            if let Some(&(t, _)) = self.samples.front() {
                if now.duration_since(t) > self.window_duration {
                    if let Some(&(next_t, _)) = self.samples.get(1) {
                        if now.duration_since(next_t) > self.window_duration {
                            self.samples.pop_front();
                            continue;
                        }
                    }
                }
            }
            break;
        }
    }

    /// Calculate time-weighted velocity in bytes per second across the active window.
    pub fn current_speed(&self, now: Instant) -> f64 {
        if self.samples.len() < 2 {
            return 0.0;
        }

        let (front_time, front_bytes) = *self.samples.front().unwrap();
        let (back_time, back_bytes) = *self.samples.back().unwrap();

        // If no new samples recorded for more than window_duration, transfer is stalled
        if now.duration_since(back_time) >= self.window_duration {
            return 0.0;
        }

        let elapsed = back_time.duration_since(front_time).as_secs_f64();
        if elapsed <= 0.001 {
            return 0.0;
        }

        let bytes_delta = back_bytes.saturating_sub(front_bytes);
        bytes_delta as f64 / elapsed
    }
}

pub type SharedProgress = Arc<Mutex<TransferProgress>>;

pub fn new_shared_progress() -> SharedProgress {
    Arc::new(Mutex::new(TransferProgress::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_speed_window_steady_flow() {
        let mut sw = SpeedWindow::new(Duration::from_millis(1500));
        let base = Instant::now();

        // 10 MB per second recorded every 100ms
        for i in 0..=20 {
            let t = base + Duration::from_millis(i * 100);
            let bytes = i * 1_000_000;
            sw.record(t, bytes);
        }

        let t_now = base + Duration::from_millis(2000);
        let speed = sw.current_speed(t_now);
        // Expect close to 10 MB/s (10,000,000 B/s)
        assert!((speed - 10_000_000.0).abs() < 50_000.0, "speed was: {}", speed);
    }

    #[test]
    fn test_speed_window_pauses_drop_to_zero() {
        let mut sw = SpeedWindow::new(Duration::from_millis(1500));
        let base = Instant::now();

        sw.record(base, 10_000_000);
        sw.record(base + Duration::from_millis(500), 20_000_000);

        // Pause for 1600ms (> 1500ms window duration)
        let t_after_pause = base + Duration::from_millis(2200);
        assert_eq!(sw.current_speed(t_after_pause), 0.0);
    }

    #[test]
    fn test_speed_window_resumes_do_not_count_prior_offset() {
        let mut sw = SpeedWindow::new(Duration::from_millis(1500));
        let base = Instant::now();

        // Resumed at 500 MB base
        let resume_offset = 500 * 1024 * 1024;
        sw.record(base, resume_offset);
        sw.record(base + Duration::from_millis(500), resume_offset + 10 * 1024 * 1024);

        let t_check = base + Duration::from_millis(500);
        let speed_mb = sw.current_speed(t_check) / (1024.0 * 1024.0);
        // 10 MB in 0.5s = 20 MB/s
        assert!((speed_mb - 20.0).abs() < 0.5, "speed was: {}", speed_mb);
    }
}
