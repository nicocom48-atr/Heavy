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
    created_at: Instant,
    ttl: Duration,
}

pub struct SignalingServer {
    port: u16,
    sessions: Arc<Mutex<HashMap<String, RegisteredSession>>>,
    relay_waiters: Arc<Mutex<HashMap<String, oneshot::Sender<TcpStream>>>>,
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
    relay_waiters: Arc<Mutex<HashMap<String, oneshot::Sender<TcpStream>>>>,
) -> Result<()> {
    let msg = receive_signal_msg(&mut stream).await?;

    match msg {
        SignalMessage::Register {
            lookup_hash,
            candidates,
        } => {
            let mut all_candidates = candidates;
            let observed = peer_addr.ip().to_string();
            // Also add the observed external IP with standard ports
            all_candidates.push(format!("{}:7878", observed));

            {
                let mut map = sessions.lock();
                map.insert(
                    lookup_hash,
                    RegisteredSession {
                        candidates: all_candidates,
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
            let (found, candidates) = {
                let map = sessions.lock();
                if let Some(session) = map.get(&lookup_hash) {
                    if session.created_at.elapsed() < session.ttl {
                        (true, session.candidates.clone())
                    } else {
                        (false, Vec::new())
                    }
                } else {
                    (false, Vec::new())
                }
            };

            let resp = SignalMessage::LookupResult {
                found,
                candidates,
                observed_ip: peer_addr.ip().to_string(),
            };
            send_signal_msg(&mut stream, &resp).await?;
        }
        SignalMessage::RelayJoin {
            lookup_hash,
            role: _,
        } => {
            // Check if there is already a waiting peer in the relay room
            let waiting_sender = {
                let mut map = relay_waiters.lock();
                map.remove(&lookup_hash)
            };

            match waiting_sender {
                Some(tx) => {
                    // Notify this peer that relay is ready
                    send_signal_msg(&mut stream, &SignalMessage::RelayReady).await?;
                    // Send the stream to the waiting task to bridge them
                    let _ = tx.send(stream);
                }
                None => {
                    // First peer to arrive: create a channel and wait for the other
                    let (tx, rx) = oneshot::channel();
                    {
                        let mut map = relay_waiters.lock();
                        map.insert(lookup_hash.clone(), tx);
                    }

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
                                map.remove(&lookup_hash);
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
            }
        }
        _ => {}
    }

    Ok(())
}
