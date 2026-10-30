use heavydrop::connectivity::{establish_receiver_connection, establish_sender_connection};
use heavydrop::discovery::DiscoveryService;
use heavydrop::manager::TransferManager;
use heavydrop::pairing::PairingCode;
use heavydrop::receiver::ReceiverConfig;
use heavydrop::session::Session;
use heavydrop::signaling::{receive_signal_msg, send_signal_msg, SignalMessage, SignalingClient};
use heavydrop::transfer_state::{ConnectionType, TransferStatus};
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// Minimal signaling fixture: no reachable direct candidates, relay joins are bridged.
// This forces both production connection paths through relay without requiring a WAN.
async fn relay_fixture() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move {
        let mut peers = Vec::new();
        while peers.len() < 2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            match receive_signal_msg(&mut stream).await.unwrap() {
                SignalMessage::Register { .. } => {
                    send_signal_msg(
                        &mut stream,
                        &SignalMessage::Registered {
                            observed_ip: "127.0.0.1".into(),
                        },
                    )
                    .await
                    .unwrap();
                }
                SignalMessage::Lookup { .. } => {
                    send_signal_msg(
                        &mut stream,
                        &SignalMessage::LookupResult {
                            found: true,
                            candidates: vec![],
                            observed_ip: "127.0.0.1".into(),
                        },
                    )
                    .await
                    .unwrap();
                }
                SignalMessage::RelayJoin { .. } => peers.push(stream),
                other => panic!("unexpected message: {other:?}"),
            }
        }
        let mut receiver = peers.pop().unwrap();
        let mut sender = peers.pop().unwrap();
        send_signal_msg(&mut sender, &SignalMessage::RelayReady)
            .await
            .unwrap();
        send_signal_msg(&mut receiver, &SignalMessage::RelayReady)
            .await
            .unwrap();
        tokio::io::copy_bidirectional(&mut sender, &mut receiver)
            .await
            .unwrap();
    });
    (addr, task)
}

#[tokio::test]
async fn both_connection_paths_use_bidirectional_relay() {
    let (addr, server) = relay_fixture().await;
    let session = Session::new().unwrap();
    let client = SignalingClient::new(Some(addr));
    let cancel = Arc::new(AtomicBool::new(false));
    let discovery = DiscoveryService::new();
    // Use an unrelated code only for LAN discovery so this test cannot select LAN.
    let code = PairingCode::generate().unwrap();
    let (send, recv) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            establish_sender_connection(&session, 0, &client, cancel.clone()),
            establish_receiver_connection(&code, &client, &discovery, cancel.clone()),
        )
    })
    .await
    .unwrap();
    let mut send = send.unwrap();
    let mut recv = recv.unwrap();
    assert_eq!(send.conn_type, ConnectionType::Relay);
    assert_eq!(recv.conn_type, ConnectionType::Relay);
    send.stream.write_all(b"offer").await.unwrap();
    let mut offer = [0; 5];
    recv.stream.read_exact(&mut offer).await.unwrap();
    assert_eq!(&offer, b"offer");
    recv.stream.write_all(b"accepted").await.unwrap();
    let mut response = [0; 8];
    send.stream.read_exact(&mut response).await.unwrap();
    assert_eq!(&response, b"accepted");
    drop(send);
    drop(recv);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn missing_server_is_visible_as_failed_and_can_retry() {
    let manager = TransferManager::new();
    let output_dir =
        std::env::temp_dir().join(format!("heavy_connection_error_{}", std::process::id()));
    for _ in 0..2 {
        let result = manager
            .run_recv(ReceiverConfig {
                code: PairingCode::generate().unwrap(),
                output_dir: output_dir.clone(),
                force: false,
                signaling_server: None,
                shared_progress: None,
                cancel_signal: None,
                discovery: None,
            })
            .await;
        assert!(result.is_err());
        assert!(!manager.is_busy());
        let progress = manager.progress().lock().clone();
        assert_eq!(progress.status, TransferStatus::Failed);
        assert!(progress.error.unwrap().contains("HEAVYDROP_SIGNAL_SERVER"));
    }
    std::fs::remove_dir(output_dir).unwrap();
}

#[test]
fn non_ascii_pairing_input_returns_error_without_panicking() {
    // U+0130 used to pass the truncated u8 alphabet check and break byte slicing.
    assert!(PairingCode::parse("İ23456789ABC").is_err());
}
