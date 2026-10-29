use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

use crate::integrity::compute_file_blake3;
use crate::protocol::{HandshakeOffer, PROTOCOL_VERSION};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PartMetadata {
    pub protocol_version: u32,
    pub file_name: String,
    pub file_size: u64,
    pub chunk_size: u32,
    pub file_hash: String,
    pub verified_bytes: u64,
    pub last_chunk_hash: [u8; 32],
}

pub enum PrepareResult {
    AlreadyCompleted(PathBuf),
    Ready {
        part_manager: Box<PartFileManager>,
        resume_offset: u64,
    },
}

pub struct PartFileManager {
    pub final_path: PathBuf,
    pub part_path: PathBuf,
    pub meta_path: PathBuf,
    pub file: File,
    pub metadata: PartMetadata,
}

impl PartFileManager {
    /// Prepare destination directory and determine whether file is complete, partial, or fresh
    pub async fn prepare(
        output_dir: &Path,
        offer: &HandshakeOffer,
        force: bool,
    ) -> Result<PrepareResult> {
        let final_path = output_dir.join(&offer.file_name);
        let part_path = output_dir.join(format!("{}.part", offer.file_name));
        let meta_path = output_dir.join(format!("{}.part.meta", offer.file_name));

        // 1. Check if final target file already exists and is complete
        if final_path.exists() && !force {
            let meta = tokio::fs::metadata(&final_path).await?;
            if meta.len() == offer.file_size && offer.file_hash != "UNVERIFIED_PREHASH" {
                let current_hash = compute_file_blake3(&final_path, false)?;
                if current_hash.eq_ignore_ascii_case(&offer.file_hash) {
                    return Ok(PrepareResult::AlreadyCompleted(final_path));
                }
            }
        }

        // 2. Check for verifiable .part and .part.meta
        let mut resume_offset = 0u64;
        let mut last_chunk_hash = [0u8; 32];

        if !force && part_path.exists() && meta_path.exists() {
            if let Ok(meta_bytes) = tokio::fs::read(&meta_path).await {
                if let Ok(meta) = serde_json::from_slice::<PartMetadata>(&meta_bytes) {
                    // Check if metadata matches current transfer
                    let matches = meta.protocol_version == PROTOCOL_VERSION
                        && meta.file_name == offer.file_name
                        && meta.file_size == offer.file_size
                        && meta.chunk_size == offer.chunk_size
                        && (offer.file_hash == "UNVERIFIED_PREHASH"
                            || meta.file_hash.eq_ignore_ascii_case(&offer.file_hash));

                    if matches && meta.verified_bytes > 0 && meta.verified_bytes <= offer.file_size
                    {
                        let actual_len = tokio::fs::metadata(&part_path).await?.len();
                        if actual_len >= meta.verified_bytes {
                            // Verify integrity of the last verified chunk on disk before accepting resume
                            let chunk_size = meta.chunk_size as u64;
                            let last_chunk_len = if meta.verified_bytes % chunk_size == 0 {
                                chunk_size
                            } else {
                                meta.verified_bytes % chunk_size
                            };
                            let last_chunk_start = meta.verified_bytes - last_chunk_len;

                            if let Ok(mut verify_file) = File::open(&part_path).await {
                                if verify_file
                                    .seek(SeekFrom::Start(last_chunk_start))
                                    .await
                                    .is_ok()
                                {
                                    let mut chunk_buf = vec![0u8; last_chunk_len as usize];
                                    if verify_file.read_exact(&mut chunk_buf).await.is_ok() {
                                        let check = blake3::hash(&chunk_buf);
                                        if check.as_bytes() == &meta.last_chunk_hash {
                                            resume_offset = meta.verified_bytes;
                                            last_chunk_hash = meta.last_chunk_hash;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 3. Open .part file for writing
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(resume_offset == 0)
            .open(&part_path)
            .await
            .with_context(|| format!("Failed to open partial file {}", part_path.display()))?;

        if resume_offset > 0 {
            file.seek(SeekFrom::Start(resume_offset)).await?;
            file.set_len(resume_offset).await?;
        }

        let metadata = PartMetadata {
            protocol_version: PROTOCOL_VERSION,
            file_name: offer.file_name.clone(),
            file_size: offer.file_size,
            chunk_size: offer.chunk_size,
            file_hash: offer.file_hash.clone(),
            verified_bytes: resume_offset,
            last_chunk_hash,
        };

        let part_manager = PartFileManager {
            final_path,
            part_path,
            meta_path,
            file,
            metadata,
        };

        Ok(PrepareResult::Ready {
            part_manager: Box::new(part_manager),
            resume_offset,
        })
    }

    /// Write verified chunk and persist metadata update
    pub async fn write_verified_chunk(
        &mut self,
        _offset: u64,
        data: &[u8],
        chunk_hash: [u8; 32],
    ) -> Result<()> {
        self.file
            .write_all(data)
            .await
            .context("Failed to write chunk to disk")?;
        self.metadata.verified_bytes += data.len() as u64;
        self.metadata.last_chunk_hash = chunk_hash;

        // Persist metadata
        let json = serde_json::to_vec(&self.metadata)?;
        tokio::fs::write(&self.meta_path, json)
            .await
            .context("Failed to write part metadata")?;

        Ok(())
    }

    /// Commit, flush and atomically rename .part to final target file
    pub async fn finalize(mut self) -> Result<PathBuf> {
        self.file
            .flush()
            .await
            .context("Failed to flush file to disk")?;
        self.file
            .sync_all()
            .await
            .context("Failed to sync file storage")?;
        drop(self.file);

        // Remove metadata
        let _ = tokio::fs::remove_file(&self.meta_path).await;

        // Atomically rename .part to final destination
        if self.final_path.exists() {
            let _ = tokio::fs::remove_file(&self.final_path).await;
        }
        tokio::fs::rename(&self.part_path, &self.final_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to rename {} to {}",
                    self.part_path.display(),
                    self.final_path.display()
                )
            })?;

        Ok(self.final_path)
    }
}
