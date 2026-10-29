use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const MAGIC_BYTES: &[u8; 8] = b"HVDROP01";
#[allow(dead_code)]
pub const DEFAULT_CHUNK_SIZE: u32 = 4 * 1024 * 1024; // 4 MB chunk size for high-throughput 50GB transfers
pub const END_OF_STREAM_OFFSET: u64 = u64::MAX;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HandshakeOffer {
    pub file_name: String,
    pub file_size: u64,
    pub chunk_size: u32,
    pub file_hash: String,
    pub auth_token: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HandshakeResponse {
    pub accepted: bool,
    pub resume_offset: u64,
    pub message: Option<String>,
}

#[derive(Debug)]
pub struct ChunkHeader {
    pub offset: u64,
    pub length: u32,
    pub checksum: [u8; 32],
}

impl ChunkHeader {
    pub fn is_end_of_stream(&self) -> bool {
        self.offset == END_OF_STREAM_OFFSET && self.length == 0
    }
}

/// Send handshake offer over the network
pub async fn send_handshake_offer<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    offer: &HandshakeOffer,
) -> Result<()> {
    writer.write_all(MAGIC_BYTES).await.context("Failed to write magic bytes")?;

    let json_bytes = serde_json::to_vec(offer).context("Failed to serialize handshake offer")?;
    let len = json_bytes.len() as u32;

    writer.write_u32(len).await.context("Failed to write handshake length")?;
    writer.write_all(&json_bytes).await.context("Failed to write handshake body")?;
    writer.flush().await.context("Failed to flush handshake offer")?;
    Ok(())
}

/// Receive handshake offer over the network
pub async fn receive_handshake_offer<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<HandshakeOffer> {
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic).await.context("Failed to read magic bytes")?;

    if &magic != MAGIC_BYTES {
        bail!("Protocol mismatch: invalid magic header received. Is the peer using heavydrop?");
    }

    let len = reader.read_u32().await.context("Failed to read handshake length")?;
    if len > 64 * 1024 {
        bail!("Handshake payload unexpectedly large ({} bytes)", len);
    }

    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await.context("Failed to read handshake payload")?;

    let offer: HandshakeOffer = serde_json::from_slice(&buf).context("Failed to parse handshake JSON")?;
    Ok(offer)
}

/// Send handshake response
pub async fn send_handshake_response<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &HandshakeResponse,
) -> Result<()> {
    let json_bytes = serde_json::to_vec(response).context("Failed to serialize response")?;
    let len = json_bytes.len() as u32;

    writer.write_u32(len).await.context("Failed to write response length")?;
    writer.write_all(&json_bytes).await.context("Failed to write response body")?;
    writer.flush().await.context("Failed to flush handshake response")?;
    Ok(())
}

/// Receive handshake response
pub async fn receive_handshake_response<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<HandshakeResponse> {
    let len = reader.read_u32().await.context("Failed to read handshake response length")?;
    if len > 64 * 1024 {
        bail!("Handshake response unexpectedly large ({} bytes)", len);
    }

    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await.context("Failed to read handshake response body")?;

    let response: HandshakeResponse =
        serde_json::from_slice(&buf).context("Failed to parse handshake response")?;
    Ok(response)
}

/// Send a chunk header followed by chunk data
pub async fn send_chunk<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    offset: u64,
    data: &[u8],
) -> Result<()> {
    let checksum = blake3::hash(data);
    
    writer.write_u64(offset).await?;
    writer.write_u32(data.len() as u32).await?;
    writer.write_all(checksum.as_bytes()).await?;
    writer.write_all(data).await?;
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

/// Receive a chunk header
pub async fn receive_chunk_header<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<ChunkHeader> {
    let offset = reader.read_u64().await.context("Failed to read chunk offset")?;
    let length = reader.read_u32().await.context("Failed to read chunk length")?;
    let mut checksum = [0u8; 32];
    reader.read_exact(&mut checksum).await.context("Failed to read chunk checksum")?;

    Ok(ChunkHeader {
        offset,
        length,
        checksum,
    })
}
