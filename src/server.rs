use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::signaling::{receive_signal_msg, send_signal_msg, SignalMessage};

#[derive(Clone)]
struct RegisteredSession {
    candidates: Vec<String>,
    observed_ip: String,
    created_at: Instant,
    ttl: Duration,
}

struct RelayWaiter {
    role: String,
    sender: oneshot::Sender<TcpStream>,
}

pub struct SignalingServer {
    port: u16,
    sessions: Arc<Mutex<HashMap<String, RegisteredSession>>>,
    relay_waiters: Arc<Mutex<HashMap<String, RelayWaiter>>>,
}

impl SignalingServer {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            relay_waiters: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.port))
            .await
            .with_context(|| format!("Failed to bind signaling server on port {}", self.port))?;

        println!(
            "⚡ HeavyDrop Signaling & Relay Server active on port {}",
            self.port
        );

        let sessions = self.sessions.clone();
        let relay_waiters = self.relay_waiters.clone();

        // Background session cleanup task
        let cleanup_sessions = self.sessions.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let mut map = cleanup_sessions.lock();
                map.retain(|_, s| s.created_at.elapsed() < s.ttl);
            }
        });

        loop {
            let (stream, peer_addr) = listener.accept().await?;
            let sessions_clone = sessions.clone();
            let relay_clone = relay_waiters.clone();

            tokio::spawn(async move {
                if let Err(e) = handle_client(stream, peer_addr, sessions_clone, relay_clone).await
                {
                    eprintln!("Signaling client error from {}: {}", peer_addr, e);
                }
            });
        }
    }
}

