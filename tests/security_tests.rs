use heavydrop::{part_file::{PartFileManager, PrepareResult}, protocol::*};

fn offer() -> HandshakeOffer {
    HandshakeOffer {
        protocol_version: PROTOCOL_VERSION, session_id: "security-test".into(),
        sender_public_key: [7; 32], session_salt: [9; 32], file_name: "payload.bin".into(),
        file_size: 64 * 1024, chunk_size: 64 * 1024, file_hash: "UNVERIFIED_PREHASH".into(),
        sender_auth_proof: [0; 32],
    }
}

#[test]
fn authenticates_file_metadata_and_resume_decisions() {
    let key = [3; 32];
    let mut original = offer();
    original.sender_auth_proof = offer_proof(&original, &key).unwrap();
    for field in 0..6 {
        let mut tampered = original.clone();
        match field {
            0 => tampered.file_name = "other.bin".into(),
            1 => tampered.file_size = 0,
            2 => tampered.chunk_size *= 2,
            3 => tampered.file_hash = "0".repeat(64),
            4 => tampered.sender_public_key[0] ^= 1,
            _ => tampered.session_salt[0] ^= 1,
        }
        assert!(!verify_proof_constant_time(&original.sender_auth_proof, &offer_proof(&tampered, &key).unwrap()));
    }
    let mut response = HandshakeResponse {
        protocol_version: PROTOCOL_VERSION, receiver_public_key: [11; 32], accepted: true,
        resume_offset: 0, receiver_auth_proof: [0; 32], message: None,
    };
    response.receiver_auth_proof = response_proof(&original, &response, &key).unwrap();
    let confirmation = completion_proof(&original, &response, &key).unwrap();
    let mut tampered = response.clone();
    tampered.resume_offset = original.file_size;
    assert_ne!(response_proof(&original, &tampered, &key).unwrap(), response.receiver_auth_proof);
    tampered = response.clone();
    tampered.accepted = false;
    assert_ne!(response_proof(&original, &tampered, &key).unwrap(), response.receiver_auth_proof);
    assert_ne!(completion_proof(&original, &tampered, &key).unwrap(), confirmation);
    assert_ne!(completion_proof(&original, &response, &[4; 32]).unwrap(), confirmation);
}

#[test]
fn rejects_low_order_keys_and_windows_file_aliases() {
    let (secret, _) = generate_keypair().unwrap();
    assert!(derive_session_cipher(&secret, &[0; 32], &[1; 32], &[2; 32]).is_err());
    let mut low_order = [0; 32]; low_order[0] = 1;
    assert!(derive_session_cipher(&secret, &low_order, &[1; 32], &[2; 32]).is_err());
    for name in ["payload.bin:stream", "CON.foo.bin", "aux.txt", "payload.", "payload ", "file?.bin", "LPT1.backup.zip"] {
        assert!(sanitize_file_name(name).is_err(), "accepted {name}");
    }
}

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        let suffix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("heavy_security_{}_{suffix}", std::process::id()));
        std::fs::create_dir(&path).unwrap(); Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        for name in ["payload.bin", "payload.bin.part", "payload.bin.part.meta"] { let _ = std::fs::remove_file(self.0.join(name)); }
        let _ = std::fs::remove_dir(&self.0);
    }
}
async fn ready(dir: &Directory, spec: &HandshakeOffer, force: bool) -> PartFileManager {
    match PartFileManager::prepare(&dir.0, spec, force).await.unwrap() {
        PrepareResult::Ready { part_manager, .. } => *part_manager,
        _ => panic!("unexpected complete file"),
    }
}

#[tokio::test]
async fn never_overwrites_an_existing_or_racing_destination_without_force() {
    let dir = Directory::new();
    let spec = offer();
    let final_path = dir.0.join("payload.bin");
    tokio::fs::write(&final_path, b"keep me").await.unwrap();
    assert!(PartFileManager::prepare(&dir.0, &spec, false).await.is_err());
    assert_eq!(tokio::fs::read(&final_path).await.unwrap(), b"keep me");
    tokio::fs::remove_file(&final_path).await.unwrap();
    let mut manager = ready(&dir, &spec, false).await;
    let bytes = vec![7; spec.file_size as usize];
    manager.write_verified_chunk(0, &bytes, *blake3::hash(&bytes).as_bytes()).await.unwrap();
    tokio::fs::write(&final_path, b"created during download").await.unwrap();
    assert!(manager.finalize().await.is_err());
    assert_eq!(tokio::fs::read(&final_path).await.unwrap(), b"created during download");
    let mut manager = ready(&dir, &spec, true).await;
    manager.write_verified_chunk(0, &bytes, *blake3::hash(&bytes).as_bytes()).await.unwrap();
    manager.finalize().await.unwrap();
    assert_eq!(tokio::fs::read(&final_path).await.unwrap(), bytes);
}

#[tokio::test]
async fn incomplete_or_out_of_order_download_cannot_be_published() {
    let dir = Directory::new();
    let spec = offer();
    let mut manager = ready(&dir, &spec, false).await;
    assert!(manager.write_verified_chunk(10, &[1; 10], [0; 32]).await.is_err());
    assert!(manager.finalize().await.is_err());
    assert!(!dir.0.join("payload.bin").exists());
}

#[tokio::test]
async fn unverified_prehash_never_resumes_a_different_same_size_file() {
    let dir = Directory::new();
    let mut spec = offer(); spec.file_size *= 2;
    let mut manager = ready(&dir, &spec, false).await;
    let bytes = vec![9; spec.chunk_size as usize];
    manager.write_verified_chunk(0, &bytes, *blake3::hash(&bytes).as_bytes()).await.unwrap();
    drop(manager);
    match PartFileManager::prepare(&dir.0, &spec, false).await.unwrap() {
        PrepareResult::Ready { resume_offset, .. } => assert_eq!(resume_offset, 0),
        _ => panic!("unverified content incorrectly treated as complete"),
    }
}
