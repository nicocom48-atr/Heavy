use anyhow::{bail, Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::integrity::compute_file_blake3;
use crate::part_file::{PartFileManager, PrepareResult};
use crate::protocol::{
    compute_auth_proof, decrypt_chunk, derive_session_cipher, generate_keypair,
    receive_chunk_header, receive_handshake_offer, sanitize_file_name, send_handshake_response,
    HandshakeResponse, CHUNK_TIMEOUT, PROTOCOL_VERSION,
};
use crate::transfer_state::{SharedProgress, TransferRole, TransferStatus};

pub struct ReceiverConfig {
    pub peer_addr: String,
    pub output_dir: PathBuf,
    pub token: Option<String>,
    pub force: bool,
    pub shared_progress: Option<SharedProgress>,
    pub cancel_signal: Option<Arc<AtomicBool>>,
}

pub async fn run_receiver(config: ReceiverConfig) -> Result<()> {
    let cancel = config
        .cancel_signal
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.role = TransferRole::Receiver;
        p.status = TransferStatus::Preparing;
        p.peer_address = config.peer_addr.clone();
        p.status_text = format!("Conectando al emisor en {}...", config.peer_addr);
    }

    tokio::fs::create_dir_all(&config.output_dir)
        .await
        .with_context(|| {
            format!(
                "Failed to create output directory: {}",
                config.output_dir.display()
            )
        })?;

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
            return Err(e)
                .with_context(|| format!("Failed to connect to sender at {}", config.peer_addr));
        }
    };

    let _ = stream.set_nodelay(true);

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Connected;
        p.status_text = "Conectado. Recibiendo detalles del archivo...".to_string();
    }

    println!(
        "{} Connected to peer! Receiving handshake offer...",
        "✓".green().bold()
    );

    // Receive Handshake Offer
    let mut offer = receive_handshake_offer(&mut stream).await?;

    // Strict Path Traversal Sanitization
    offer.file_name = sanitize_file_name(&offer.file_name)?;

    println!("\n{}", "[2/3] Validating transfer details...".cyan().bold());
    println!("  File Name : {}", offer.file_name.bold());
    println!(
        "  File Size : {} ({:.2} GB)",
        indicatif::HumanBytes(offer.file_size),
        offer.file_size as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    println!(
        "  Chunk Size: {}",
        indicatif::HumanBytes(offer.chunk_size as u64)
    );
    println!("  BLAKE3    : {}", offer.file_hash.green());

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.file_name = offer.file_name.clone();
        p.total_bytes = offer.file_size;
    }

    // Generate ephemeral X25519 keypair for Perfect Forward Secrecy transport encryption
    let (receiver_secret, receiver_public) = generate_keypair()?;

    // Compute Cryptographic Authentication Proof if required (Zero password disclosure)
    let auth_proof = if offer.auth_required {
        match config.token {
            Some(ref secret_token) => Some(compute_auth_proof(secret_token, &offer.auth_challenge)),
            None => {
                let resp = HandshakeResponse {
                    protocol_version: PROTOCOL_VERSION,
                    receiver_public_key: receiver_public,
                    accepted: false,
                    resume_offset: 0,
                    auth_proof: None,
                    message: Some(
                        "Authentication failed: server requires security token (--token)"
                            .to_string(),
                    ),
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

    // Prepare destination using .part file and verifiable metadata
    let prep = PartFileManager::prepare(&config.output_dir, &offer, config.force).await?;

    let (mut part_manager, resume_offset) = match prep {
        PrepareResult::AlreadyCompleted(path) => {
            println!(
                "\n{}",
                "✓ File is already 100% downloaded and verified!"
                    .green()
                    .bold()
            );
            let resp = HandshakeResponse {
                protocol_version: PROTOCOL_VERSION,
                receiver_public_key: receiver_public,
                accepted: false,
                resume_offset: offer.file_size,
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
            println!(
                "{} File is ready at: {}",
                "SUCCESS:".green().bold(),
                path.display().to_string().cyan()
            );
            return Ok(());
        }
        PrepareResult::Ready {
            part_manager,
            resume_offset,
        } => (*part_manager, resume_offset),
    };

    if resume_offset > 0 {
        let percent = (resume_offset as f64 / offer.file_size as f64) * 100.0;
        println!(
            "  {} {}",
            "🔄 Verified .part found: Resuming securely from byte"
                .yellow()
                .bold(),
            format!("{} / {:.2}%", indicatif::HumanBytes(resume_offset), percent).yellow()
        );
    }

    // Accept handshake and provide resume offset along with ephemeral public key and auth proof
    let handshake_resp = HandshakeResponse {
        protocol_version: PROTOCOL_VERSION,
        receiver_public_key: receiver_public,
        accepted: true,
        resume_offset,
        auth_proof,
        message: None,
    };
    send_handshake_response(&mut stream, &handshake_resp).await?;

    // Derive symmetric ChaCha20-Poly1305 AEAD cipher
    let cipher = derive_session_cipher(
        &receiver_secret,
        &offer.sender_public_key,
        config.token.as_deref(),
    );
    println!(
        "{} ChaCha20-Poly1305 AEAD Transport Encryption active!",
        "✓".green().bold()
    );

    println!(
        "\n{}",
        "[3/3] Receiving and verifying encrypted chunks in real-time..."
            .cyan()
            .bold()
    );

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
        p.status_text = "Recibiendo y descifrando bloques...".to_string();
    }

    // Buffer for reading ciphertext from network
    // ChaCha20-Poly1305 appends a 16-byte Poly1305 tag
    let max_cipher_len = (offer.chunk_size as usize) + 16;
    let mut cipher_buffer = vec![0u8; max_cipher_len];
    let mut current_offset = resume_offset;
    let mut last_instant = Instant::now();
    let mut last_bytes = current_offset;

    loop {
        // Cooperative cancellation check
        if cancel.load(Ordering::Relaxed) {
            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.status = TransferStatus::Cancelled;
                p.status_text = "Transferencia cancelada por el usuario.".to_string();
            }
            bail!("Transfer cancelled cooperatively by user");
        }

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

        let cipher_len = header.ciphertext_length as usize;
        if cipher_len == 0 || cipher_len > cipher_buffer.len() {
            bail!(
                "Invalid ciphertext length received: {} bytes (max allowed: {})",
                cipher_len,
                cipher_buffer.len()
            );
        }

        // Security check: Total received bytes must not exceed advertised file size
        // Note: plaintext length is ciphertext_length - 16
        let plain_len = cipher_len.saturating_sub(16);
        if current_offset.saturating_add(plain_len as u64) > offer.file_size {
            bail!(
                "Security violation: received data exceeds advertised file size ({} + {} > {})",
                current_offset,
                plain_len,
                offer.file_size
            );
        }

        // Read exact ciphertext payload with network timeout
        tokio::time::timeout(
            CHUNK_TIMEOUT,
            stream.read_exact(&mut cipher_buffer[..cipher_len]),
        )
        .await
        .context("Timed out reading chunk data from peer")?
        .context("Connection dropped while reading chunk data")?;

        let chunk_index = current_offset / (offer.chunk_size as u64);

        // Decrypt chunk and verify Poly1305 MAC tag
        let decrypted_chunk = decrypt_chunk(&cipher, chunk_index, &cipher_buffer[..cipher_len])
            .context("Decryption / AEAD tag verification failure: corrupted or tampered chunk!")?;

        // Cryptographically verify chunk BLAKE3 checksum against header
        let computed_chunk_hash = blake3::hash(&decrypted_chunk);
        if computed_chunk_hash.as_bytes() != &header.plaintext_checksum {
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

        // Write directly to .part file and persist verified metadata
        part_manager
            .write_verified_chunk(
                current_offset,
                &decrypted_chunk,
                *computed_chunk_hash.as_bytes(),
            )
            .await?;

        current_offset += decrypted_chunk.len() as u64;

        if let Some(ref p) = pb {
            p.inc(decrypted_chunk.len() as u64);
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

    // Verify full file hash if available before final commit
    if offer.file_hash != "UNVERIFIED_PREHASH" {
        if let Some(ref p) = pb {
            p.set_message("Verifying full file BLAKE3 checksum...");
        }
        if let Some(ref sp) = config.shared_progress {
            let mut p = sp.lock();
            p.status_text = "Verificando hash global BLAKE3 del archivo...".to_string();
        }

        // Flush part file before hash check
        part_manager.file.flush().await?;
        let final_hash = compute_file_blake3(&part_manager.part_path, false)?;
        if !final_hash.eq_ignore_ascii_case(&offer.file_hash) {
            let _ = stream.write_all(&[0u8]).await;
            if let Some(ref p) = pb {
                p.abandon();
            }
            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.status = TransferStatus::Failed;
                p.error = Some(
                    "Verificación final de hash fallida: el archivo está corrupto".to_string(),
                );
            }
            bail!(
                "Full file integrity check failed!\nExpected: {}\nActual:   {}",
                offer.file_hash,
                final_hash
            );
        }
    }

    // Atomically commit .part to final target file
    let final_destination = part_manager.finalize().await?;

    // Send ACK byte (1 = success)
    stream
        .write_all(&[1u8])
        .await
        .context("Failed to send ACK to sender")?;
    stream.flush().await?;

    if let Some(ref p) = pb {
        p.finish_with_message("Transfer complete and verified!".green().bold().to_string());
    }

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Completed;
        p.transferred_bytes = offer.file_size;
        p.bytes_per_sec = 0.0;
        p.status_text = format!("¡Archivo guardado en {}!", final_destination.display());
    }

    println!(
        "\n{} File saved to: {}",
        "SUCCESS:".green().bold(),
        final_destination
            .canonicalize()
            .unwrap_or(final_destination)
            .display()
            .to_string()
            .cyan()
    );

    Ok(())
}
