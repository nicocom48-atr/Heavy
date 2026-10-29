use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const DEFAULT_SIGNALING_PORT: u16 = 7880;
pub const SIGNALING_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum SignalMessage {
    // 1. Sender registers session candidates
    Register {
        lookup_hash: String,
        candidates: Vec<String>,
    },
    Registered {
        observed_ip: String,
    },

    // 2. Receiver looks up session
    Lookup {
        lookup_hash: String,
    },
    LookupResult {
        found: bool,
        candidates: Vec<String>,
        observed_ip: String,
    },

    // 3. Fallback: Relay tunnel request
    RelayJoin {
        lookup_hash: String,
        role: String, // "sender" or "receiver"
    },
    RelayReady,

    Error {
        message: String,
    },
}

#[derive(Clone)]
pub struct SignalingClient {
    server_addr: Option<String>,
}

impl SignalingClient {
    pub fn new(server_addr: Option<String>) -> Self {
        Self { server_addr }
    }

    /// Default signaling server (can be overridden via HEAVYDROP_SIGNAL_SERVER env var)
    pub fn default_server() -> Option<String> {
        if let Ok(addr) = std::env::var("HEAVYDROP_SIGNAL_SERVER") {
            if !addr.trim().is_empty() {
                return Some(addr.trim().to_string());
            }
        }
        None
    }

    pub fn is_available(&self) -> bool {
        self.server_addr.is_some()
    }

    /// Register a session on the rendezvous server with candidate endpoints
    pub async fn register_session(
        &self,
        lookup_hash: &str,
        candidates: &[SocketAddr],
    ) -> Result<Option<String>> {
        let server = match &self.server_addr {
            Some(s) => s,
            None => return Ok(None),
        };

        let cand_strings = candidates.iter().map(|c| c.to_string()).collect();
        let msg = SignalMessage::Register {
            lookup_hash: lookup_hash.to_string(),
            candidates: cand_strings,
        };

        let mut stream = tokio::time::timeout(SIGNALING_TIMEOUT, TcpStream::connect(server))
            .await
            .context("Signaling connection timeout")?
            .context("Failed to connect to signaling server")?;

        send_signal_msg(&mut stream, &msg).await?;
        let resp = receive_signal_msg(&mut stream).await?;

        match resp {
            SignalMessage::Registered { observed_ip } => Ok(Some(observed_ip)),
            SignalMessage::Error { message } => bail!("Signaling error: {}", message),
            other => bail!("Unexpected response from signaling server: {:?}", other),
        }
    }

    /// Resolve session candidates for a pairing code from the rendezvous server
    pub async fn lookup_session(
        &self,
        lookup_hash: &str,
    ) -> Result<Option<(Vec<SocketAddr>, String)>> {
        let server = match &self.server_addr {
            Some(s) => s,
            None => return Ok(None),
        };

        let msg = SignalMessage::Lookup {
            lookup_hash: lookup_hash.to_string(),
        };

        let mut stream = tokio::time::timeout(SIGNALING_TIMEOUT, TcpStream::connect(server))
            .await
            .context("Signaling connection timeout")?
            .context("Failed to connect to signaling server")?;

        send_signal_msg(&mut stream, &msg).await?;
        let resp = receive_signal_msg(&mut stream).await?;

        match resp {
            SignalMessage::LookupResult {
                found: true,
                candidates,
                observed_ip,
            } => {
                let parsed: Vec<SocketAddr> =
                    candidates.iter().filter_map(|s| s.parse().ok()).collect();
                Ok(Some((parsed, observed_ip)))
            }
            SignalMessage::LookupResult { found: false, .. } => Ok(None),
            SignalMessage::Error { message } => bail!("Signaling error: {}", message),
            other => bail!("Unexpected response from signaling server: {:?}", other),
        }
    }

    /// Establish a relay stream through the rendezvous server when direct connection is blocked by NAT
    pub async fn connect_relay(
        &self,
        lookup_hash: &str,
        role: &str,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Unpin + Send>> {
        let server = match &self.server_addr {
            Some(s) => s,
            None => bail!("No hay servidor de señalización configurado para el modo Relay"),
        };

        let mut stream = tokio::time::timeout(SIGNALING_TIMEOUT, TcpStream::connect(server))
            .await
            .context("Relay connection timeout")?
            .context("Failed to connect to relay server")?;

        let msg = SignalMessage::RelayJoin {
            lookup_hash: lookup_hash.to_string(),
            role: role.to_string(),
        };

        send_signal_msg(&mut stream, &msg).await?;
        let resp = receive_signal_msg(&mut stream).await?;

        match resp {
            SignalMessage::RelayReady => Ok(Box::new(stream)),
            SignalMessage::Error { message } => bail!("Relay error: {}", message),
            other => bail!("Unexpected response from relay server: {:?}", other),
        }
    }
}

pub async fn send_signal_msg<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    msg: &SignalMessage,
) -> Result<()> {
    let bytes = serde_json::to_vec(msg)?;
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn receive_signal_msg<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<SignalMessage> {
    let len = reader.read_u32().await?;
    if len > 32 * 1024 {
        bail!("Signaling payload too large");
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    let msg = serde_json::from_slice(&buf)?;
    Ok(msg)
}
