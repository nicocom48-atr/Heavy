use anyhow::{bail, Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;

use crate::discovery::spawn_beacon;
use crate::integrity::compute_file_blake3;
use crate::network::print_sender_connection_info;
use crate::protocol::{
    derive_session_cipher, encrypt_chunk, generate_auth_challenge, generate_keypair,
    send_encrypted_chunk, send_end_of_stream, send_handshake_offer, verify_auth_proof,
    HandshakeOffer, PROTOCOL_VERSION,
};
use crate::transfer_state::{SharedProgress, TransferRole, TransferStatus};

pub struct SenderConfig {
    pub file_path: PathBuf,
    pub bind_addr: String,
    pub port: u16,
    pub chunk_size: u32,
    pub token: Option<String>,
    pub skip_prehash: bool,
    pub shared_progress: Option<SharedProgress>,
    pub cancel_signal: Option<Arc<AtomicBool>>,
}

pub async fn run_sender(config: SenderConfig) -> Result<()> {
    let cancel = config
        .cancel_signal
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

    if !config.file_path.exists() {
        if let Some(ref sp) = config.shared_progress {
            let mut p = sp.lock();
            p.status = TransferStatus::Failed;
            p.error = Some(format!(
                "Archivo no encontrado: {}",
                config.file_path.display()
            ));
        }
        bail!("File not found: {}", config.file_path.display());
    }

    let file_metadata = tokio::fs::metadata(&config.file_path)
        .await
        .context("Failed to read file metadata")?;

    if !file_metadata.is_file() {
        if let Some(ref sp) = config.shared_progress {
            let mut p = sp.lock();
            p.status = TransferStatus::Failed;
            p.error = Some("La ruta indicada no es un archivo".to_string());
        }
        bail!(
            "The path specified is not a file: {}",
            config.file_path.display()
        );
    }

    let file_size = file_metadata.len();
    let file_name = config
        .file_path
        .file_name()
        .context("Invalid file name")?
        .to_string_lossy()
        .to_string();

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.role = TransferRole::Sender;
        p.status = TransferStatus::Preparing;
        p.file_name = file_name.clone();
        p.total_bytes = file_size;
        p.status_text = "Preparando archivo y calculando hash...".to_string();
    }

    println!(
        "{}",
        "\n[1/3] Preparing file for encrypted P2P transfer..."
            .cyan()
            .bold()
    );
    println!("  File Name : {}", file_name.bold());
    println!(
        "  File Size : {} ({:.2} GB)",
        indicatif::HumanBytes(file_size),
        file_size as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    println!(
        "  Chunk Size: {}",
        indicatif::HumanBytes(config.chunk_size as u64)
    );

    // Calculate full file hash
    let file_hash = if config.skip_prehash {
        println!(
            "  {}",
            "Skipping full pre-hash as requested. Chunks will be verified individually on arrival."
                .yellow()
        );
        "UNVERIFIED_PREHASH".to_string()
    } else {
        println!("  Computing BLAKE3 integrity checksum...");
        compute_file_blake3(&config.file_path, config.shared_progress.is_none())
            .context("Failed to compute file BLAKE3 checksum")?
    };

    println!("  BLAKE3 Hash: {}", file_hash.green());

    // Bind TCP socket
    let bind_target = format!("{}:{}", config.bind_addr, config.port);
    let listener = match TcpListener::bind(&bind_target).await {
        Ok(l) => l,
        Err(e) => {
            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.status = TransferStatus::Failed;
                p.error = Some(format!("No se pudo abrir el puerto {}: {}", config.port, e));
            }
            return Err(e).with_context(|| format!("Failed to bind to {}", bind_target));
        }
    };

    let local_port = listener.local_addr()?.port();
    print_sender_connection_info(local_port, &config.token);

    // Spawn local discovery beacon on LAN
    spawn_beacon(file_name.clone(), file_size, local_port, cancel.clone());

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::WaitingForPeer;
        p.status_text = format!(
            "Esperando conexión del receptor en puerto {}...",
            local_port
        );
    }

    // Wait for receiver connection or cancellation
    let (mut stream, peer_addr) = tokio::select! {
        conn = listener.accept() => {
            conn.context("Failed to accept connection from peer")?
        }
        _ = async {
            while !cancel.load(Ordering::Relaxed) {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        } => {
            bail!("Transfer cancelled cooperatively by user while waiting for connection");
        }
    };

    let _ = stream.set_nodelay(true);

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Connected;
        p.peer_address = peer_addr.to_string();
        p.status_text = format!("Receptor conectado desde {}", peer_addr);
    }

    println!(
        "\n{} Peer connected from: {}",
        "✓".green().bold(),
        peer_addr.to_string().cyan().bold()
    );

    // Generate ephemeral X25519 keypair for Perfect Forward Secrecy transport encryption
    let (sender_secret, sender_public) = generate_keypair()?;

    // Cryptographic Challenge Generation (Zero secret token leakage)
    let (auth_required, auth_challenge) = if config.token.is_some() {
        (true, generate_auth_challenge()?)
    } else {
        (false, [0u8; 32])
    };

    // Perform Handshake
    println!(
        "{}",
        "[2/3] Performing encrypted protocol handshake with receiver...".cyan()
    );
    let offer = HandshakeOffer {
        protocol_version: PROTOCOL_VERSION,
        sender_public_key: sender_public,
        file_name: file_name.clone(),
        file_size,
        chunk_size: config.chunk_size,
        file_hash: file_hash.clone(),
        auth_required,
        auth_challenge,
    };

    send_handshake_offer(&mut stream, &offer).await?;

    let response = crate::protocol::receive_handshake_response(&mut stream).await?;

    // Server-Side Authentication Verification (Constant-Time)
    if let Some(ref secret_token) = config.token {
        let is_valid = match response.auth_proof {
            Some(ref proof) => verify_auth_proof(secret_token, &auth_challenge, proof),
            None => false,
        };

        if !is_valid {
            eprintln!(
                "{} Peer authentication failed from {}: invalid or missing token.",
                "SECURITY ERROR:".red().bold(),
                peer_addr
            );
            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.status = TransferStatus::Failed;
                p.error = Some(
                    "Conexión rechazada: el receptor no proporcionó la clave correcta".to_string(),
                );
            }
            bail!("Unauthorized connection: peer failed security token authentication.");
        }
        println!(
            "{} Peer authentication verified cryptographically!",
            "✓".green().bold()
        );
    }

    if !response.accepted {
        let msg = response
            .message
            .unwrap_or_else(|| "Unknown rejection reason".to_string());
        if let Some(ref sp) = config.shared_progress {
            let mut p = sp.lock();
            p.status = TransferStatus::Failed;
            p.error = Some(format!("Receptor rechazó la transferencia: {}", msg));
        }
        bail!("Receiver rejected the file transfer: {}", msg.red());
    }

    // Derive symmetric ChaCha20-Poly1305 AEAD cipher
    let cipher = derive_session_cipher(
        &sender_secret,
        &response.receiver_public_key,
        config.token.as_deref(),
    );
    println!(
        "{} ChaCha20-Poly1305 AEAD Transport Encryption active!",
        "✓".green().bold()
    );

    let resume_offset = response.resume_offset;
    if resume_offset > file_size {
        bail!(
            "Security violation: peer requested resume offset ({}) beyond file size ({})",
            resume_offset,
            file_size
        );
    }
    if resume_offset % (config.chunk_size as u64) != 0 && resume_offset != file_size {
        bail!(
            "Protocol violation: resume offset {} is not aligned to chunk size {}",
            resume_offset,
            config.chunk_size
        );
    }

    if resume_offset > 0 {
        let percent = (resume_offset as f64 / file_size as f64) * 100.0;
        println!(
            "  {} {}",
            "🔄 Resuming transfer from byte:".yellow().bold(),
            format!(
                "{} / {:.2}% already present on receiver",
                indicatif::HumanBytes(resume_offset),
                percent
            )
            .yellow()
        );
    } else {
        println!("  Handshake accepted. Starting full transfer.");
    }

    // Open file for async streaming
    let mut file = File::open(&config.file_path).await?;
    if resume_offset > 0 {
        file.seek(SeekFrom::Start(resume_offset)).await?;
    }

    println!(
        "\n{}",
        "[3/3] Streaming encrypted file chunks...".cyan().bold()
    );

    let pb = if config.shared_progress.is_none() {
        let p = ProgressBar::new(file_size);
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
        p.status_text = "Transfiriendo datos cifrados...".to_string();
    }

    let mut buffer = vec![0u8; config.chunk_size as usize];
    let mut current_offset = resume_offset;

    let mut last_instant = Instant::now();
    let mut last_bytes = current_offset;

    while current_offset < file_size {
        // Cooperative cancellation check
        if cancel.load(Ordering::Relaxed) {
            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.status = TransferStatus::Cancelled;
                p.status_text = "Transferencia cancelada por el usuario.".to_string();
            }
            bail!("Transfer cancelled cooperatively by user");
        }

        let to_read = std::cmp::min(
            config.chunk_size as usize,
            (file_size - current_offset) as usize,
        );
        let n = file.read_exact(&mut buffer[..to_read]).await?;

        if n == 0 {
            break;
        }

        let chunk_index = current_offset / (config.chunk_size as u64);
        let ciphertext = encrypt_chunk(&cipher, chunk_index, &buffer[..n])?;

        send_encrypted_chunk(&mut stream, current_offset, &buffer[..n], &ciphertext).await?;

        current_offset += n as u64;

        if let Some(ref p) = pb {
            p.inc(n as u64);
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

    // Send end of stream indicator
    send_end_of_stream(&mut stream).await?;

    // Wait for receiver ACK
    let mut ack = [0u8; 1];
    let ack_result = stream.read_exact(&mut ack).await;
    match ack_result {
        Ok(_) if ack[0] == 1 => {
            if let Some(ref p) = pb {
                p.finish_with_message(
                    "Transfer completed & verified by receiver!"
                        .green()
                        .bold()
                        .to_string(),
                );
            }
            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.status = TransferStatus::Completed;
                p.transferred_bytes = file_size;
                p.bytes_per_sec = 0.0;
                p.status_text = "¡Transferencia completada y verificada exitosamente!".to_string();
            }
            println!(
                "\n{} Transfer was 100% successful and verified!",
                "SUCCESS:".green().bold()
            );
        }
        _ => {
            if let Some(ref p) = pb {
                p.abandon();
            }
            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.status = TransferStatus::Failed;
                p.error = Some("El receptor reportó fallo de verificación o abortó".to_string());
            }
            bail!("Transfer ended, but receiver failed to verify data integrity or aborted!");
        }
    }

    Ok(())
}
