use anyhow::{bail, Context, Result};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use x25519_dalek::{PublicKey, StaticSecret};

pub const PROTOCOL_VERSION: u32 = 2;
pub const MAGIC_BYTES: &[u8; 8] = b"HVDROP02";
pub const MIN_CHUNK_SIZE: u32 = 64 * 1024; // 64 KB
pub const MAX_CHUNK_SIZE: u32 = 64 * 1024 * 1024; // 64 MB
#[allow(dead_code)]
pub const DEFAULT_CHUNK_SIZE: u32 = 4 * 1024 * 1024; // 4 MB chunk size for high-throughput 50GB transfers
pub const END_OF_STREAM_OFFSET: u64 = u64::MAX;

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
pub const CHUNK_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HandshakeOffer {
    pub protocol_version: u32,
    pub sender_public_key: [u8; 32],
    pub file_name: String,
    pub file_size: u64,
    pub chunk_size: u32,
    pub file_hash: String,
    pub auth_required: bool,
    pub auth_challenge: [u8; 32],
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HandshakeResponse {
    pub protocol_version: u32,
    pub receiver_public_key: [u8; 32],
    pub accepted: bool,
    pub resume_offset: u64,
    pub auth_proof: Option<[u8; 32]>,
    pub message: Option<String>,
}

#[derive(Debug)]
pub struct ChunkHeader {
    pub offset: u64,
    pub ciphertext_length: u32,
    pub plaintext_checksum: [u8; 32],
}

impl ChunkHeader {
    pub fn is_end_of_stream(&self) -> bool {
        self.offset == END_OF_STREAM_OFFSET && self.ciphertext_length == 0
    }
}

/// Generate an ephemeral X25519 keypair for Perfect Forward Secrecy transport encryption
pub fn generate_keypair() -> Result<(StaticSecret, [u8; 32])> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| anyhow::anyhow!("Failed to generate secure random bytes: {}", e))?;
    let secret = StaticSecret::from(bytes);
    let public = PublicKey::from(&secret);
    Ok((secret, *public.as_bytes()))
}

/// Derive a symmetric ChaCha20-Poly1305 session cipher from Diffie-Hellman shared secret and optional auth token
pub fn derive_session_cipher(
    secret: &StaticSecret,
    peer_public: &[u8; 32],
    token: Option<&str>,
) -> ChaCha20Poly1305 {
    let peer_pub = PublicKey::from(*peer_public);
    let shared = secret.diffie_hellman(&peer_pub);
    let mut ikm = Vec::with_capacity(64);
    ikm.extend_from_slice(shared.as_bytes());
    if let Some(tok) = token {
        ikm.extend_from_slice(tok.as_bytes());
    }
    let key_bytes = blake3::derive_key("heavydrop-v2-transport-key", &ikm);
    ChaCha20Poly1305::new((&key_bytes).into())
}

/// Encrypt a chunk payload using ChaCha20-Poly1305 AEAD
pub fn encrypt_chunk(
    cipher: &ChaCha20Poly1305,
    chunk_index: u64,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes[4..12].copy_from_slice(&chunk_index.to_be_bytes());
    let nonce = (&nonce_bytes).into();
    cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("Encryption error: {}", e))
}

/// Decrypt a chunk payload using ChaCha20-Poly1305 AEAD and verify Poly1305 MAC tag
pub fn decrypt_chunk(
    cipher: &ChaCha20Poly1305,
    chunk_index: u64,
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes[4..12].copy_from_slice(&chunk_index.to_be_bytes());
    let nonce = (&nonce_bytes).into();
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("Decryption / AEAD authentication tag failure: {}", e))
}

/// Generate a cryptographically secure random 32-byte challenge for authentication
pub fn generate_auth_challenge() -> Result<[u8; 32]> {
    let mut challenge = [0u8; 32];
    getrandom::getrandom(&mut challenge)
        .map_err(|e| anyhow::anyhow!("Failed to generate secure cryptographic challenge: {}", e))?;
    Ok(challenge)
}

/// Compute HMAC-like authentication proof from token and challenge using BLAKE3 keyed hashing
pub fn compute_auth_proof(token: &str, challenge: &[u8; 32]) -> [u8; 32] {
    let key = blake3::hash(token.as_bytes());
    let proof = blake3::keyed_hash(key.as_bytes(), challenge);
    *proof.as_bytes()
}

