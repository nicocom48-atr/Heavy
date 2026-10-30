use anyhow::{bail, Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::connectivity::establish_receiver_connection;
use crate::discovery::DiscoveryService;
use crate::integrity::compute_file_blake3_async;
use crate::pairing::PairingCode;
use crate::part_file::{PartFileManager, PrepareResult};
use crate::protocol::{
    decrypt_chunk, derive_session_cipher, generate_keypair, receive_chunk_header,
    receive_handshake_offer, sanitize_file_name, send_handshake_response,
    verify_proof_constant_time, HandshakeResponse, CHUNK_TIMEOUT, PROTOCOL_VERSION,
};
use crate::signaling::SignalingClient;
use crate::transfer_state::{SharedProgress, TransferRole, TransferStatus};

pub struct ReceiverConfig {
    pub code: PairingCode,
    pub output_dir: PathBuf,
    pub force: bool,
    pub signaling_server: Option<String>,
    pub shared_progress: Option<SharedProgress>,
    pub cancel_signal: Option<Arc<AtomicBool>>,
    pub discovery: Option<Arc<DiscoveryService>>,
    pub speed_limiter: Option<crate::speed_limiter::SpeedLimiter>,
}

pub async fn run_receiver(config: ReceiverConfig) -> Result<()> {
    let cancel = config
        .cancel_signal
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

    let code_formatted = config.code.formatted();

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.role = TransferRole::Receiver;
        p.status = TransferStatus::Connecting;
        p.pairing_code = Some(code_formatted.clone());
        p.status_text = format!("Buscando al emisor con el código {}...", code_formatted);
    }

    tokio::fs::create_dir_all(&config.output_dir)
        .await
        .with_context(|| {
            format!(
                "Failed to create output directory: {}",
                config.output_dir.display()
            )
        })?;

    println!(
        "{}",
        "\n================ [ HEAVYDROP P2P TRANSFER ] ================"
            .bold()
            .cyan()
    );
    println!("  Código  : {}", code_formatted.green().bold());
    println!(
        "  Destino : {}",
        config.output_dir.display().to_string().cyan()
    );
    println!("  Buscando emisor en red local y servidores de señalización...");
    println!(
        "{}",
        "============================================================\n"
            .bold()
            .cyan()
    );

    let signal_client = SignalingClient::new(config.signaling_server);
    let discovery = config
        .discovery
        .unwrap_or_else(|| Arc::new(DiscoveryService::new()));

    // Automatically resolve and connect to peer without needing IP or port
    let mut established = if config.code.invitation().is_some() {
        crate::automatic::connect_receiver(&config.code, config.shared_progress.clone()).await?
    } else {
        establish_receiver_connection(&config.code, &signal_client, &discovery, cancel.clone())
            .await?
    };

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Connected;
        p.connection_type = established.conn_type;
        p.peer_address = established.peer_description.clone();
        p.status_text = format!(
            "Conectado ({})! Verificando detalles del archivo...",
            established.conn_type.label()
        );
    }

    println!(
        "{} Conectado al emisor ({})!",
        "✓".green().bold(),
        established.conn_type.label().green()
    );

    // Receive Handshake Offer
    let mut offer = receive_handshake_offer(&mut established.stream).await?;

    // Verify Sender Authentication Proof in constant time
    let expected_sender_proof = config
        .code
        .compute_sender_proof(&offer.session_salt, &offer.sender_public_key);

    if !verify_proof_constant_time(&expected_sender_proof, &offer.sender_auth_proof) {
        if let Some(ref sp) = config.shared_progress {
            let mut p = sp.lock();
            p.status = TransferStatus::Failed;
            p.error = Some("Autenticación fallida: emisor no coincide con el código".to_string());
        }
        bail!("Autenticación rechazada: el emisor no presentó una prueba válida del código.");
    }

    // Strict Path Traversal Sanitization
    offer.file_name = sanitize_file_name(&offer.file_name)?;

    println!("\n{}", "Detalles de la transferencia:".cyan().bold());
    println!("  Archivo : {}", offer.file_name.bold());
    println!(
        "  Tamaño  : {} ({:.2} GB)",
        indicatif::HumanBytes(offer.file_size),
        offer.file_size as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    if offer.file_hash != "UNVERIFIED_PREHASH" {
        println!("  BLAKE3  : {}", offer.file_hash.green());
    } else {
        println!("  BLAKE3  : {}", "Verificación continua por bloque (ChaCha20-Poly1305 + BLAKE3)".green());
    }

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.file_name = offer.file_name.clone();
        p.total_bytes = offer.file_size;
    }

    // Generate ephemeral X25519 keypair
    let (receiver_secret, receiver_public) = generate_keypair()?;

    // Compute mutual authentication proof for receiver
    let receiver_auth_proof = config
        .code
        .compute_receiver_proof(&offer.session_salt, &receiver_public);

    // Prepare destination using .part file and verifiable metadata
    let prep = PartFileManager::prepare(&config.output_dir, &offer, config.force).await?;

    let (mut part_manager, resume_offset) = match prep {
        PrepareResult::AlreadyCompleted(_path) => {
            println!(
                "\n{}",
                "✓ El archivo ya estaba descargado y es 100% íntegro."
                    .green()
                    .bold()
            );
            let resp = HandshakeResponse {
                protocol_version: PROTOCOL_VERSION,
                receiver_public_key: receiver_public,
                accepted: false,
                resume_offset: offer.file_size,
                receiver_auth_proof,
                message: Some("El archivo ya existe y su hash coincide perfectamente.".to_string()),
            };
            send_handshake_response(&mut established.stream, &resp).await?;
            established.stream.close(true).await;
            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.status = TransferStatus::Completed;
                p.transferred_bytes = offer.file_size;
                p.status_text = "El archivo ya estaba descargado y es 100% íntegro.".to_string();
            }
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
            "🔄 Archivo .part verificado: reanudando desde"
                .yellow()
                .bold(),
            format!("{} / {:.2}%", indicatif::HumanBytes(resume_offset), percent).yellow()
        );
    }

    // Accept handshake and send response
    let handshake_resp = HandshakeResponse {
        protocol_version: PROTOCOL_VERSION,
        receiver_public_key: receiver_public,
        accepted: true,
        resume_offset,
        receiver_auth_proof,
        message: None,
    };
    send_handshake_response(&mut established.stream, &handshake_resp).await?;

    // Derive symmetric ChaCha20-Poly1305 session cipher
    let cipher = derive_session_cipher(
        &receiver_secret,
        &offer.sender_public_key,
        &config.code.derive_auth_key(),
        &offer.session_salt,
    );

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Transferring;
        p.resume_offset = resume_offset;
        p.transferred_bytes = resume_offset;
        p.status_text = "Recibiendo y verificando bloques cifrados...".to_string();
    }

    let pb = if config.shared_progress.is_none() {
        let bar = ProgressBar::new(offer.file_size);
        bar.set_position(resume_offset);
        bar.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {msg}")
                .unwrap()
                .progress_chars("#>-"),
        );
        Some(bar)
    } else {
        None
    };

    let mut current_offset = resume_offset;
    let mut chunk_index = current_offset / (offer.chunk_size as u64);
    let start_time = Instant::now();
    let mut last_speed_update = Instant::now();
    let mut bytes_since_last_update = 0u64;

    while current_offset < offer.file_size {
        if cancel.load(Ordering::Relaxed) {
            bail!("Transferencia cancelada por el usuario");
        }

        let chunk_header = receive_chunk_header(&mut established.stream).await?;

        if chunk_header.is_end_of_stream() {
            if current_offset < offer.file_size {
                bail!(
                    "El emisor cerró el stream prematuramente en byte {}/{}",
                    current_offset,
                    offer.file_size
                );
            }
            break;
        }

        if chunk_header.offset != current_offset {
            bail!(
                "Error de sincronización: offset recibido {} != esperado {}",
                chunk_header.offset,
                current_offset
            );
        }

        let expected_length =
            (offer.file_size - current_offset).min(offer.chunk_size as u64) as usize;
        if chunk_header.ciphertext_length as usize != expected_length + 16 {
            bail!("Tamaño de bloque cifrado inválido");
        }
        let mut ciphertext = vec![0u8; chunk_header.ciphertext_length as usize];
        tokio::time::timeout(
            CHUNK_TIMEOUT,
            established.stream.read_exact(&mut ciphertext),
        )
        .await
        .context("Timeout de lectura del bloque cifrado")?
        .context("Fallo de red al leer bloque cifrado")?;

        let plaintext = decrypt_chunk(&cipher, &offer.session_salt, chunk_index, &ciphertext)?;

        let check = blake3::hash(&plaintext);
        if check.as_bytes() != &chunk_header.plaintext_checksum {
            bail!(
                "Verificación de integridad de bloque fallida en byte {}",
                current_offset
            );
        }

        part_manager
            .write_verified_chunk(current_offset, &plaintext, *check.as_bytes())
            .await?;

        let bytes_len = plaintext.len() as u64;

        if let Some(ref limiter) = config.speed_limiter {
            limiter.consume(bytes_len as usize).await;
        }

        current_offset += bytes_len;
        chunk_index += 1;
        bytes_since_last_update += bytes_len;

        if let Some(ref bar) = pb {
            bar.inc(bytes_len);
        }

        let now = Instant::now();
        if now.duration_since(last_speed_update) >= std::time::Duration::from_millis(250) {
            let elapsed_sec = now.duration_since(last_speed_update).as_secs_f64();
            let bps = if elapsed_sec > 0.0 {
                bytes_since_last_update as f64 / elapsed_sec
            } else {
                0.0
            };

            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.transferred_bytes = current_offset;
                p.bytes_per_sec = bps;
            }

            last_speed_update = now;
            bytes_since_last_update = 0;
        }
    }

    if let Some(ref bar) = pb {
        bar.finish_with_message("Descarga finalizada. Verificando integridad global...");
    }

    let end = receive_chunk_header(&mut established.stream).await?;
    if !end.is_end_of_stream() {
        bail!("Falta el final de la transferencia");
    }
    part_manager.file.flush().await?;

    if offer.file_hash != "UNVERIFIED_PREHASH" {
        let final_hash = compute_file_blake3_async(&part_manager.part_path).await?;
        if !final_hash.eq_ignore_ascii_case(&offer.file_hash) {
            bail!("Integridad global fallida: el hash final no coincide");
        }
    }
    let final_path = part_manager.finalize().await?;
    established
        .stream
        .write_all(crate::protocol::COMPLETION_ACK)
        .await?;
    established.stream.flush().await?;
    established.stream.close(true).await;

    let elapsed = start_time.elapsed().as_secs_f64();
    let downloaded_bytes = offer.file_size.saturating_sub(resume_offset);
    let avg_speed_mb = if elapsed > 0.0 {
        (downloaded_bytes as f64 / (1024.0 * 1024.0)) / elapsed
    } else {
        0.0
    };

    println!(
        "\n{} Archivo guardado en: {}",
        "SUCCESS:".green().bold(),
        final_path.display().to_string().cyan()
    );
    println!("  Velocidad promedio: {:.2} MB/s", avg_speed_mb);

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Completed;
        p.transferred_bytes = offer.file_size;
        p.status_text = format!("¡Archivo recibido con éxito a {:.2} MB/s!", avg_speed_mb);
        p.bytes_per_sec = 0.0;
    }

    Ok(())
}
