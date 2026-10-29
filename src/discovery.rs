use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

pub const DISCOVERY_PORT: u16 = 7879;
pub const APP_ID: &str = "heavydrop";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PeerAnnouncement {
    pub app: String,
    pub version: u32,
    pub device_name: String,
    pub port: u16,
    pub file_name: String,
    pub file_size: u64,
}

#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub announcement: PeerAnnouncement,
    pub address: String,
    pub last_seen: Instant,
}

/// Spawns a background beacon broadcasting sender details to the local network
pub fn spawn_beacon(
    file_name: String,
    file_size: u64,
    listen_port: u16,
    cancel_signal: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let socket = match UdpSocket::bind("0.0.0.0:0").await {
            Ok(s) => s,
            Err(_) => return,
        };
        let _ = socket.set_broadcast(true);

        let device_name = match local_ip_address::local_ip() {
            Ok(ip) => format!("HeavyDrop ({})", ip),
            Err(_) => "HeavyDrop Peer".to_string(),
        };

        let announcement = PeerAnnouncement {
            app: APP_ID.to_string(),
            version: crate::protocol::PROTOCOL_VERSION,
            device_name,
            port: listen_port,
            file_name,
            file_size,
        };

        let json_bytes = match serde_json::to_vec(&announcement) {
            Ok(b) => b,
            Err(_) => return,
        };

        let broadcast_addr: SocketAddr = format!("255.255.255.255:{}", DISCOVERY_PORT)
            .parse()
            .expect("Invalid broadcast addr");

        while !cancel_signal.load(Ordering::Relaxed) {
            let _ = socket.send_to(&json_bytes, broadcast_addr).await;
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
    });
}

/// Discovers senders advertising on the local network
pub struct DiscoveryService {
    peers: Arc<Mutex<HashMap<String, DiscoveredPeer>>>,
    cancel_signal: Arc<AtomicBool>,
}

impl DiscoveryService {
    pub fn new() -> Self {
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let cancel_signal = Arc::new(AtomicBool::new(false));

        let peers_clone = peers.clone();
        let cancel_clone = cancel_signal.clone();

        tokio::spawn(async move {
            let socket = match UdpSocket::bind(format!("0.0.0.0:{}", DISCOVERY_PORT)).await {
                Ok(s) => s,
                Err(_) => return,
            };

            let mut buf = [0u8; 1024];

            while !cancel_clone.load(Ordering::Relaxed) {
                tokio::select! {
                    recv_res = socket.recv_from(&mut buf) => {
                        if let Ok((len, src)) = recv_res {
                            if let Ok(ann) = serde_json::from_slice::<PeerAnnouncement>(&buf[..len]) {
                                if ann.app == APP_ID && ann.version == crate::protocol::PROTOCOL_VERSION {
                                    let peer_addr = format!("{}:{}", src.ip(), ann.port);
                                    let mut map = peers_clone.lock();
                                    map.insert(
                                        peer_addr.clone(),
                                        DiscoveredPeer {
                                            announcement: ann,
                                            address: peer_addr,
                                            last_seen: Instant::now(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {
                        // Clean peers unseen for more than 4 seconds
                        let mut map = peers_clone.lock();
                        map.retain(|_, p| p.last_seen.elapsed() < Duration::from_secs(5));
                    }
                }
            }
        });

        Self {
            peers,
            cancel_signal,
        }
    }

    pub fn get_active_peers(&self) -> Vec<DiscoveredPeer> {
        let map = self.peers.lock();
        let mut list: Vec<DiscoveredPeer> = map.values().cloned().collect();
        list.sort_by_key(|a| std::cmp::Reverse(a.last_seen));
        list
    }
}

impl Default for DiscoveryService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DiscoveryService {
    fn drop(&mut self) {
        self.cancel_signal.store(true, Ordering::Relaxed);
    }
}
