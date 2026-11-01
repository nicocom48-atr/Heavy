use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

use crate::integrity::compute_file_blake3_async;
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
    overwrite: bool,
}

impl PartFileManager {
    /// Prepare destination directory and determine whether file is complete, partial, or fresh
    pub async fn prepare(
        output_dir: &Path,
        offer: &HandshakeOffer,
        force: bool,
    ) -> Result<PrepareResult> {
        if crate::protocol::sanitize_file_name(&offer.file_name)? != offer.file_name {
            bail!("El nombre de destino contiene una ruta");
        }
        if !(crate::protocol::MIN_CHUNK_SIZE..=crate::protocol::MAX_CHUNK_SIZE).contains(&offer.chunk_size) {
            bail!("Tamaño de bloque inválido");
        }
        let final_path = output_dir.join(&offer.file_name);
        let part_path = output_dir.join(format!("{}.part", offer.file_name));
        let meta_path = output_dir.join(format!("{}.part.meta", offer.file_name));
        for path in [&final_path, &part_path, &meta_path] {
            if tokio::fs::symlink_metadata(path).await.is_ok_and(|m| m.file_type().is_symlink()) {
                bail!("El destino contiene un enlace simbólico: {}", path.display());
            }
        }

        // 1. Check if final target file already exists and is complete
        if final_path.exists() && !force {
            let meta = tokio::fs::metadata(&final_path).await?;
            if meta.len() == offer.file_size && offer.file_hash != "UNVERIFIED_PREHASH" {
                let current_hash = compute_file_blake3_async(&final_path).await?;
                if current_hash.eq_ignore_ascii_case(&offer.file_hash) {
                    return Ok(PrepareResult::AlreadyCompleted(final_path));
                }
            }
            bail!("El archivo de destino ya existe. Usa --force para reemplazarlo o elige otra carpeta.");
        }

        // 2. Check for verifiable .part and .part.meta
        let mut resume_offset = 0u64;
        let mut last_chunk_hash = [0u8; 32];

        if !force && offer.file_hash != "UNVERIFIED_PREHASH" && part_path.exists()
            && tokio::fs::metadata(&meta_path).await.is_ok_and(|m| m.len() <= 16 * 1024) {
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
                        && (meta.verified_bytes == offer.file_size || meta.verified_bytes % offer.chunk_size as u64 == 0)
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
            overwrite: force,
        };

        Ok(PrepareResult::Ready {
            part_manager: Box::new(part_manager),
            resume_offset,
        })
    }

    /// Write verified chunk and persist metadata update
    pub async fn write_verified_chunk(
        &mut self,
        offset: u64,
        data: &[u8],
        chunk_hash: [u8; 32],
    ) -> Result<()> {
        if offset != self.metadata.verified_bytes || data.len() as u64 > self.metadata.file_size.saturating_sub(offset) {
            bail!("Bloque fuera de orden o fuera de los límites del archivo");
        }
        self.file
            .write_all(data)
            .await
            .context("Failed to write chunk to disk")?;
        self.metadata.verified_bytes += data.len() as u64;
        self.metadata.last_chunk_hash = chunk_hash;

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
        if self.metadata.verified_bytes != self.metadata.file_size || self.file.metadata().await?.len() != self.metadata.file_size {
            bail!("No se puede finalizar un archivo incompleto");
        }
        self.file
            .sync_all()
            .await
            .context("Failed to sync file storage")?;
        drop(self.file);

        publish_file(&self.part_path, &self.final_path, self.overwrite).await
            .with_context(|| {
                format!(
                    "Failed to rename {} to {}",
                    self.part_path.display(),
                    self.final_path.display()
                )
            })?;
        let _ = tokio::fs::remove_file(&self.meta_path).await;

        Ok(self.final_path)
    }
}

#[cfg(target_os = "windows")]
async fn publish_file(source: &Path, target: &Path, overwrite: bool) -> std::io::Result<()> {
    let source = source.to_path_buf();
    let target = target.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" { fn MoveFileExW(source: *const u16, target: *const u16, flags: u32) -> i32; }
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        // No REPLACE_EXISTING unless explicitly requested. Works on NTFS and exFAT.
        if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), u32::from(overwrite)) } == 0 {
            Err(std::io::Error::last_os_error())
        } else { Ok(()) }
    }).await.map_err(std::io::Error::other)?
}

#[cfg(not(target_os = "windows"))]
async fn publish_file(source: &Path, target: &Path, overwrite: bool) -> std::io::Result<()> {
    if overwrite { tokio::fs::rename(source, target).await }
    else {
        tokio::fs::hard_link(source, target).await?;
        tokio::fs::remove_file(source).await
    }
}