async fn handle_client(
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    sessions: Arc<Mutex<HashMap<String, RegisteredSession>>>,
    relay_waiters: Arc<Mutex<HashMap<String, RelayWaiter>>>,
) -> Result<()> {
    let msg = tokio::time::timeout(
        crate::signaling::SIGNALING_TIMEOUT,
        receive_signal_msg(&mut stream),
    )
    .await??;

    match msg {
        SignalMessage::Register {
            lookup_hash,
            candidates,
        } => {
            let mut all_candidates = candidates;
            let observed = peer_addr.ip().to_string();
            // Preserve the sender's real listening port, including fallback ports.
            let ports: std::collections::HashSet<u16> = all_candidates
                .iter()
                .filter_map(|s| s.parse::<SocketAddr>().ok().map(|a| a.port()))
                .collect();
            for port in ports {
                all_candidates.push(SocketAddr::new(peer_addr.ip(), port).to_string());
            }

            {
                let mut map = sessions.lock();
                map.insert(
                    lookup_hash,
                    RegisteredSession {
                        candidates: all_candidates,
                        observed_ip: observed.clone(),
                        created_at: Instant::now(),
                        ttl: crate::session::DEFAULT_SESSION_TTL,
                    },
                );
            }

            let resp = SignalMessage::Registered {
                observed_ip: observed,
            };
            send_signal_msg(&mut stream, &resp).await?;
        }
        SignalMessage::Lookup { lookup_hash } => {
            let (found, candidates, observed_ip) = {
                let map = sessions.lock();
                if let Some(session) = map.get(&lookup_hash) {
                    if session.created_at.elapsed() < session.ttl {
                        (
                            true,
                            session.candidates.clone(),
                            session.observed_ip.clone(),
                        )
                    } else {
                        (false, Vec::new(), String::new())
                    }
                } else {
                    (false, Vec::new(), String::new())
                }
            };

            let resp = SignalMessage::LookupResult {
                found,
                candidates,
                observed_ip,
            };
            send_signal_msg(&mut stream, &resp).await?;
        }
        SignalMessage::RelayJoin { lookup_hash, role } => {
            let valid_session = sessions
                .lock()
                .get(&lookup_hash)
                .is_some_and(|s| s.created_at.elapsed() < s.ttl);
            if !valid_session || (role != "sender" && role != "receiver") {
                send_signal_msg(
                    &mut stream,
                    &SignalMessage::Error {
                        message: "Sesión inexistente, expirada o rol inválido".to_string(),
                    },
                )
                .await?;
                return Ok(());
            }
            // Inspect and insert under one lock so simultaneous joins cannot overwrite a room.
            let room = {
                let mut map = relay_waiters.lock();
                map.retain(|_, waiter| !waiter.sender.is_closed());
                if map.get(&lookup_hash).is_some_and(|w| w.role == role) {
                    Err(())
                } else if let Some(waiter) = map.remove(&lookup_hash) {
                    Ok((Some(waiter.sender), None))
                } else {
                    let (tx, rx) = oneshot::channel();
                    map.insert(lookup_hash.clone(), RelayWaiter { role, sender: tx });
                    Ok((None, Some(rx)))
                }
            };
            match room {
                Err(()) => {
                    send_signal_msg(
                        &mut stream,
                        &SignalMessage::Error {
                            message: "Ya hay un equipo con ese rol esperando en el relay"
                                .to_string(),
                        },
                    )
                    .await?;
                }
                Ok((Some(tx), _)) => {
                    // Notify this peer that relay is ready
                    send_signal_msg(&mut stream, &SignalMessage::RelayReady).await?;
                    // Send the stream to the waiting task to bridge them
                    let _ = tx.send(stream);
                }
                Ok((None, Some(rx))) => {
                    // Await peer connection with timeout
                    match tokio::time::timeout(Duration::from_secs(60), rx).await {
                        Ok(Ok(mut other_stream)) => {
                            send_signal_msg(&mut stream, &SignalMessage::RelayReady).await?;
                            // Bridge the two encrypted streams (dumb pipe)
                            let _ =
                                tokio::io::copy_bidirectional(&mut stream, &mut other_stream).await;
                        }
                        _ => {
                            {
                                let mut map = relay_waiters.lock();
                                if map.get(&lookup_hash).is_some_and(|w| w.sender.is_closed()) {
                                    map.remove(&lookup_hash);
                                }
                            }
                            let _ = send_signal_msg(
                                &mut stream,
                                &SignalMessage::Error {
                                    message: "Relay timeout: peer did not join".to_string(),
                                },
                            )
                            .await;
                        }
                    }
                }
                Ok((None, None)) => unreachable!(),
            }
        }
        _ => {}
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{decrypt_chunk, derive_session_cipher, encrypt_chunk, generate_keypair};
    use crate::signaling::SignalingClient;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct TestServer(tokio::task::JoinHandle<()>);
    impl Drop for TestServer {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    async fn start() -> (SignalingClient, TestServer) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = SignalingClient::new(Some(listener.local_addr().unwrap().to_string()));
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let waiters = Arc::new(Mutex::new(HashMap::new()));
        let task = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        // Model the sender's WAN address separately from localhost transport.
                        clients.spawn(handle_client(stream, "203.0.113.10:40000".parse().unwrap(),
                            sessions.clone(), waiters.clone()));
                    }
                    _ = clients.join_next(), if !clients.is_empty() => {}
                }
            }
        });
        (client, TestServer(task))
    }

    #[tokio::test]
    async fn lookup_keeps_sender_address_and_non_default_port() {
        let (client, _server) = start().await;
        client
            .register_session("session", &["192.168.1.20:51666".parse().unwrap()])
            .await
            .unwrap();
        let (candidates, observed) = client.lookup_session("session").await.unwrap().unwrap();
        assert_eq!(observed, "203.0.113.10");
        assert!(candidates.contains(&"203.0.113.10:51666".parse().unwrap()));
        assert!(!candidates.contains(&"203.0.113.10:7878".parse().unwrap()));
    }

    #[tokio::test]
    async fn real_relay_preserves_encrypted_payload_in_both_arrival_orders() {
        let (client, _server) = start().await;
        for receiver_first in [false, true] {
            let hash = format!("relay-{receiver_first}");
            client.register_session(&hash, &[]).await.unwrap();
            let first_role = if receiver_first { "receiver" } else { "sender" };
            let second_role = if receiver_first { "sender" } else { "receiver" };
            let (first, second) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(
                    client.connect_relay(&hash, first_role),
                    client.connect_relay(&hash, second_role)
                )
            })
            .await
            .unwrap();
            let mut first = first.unwrap();
            let mut second = second.unwrap();
            let (a_secret, a_public) = generate_keypair().unwrap();
            let (b_secret, b_public) = generate_keypair().unwrap();
            let salt = [7; 32];
            let a_cipher = derive_session_cipher(&a_secret, &b_public, &[9; 32], &salt);
            let b_cipher = derive_session_cipher(&b_secret, &a_public, &[9; 32], &salt);
            let payload = b"private file contents";
            let encrypted = encrypt_chunk(&a_cipher, &salt, 0, payload).unwrap();
            first.write_all(&encrypted).await.unwrap();
            let mut received = vec![0; encrypted.len()];
            second.read_exact(&mut received).await.unwrap();
            assert_ne!(received, payload);
            assert_eq!(
                decrypt_chunk(&b_cipher, &salt, 0, &received).unwrap(),
                payload
            );
            second.write_all(b"ack").await.unwrap();
            let mut ack = [0; 3];
            first.read_exact(&mut ack).await.unwrap();
            assert_eq!(&ack, b"ack");
        }
    }

    #[tokio::test]
    async fn relay_rejects_unknown_session_and_invalid_role() {
        let (client, _server) = start().await;
        assert!(client.connect_relay("unknown", "receiver").await.is_err());
        client.register_session("registered", &[]).await.unwrap();
        assert!(client.connect_relay("registered", "invalid").await.is_err());
    }
}
