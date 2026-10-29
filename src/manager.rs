use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::receiver::{run_receiver, ReceiverConfig};
use crate::sender::{run_sender, SenderConfig};
use crate::transfer_state::{new_shared_progress, SharedProgress, TransferStatus};

#[derive(Clone)]
pub struct TransferManager {
    progress: SharedProgress,
    cancel_signal: Arc<AtomicBool>,
}

impl TransferManager {
    pub fn new() -> Self {
        Self {
            progress: new_shared_progress(),
            cancel_signal: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_progress(progress: SharedProgress) -> Self {
        Self {
            progress,
            cancel_signal: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn progress(&self) -> SharedProgress {
        self.progress.clone()
    }

    pub fn cancel_signal(&self) -> Arc<AtomicBool> {
        self.cancel_signal.clone()
    }

    pub fn is_busy(&self) -> bool {
        let p = self.progress.lock();
        matches!(
            p.status,
            TransferStatus::Preparing
                | TransferStatus::WaitingForPeer
                | TransferStatus::Connecting
                | TransferStatus::Connected
                | TransferStatus::Transferring
        )
    }

    /// Cooperative cancellation: signals background transfer loops to exit cleanly
    pub fn cancel(&self) {
        self.cancel_signal.store(true, Ordering::Relaxed);
        let mut p = self.progress.lock();
        p.status = TransferStatus::Cancelled;
        p.status_text = "Transferencia cancelada por el usuario.".to_string();
        p.bytes_per_sec = 0.0;
    }

    pub fn reset_cancel(&self) {
        self.cancel_signal.store(false, Ordering::Relaxed);
    }

    pub async fn run_send(&self, mut config: SenderConfig) -> Result<()> {
        self.reset_cancel();
        config.shared_progress = Some(self.progress.clone());
        config.cancel_signal = Some(self.cancel_signal.clone());
        run_sender(config).await
    }

    pub async fn run_recv(&self, mut config: ReceiverConfig) -> Result<()> {
        self.reset_cancel();
        config.shared_progress = Some(self.progress.clone());
        config.cancel_signal = Some(self.cancel_signal.clone());
        run_receiver(config).await
    }
}

impl Default for TransferManager {
    fn default() -> Self {
        Self::new()
    }
}