/// Cryptographically verify authentication proof in constant time to prevent timing attacks
pub fn verify_auth_proof(token: &str, challenge: &[u8; 32], proof: &[u8; 32]) -> bool {
    let expected = compute_auth_proof(token, challenge);
    constant_time_eq::constant_time_eq(&expected, proof)
}

/// Sanitize file name to strictly prevent directory traversal, illegal chars, and Windows device names
pub fn sanitize_file_name(raw: &str) -> Result<String> {
    if raw.chars().any(|c| c.is_control()) {
        bail!("File name contains invalid control characters");
    }

    let path = std::path::Path::new(raw);
    let base_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .context("File name is invalid or empty")?;

    if base_name == "." || base_name == ".." || base_name.contains('/') || base_name.contains('\\')
    {
        bail!(
            "File name contains illegal path traversal components: {}",
            raw
        );
    }

    let stem = std::path::Path::new(base_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(base_name)
        .to_ascii_uppercase();

    const RESERVED_NAMES: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    if RESERVED_NAMES.contains(&stem.as_str()) {
        bail!(
            "File name uses a reserved system device name: {}",
            base_name
        );
    }

    if base_name.len() > 255 {
        bail!("File name exceeds maximum allowed length of 255 characters");
    }

    Ok(base_name.to_string())
}

/// Send handshake offer over the network
pub async fn send_handshake_offer<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    offer: &HandshakeOffer,
) -> Result<()> {
    writer
        .write_all(MAGIC_BYTES)
        .await
        .context("Failed to write magic bytes")?;

    let json_bytes = serde_json::to_vec(offer).context("Failed to serialize handshake offer")?;
    let len = json_bytes.len() as u32;

    writer
        .write_u32(len)
        .await
        .context("Failed to write handshake length")?;
    writer
        .write_all(&json_bytes)
        .await
        .context("Failed to write handshake body")?;
    writer
        .flush()
        .await
        .context("Failed to flush handshake offer")?;
    Ok(())
}

/// Receive handshake offer over the network with timeout
pub async fn receive_handshake_offer<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<HandshakeOffer> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let mut magic = [0u8; 8];
        reader.read_exact(&mut magic).await.context("Failed to read magic bytes")?;

        if &magic != MAGIC_BYTES {
            bail!("Protocol mismatch: incompatible peer magic bytes. Make sure both peers run HeavyDrop v2.");
        }

        let len = reader.read_u32().await.context("Failed to read handshake length")?;
        if len > 64 * 1024 {
            bail!("Handshake payload unexpectedly large ({} bytes)", len);
        }

        let mut buf = vec![0u8; len as usize];
        reader.read_exact(&mut buf).await.context("Failed to read handshake payload")?;

        let offer: HandshakeOffer = serde_json::from_slice(&buf).context("Failed to parse handshake JSON")?;

        if offer.protocol_version != PROTOCOL_VERSION {
            bail!(
                "Incompatible protocol version: expected v{}, received v{}. Please update both peers.",
                PROTOCOL_VERSION,
                offer.protocol_version
            );
        }

        if offer.chunk_size < MIN_CHUNK_SIZE || offer.chunk_size > MAX_CHUNK_SIZE {
            bail!(
                "Advertised chunk size is out of valid bounds ({} KB to {} MB): {} bytes",
                MIN_CHUNK_SIZE / 1024,
                MAX_CHUNK_SIZE / (1024 * 1024),
                offer.chunk_size
            );
        }

        Ok(offer)
    })
    .await
    .context("Timed out waiting for handshake offer from peer")?
}

/// Send handshake response
pub async fn send_handshake_response<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &HandshakeResponse,
) -> Result<()> {
    let json_bytes = serde_json::to_vec(response).context("Failed to serialize response")?;
    let len = json_bytes.len() as u32;

    writer
        .write_u32(len)
        .await
        .context("Failed to write response length")?;
    writer
        .write_all(&json_bytes)
        .await
        .context("Failed to write response body")?;
    writer
        .flush()
        .await
        .context("Failed to flush handshake response")?;
    Ok(())
}

