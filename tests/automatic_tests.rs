use heavydrop::{automatic::TransportMode, manager::TransferManager, pairing::PairingCode,
    receiver::ReceiverConfig, sender::SenderConfig, session::Session, transfer_state::{ConnectionType, TransferStatus}};
use std::{path::PathBuf, time::Duration};

fn directory(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("heavy_auto_{name}_{}_{unique}", std::process::id()));
    std::fs::create_dir(&path).unwrap();
    path
}

async fn invitation(manager: &TransferManager) -> PairingCode {
    tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            let progress = manager.progress().lock().clone();
            if let Some(token) = progress.pairing_code {
                assert_eq!(progress.status, TransferStatus::WaitingForPeer);
                assert!(token.starts_with("heavy1_"));
                return PairingCode::parse(&token).unwrap();
            }
            assert_ne!(progress.status, TransferStatus::Failed, "{:?}", progress.error);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("sender never became ready")
}

async fn port_is_released(code: &PairingCode) {
    let ports: std::collections::BTreeSet<_> = code.invitation().unwrap().endpoint.ip_addrs()
        .filter(|addr| addr.is_ipv4()).map(|addr| addr.port()).collect();
    for port in ports {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(socket) = std::net::UdpSocket::bind(("0.0.0.0", port)) { drop(socket); break; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.expect("temporary UDP listener still exists");
    }
}

fn sender_config(file_path: PathBuf, session: Session, transport_mode: TransportMode) -> SenderConfig {
    SenderConfig { transport_mode, file_path, session, preferred_port: 0, chunk_size: 64 * 1024,
        skip_prehash: false, signaling_server: None, shared_progress: None, cancel_signal: None, speed_limiter: None }
}

async fn transfer(mode: TransportMode, bytes: &[u8], name: &str) {
    let source = directory(name);
    let output = directory("download");
    let path = source.join("payload.bin");
    tokio::fs::write(&path, bytes).await.unwrap();
    let sender = TransferManager::new();
    let receiver = TransferManager::new();
    let expected_route = if matches!(mode, TransportMode::RelayOnly(_)) { ConnectionType::Relay } else { ConnectionType::DirectLocal };
    let config = sender_config(path, Session::new().unwrap(), mode);
    let mut used_code = None;
    let (sent, received) = tokio::time::timeout(Duration::from_secs(45), async {
        tokio::join!(sender.run_send(config), async {
            let code = invitation(&sender).await;
            used_code = Some(code.clone());
            receiver.run_recv(ReceiverConfig { code, output_dir: output.clone(), force: false,
                signaling_server: None, shared_progress: None, cancel_signal: None, discovery: None, speed_limiter: None }).await
        })
    }).await.unwrap();
    sent.unwrap();
    received.unwrap();
    assert_eq!(tokio::fs::read(output.join("payload.bin")).await.unwrap(), bytes);
    assert_eq!(sender.progress().lock().status, TransferStatus::Completed);
    assert_eq!(receiver.progress().lock().status, TransferStatus::Completed);
    assert_eq!(sender.progress().lock().connection_type, expected_route);
    assert!(!sender.is_busy());
    port_is_released(&used_code.unwrap()).await;
    std::fs::remove_dir_all(source).unwrap();
    std::fs::remove_dir_all(output).unwrap();
}

#[tokio::test]
async fn full_automatic_invitation_transfers_and_closes_direct_listener() {
    transfer(TransportMode::DirectOnly, &vec![0x37; 2 * 1024 * 1024 + 17], "direct").await;
}

#[tokio::test]
async fn empty_file_also_waits_for_receiver_confirmation() {
    transfer(TransportMode::DirectOnly, b"", "empty").await;
}

#[tokio::test]
async fn full_transfer_through_real_iroh_relay() {
    let mut config = iroh_relay::server::ServerConfig::default();
    config.relay = Some(iroh_relay::server::RelayConfig::new("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap()));
    let server = iroh_relay::server::Server::spawn(config).await.unwrap();
    let relay = format!("http://{}", server.http_addr().unwrap()).parse().unwrap();
    transfer(TransportMode::RelayOnly(relay), &vec![0x9a; 256 * 1024 + 3], "relay").await;
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_closes_server_and_allows_a_fresh_transfer() {
    let source = directory("cancel");
    let path = source.join("file.txt");
    tokio::fs::write(&path, b"cancel me").await.unwrap();
    let sender = TransferManager::new();
    for _ in 0..2 {
        let mut used = None;
        let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(sender.run_send(sender_config(path.clone(), Session::new().unwrap(), TransportMode::DirectOnly)), async {
                used = Some(invitation(&sender).await);
                sender.cancel();
            })
        }).await.unwrap();
        assert!(result.is_err());
        assert_eq!(sender.progress().lock().status, TransferStatus::Cancelled);
        assert!(!sender.is_busy());
        port_is_released(&used.unwrap()).await;
    }
    std::fs::remove_dir_all(source).unwrap();
}

#[tokio::test]
async fn expiration_closes_temporary_server() {
    let source = directory("expired");
    let path = source.join("file.txt");
    tokio::fs::write(&path, b"expire me").await.unwrap();
    let sender = TransferManager::new();
    let mut used = None;
    let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(sender.run_send(sender_config(path, Session::with_ttl(Duration::from_millis(1500)).unwrap(), TransportMode::DirectOnly)), async {
            used = Some(invitation(&sender).await);
        })
    }).await.unwrap();
    assert!(format!("{:#}", result.unwrap_err()).contains("expiró"));
    assert_eq!(sender.progress().lock().status, TransferStatus::Failed);
    port_is_released(&used.unwrap()).await;
    std::fs::remove_dir_all(source).unwrap();
}

#[tokio::test]
#[ignore = "requires Internet and public n0 relay infrastructure; transfers only generated test bytes"]
async fn public_relay_automatic_configuration_smoke_test() {
    transfer(TransportMode::Automatic, &vec![0x42; 32 * 1024], "public").await;
}
