use anyhow::{bail, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::pairing::PairingCode;

pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(5 * 60); // 5 minutes

#[derive(Clone)]
pub struct Session {
    id: String,
    code: PairingCode,
    salt: [u8; 32],
    created_at: Arc<parking_lot::Mutex<Instant>>,
    ttl: Duration,
    consumed: Arc<AtomicBool>,
}

impl Session {
    /// Create a new session with an automatically generated cryptographically secure pairing code
    pub fn new() -> Result<Self> {
        let code = PairingCode::generate()?;
        let mut salt = [0u8; 32];
        getrandom::getrandom(&mut salt)
            .map_err(|e| anyhow::anyhow!("Failed to generate session salt: {}", e))?;

        let id = format!("hvs-{}", &blake3::hash(&salt).to_hex()[..16]);

        Ok(Self {
            id,
            code,
            salt,
            created_at: Arc::new(parking_lot::Mutex::new(Instant::now())),
            ttl: DEFAULT_SESSION_TTL,
            consumed: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn with_ttl(ttl: Duration) -> Result<Self> {
        let mut session = Self::new()?;
        session.ttl = ttl;
        Ok(session)
    }

    /// Construct a receiver session from a user-entered code
    pub fn from_code(code: PairingCode) -> Result<Self> {
        let mut salt = [0u8; 32];
        getrandom::getrandom(&mut salt)
            .map_err(|e| anyhow::anyhow!("Failed to generate receiver session salt: {}", e))?;

        let id = format!("hvr-{}", &blake3::hash(&salt).to_hex()[..16]);

        Ok(Self {
            id,
            code,
            salt,
            created_at: Arc::new(parking_lot::Mutex::new(Instant::now())),
            ttl: DEFAULT_SESSION_TTL,
            consumed: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn code(&self) -> &PairingCode {
        &self.code
    }

    pub fn salt(&self) -> &[u8; 32] {
        &self.salt
    }

    pub fn is_expired(&self) -> bool {
        self.created_at.lock().elapsed() >= self.ttl
    }

    pub fn remaining_time(&self) -> Duration {
        let elapsed = self.created_at.lock().elapsed();
        if elapsed >= self.ttl {
            Duration::ZERO
        } else {
            self.ttl - elapsed
        }
    }

    /// Format remaining time as MM:SS (e.g. "04:59")
    pub fn format_remaining(&self) -> String {
        let secs = self.remaining_time().as_secs();
        let mins = secs / 60;
        let rem = secs % 60;
        format!("{:02}:{:02}", mins, rem)
    }

    /// Mark session code as consumed (one-time use)
    pub fn consume(&self) -> Result<()> {
        if self.consumed.swap(true, Ordering::SeqCst) {
            bail!("El código de sesión ya ha sido utilizado.");
        }
        Ok(())
    }

    pub fn is_consumed(&self) -> bool {
        self.consumed.load(Ordering::SeqCst)
    }

    /// Start the invitation lifetime only when the file and listener are ready.
    pub fn start_waiting(&self) {
        *self.created_at.lock() = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_lifecycle_and_countdown() {
        let session = Session::new().unwrap();
        assert!(!session.is_expired());
        assert!(!session.is_consumed());
        assert!(session.remaining_time().as_secs() > 290);

        // One-time consumption test
        assert!(session.consume().is_ok());
        assert!(session.is_consumed());
        assert!(session.consume().is_err());
    }
}
