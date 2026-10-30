use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

use crate::pairing::PairingCode;
use crate::protocol::PROTOCOL_VERSION;

pub const DISCOVERY_PORT: u16 = 7879;
pub const APP_ID: &str = "heavydrop-p2p";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BlindBeaconAnnouncement {
    pub app: String,
    pub version: u32,
    pub blind_ticket: [u8; 32],
    pub listen_port: u16,
}

#[derive(Debug, Clone)]
pub struct DiscoveredBeacon {
    pub sender_addr: SocketAddr,
    pub last_seen: Instant,
}

/// Spawns a background UDP beacon broadcasting a blind session ticket on LAN
pub fn spawn_blind_beacon(
    blind_ticket: [u8; 32],
    listen_port: u16,
    cancel_signal: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let socket = match UdpSocket::bind("0.0.0.0:0").await {
            Ok(s) => s,
            Err(_) => return,
        };
        let _ = socket.set_broadcast(true);

        let announcement = BlindBeaconAnnouncement {
            app: APP_ID.to_string(),
            version: PROTOCOL_VERSION,
            blind_ticket,
            listen_port,
        };

        let json_bytes = match serde_json::to_vec(&announcement) {
            Ok(b) => b,
            Err(_) => return,
        };

        let mut target_addrs: Vec<SocketAddr> = Vec::new();
        // 1. Loopback for same-machine discovery
        if let Ok(addr) = format!("127.0.0.1:{}", DISCOVERY_PORT).parse() {
            target_addrs.push(addr);
        }
        // 2. Global broadcast
        if let Ok(addr) = format!("255.255.255.255:{}", DISCOVERY_PORT).parse() {
            target_addrs.push(addr);
        }
        // 3. Subnet broadcast for all local network adapters (e.g. Wi-Fi / Ethernet)
        for (_iface, ip) in crate::network::get_available_local_ips() {
            if let std::net::IpAddr::V4(v4) = ip {
                let oct = v4.octets();
                let bcast = std::net::Ipv4Addr::new(oct[0], oct[1], oct[2], 255);
                target_addrs.push(SocketAddr::new(std::net::IpAddr::V4(bcast), DISCOVERY_PORT));
            }
        }

        while !cancel_signal.load(Ordering::Relaxed) {
            for &dest in &target_addrs {
                let _ = socket.send_to(&json_bytes, dest).await;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
}

fn bind_reuse_udp(port: u16) -> Result<UdpSocket, Box<dyn std::error::Error + Send + Sync>> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = sock.set_reuse_address(true);
    let addr: SocketAddr = format!("0.0.0.0:{}", port).parse()?;
    sock.bind(&addr.into())?;
    sock.set_nonblocking(true)?;
    let std_sock: std::net::UdpSocket = sock.into();
    let tokio_sock = UdpSocket::from_std(std_sock)?;
    Ok(tokio_sock)
}

/// Listens for LAN beacons and resolves pairing codes locally in milliseconds
pub struct DiscoveryService {
    beacons: Arc<Mutex<HashMap<[u8; 32], DiscoveredBeacon>>>,
    cancel_signal: Arc<AtomicBool>,
}

impl DiscoveryService {
    pub fn new() -> Self {
        let beacons = Arc::new(Mutex::new(HashMap::new()));
        let cancel_signal = Arc::new(AtomicBool::new(false));

        let beacons_clone = beacons.clone();
        let cancel_clone = cancel_signal.clone();

        let task = async move {
            let socket = match bind_reuse_udp(DISCOVERY_PORT) {
                Ok(s) => s,
                Err(_) => match UdpSocket::bind(format!("0.0.0.0:{}", DISCOVERY_PORT)).await {
                    Ok(s) => s,
                    Err(_) => return,
                },
            };

            let mut buf = [0u8; 1024];

            while !cancel_clone.load(Ordering::Relaxed) {
                tokio::select! {
                    recv_res = socket.recv_from(&mut buf) => {
                        if let Ok((len, src)) = recv_res {
                            if let Ok(ann) = serde_json::from_slice::<BlindBeaconAnnouncement>(&buf[..len]) {
                                if ann.app == APP_ID && ann.version == PROTOCOL_VERSION {
                                    let mut map = beacons_clone.lock();
                                    let sender_addr = SocketAddr::new(src.ip(), ann.listen_port);
                                    map.insert(
                                        ann.blind_ticket,
                                        DiscoveredBeacon {
                                            sender_addr,
                                            last_seen: Instant::now(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {
                        // Purge expired beacons unseen for more than 5 seconds
                        let mut map = beacons_clone.lock();
                        map.retain(|_, b| b.last_seen.elapsed() < Duration::from_secs(5));
                    }
                }
            }
        };

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(task);
        }

        Self {
            beacons,
            cancel_signal,
        }
    }

    /// Check if a sender for the given code is actively advertising on the local network
    pub fn check_local(&self, code: &PairingCode) -> Option<SocketAddr> {
        let ticket = code.lan_blind_ticket();
        let map = self.beacons.lock();
        map.get(&ticket).map(|b| b.sender_addr)
    }

    /// Actively listen on LAN for up to `timeout` looking for a sender with this pairing code
    pub async fn resolve_local(&self, code: &PairingCode, timeout: Duration) -> Option<SocketAddr> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Some(addr) = self.check_local(code) {
                return Some(addr);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
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
