use anyhow::{bail, Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::io::SeekFrom;
use std::path::PathBuf;
use std::time::Instant;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::integrity::compute_file_blake3;
use crate::protocol::{
    compute_auth_proof, receive_chunk_header, receive_handshake_offer, sanitize_file_name,
    send_handshake_response, HandshakeResponse, CHUNK_TIMEOUT,
};
use crate::transfer_state::{SharedProgress, TransferRole, TransferStatus};

pub struct ReceiverConfig {
    pub peer_addr: String,
    pub output_dir: PathBuf,
    pub token: Option<String>,
    pub force: bool,
    pub shared_progress: Option<SharedProgress>,
}

pub async fn run_receiver(config: ReceiverConfig) -> Result<()> {
    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.role = TransferRole::Receiver;
        p.status = TransferStatus::Preparing;
        p.peer_address = config.peer_addr.clone();
        p.status_text = format!("Conectando al emisor en {}...", config.peer_addr);
    }

    tokio::fs::create_dir_all(&config.output_dir)
        .await
        .with_context(|| format!("Failed to create output directory: {}", config.output_dir.display()))?;

    println!("{}", "\n[1/3] Connecting to peer sender...".cyan().bold());
    println!("  Target Address: {}", config.peer_addr.yellow().bold());

    let mut stream = match TcpStream::connect(&config.peer_addr).await {
        Ok(s) => s,
        Err(e) => {
            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.status = TransferStatus::Failed;
                p.error = Some(format!("No se pudo conectar a {}: {}", config.peer_addr, e));
            }
            return Err(e).with_context(|| format!("Failed to connect to sender at {}", config.peer_addr));
        }
    };

    let _ = stream.set_nodelay(true);

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Connected;
        p.status_text = "Conectado. Recibiendo detalles del archivo...".to_string();
    }

    println!("{} Connected to peer! Receiving handshake offer...", "✓".green().bold());

    // Receive Handshake Offer
    let offer = receive_handshake_offer(&mut stream).await?;

    // Strict Path Traversal Sanitization
    let safe_file_name = sanitize_file_name(&offer.file_name)?;

    println!("\n{}", "[2/3] Validating transfer details...".cyan().bold());
    println!("  File Name : {}", safe_file_name.bold());
    println!(
        "  File Size : {} ({:.2} GB)",
        indicatif::HumanBytes(offer.file_size),
        offer.file_size as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    println!("  Chunk Size: {}", indicatif::HumanBytes(offer.chunk_size as u64));
    println!("  BLAKE3    : {}", offer.file_hash.green());

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.file_name = safe_file_name.clone();
        p.total_bytes = offer.file_size;
    }

    // Compute Cryptographic Authentication Proof if required (Zero password disclosure)
    let auth_proof = if offer.auth_required {
        match config.token {
            Some(ref secret_token) => {
                Some(compute_auth_proof(secret_token, &offer.auth_challenge))
            }
            None => {
                let resp = HandshakeResponse {
                    accepted: false,
                    resume_offset: 0,
                    auth_proof: None,
                    message: Some("Authentication failed: server requires security token (--token)".to_string()),
                };
                let _ = send_handshake_response(&mut stream, &resp).await;
                if let Some(ref sp) = config.shared_progress {
                    let mut p = sp.lock();
                    p.status = TransferStatus::Failed;
                    p.error = Some("El emisor requiere contraseña/token (--token)".to_string());
                }
                bail!("Server requires authentication: please specify a security token (--token <PASSWORD>)");
            }
        }
    } else {
        None
    };

    // Determine target output path safely within output_dir
    let destination_path = config.output_dir.join(&safe_file_name);
    if destination_path.parent() != Some(&config.output_dir) {
        bail!("Security violation: destination path escapes output directory");
    }

    let mut resume_offset = 0u64;

    if destination_path.exists() && !config.force {
        let existing_meta = tokio::fs::metadata(&destination_path).await?;
        let existing_len = existing_meta.len();

        if existing_len >= offer.file_size {
            println!("  File already exists with full size ({} bytes). Verifying integrity...", existing_len);
            if offer.file_hash != "UNVERIFIED_PREHASH" {
                let current_hash = compute_file_blake3(&destination_path, config.shared_progress.is_none())?;
                if current_hash.eq_ignore_ascii_case(&offer.file_hash) {
                    println!("\n{}", "✓ File is already 100% downloaded and verified!".green().bold());
                    let resp = HandshakeResponse {
                        accepted: false,
                        resume_offset: existing_len,
                        auth_proof,
                        message: Some("File already exists and matches hash perfectly.".to_string()),
                    };
                    send_handshake_response(&mut stream, &resp).await?;
                    if let Some(ref sp) = config.shared_progress {
                        let mut p = sp.lock();
                        p.status = TransferStatus::Completed;
                        p.transferred_bytes = offer.file_size;
                        p.status_text = "El archivo ya estaba descargado y es 100% íntegro.".to_string();
                    }
                    return Ok(());
                }
            }
            println!("  Existing file does not match or was incomplete. Re-downloading from scratch.");
            resume_offset = 0;
        } else {
            // Align resume offset to chunk boundary
            let chunk_size = offer.chunk_size as u64;
            resume_offset = (existing_len / chunk_size) * chunk_size;
            let percent = (resume_offset as f64 / offer.file_size as f64) * 100.0;
            println!(
                "  {} {}",
                "🔄 Partial file found: Resuming from byte".yellow().bold(),
                format!("{} / {:.2}%", indicatif::HumanBytes(resume_offset), percent).yellow()
            );
        }
    }

    // Accept handshake and provide resume offset along with auth proof
    let handshake_resp = HandshakeResponse {
        accepted: true,
        resume_offset,
        auth_proof,
        message: None,
    };
    send_handshake_response(&mut stream, &handshake_resp).await?;

    // Open file for writing
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(resume_offset == 0)
        .open(&destination_path)
        .await
        .with_context(|| format!("Failed to open destination file {}", destination_path.display()))?;

    if resume_offset > 0 {
        file.seek(SeekFrom::Start(resume_offset)).await?;
        file.set_len(resume_offset).await?;
    }

    println!("\n{}", "[3/3] Receiving and verifying chunks in real-time...".cyan().bold());

    let pb = if config.shared_progress.is_none() {
        let p = ProgressBar::new(offer.file_size);
        p.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, ETA {eta})")
                .expect("Invalid progress template")
                .progress_chars("#>-"),
        );
        p.set_position(resume_offset);
        Some(p)
    } else {
        None
    };

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Transferring;
        p.transferred_bytes = resume_offset;
        p.resume_offset = resume_offset;
        p.status_text = "Recibiendo y verificando bloques...".to_string();
    }

    let mut chunk_buffer = vec![0u8; offer.chunk_size as usize];
    let mut current_offset = resume_offset;
    let mut last_instant = Instant::now();
    let mut last_bytes = current_offset;

    loop {
        let header = receive_chunk_header(&mut stream).await?;

        if header.is_end_of_stream() {
            break;
        }

        // Security check: Offset must strictly match current expected offset
        if header.offset != current_offset {
            bail!(
                "Protocol violation: expected chunk at offset {}, received offset {}",
                current_offset,
                header.offset
            );
        }

        let chunk_len = header.length as usize;
        // Security check: Chunk length must not exceed advertised chunk buffer
        if chunk_len == 0 || chunk_len > chunk_buffer.len() {
            bail!("Invalid chunk length received: {} bytes (max allowed: {})", chunk_len, chunk_buffer.len());
        }

        // Security check: Total received bytes must not exceed advertised file size
        if current_offset.saturating_add(header.length as u64) > offer.file_size {
            bail!(
                "Security violation: received data exceeds advertised file size ({} + {} > {})",
                current_offset,
                header.length,
                offer.file_size
            );
        }

        // Read exact chunk payload with network timeout
        tokio::time::timeout(
            CHUNK_TIMEOUT,
            stream.read_exact(&mut chunk_buffer[..chunk_len])
        )
        .await
        .context("Timed out reading chunk data from peer")?
        .context("Connection dropped while reading chunk data")?;

        // Cryptographically verify chunk BLAKE3 checksum immediately
        let computed_chunk_hash = blake3::hash(&chunk_buffer[..chunk_len]);
        if computed_chunk_hash.as_bytes() != &header.checksum {
            let _ = stream.write_all(&[0u8]).await;
            if let Some(ref p) = pb {
                p.abandon();
            }
            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.status = TransferStatus::Failed;
                p.error = Some(format!("Error de integridad en bloque {}!", header.offset));
            }
            bail!(
                "Integrity failure at byte offset {}: corrupted data received over network!",
                header.offset
            );
        }

        // Write directly to disk
        file.write_all(&chunk_buffer[..chunk_len]).await
            .context("Failed to write chunk to disk")?;

        current_offset += chunk_len as u64;

        if let Some(ref p) = pb {
            p.inc(chunk_len as u64);
        }

        let elapsed = last_instant.elapsed();
        if elapsed.as_millis() >= 100 {
            let bytes_diff = current_offset - last_bytes;
            let speed = (bytes_diff as f64) / elapsed.as_secs_f64();
            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.transferred_bytes = current_offset;
                p.bytes_per_sec = speed;
            }
            last_instant = Instant::now();
            last_bytes = current_offset;
        }
    }

    // Flush and commit to physical storage
    file.flush().await?;
    file.sync_all().await?;

    // Verify full file hash if available
    if offer.file_hash != "UNVERIFIED_PREHASH" {
        if let Some(ref p) = pb {
            p.set_message("Verifying full file BLAKE3 checksum...");
        }
        if let Some(ref sp) = config.shared_progress {
            let mut p = sp.lock();
            p.status_text = "Verificando hash global BLAKE3 del archivo...".to_string();
        }

        let final_hash = compute_file_blake3(&destination_path, false)?;
        if !final_hash.eq_ignore_ascii_case(&offer.file_hash) {
            let _ = stream.write_all(&[0u8]).await;
            if let Some(ref p) = pb {
                p.abandon();
            }
            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.status = TransferStatus::Failed;
                p.error = Some("Verificación final de hash fallida: el archivo está corrupto".to_string());
            }
            bail!(
                "Full file integrity check failed!\nExpected: {}\nActual:   {}",
                offer.file_hash,
                final_hash
            );
        }
    }

    // Send ACK byte (1 = success)
    stream.write_all(&[1u8]).await.context("Failed to send ACK to sender")?;
    stream.flush().await?;

    if let Some(ref p) = pb {
        p.finish_with_message("Transfer complete and verified!".green().bold().to_string());
    }

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Completed;
        p.transferred_bytes = offer.file_size;
        p.bytes_per_sec = 0.0;
        p.status_text = format!("¡Archivo guardado en {}!", destination_path.display());
    }

    println!(
        "\n{} File saved to: {}",
        "SUCCESS:".green().bold(),
        destination_path.canonicalize().unwrap_or(destination_path).display().to_string().cyan()
    );

    Ok(())
}
