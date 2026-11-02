use heavydrop::{automatic::TransportMode, manager::TransferManager, pairing::PairingCode,
    receiver::ReceiverConfig, sender::SenderConfig, session::Session, transfer_state::{ConnectionType, TransferStatus}};
use std::{path::PathBuf, sync::atomic::AtomicBool, sync::Arc, time::Duration};

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
    sender_config_with_chunk(file_path, session, transport_mode, 64 * 1024)
}

fn sender_config_with_chunk(file_path: PathBuf, session: Session, transport_mode: TransportMode, chunk_size: u32) -> SenderConfig {
    SenderConfig { transport_mode, file_path, session, preferred_port: 0, chunk_size,
        skip_prehash: false, signaling_server: None, shared_progress: None, cancel_signal: None, speed_limiter: None }
}

async fn transfer(mode: TransportMode, bytes: &[u8], name: &str) {
    transfer_with_chunk(mode, bytes, name, 64 * 1024).await;
}

async fn transfer_with_chunk(mode: TransportMode, bytes: &[u8], name: &str, chunk_size: u32) {
    let source = directory(name);
    let output = directory("download");
    let path = source.join("payload.bin");
    tokio::fs::write(&path, bytes).await.unwrap();
    let sender = TransferManager::new();
    let receiver = TransferManager::new();
    let expected_route = if matches!(mode, TransportMode::RelayOnly(_)) { ConnectionType::Relay } else { ConnectionType::DirectLocal };
    let direct_only = mode == TransportMode::DirectOnly;
    let config = sender_config_with_chunk(path, Session::new().unwrap(), mode, chunk_size);
    let mut used_code = None;
    let timeout_secs = 60 + (bytes.len() / (1024 * 1024)) as u64;
    let (sent, received) = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        tokio::join!(sender.run_send(config), async {
            let code = invitation(&sender).await;
            used_code = Some(code.clone());
            receiver.run_recv(ReceiverConfig { allow_legacy_tcp: false, direct_only, code, output_dir: output.clone(), force: false,
                signaling_server: None, shared_progress: None, cancel_signal: None, discovery: None, speed_limiter: None }).await
        })
    }).await.unwrap();
    sent.unwrap();
    received.unwrap();
    assert_eq!(tokio::fs::read(output.join("payload.bin")).await.unwrap(), bytes);
    assert_eq!(sender.progress().lock().status, TransferStatus::Completed);
    assert_eq!(receiver.progress().lock().status, TransferStatus::Completed);
    if direct_only {
        assert!(matches!(
            sender.progress().lock().connection_type,
            ConnectionType::DirectLocal | ConnectionType::DirectInternet
        ));
    } else {
        assert_eq!(sender.progress().lock().connection_type, expected_route);
    }
    assert!(!sender.is_busy());
    port_is_released(&used_code.unwrap()).await;
    std::fs::remove_dir_all(source).unwrap();
    std::fs::remove_dir_all(output).unwrap();
}

#[tokio::test]
async fn multi_stream_4mb_chunks_transfer_test() {
    transfer_with_chunk(TransportMode::DirectOnly, &vec![0x42; 24 * 1024 * 1024], "multichunk4m", 4 * 1024 * 1024).await;
}

#[tokio::test]
async fn multi_stream_uneven_last_chunk_transfer_test() {
    transfer_with_chunk(TransportMode::DirectOnly, &vec![0x99; 14 * 1024 * 1024 + 12345], "unevenchunk", 4 * 1024 * 1024).await;
}

#[tokio::test]
async fn multi_stream_past_chunk_3593_test() {
    transfer_with_chunk(TransportMode::DirectOnly, &vec![0x77; 240 * 1024 * 1024], "chunk3593", 64 * 1024).await;
}

#[tokio::test]
async fn upload_and_download_speed_limiter_transfer_test() {
    let source = directory("speed_test_src");
    let output = directory("speed_test_dst");
    let path = source.join("payload.bin");
    let bytes = vec![0x55u8; 4 * 1024 * 1024]; // 4 MB
    tokio::fs::write(&path, &bytes).await.unwrap();

    let upload_limiter = heavydrop::speed_limiter::SpeedLimiter::new(2 * 1024 * 1024); // 2 MB/s
    let download_limiter = heavydrop::speed_limiter::SpeedLimiter::default(); // unlimited

    let sender = TransferManager::new();
    let receiver = TransferManager::new();

    let mut config = sender_config_with_chunk(path, Session::new().unwrap(), TransportMode::DirectOnly, 512 * 1024);
    config.speed_limiter = Some(upload_limiter.clone());

    let start_time = std::time::Instant::now();
    let mut used_code = None;

    let (sent, received) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(sender.run_send(config), async {
            let code = invitation(&sender).await;
            used_code = Some(code.clone());

            // Test dynamic update "en caliente": change upload limit on the fly
            upload_limiter.set_limit_mbps(5);
            assert_eq!(upload_limiter.get_limit_mbps(), 5);

            receiver.run_recv(ReceiverConfig {
                allow_legacy_tcp: false,
                direct_only: true,
                code,
                output_dir: output.clone(),
                force: false,
                signaling_server: None,
                shared_progress: None,
                cancel_signal: None,
                discovery: None,
                speed_limiter: Some(download_limiter.clone()),
            }).await
        })
    }).await.unwrap();

    sent.unwrap();
    received.unwrap();
    assert_eq!(tokio::fs::read(output.join("payload.bin")).await.unwrap(), bytes);
    assert!(start_time.elapsed() >= Duration::from_millis(600));

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
async fn strict_p2p_rejects_a_relay_only_invitation_without_contacting_it() {
    let address = iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public())
        .with_relay_url("http://127.0.0.1:9".parse().unwrap());
    let code = PairingCode::generate().unwrap().with_invitation(address, Duration::from_secs(60)).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1),
        heavydrop::automatic::connect_receiver_with_policy(&code, None, true)).await.unwrap();
    assert!(format!("{:#}", result.err().expect("strict policy accepted relay-only invitation")).contains("direcciones directas"));
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

