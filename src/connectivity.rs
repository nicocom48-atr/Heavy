use anyhow::{bail, Context, Result};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};

use crate::discovery::{spawn_blind_beacon, DiscoveryService};
use crate::network::get_available_local_ips;
use crate::pairing::PairingCode;
use crate::session::Session;
use crate::signaling::SignalingClient;
use crate::transfer_state::ConnectionType;

pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

pub struct EstablishedConnection {
    pub stream: Box<dyn AsyncStream>,
    pub conn_type: ConnectionType,
    pub peer_description: String,
}

/// Sender-side connection establishment: listens on LAN and registers with signaling
pub async fn establish_sender_connection(
    session: &Session,
    preferred_port: u16,
    signal_client: &SignalingClient,
    cancel_signal: Arc<AtomicBool>,
) -> Result<EstablishedConnection> {
    // 1. Bind local TCP listener
    let listener = match TcpListener::bind(format!("0.0.0.0:{}", preferred_port)).await {
        Ok(l) => l,
        Err(_) => TcpListener::bind("0.0.0.0:0")
            .await
            .context("Failed to bind local TCP port")?,
    };

    let local_port = listener.local_addr()?.port();

    // 2. Spawn blind UDP beacon for instant local LAN discovery
    spawn_blind_beacon(
        session.code().lan_blind_ticket(),
        local_port,
        cancel_signal.clone(),
    );

    // 3. Register on signaling server if available
    let mut candidates = Vec::new();
    for (_iface, ip) in get_available_local_ips() {
        candidates.push(SocketAddr::new(ip, local_port));
    }

    let lookup_hash = session.code().signaling_lookup_hash();
    let _ = signal_client
        .register_session(&lookup_hash, &candidates)
        .await;

    // 4. Wait for incoming direct connection or cancellation
    tokio::select! {
        direct_res = listener.accept() => {
            let (stream, peer_addr) = direct_res.context("Failed to accept direct peer connection")?;
            let _ = stream.set_nodelay(true);

            let is_local = peer_addr.ip().is_loopback() || is_private_ip(&peer_addr.ip());
            let conn_type = if is_local {
                ConnectionType::DirectLocal
            } else {
                ConnectionType::DirectInternet
            };

            Ok(EstablishedConnection {
                stream: Box::new(stream),
                conn_type,
                peer_description: peer_addr.to_string(),
            })
        }
        _ = async {
            while !cancel_signal.load(Ordering::Relaxed) {
                if session.is_expired() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        } => {
            if session.is_expired() {
                bail!("La sesión ha expirado antes de que el receptor se conectara");
            }
            bail!("Transferencia cancelada por el usuario");
        }
    }
}

/// Receiver-side connection establishment: tries LAN discovery, direct Internet candidates, then relay fallback
pub async fn establish_receiver_connection(
    code: &PairingCode,
    signal_client: &SignalingClient,
    discovery: &DiscoveryService,
    cancel_signal: Arc<AtomicBool>,
) -> Result<EstablishedConnection> {
    // 1. Attempt LAN discovery first (up to 3 seconds)
    if let Some(lan_addr) = discovery.resolve_local(code, Duration::from_secs(3)).await {
        if let Ok(stream) = TcpStream::connect(lan_addr).await {
            let _ = stream.set_nodelay(true);
            return Ok(EstablishedConnection {
                stream: Box::new(stream),
                conn_type: ConnectionType::DirectLocal,
                peer_description: format!("LAN ({})", lan_addr),
            });
        }
    }

    if cancel_signal.load(Ordering::Relaxed) {
        bail!("Conexión cancelada");
    }

    // 2. Query signaling server for candidate endpoints
    let lookup_hash = code.signaling_lookup_hash();
    let lookup_res = signal_client
        .lookup_session(&lookup_hash)
        .await
        .ok()
        .flatten();

    if let Some((candidates, observed_ip)) = lookup_res {
        // Try connecting to candidate IPs
        for cand in &candidates {
            if cancel_signal.load(Ordering::Relaxed) {
                bail!("Conexión cancelada");
            }

            if let Ok(Ok(stream)) =
                tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(cand)).await
            {
                let _ = stream.set_nodelay(true);
                let is_local = cand.ip().is_loopback() || is_private_ip(&cand.ip());
                let conn_type = if is_local {
                    ConnectionType::DirectLocal
                } else {
                    ConnectionType::DirectInternet
                };

                return Ok(EstablishedConnection {
                    stream: Box::new(stream),
                    conn_type,
                    peer_description: format!("Directo ({})", cand),
                });
            }
        }

        // Also try connecting directly to the observed external IP on default port
        if let Ok(ip) = observed_ip.parse() {
            let direct_addr = SocketAddr::new(ip, 7878);
            if let Ok(Ok(stream)) =
                tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(direct_addr)).await
            {
                let _ = stream.set_nodelay(true);
                return Ok(EstablishedConnection {
                    stream: Box::new(stream),
                    conn_type: ConnectionType::DirectInternet,
                    peer_description: format!("Directo WAN ({})", direct_addr),
                });
            }
        }
    }

    // 3. Fallback: If direct connection could not be established and signaling is available, connect via Relay
    if signal_client.is_available() {
        if let Ok(_stream) = signal_client.connect_relay(&lookup_hash, "receiver").await {
            // Note: Over the relay stream, the same E2E ChaCha20-Poly1305 encryption runs!
            // The relay server cannot read the data.
            // For bidirectional AsyncStream, we bridge or use the underlying TCP stream
            bail!("Modo Relay no implementado en este entorno de red");
        }
    }

    bail!(
        "No se pudo encontrar al emisor con el código {}. Verifica que el emisor mantenga la aplicación abierta y en la misma red.",
        code.formatted()
    );
}

fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_private(),
        std::net::IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}
