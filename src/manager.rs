use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::receiver::{run_receiver, ReceiverConfig};
use crate::sender::{run_sender, SenderConfig};
use crate::transfer_state::{
    new_shared_progress, SharedProgress, TransferProgress, TransferRole, TransferStatus,
};

#[derive(Clone)]
pub struct TransferManager {
    progress: SharedProgress,
    cancel_signal: Arc<AtomicBool>,
    active: Arc<tokio::sync::Mutex<()>>,
}

impl TransferManager {
    pub fn new() -> Self {
        Self {
            progress: new_shared_progress(),
            cancel_signal: Arc::new(AtomicBool::new(false)),
            active: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn with_progress(progress: SharedProgress) -> Self {
        Self {
            progress,
            cancel_signal: Arc::new(AtomicBool::new(false)),
            active: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn progress(&self) -> SharedProgress {
        self.progress.clone()
    }

    pub fn cancel_signal(&self) -> Arc<AtomicBool> {
        self.cancel_signal.clone()
    }

    pub fn is_busy(&self) -> bool {
        if self.active.try_lock().is_err() { return true; }
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
        p.status_text = "Cancelando transferencia y cerrando servidor temporal...".to_string();
        p.bytes_per_sec = 0.0;
    }

    pub fn reset_cancel(&self) {
        self.cancel_signal.store(false, Ordering::Relaxed);
    }

    pub async fn run_send(&self, mut config: SenderConfig) -> Result<()> {
        let _active = self.active.try_lock().map_err(|_| anyhow::anyhow!("Ya hay una transferencia en curso"))?;
        self.reset_cancel();
        self.start(TransferRole::Sender);
        config.shared_progress = Some(self.progress.clone());
        config.cancel_signal = Some(self.cancel_signal.clone());
        let result = self.until_cancelled(run_sender(config)).await;
        self.finish(&result);
        result
    }

    pub async fn run_recv(&self, mut config: ReceiverConfig) -> Result<()> {
        let _active = self.active.try_lock().map_err(|_| anyhow::anyhow!("Ya hay una transferencia en curso"))?;
        self.reset_cancel();
        self.start(TransferRole::Receiver);
        config.shared_progress = Some(self.progress.clone());
        config.cancel_signal = Some(self.cancel_signal.clone());
        let result = self.until_cancelled(run_receiver(config)).await;
        self.finish(&result);
        result
    }

    fn start(&self, role: TransferRole) {
        *self.progress.lock() = TransferProgress {
            role,
            status: TransferStatus::Preparing,
            ..TransferProgress::default()
        };
    }

    async fn until_cancelled(
        &self,
        transfer: impl std::future::Future<Output = Result<()>>,
    ) -> Result<()> {
        tokio::select! {
            result = transfer => result,
            _ = async {
                while !self.cancel_signal.load(Ordering::Relaxed) {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            } => anyhow::bail!("Transferencia cancelada por el usuario"),
        }
    }

    fn finish(&self, result: &Result<()>) {
        if let Err(error) = result {
            let mut p = self.progress.lock();
            p.bytes_per_sec = 0.0;
            if self.cancel_signal.load(Ordering::Relaxed) {
                p.status = TransferStatus::Cancelled;
                p.status_text = "Transferencia cancelada por el usuario".to_string();
            } else {
                p.status = TransferStatus::Failed;
                p.status_text = "La transferencia falló".to_string();
                p.error = Some(format!("{error:#}"));
            }
        }
    }
}

impl Default for TransferManager {
    fn default() -> Self {
        Self::new()
    }
}