#[tokio::test]
async fn test_quic_mid_transfer_interruption_and_resume() {
    let source = directory("resume_src");
    let output = directory("resume_dst");
    let path = source.join("large_payload.bin");

    // 16 MB test payload with 1 MB chunks (16 chunks)
    let data: Vec<u8> = (0..(16 * 1024 * 1024)).map(|i| (i % 251) as u8).collect();
    tokio::fs::write(&path, &data).await.unwrap();
    let expected_hash = blake3::hash(&data);

    let sender = TransferManager::new();
    let mut sender_cfg = sender_config(path.clone(), Session::new().unwrap(), TransportMode::DirectOnly);
    sender_cfg.chunk_size = 1024 * 1024; // 1 MB chunks

    let sender_mgr = sender.clone();
    let sender_handle = tokio::spawn(async move {
        sender_mgr.run_send(sender_cfg).await
    });

    let code = invitation(&sender).await;

    // Receiver attempt 1: throttled to 512 KB/s so we can cut midway deterministically
    let receiver1 = TransferManager::new();
    let cancel1 = Arc::new(AtomicBool::new(false));
    let recv1_cancel = cancel1.clone();
    let code_1 = code.clone();
    let output_1 = output.clone();
    let recv1_mgr = receiver1.clone();

    let recv1_handle = tokio::spawn(async move {
        recv1_mgr.run_recv(ReceiverConfig {
            allow_legacy_tcp: false,
            direct_only: true,
            code: code_1,
            output_dir: output_1,
            force: false,
            signaling_server: None,
            shared_progress: None,
            cancel_signal: Some(recv1_cancel),
            discovery: None,
            speed_limiter: Some(heavydrop::speed_limiter::SpeedLimiter::new(512 * 1024)),
        }).await
    });

    // Wait until at least 2 MB is received into memory progress
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let transferred = receiver1.progress().lock().transferred_bytes;
            if transferred >= 2 * 1024 * 1024 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("Receiver 1 never received at least 2 MB");

    // Abruptly abort receiver 1 task to simulate sudden connection drop
    recv1_handle.abort();
    let _ = recv1_handle.await;

    // Give OS a moment to flush file buffers
    tokio::time::sleep(Duration::from_millis(200)).await;

    let part_path = output.join("large_payload.bin.part");
    let final_path = output.join("large_payload.bin");

    // Verify final file does NOT exist yet and .part file exists
    assert!(!final_path.exists(), "Final file should not exist yet");
    assert!(part_path.exists(), ".part file should exist after partial transfer");

    // Receiver attempt 2: reconnects to the same sender invitation, resumes from .part and completes at full unthrottled speed
    let receiver2 = TransferManager::new();
    let code_2 = code.clone();
    let output_2 = output.clone();

    let recv2_res = tokio::time::timeout(Duration::from_secs(35), async {
        receiver2.run_recv(ReceiverConfig {
            allow_legacy_tcp: false,
            direct_only: true,
            code: code_2,
            output_dir: output_2,
            force: false,
            signaling_server: None,
            shared_progress: None,
            cancel_signal: None,
            discovery: None,
            speed_limiter: None,
        }).await
    }).await.expect("Timed out waiting for resumed transfer");

    recv2_res.expect("Receiver attempt 2 failed to resume and complete");

    // Sender completes transfer
    let send_res = tokio::time::timeout(Duration::from_secs(10), sender_handle)
        .await
        .expect("Sender timed out")
        .expect("Sender panicked");
    send_res.expect("Sender transfer failed");

    // Verify finalized file and BLAKE3 hash
    assert!(final_path.exists(), "Final file was not created");
    assert!(!part_path.exists(), ".part file should be finalized and removed");
    let downloaded_bytes = tokio::fs::read(&final_path).await.unwrap();
    assert_eq!(downloaded_bytes.len(), data.len());
    let final_hash = blake3::hash(&downloaded_bytes);
    assert_eq!(final_hash, expected_hash, "BLAKE3 hash mismatch after resume");

    std::fs::remove_dir_all(source).unwrap();
    std::fs::remove_dir_all(output).unwrap();
}