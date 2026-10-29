use heavydrop::integrity::compute_file_blake3;
use heavydrop::manager::TransferManager;
use heavydrop::part_file::{PartFileManager, PrepareResult};
use heavydrop::protocol::*;
use heavydrop::receiver::ReceiverConfig;
use heavydrop::sender::SenderConfig;
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

fn temp_test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("heavydrop_test_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn test_x25519_and_chacha20poly1305_roundtrip() {
    let (alice_sec, alice_pub) = generate_keypair().unwrap();
    let (bob_sec, bob_pub) = generate_keypair().unwrap();

    let token = Some("super_secret_auth_token_456");
    let alice_cipher = derive_session_cipher(&alice_sec, &bob_pub, token);
    let bob_cipher = derive_session_cipher(&bob_sec, &alice_pub, token);

    let data = b"HeavyDrop 50GB massive P2P transfer test payload!";
    let chunk_index = 42;

    let encrypted = encrypt_chunk(&alice_cipher, chunk_index, data).unwrap();
    assert_ne!(&encrypted, data);

    let decrypted = decrypt_chunk(&bob_cipher, chunk_index, &encrypted).unwrap();
    assert_eq!(&decrypted, data);

    // Wrong token fails decryption Poly1305 authentication tag check
    let charlie_cipher = derive_session_cipher(&bob_sec, &alice_pub, Some("wrong_token"));
    assert!(decrypt_chunk(&charlie_cipher, chunk_index, &encrypted).is_err());

    // Tampered ciphertext fails MAC verification
    let mut tampered = encrypted.clone();
    tampered[10] ^= 0x42;
    assert!(decrypt_chunk(&bob_cipher, chunk_index, &tampered).is_err());
}

#[tokio::test]
async fn test_path_traversal_sanitization() {
    assert_eq!(
        sanitize_file_name("archivo_50gb.iso").unwrap(),
        "archivo_50gb.iso"
    );
    assert_eq!(
        sanitize_file_name("backup.tar.gz").unwrap(),
        "backup.tar.gz"
    );
    assert_eq!(sanitize_file_name("../../etc/passwd").unwrap(), "passwd");
    assert_eq!(
        sanitize_file_name("C:\\Windows\\System32\\calc.exe").unwrap(),
        "calc.exe"
    );

    assert!(sanitize_file_name("..").is_err());
    assert!(sanitize_file_name(".").is_err());
    assert!(sanitize_file_name("").is_err());
    assert!(sanitize_file_name("   ").is_err());
    assert!(sanitize_file_name("NUL").is_err());
    assert!(sanitize_file_name("nul.txt").is_err());
    assert!(sanitize_file_name("CON").is_err());
    assert!(sanitize_file_name("aux.dat").is_err());
    assert!(sanitize_file_name("evil\0file.txt").is_err());
}