/// Receive handshake response with timeout
pub async fn receive_handshake_response<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<HandshakeResponse> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let len = reader
            .read_u32()
            .await
            .context("Failed to read handshake response length")?;
        if len > 64 * 1024 {
            bail!("Handshake response unexpectedly large ({} bytes)", len);
        }

        let mut buf = vec![0u8; len as usize];
        reader
            .read_exact(&mut buf)
            .await
            .context("Failed to read handshake response body")?;

        let response: HandshakeResponse =
            serde_json::from_slice(&buf).context("Failed to parse handshake response")?;
        Ok(response)
    })
    .await
    .context("Timed out waiting for handshake response from peer")?
}

/// Send an encrypted chunk header followed by encrypted ciphertext
pub async fn send_encrypted_chunk<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    offset: u64,
    plaintext: &[u8],
    ciphertext: &[u8],
) -> Result<()> {
    let checksum = blake3::hash(plaintext);

    writer.write_u64(offset).await?;
    writer.write_u32(ciphertext.len() as u32).await?;
    writer.write_all(checksum.as_bytes()).await?;
    writer.write_all(ciphertext).await?;
    Ok(())
}

/// Send end-of-stream signal
pub async fn send_end_of_stream<W: AsyncWriteExt + Unpin>(writer: &mut W) -> Result<()> {
    writer.write_u64(END_OF_STREAM_OFFSET).await?;
    writer.write_u32(0).await?;
    writer.write_all(&[0u8; 32]).await?;
    writer.flush().await?;
    Ok(())
}

/// Receive a chunk header with timeout
pub async fn receive_chunk_header<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<ChunkHeader> {
    tokio::time::timeout(CHUNK_TIMEOUT, async {
        let offset = reader
            .read_u64()
            .await
            .context("Failed to read chunk offset")?;
        let ciphertext_length = reader
            .read_u32()
            .await
            .context("Failed to read chunk length")?;
        let mut plaintext_checksum = [0u8; 32];
        reader
            .read_exact(&mut plaintext_checksum)
            .await
            .context("Failed to read chunk checksum")?;

        Ok(ChunkHeader {
            offset,
            ciphertext_length,
            plaintext_checksum,
        })
    })
    .await
    .context("Timed out waiting for chunk header from peer")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_x25519_key_exchange_and_aead_encryption() {
        let (alice_sec, alice_pub) = generate_keypair().unwrap();
        let (bob_sec, bob_pub) = generate_keypair().unwrap();

        let token = Some("secret-token-123");
        let alice_cipher = derive_session_cipher(&alice_sec, &bob_pub, token);
        let bob_cipher = derive_session_cipher(&bob_sec, &alice_pub, token);

        let data = b"HeavyDrop 50GB massive P2P transfer test payload!";
        let chunk_index = 42;

        let encrypted = encrypt_chunk(&alice_cipher, chunk_index, data).unwrap();
        assert_ne!(&encrypted, data);

        let decrypted = decrypt_chunk(&bob_cipher, chunk_index, &encrypted).unwrap();
        assert_eq!(&decrypted, data);

        // Wrong token fails decryption tag verification
        let charlie_cipher = derive_session_cipher(&bob_sec, &alice_pub, Some("wrong-token"));
        assert!(decrypt_chunk(&charlie_cipher, chunk_index, &encrypted).is_err());

        // Tampered ciphertext fails Poly1305 MAC check
        let mut tampered = encrypted.clone();
        tampered[10] ^= 0x42;
        assert!(decrypt_chunk(&bob_cipher, chunk_index, &tampered).is_err());
    }

    #[test]
    fn test_auth_proof_valid_and_invalid() {
        let challenge = generate_auth_challenge().expect("challenge generation should succeed");
        let token = "super_secure_pass_456!";
        let proof = compute_auth_proof(token, &challenge);

        assert!(verify_auth_proof(token, &challenge, &proof));
        assert!(!verify_auth_proof("wrong_password", &challenge, &proof));

        let mut tampered_challenge = challenge;
        tampered_challenge[0] ^= 0xFF;
        assert!(!verify_auth_proof(token, &tampered_challenge, &proof));
    }

    #[test]
    fn test_file_name_sanitization() {
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
}
