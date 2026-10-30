//! A transfer owns its endpoint: no permanent listener or background service.
use anyhow::{bail, Context, Result};
use iroh::{
    endpoint::{presets, Connection, RecvStream, SendStream},
    Endpoint, RelayMode,
};
use std::{
    io,
    pin::Pin,
    task::{Context as TaskContext, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::{
    connectivity::{AsyncStream, EstablishedConnection},
    pairing::PairingCode,
    session::Session,
    transfer_state::{ConnectionType, SharedProgress, TransferStatus},
};

pub const ALPN: &[u8] = b"heavydrop/4";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TransportMode {
    #[default]
    Automatic,
    /// Iroh without public infrastructure; useful on LAN and for deterministic tests.
    DirectOnly,
    /// Diagnostics and relay-only networks; still uses end-to-end encryption.
    RelayOnly(iroh::RelayUrl),
    /// Previous short-code TCP discovery and optional custom rendezvous.
    LegacyTcp,
}

struct EndpointGuard(Option<Endpoint>);
impl EndpointGuard {
    fn endpoint(&self) -> &Endpoint {
        self.0.as_ref().expect("live endpoint")
    }
    async fn close(&mut self) {
        if let Some(endpoint) = self.0.take() {
            endpoint.close().await;
        }
    }
}
impl Drop for EndpointGuard {
    fn drop(&mut self) {
        if let Some(endpoint) = self.0.take() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    endpoint.close().await;
                });
            }
        }
    }
}

pub async fn listen_sender(
    session: &Session,
    mode: TransportMode,
    progress: Option<SharedProgress>,
) -> Result<EstablishedConnection> {
    let relay_mode = match &mode {
        TransportMode::DirectOnly => RelayMode::Disabled,
        TransportMode::RelayOnly(url) => RelayMode::Custom([url.clone()].into_iter().collect()),
        _ => RelayMode::Default,
    };
    let mut builder = Endpoint::builder(presets::Minimal)
            .alpns(vec![ALPN.to_vec()])
            .relay_mode(relay_mode);
    if matches!(mode, TransportMode::RelayOnly(_)) { builder = builder.clear_ip_transports(); }
    let mut owner = EndpointGuard(Some(
        builder
            .bind()
            .await
            .context("No se pudo abrir el servidor temporal de transferencia")?,
    ));
    if mode != TransportMode::DirectOnly {
        tokio::time::timeout(Duration::from_secs(25), owner.endpoint().online()).await
            .context("No se pudo acceder a los relays de conexión automática. Comprueba Internet y el firewall")?;
    }
    // Local interface enumeration can finish just after bind returns.
    tokio::time::timeout(Duration::from_secs(5), async {
        while owner.endpoint().addr().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("No se encontraron rutas para el servidor temporal")?;
    session.start_waiting();
    let invitation = session
        .code()
        .with_invitation(owner.endpoint().addr(), session.remaining_time())?
        .formatted();
    if let Some(ref shared) = progress {
        let mut p = shared.lock();
        p.status = TransferStatus::WaitingForPeer;
        p.pairing_code = Some(invitation.clone());
        p.status_text = "Servidor temporal listo. Comparte la invitación con el receptor.".into();
    }
    println!(
        "  Invitación: {invitation}\n  Servidor temporal listo. Expira en {}",
        session.format_remaining()
    );
    let accepted = tokio::time::timeout(session.remaining_time(), async {
        loop {
            let incoming = owner
                .endpoint()
                .accept()
                .await
                .context("El servidor temporal se cerró")?;
            // A stalled or incompatible peer must not occupy the whole invitation lifetime.
            let attempt = tokio::time::timeout(Duration::from_secs(5), async {
                let connection = incoming.await?;
                let (send, mut recv) = connection.accept_bi().await?;
                if recv.read_u8().await? != 4 {
                    bail!("Prefijo de conexión incompatible");
                }
                Ok::<_, anyhow::Error>((connection, send, recv))
            })
            .await;
            if let Ok(Ok(parts)) = attempt {
                return Ok::<_, anyhow::Error>(parts);
            }
        }
    })
    .await;
    let (connection, send, recv) = match accepted {
        Ok(result) => result?,
        Err(_) => {
            owner.close().await;
            bail!("La invitación expiró. El servidor temporal se cerró");
        }
    };
    Ok(established(owner, connection, send, recv, progress))
}

pub async fn connect_receiver(
    code: &PairingCode,
    progress: Option<SharedProgress>,
) -> Result<EstablishedConnection> {
    let invitation = code
        .invitation()
        .context("Falta la invitación automática")?;
    if invitation.expires_at <= crate::pairing::unix_now()? {
        bail!("La invitación expiró");
    }
    let relay_mode = if invitation.endpoint.relay_urls().next().is_some() {
        RelayMode::Custom(invitation.endpoint.relay_urls().cloned().collect())
    } else {
        RelayMode::Disabled
    };
    let owner = EndpointGuard(Some(
        Endpoint::builder(presets::Minimal)
            .relay_mode(relay_mode)
            .bind()
            .await?,
    ));
    let connection = tokio::time::timeout(Duration::from_secs(30),
        owner.endpoint().connect(invitation.endpoint.clone(), ALPN)).await
        .context("No se pudo conectar a tiempo. Comprueba que el emisor siga abierto y la invitación no haya expirado")??;
    let (mut send, recv) = connection.open_bi().await?;
    // A QUIC stream becomes visible remotely only after the initiator writes.
    send.write_u8(4).await?;
    Ok(established(owner, connection, send, recv, progress))
}

fn connection_type(connection: &Connection) -> ConnectionType {
    for path in connection.paths().iter().filter(|p| p.is_selected()) {
        return if path.is_relay() {
            ConnectionType::Relay
        } else {
            match path.remote_addr() {
                iroh::TransportAddr::Ip(addr)
                    if addr.ip().is_loopback()
                        || matches!(addr.ip(), std::net::IpAddr::V4(ip) if ip.is_private()) =>
                {
                    ConnectionType::DirectLocal
                }
                _ => ConnectionType::DirectInternet,
            }
        };
    }
    ConnectionType::None
}

fn established(
    owner: EndpointGuard,
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
    progress: Option<SharedProgress>,
) -> EstablishedConnection {
    let conn_type = connection_type(&connection);
    let peer_description = connection.remote_id().to_string();
    let monitor = progress.map(|shared| {
        let connection = connection.clone();
        tokio::spawn(async move {
            loop {
                shared.lock().connection_type = connection_type(&connection);
                tokio::select! {
                    _ = connection.closed() => break,
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                }
            }
        })
    });
    EstablishedConnection {
        stream: Box::new(IrohStream {
            owner,
            connection,
            send,
            recv,
            monitor,
        }),
        conn_type,
        peer_description,
    }
}

struct IrohStream {
    owner: EndpointGuard,
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
    monitor: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for IrohStream {
    fn drop(&mut self) {
        if let Some(task) = self.monitor.take() {
            task.abort();
        }
    }
}
impl AsyncRead for IrohStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.recv), cx, buf)
    }
}
impl AsyncWrite for IrohStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.send), cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.send), cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.send), cx)
    }
}
impl AsyncStream for IrohStream {
    fn close(
        &mut self,
        wait_for_peer: bool,
    ) -> Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if wait_for_peer {
                // Let the sender read the completion ACK before closing QUIC.
                let _ =
                    tokio::time::timeout(Duration::from_secs(3), self.connection.closed()).await;
            }
            self.owner.close().await;
            if let Some(task) = self.monitor.take() {
                task.abort();
                let _ = task.await;
            }
        })
    }
}
