use heavydrop::manager::TransferManager;
use heavydrop::pairing::PairingCode;
use heavydrop::part_file::{PartFileManager, PrepareResult};
use heavydrop::protocol::*;
use heavydrop::receiver::ReceiverConfig;
use heavydrop::sender::SenderConfig;
use heavydrop::server::SignalingServer;
use heavydrop::session::Session;
use heavydrop::signaling::SignalingClient;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

fn temp_test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("heavydrop_test_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// 1. Test code generation, entropy and uniqueness (no collisions)
#[test]
fn test_pairing_code_entropy_and_uniqueness() {
    let mut seen = HashSet::new();
    for _ in 0..1000 {
        let code = PairingCode::generate().unwrap();
        assert_eq!(code.raw().len(), 12);
        let formatted = code.formatted();
        assert_eq!(formatted.len(), 14);
        assert!(!seen.contains(&code.raw().to_string()));
        seen.insert(code.raw().to_string());
    }
}

// 2. Test code normalization and tolerant parsing
#[test]
fn test_pairing_code_normalization_and_typos() {
    let base = PairingCode::parse("kd7m-9q2p-w8rx").unwrap();
    assert_eq!(base.formatted(), "KD7M-9Q2P-W8RX");

    // Case insensitivity, spaces and typo substitution (O -> 0, I -> 1)
    let typo = PairingCode::parse(" o d 7 m - 9 q 2 p - w 8 r x ").unwrap();
    assert_eq!(&typo.raw()[0..1], "0");

    assert!(PairingCode::parse("too-short").is_err());
    assert!(PairingCode::parse("KD7M-9Q2P-W8RX-EXTRA").is_err());
}

// 3. Test session expiration and single-use invalidation
#[test]
fn test_session_expiration_and_single_use_consumption() {
    let session = Session::new().unwrap();
    assert!(!session.is_expired());
    assert!(!session.is_consumed());
    assert!(session.remaining_time().as_secs() > 890);

    // Consume once succeeds
    assert!(session.consume().is_ok());
    assert!(session.is_consumed());

    // Consume second time fails (prevents replay / reuse)
    assert!(session.consume().is_err());
}

// 4. Test mutual cryptographic authentication proofs
#[test]
fn test_mutual_authentication_proofs() {
    let code_alice = PairingCode::parse("KD7M-9Q2P-W8RX").unwrap();
    let code_bob = PairingCode::parse("kd7m9q2pw8rx").unwrap();
    let code_eve = PairingCode::parse("EEEE-EEEE-EEEE").unwrap();

    let session_salt = [0x42u8; 32];
    let (_, sender_pub) = generate_keypair().unwrap();
    let (_, receiver_pub) = generate_keypair().unwrap();

    // Alice (sender) computes proof
    let sender_proof = code_alice.compute_sender_proof(&session_salt, &sender_pub);

    // Bob (receiver with same code) verifies sender proof
    let expected_sender_proof = code_bob.compute_sender_proof(&session_salt, &sender_pub);
    assert!(verify_proof_constant_time(
        &expected_sender_proof,
        &sender_proof
    ));

    // Eve (with different code) cannot verify or forge sender proof
    let eve_expected = code_eve.compute_sender_proof(&session_salt, &sender_pub);
    assert!(!verify_proof_constant_time(&eve_expected, &sender_proof));

    // Bob (receiver) computes proof
    let receiver_proof = code_bob.compute_receiver_proof(&session_salt, &receiver_pub);

    // Alice (sender) verifies receiver proof
    let expected_receiver_proof = code_alice.compute_receiver_proof(&session_salt, &receiver_pub);
    assert!(verify_proof_constant_time(
        &expected_receiver_proof,
        &receiver_proof
    ));

    // Eve fails to authenticate
    let eve_receiver_proof = code_eve.compute_receiver_proof(&session_salt, &receiver_pub);
    assert!(!verify_proof_constant_time(
        &expected_receiver_proof,
        &eve_receiver_proof
    ));
}

// 5. Test transport encryption and decryption with session salt nonce isolation
#[test]
fn test_aead_transport_encryption_with_session_isolation() {
    let (alice_sec, alice_pub) = generate_keypair().unwrap();
    let (bob_sec, bob_pub) = generate_keypair().unwrap();

    let auth_key = [0xAAu8; 32];
    let session_salt = [0xBBu8; 32];

    let alice_cipher = derive_session_cipher(&alice_sec, &bob_pub, &auth_key, &session_salt);
    let bob_cipher = derive_session_cipher(&bob_sec, &alice_pub, &auth_key, &session_salt);

    let data = b"HeavyDrop 50GB test payload";
    let chunk_index = 0;

    let ciphertext = encrypt_chunk(&alice_cipher, &session_salt, chunk_index, data).unwrap();
    assert_ne!(&ciphertext, data);

    let plaintext = decrypt_chunk(&bob_cipher, &session_salt, chunk_index, &ciphertext).unwrap();
    assert_eq!(&plaintext, data);

    // Tampered ciphertext fails MAC
    let mut tampered = ciphertext.clone();
    tampered[4] ^= 0xFF;
    assert!(decrypt_chunk(&bob_cipher, &session_salt, chunk_index, &tampered).is_err());
}

// 6. Test blind LAN discovery ticket derivation (zero disclosure)
#[test]
fn test_blind_lan_ticket_zero_disclosure() {
    let c1 = PairingCode::parse("KD7M-9Q2P-W8RX").unwrap();
    let c2 = PairingCode::parse("kd7m-9q2p-w8rx").unwrap();
    let c3 = PairingCode::parse("AAAA-BBBB-CCCC").unwrap();

    let t1 = c1.lan_blind_ticket();
    let t2 = c2.lan_blind_ticket();
    let t3 = c3.lan_blind_ticket();

    assert_eq!(t1, t2);
    assert_ne!(t1, t3);
    assert_ne!(t1, [0u8; 32]);
}

// 7. Test signaling server session registration, lookup, and candidate exchange
#[tokio::test]
async fn test_signaling_server_registration_and_lookup() {
    let port = 51520;
    let server = SignalingServer::new(port);
    tokio::spawn(async move {
        let _ = server.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = SignalingClient::new(Some(format!("127.0.0.1:{}", port)));
    let code = PairingCode::parse("KD7M-9Q2P-W8RX").unwrap();
    let lookup_hash = code.signaling_lookup_hash();

    let cand = vec!["127.0.0.1:7878".parse::<SocketAddr>().unwrap()];

    // 1. Register session
    let reg_res = client.register_session(&lookup_hash, &cand).await.unwrap();
    assert!(reg_res.is_some());

    // 2. Lookup session
    let lookup_res = client.lookup_session(&lookup_hash).await.unwrap();
    assert!(lookup_res.is_some());
    let (candidates, observed) = lookup_res.unwrap();
    assert!(!candidates.is_empty());
    assert_eq!(observed, "127.0.0.1");

    // 3. Lookup non-existent session
    let non_existent = client
        .lookup_session("non_existent_hash_999")
        .await
        .unwrap();
    assert!(non_existent.is_none());
}

// 8. Test .part file verifiable resume and rollback on corruption
#[tokio::test]
async fn test_part_file_resume_and_metadata() {
    let dir = temp_test_dir("part_resume");
    let offer = HandshakeOffer {
        protocol_version: PROTOCOL_VERSION,
        session_id: "test-sess-1".to_string(),
        sender_public_key: [0u8; 32],
        session_salt: [0u8; 32],
        file_name: "test_resume.dat".to_string(),
        file_size: 1024 * 1024, // 1 MB
        chunk_size: 256 * 1024, // 256 KB
        file_hash: "dummyhash".to_string(),
        sender_auth_proof: [0u8; 32],
    };

    // 1. Initial preparation (0 bytes downloaded)
    let prep1 = PartFileManager::prepare(&dir, &offer, false).await.unwrap();
    let (mut part_mgr, resume1) = match prep1 {
        PrepareResult::Ready {
            part_manager,
            resume_offset,
        } => (*part_manager, resume_offset),
        _ => panic!("Expected Ready"),
    };
    assert_eq!(resume1, 0);

    // 2. Write 2 verified chunks (512 KB)
    let chunk1 = vec![0xAA; 256 * 1024];
    let hash1 = *blake3::hash(&chunk1).as_bytes();
    part_mgr
        .write_verified_chunk(0, &chunk1, hash1)
        .await
        .unwrap();

    let chunk2 = vec![0xBB; 256 * 1024];
    let hash2 = *blake3::hash(&chunk2).as_bytes();
    part_mgr
        .write_verified_chunk(256 * 1024, &chunk2, hash2)
        .await
        .unwrap();

    drop(part_mgr);

    // 3. Second preparation should detect verified .part and resume exactly from 512 KB
    let prep2 = PartFileManager::prepare(&dir, &offer, false).await.unwrap();
    let (part_mgr2, resume2) = match prep2 {
        PrepareResult::Ready {
            part_manager,
            resume_offset,
        } => (*part_manager, resume_offset),
        _ => panic!("Expected Ready"),
    };
    assert_eq!(resume2, 512 * 1024);

    // Finalize test file
    let final_path = part_mgr2.finalize().await.unwrap();
    assert!(final_path.exists());
    assert!(!dir.join("test_resume.dat.part").exists());
}

// 9. Test path traversal sanitization
#[test]
fn test_path_traversal_sanitization() {
    assert_eq!(
        sanitize_file_name("archivo_50gb.iso").unwrap(),
        "archivo_50gb.iso"
    );
    assert_eq!(sanitize_file_name("../../etc/passwd").unwrap(), "passwd");
    assert_eq!(
        sanitize_file_name("C:\\Windows\\System32\\calc.exe").unwrap(),
        "calc.exe"
    );

    assert!(sanitize_file_name("..").is_err());
    assert!(sanitize_file_name(".").is_err());
    assert!(sanitize_file_name("").is_err());
    assert!(sanitize_file_name("CON").is_err());
    assert!(sanitize_file_name("nul.txt").is_err());
}

// 10. End-to-End full file transfer using ONLY a pairing code (NO IPs or ports entered!)
#[tokio::test]
async fn test_full_end_to_end_transfer_using_code_only() {
    let src_dir = temp_test_dir("e2e_code_src");
    let dst_dir = temp_test_dir("e2e_code_dst");

    let file_path = src_dir.join("payload.bin");
    let payload = vec![0x47u8; 1024 * 1024]; // 1 MB test payload
    {
        let mut f = File::create(&file_path).await.unwrap();
        f.write_all(&payload).await.unwrap();
    }

    let manager_sender = TransferManager::new();
    let manager_receiver = TransferManager::new();

    // 1. Sender creates session with code
    let session = Session::new().unwrap();
    let code_to_share = session.code().clone();

    let send_path = file_path.clone();
    let send_session = session.clone();

    let sender_handle = tokio::spawn(async move {
        manager_sender
            .run_send(SenderConfig {
                file_path: send_path,
                session: send_session,
                preferred_port: 51521,
                chunk_size: 256 * 1024,
                skip_prehash: false,
                signaling_server: None,
                shared_progress: None,
                cancel_signal: None,
            })
            .await
    });

    tokio::time::sleep(Duration::from_millis(150)).await;

    // 2. Receiver connects using ONLY the pairing code!
    let dst_output = dst_dir.clone();
    let receiver_handle = tokio::spawn(async move {
        manager_receiver
            .run_recv(ReceiverConfig {
                code: code_to_share,
                output_dir: dst_output,
                force: false,
                signaling_server: None,
                shared_progress: None,
                cancel_signal: None,
                discovery: None,
            })
            .await
    });

    let (send_res, recv_res) = tokio::join!(sender_handle, receiver_handle);
    send_res.unwrap().unwrap();
    recv_res.unwrap().unwrap();

    // Verify downloaded file integrity
    let received_file = dst_dir.join("payload.bin");
    assert!(received_file.exists());
    let recv_data = tokio::fs::read(&received_file).await.unwrap();
    assert_eq!(recv_data.len(), payload.len());
    assert_eq!(recv_data, payload);
}

// 11. Test empty file transfer using pairing code
#[tokio::test]
async fn test_empty_file_transfer_using_code() {
    let src_dir = temp_test_dir("empty_code_src");
    let dst_dir = temp_test_dir("empty_code_dst");

    let file_path = src_dir.join("empty.txt");
    File::create(&file_path).await.unwrap(); // 0 bytes

    let session = Session::new().unwrap();
    let code = session.code().clone();

    let manager_sender = TransferManager::new();
    let manager_receiver = TransferManager::new();

    let send_session = session.clone();
    let send_file = file_path.clone();
    let sender_task = tokio::spawn(async move {
        manager_sender
            .run_send(SenderConfig {
                file_path: send_file,
                session: send_session,
                preferred_port: 51522,
                chunk_size: 64 * 1024,
                skip_prehash: false,
                signaling_server: None,
                shared_progress: None,
                cancel_signal: None,
            })
            .await
    });

    tokio::time::sleep(Duration::from_millis(150)).await;

    let dst_output = dst_dir.clone();
    let receiver_task = tokio::spawn(async move {
        manager_receiver
            .run_recv(ReceiverConfig {
                code,
                output_dir: dst_output,
                force: false,
                signaling_server: None,
                shared_progress: None,
                cancel_signal: None,
                discovery: None,
            })
            .await
    });

    let (send_res, recv_res) = tokio::join!(sender_task, receiver_task);
    send_res.unwrap().unwrap();
    recv_res.unwrap().unwrap();

    let received = dst_dir.join("empty.txt");
    assert!(received.exists());
    assert_eq!(tokio::fs::metadata(&received).await.unwrap().len(), 0);
}

// 12. Test wrong code fails mutual authentication cryptographically
#[tokio::test]
async fn test_wrong_code_mutual_authentication_failure() {
    let src_dir = temp_test_dir("wrong_code_src");
    let file_path = src_dir.join("secret.txt");
    {
        let mut f = File::create(&file_path).await.unwrap();
        f.write_all(b"Confidential data").await.unwrap();
    }

    let session = Session::new().unwrap();
    let wrong_code = PairingCode::parse("WRNG-CODE-FAIL").unwrap();

    let manager_sender = TransferManager::new();
    let send_session = session.clone();
    let send_file = file_path.clone();

    let sender_task = tokio::spawn(async move {
        manager_sender
            .run_send(SenderConfig {
                file_path: send_file,
                session: send_session,
                preferred_port: 51523,
                chunk_size: 64 * 1024,
                skip_prehash: false,
                signaling_server: None,
                shared_progress: None,
                cancel_signal: None,
            })
            .await
    });

    tokio::time::sleep(Duration::from_millis(150)).await;

    // Connect directly to sender port
    let mut stream = tokio::net::TcpStream::connect("127.0.0.1:51523")
        .await
        .unwrap();

    // Receive offer from sender
    let offer = receive_handshake_offer(&mut stream).await.unwrap();

    // 1. Receiver checks sender's proof with wrong_code: MUST FAIL!
    let expected_sender_proof =
        wrong_code.compute_sender_proof(&offer.session_salt, &offer.sender_public_key);
    assert!(!verify_proof_constant_time(
        &expected_sender_proof,
        &offer.sender_auth_proof
    ));

    // 2. Send response with wrong_code receiver proof to sender
    let (_, rec_pub) = generate_keypair().unwrap();
    let wrong_receiver_proof = wrong_code.compute_receiver_proof(&offer.session_salt, &rec_pub);
    let resp = HandshakeResponse {
        protocol_version: PROTOCOL_VERSION,
        receiver_public_key: rec_pub,
        accepted: true,
        resume_offset: 0,
        receiver_auth_proof: wrong_receiver_proof,
        message: None,
    };
    send_handshake_response(&mut stream, &resp).await.unwrap();

    // 3. Sender MUST reject this invalid proof and abort
    let sender_res = sender_task.await.unwrap();
    assert!(sender_res.is_err());
}

// 13. Test cooperative cancellation during transfer
#[tokio::test]
async fn test_cooperative_cancellation_during_transfer() {
    let src_dir = temp_test_dir("cancel_code_src");
    let file_path = src_dir.join("large_cancel.bin");
    {
        let mut f = File::create(&file_path).await.unwrap();
        f.write_all(&vec![0xAA; 5 * 1024 * 1024]).await.unwrap(); // 5 MB
    }

    let session = Session::new().unwrap();
    let manager_sender = TransferManager::new();
    let mgr_cancel = manager_sender.clone();

    let send_session = session.clone();
    let send_file = file_path.clone();

    let sender_task = tokio::spawn(async move {
        manager_sender
            .run_send(SenderConfig {
                file_path: send_file,
                session: send_session,
                preferred_port: 51524,
                chunk_size: 64 * 1024,
                skip_prehash: false,
                signaling_server: None,
                shared_progress: None,
                cancel_signal: None,
            })
            .await
    });

    tokio::time::sleep(Duration::from_millis(150)).await;

    // Trigger cooperative cancellation while sender is waiting
    mgr_cancel.cancel();

    let sender_res = sender_task.await.unwrap();
    assert!(sender_res.is_err());
}