#[tokio::test]
async fn test_part_file_resume_and_metadata() {
    let dir = temp_test_dir("part_resume");
    let offer = HandshakeOffer {
        protocol_version: PROTOCOL_VERSION,
        sender_public_key: [0u8; 32],
        file_name: "test_resume.dat".to_string(),
        file_size: 1024 * 1024, // 1 MB
        chunk_size: 256 * 1024, // 256 KB
        file_hash: "dummyhash".to_string(),
        auth_required: false,
        auth_challenge: [0u8; 32],
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

    // 2. Write 2 chunks (512 KB)
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
    assert!(!dir.join("test_resume.dat.part.meta").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_corrupt_part_file_fails_resume_gracefully() {
    let dir = temp_test_dir("corrupt_part");
    let offer = HandshakeOffer {
        protocol_version: PROTOCOL_VERSION,
        sender_public_key: [0u8; 32],
        file_name: "test_corrupt.dat".to_string(),
        file_size: 512 * 1024,
        chunk_size: 256 * 1024,
        file_hash: "dummyhash".to_string(),
        auth_required: false,
        auth_challenge: [0u8; 32],
    };

    let prep1 = PartFileManager::prepare(&dir, &offer, false).await.unwrap();
    let (mut part_mgr, _) = match prep1 {
        PrepareResult::Ready {
            part_manager,
            resume_offset,
        } => (part_manager, resume_offset),
        _ => panic!("Expected Ready"),
    };

    let chunk = vec![0xAA; 256 * 1024];
    let hash = *blake3::hash(&chunk).as_bytes();
    part_mgr
        .write_verified_chunk(0, &chunk, hash)
        .await
        .unwrap();
    drop(part_mgr);

    // Corrupt the .part file on disk by tampering with bytes
    let part_path = dir.join("test_corrupt.dat.part");
    let mut data = tokio::fs::read(&part_path).await.unwrap();
    data[100] ^= 0xFF; // Flip bit
    tokio::fs::write(&part_path, data).await.unwrap();

    // Re-prepare should detect checksum mismatch and safely start from 0 instead of corrupt resume
    let prep2 = PartFileManager::prepare(&dir, &offer, false).await.unwrap();
    let (_, resume2) = match prep2 {
        PrepareResult::Ready {
            part_manager,
            resume_offset,
        } => (part_manager, resume_offset),
        _ => panic!("Expected Ready"),
    };
    assert_eq!(resume2, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_end_to_end_loopback_transfer() {
    let src_dir = temp_test_dir("e2e_src");
    let dst_dir = temp_test_dir("e2e_dst");

    let src_file = src_dir.join("payload.bin");
    let payload_size = 1024 * 1024; // 1 MB payload
    let test_data = vec![0x5A; payload_size];

    let mut f = File::create(&src_file).await.unwrap();
    f.write_all(&test_data).await.unwrap();
    f.flush().await.unwrap();
    drop(f);

    let expected_hash = compute_file_blake3(&src_file, false).unwrap();

    // Bind random OS port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let mgr_sender = TransferManager::new();
    let mgr_receiver = TransferManager::new();

    let send_file = src_file.clone();
    let token = Some("heavydrop_secret_test_token".to_string());
    let token_clone = token.clone();

    // Spawn Sender
    let send_handle = tokio::spawn(async move {
        mgr_sender
            .run_send(SenderConfig {
                file_path: send_file,
                bind_addr: "127.0.0.1".to_string(),
                port,
                chunk_size: 256 * 1024,
                token: token_clone,
                skip_prehash: false,
                shared_progress: None,
                cancel_signal: None,
            })
            .await
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Run Receiver
    let recv_res = mgr_receiver
        .run_recv(ReceiverConfig {
            peer_addr: format!("127.0.0.1:{}", port),
            output_dir: dst_dir.clone(),
            token,
            force: true,
            shared_progress: None,
            cancel_signal: None,
        })
        .await;

    if let Err(e) = &recv_res {
        eprintln!("RECV ERROR: {:#}", e);
    }
    assert!(recv_res.is_ok());
    assert!(send_handle.await.unwrap().is_ok());

    let received_file = dst_dir.join("payload.bin");
    assert!(received_file.exists());
    let received_hash = compute_file_blake3(&received_file, false).unwrap();
    assert_eq!(received_hash, expected_hash);

    let _ = std::fs::remove_dir_all(&src_dir);
    let _ = std::fs::remove_dir_all(&dst_dir);
}

#[tokio::test]
async fn test_empty_file_transfer() {
    let src_dir = temp_test_dir("empty_src");
    let dst_dir = temp_test_dir("empty_dst");

    let src_file = src_dir.join("empty.txt");
    let f = File::create(&src_file).await.unwrap();
    drop(f);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let mgr_sender = TransferManager::new();
    let mgr_receiver = TransferManager::new();

    let send_file = src_file.clone();
    let send_handle = tokio::spawn(async move {
        mgr_sender
            .run_send(SenderConfig {
                file_path: send_file,
                bind_addr: "127.0.0.1".to_string(),
                port,
                chunk_size: 64 * 1024,
                token: None,
                skip_prehash: false,
                shared_progress: None,
                cancel_signal: None,
            })
            .await
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let recv_res = mgr_receiver
        .run_recv(ReceiverConfig {
            peer_addr: format!("127.0.0.1:{}", port),
            output_dir: dst_dir.clone(),
            token: None,
            force: true,
            shared_progress: None,
            cancel_signal: None,
        })
        .await;

    if let Err(e) = &recv_res {
        eprintln!("RECV ERROR: {:#}", e);
    }
    assert!(recv_res.is_ok());
    assert!(send_handle.await.unwrap().is_ok());

    let received_file = dst_dir.join("empty.txt");
    assert!(received_file.exists());
    assert_eq!(std::fs::metadata(&received_file).unwrap().len(), 0);

    let _ = std::fs::remove_dir_all(&src_dir);
    let _ = std::fs::remove_dir_all(&dst_dir);
}

#[tokio::test]
async fn test_auth_challenge_response_failure() {
    let src_dir = temp_test_dir("auth_fail_src");
    let dst_dir = temp_test_dir("auth_fail_dst");

    let src_file = src_dir.join("secret_data.txt");
    let mut f = File::create(&src_file).await.unwrap();
    f.write_all(b"CONFIDENTIAL").await.unwrap();
    drop(f);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let mgr_sender = TransferManager::new();
    let mgr_receiver = TransferManager::new();

    let send_file = src_file.clone();
    let send_handle = tokio::spawn(async move {
        mgr_sender
            .run_send(SenderConfig {
                file_path: send_file,
                bind_addr: "127.0.0.1".to_string(),
                port,
                chunk_size: 64 * 1024,
                token: Some("correct_password_123".to_string()),
                skip_prehash: false,
                shared_progress: None,
                cancel_signal: None,
            })
            .await
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Connect with WRONG password
    let recv_res = mgr_receiver
        .run_recv(ReceiverConfig {
            peer_addr: format!("127.0.0.1:{}", port),
            output_dir: dst_dir.clone(),
            token: Some("wrong_password_999".to_string()),
            force: true,
            shared_progress: None,
            cancel_signal: None,
        })
        .await;

    // Both should fail
    assert!(recv_res.is_err());
    assert!(send_handle.await.unwrap().is_err());

    // File should NOT have been downloaded
    assert!(!dst_dir.join("secret_data.txt").exists());

    let _ = std::fs::remove_dir_all(&src_dir);
    let _ = std::fs::remove_dir_all(&dst_dir);
}
