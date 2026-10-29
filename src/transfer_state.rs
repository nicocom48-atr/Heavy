use parking_lot::Mutex;
use std::sync::Arc;

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
            bytes_per_sec: 0.0,
            status_text: String::new(),
            peer_address: String::new(),
            error: None,
        }
    }
}

pub type SharedProgress = Arc<Mutex<TransferProgress>>;

pub fn new_shared_progress() -> SharedProgress {
    Arc::new(Mutex::new(TransferProgress::default()))
}
