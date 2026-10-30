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

pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {
    fn close(
        &mut self,
        _wait_for_peer: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}
impl AsyncStream for TcpStream {}

pub struct EstablishedConnection {
    pub stream: Box<dyn AsyncStream>,
    pub conn_type: ConnectionType,
    pub peer_description: String,
}

// Stop advertising on success, failure, or when the connection future is cancelled.
struct BeaconGuard(tokio::task::JoinHandle<()>);
impl Drop for BeaconGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
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
    let _beacon = BeaconGuard(spawn_blind_beacon(
        session.code().lan_blind_ticket(),
        local_port,
        cancel_signal.clone(),
    ));

    // 3. Register on signaling server if available
    let mut candidates = Vec::new();
    for (_iface, ip) in get_available_local_ips() {
        candidates.push(SocketAddr::new(ip, local_port));
    }

    let lookup_hash = session.code().signaling_lookup_hash();
    signal_client
        .register_session(&lookup_hash, &candidates)
        .await
        .context("No se pudo registrar la transferencia en Internet. Comprueba el servidor configurado o deja el campo vacío para usar solo LAN")?;

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
        relay = async {
            if signal_client.is_available() {
                loop {
                    match signal_client.connect_relay(&lookup_hash, "sender").await {
                        Ok(stream) => return stream,
                        Err(error) => {
                            eprintln!("Relay: {error:#}");
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            }
            std::future::pending::<TcpStream>().await
        } => {
            Ok(EstablishedConnection {
                stream: Box::new(relay),
                conn_type: ConnectionType::Relay,
                peer_description: "Relay cifrado".to_string(),
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
    if code.invitation().is_some() {
        return crate::automatic::connect_receiver(code, None).await;
    }
    // 1. Attempt LAN discovery first (up to 3 seconds)
    if let Some(lan_addr) = discovery.resolve_local(code, Duration::from_secs(3)).await {
        if let Ok(Ok(stream)) =
            tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(lan_addr)).await
        {
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
    if !signal_client.is_available() {
        bail!("No se encontró al emisor en la red local. Para conectar entre redes distintas, configura el mismo servidor de señalización en ambos equipos (HEAVYDROP_SIGNAL_SERVER o el campo Servidor de Internet). Comprueba también el permiso de HeavyDrop en el firewall de Windows.");
    }
    let lookup_res = signal_client
        .lookup_session(&lookup_hash)
        .await
        .context("No se pudo consultar el servidor de señalización. Comprueba la dirección y el puerto en ambos equipos")?;

    if let Some((candidates, _observed_ip)) = lookup_res {
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

        let stream = signal_client
            .connect_relay(&lookup_hash, "receiver")
            .await
            .context("Falló la conexión mediante relay")?;
        return Ok(EstablishedConnection {
            stream: Box::new(stream),
            conn_type: ConnectionType::Relay,
            peer_description: "Relay cifrado".to_string(),
        });
    }

    bail!(
        "No se encontró una sesión activa para el código {}. Comprueba que no haya expirado, que el emisor haya terminado de preparar el archivo y que ambos equipos usen el mismo servidor de Internet.",
        code.formatted()
    );
}

fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_private(),
        std::net::IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}
