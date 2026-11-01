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

use crate::automatic::TransportMode;
use crate::connectivity::establish_sender_connection;
use crate::integrity::compute_file_blake3_async;
use crate::protocol::{
    derive_session_cipher, encrypt_chunk, generate_keypair,
    send_end_of_stream, send_handshake_offer, verify_proof_constant_time, HandshakeOffer,
    PROTOCOL_VERSION,
};
use crate::session::Session;
use crate::signaling::SignalingClient;
use crate::transfer_state::{SharedProgress, TransferRole, TransferStatus};

pub struct SenderConfig {
    pub transport_mode: TransportMode,
    pub file_path: PathBuf,
    pub session: Session,
    pub preferred_port: u16,
    pub chunk_size: u32,
    pub skip_prehash: bool,
    pub signaling_server: Option<String>,
    pub shared_progress: Option<SharedProgress>,
    pub cancel_signal: Option<Arc<AtomicBool>>,
    pub speed_limiter: Option<crate::speed_limiter::SpeedLimiter>,
}

pub async fn run_sender(config: SenderConfig) -> Result<()> {
    if !(crate::protocol::MIN_CHUNK_SIZE..=crate::protocol::MAX_CHUNK_SIZE).contains(&config.chunk_size) {
        bail!("Tamaño de bloque fuera de los límites permitidos");
    }
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

    let code_formatted = config.session.code().formatted();

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.role = TransferRole::Sender;
        p.file_name = file_name.clone();
        p.total_bytes = file_size;
        p.code_expires_in = Some(config.session.format_remaining());
            p.status = TransferStatus::Preparing;
            p.pairing_code = None;
            p.status_text = if config.skip_prehash { "Abriendo servidor temporal..." } else { "Verificando archivo para permitir reanudación segura..." }.into();
    }

    println!(
        "{}",
        "\n================ [ HEAVYDROP P2P TRANSFER ] ================"
            .bold()
            .cyan()
    );
    println!("  Archivo : {}", file_name.bold());
    println!(
        "  Tamaño  : {} ({:.2} GB)",
        indicatif::HumanBytes(file_size),
        file_size as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    if config.transport_mode == TransportMode::LegacyTcp {
        println!("  Código  : {}", code_formatted.green().bold());
        println!(
            "  Estado  : Esperando que el receptor ingrese el código ({})",
            config.session.format_remaining().yellow()
        );
    } else {
        println!("  Estado  : Abriendo servidor temporal...");
    }
    println!(
        "{}",
        "============================================================\n"
            .bold()
            .cyan()
    );

    // Do not accept a peer while hashing: terabyte files can exceed handshake timeouts.
    // The invitation lifetime starts after this cancellable, bounded-buffer pass.
    let file_hash = if config.skip_prehash { "UNVERIFIED_PREHASH".into() }
        else { compute_file_blake3_async(&config.file_path).await? };

    if config.transport_mode == TransportMode::LegacyTcp {
        config.session.start_waiting();
        if let Some(ref shared) = config.shared_progress {
            let mut p = shared.lock();
            p.status = TransferStatus::WaitingForPeer;
            p.pairing_code = Some(code_formatted.clone());
        }
    }

    let signal_client = SignalingClient::new(config.signaling_server);

    // Establish direct or relay connection
    let mut established = if config.transport_mode == TransportMode::LegacyTcp {
        println!("  Código LAN/manual: {code_formatted}");
        match establish_sender_connection(
            &config.session,
            config.preferred_port,
            &signal_client,
            cancel.clone(),
        )
        .await
        {
            Ok(est) => est,
            Err(e) => {
                return Err(e);
            }
        }
    } else {
        match crate::automatic::listen_sender(
            &config.session,
            config.transport_mode,
            config.shared_progress.clone(),
        )
        .await
        {
            Ok(est) => est,
            Err(e) => {
                return Err(e);
            }
        }
    };

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Connected;
        p.connection_type = established.conn_type;
        p.peer_address = established.peer_description.clone();
        p.status_text = format!(
            "Receptor conectado ({})! Verificando archivo...",
            established.conn_type.label()
        );
    }

    println!(
        "✓ Receptor conectado ({})! Realizando autenticación criptográfica...",
        established.conn_type.label().green()
    );

    // Generate ephemeral X25519 keypair
    let (sender_secret, sender_public) = generate_keypair()?;

    let auth_key = config.session.code().derive_auth_key();
    let mut offer = HandshakeOffer {
        protocol_version: PROTOCOL_VERSION,
        session_id: config.session.id().to_string(),
        sender_public_key: sender_public,
        session_salt: *config.session.salt(),
        file_name: file_name.clone(),
        file_size,
        chunk_size: config.chunk_size,
        file_hash,
        sender_auth_proof: [0; 32],
    };
    offer.sender_auth_proof = crate::protocol::offer_proof(&offer, &auth_key)?;

    // Send Handshake Offer to receiver
    send_handshake_offer(&mut established.stream, &offer).await?;

    // Receive Handshake Response from receiver
    let response = crate::protocol::receive_handshake_response(&mut established.stream).await?;

    // Verify Receiver Authentication Proof in constant time
    let expected_receiver_proof = crate::protocol::response_proof(&offer, &response, &auth_key)?;

    if !verify_proof_constant_time(&expected_receiver_proof, &response.receiver_auth_proof) {
        if let Some(ref sp) = config.shared_progress {
            let mut p = sp.lock();
            p.status = TransferStatus::Failed;
            p.error = Some("Autenticación fallida: código incorrecto del receptor".to_string());
        }
        bail!("Autenticación rechazada: el receptor no presentó la prueba válida del código");
    }

    // Invalidate/consume code so it cannot be reused
    config.session.consume()?;

    if !response.accepted {
        let msg = response
            .message
            .unwrap_or_else(|| "El receptor rechazó la transferencia".to_string());
        if let Some(ref sp) = config.shared_progress {
            let mut p = sp.lock();
            p.status = TransferStatus::Completed;
            p.status_text = msg.clone();
        }
        println!("ℹ {}", msg.yellow());
        established.stream.close(false).await;
        return Ok(());
    }

    // Derive symmetric ChaCha20-Poly1305 session cipher
    let cipher = derive_session_cipher(
        &sender_secret,
        &response.receiver_public_key,
        &config.session.code().derive_auth_key(),
        config.session.salt(),
    )?;

    let resume_offset = response.resume_offset;
    if resume_offset > file_size || (resume_offset != file_size && resume_offset % config.chunk_size as u64 != 0) {
        bail!("Offset de reanudación inválido");
    }

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Transferring;
        p.resume_offset = resume_offset;
        p.transferred_bytes = resume_offset;
        p.status_text = "Transfiriendo archivo cifrado...".to_string();
    }

    let mut file = File::open(&config.file_path).await?;
    if resume_offset > 0 {
        file.seek(SeekFrom::Start(resume_offset)).await?;
        println!(
            "🔄 Reanudando transferencia desde byte {}",
            indicatif::HumanBytes(resume_offset)
        );
    }

    let remaining_bytes = file_size.saturating_sub(resume_offset);
    let pb = if config.shared_progress.is_none() {
        let bar = ProgressBar::new(file_size);
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

    struct PreparedChunk {
        offset: u64,
        bytes_len: usize,
        checksum: [u8; 32],
        ciphertext: Vec<u8>,
    }

    let mut current_offset = resume_offset;
    let start_time = Instant::now();
    let mut last_speed_update = Instant::now();
    let mut bytes_since_last_update = 0u64;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<PreparedChunk>>(2);
    let read_cancel = cancel.clone();
    let cipher_clone = cipher.clone();
    let salt = *config.session.salt();
    let chunk_size = config.chunk_size as u64;

    let reader_handle = crate::task_guard::AbortOnDrop::new(tokio::spawn(async move {
        let mut offset = resume_offset;
        let mut idx = offset / chunk_size;
        let mut buf = vec![0u8; chunk_size as usize];

        while offset < file_size {
            if read_cancel.load(Ordering::Relaxed) {
                break;
            }
            let to_read = std::cmp::min(chunk_size, file_size - offset) as usize;
            if let Err(e) = file.read_exact(&mut buf[..to_read]).await {
                let _ = tx.send(Err(e.into())).await;
                return;
            }
            let ciphertext = match encrypt_chunk(&cipher_clone, &salt, idx, &buf[..to_read]) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };
            let chunk = PreparedChunk {
                offset,
                bytes_len: to_read,
                checksum: *blake3::hash(&buf[..to_read]).as_bytes(),
                ciphertext,
            };
            if tx.send(Ok(chunk)).await.is_err() {
                break;
            }
            offset += to_read as u64;
            idx += 1;
        }
    }));

    while let Some(chunk_res) = rx.recv().await {
        if cancel.load(Ordering::Relaxed) {
            reader_handle.abort();
            bail!("Transferencia cancelada por el usuario");
        }

        let chunk = chunk_res?;
        let bytes_len = chunk.bytes_len;

        if let Some(ref limiter) = config.speed_limiter {
            limiter.consume(bytes_len).await;
        }

        crate::protocol::send_prepared_chunk(
            &mut established.stream,
            chunk.offset,
            &chunk.checksum,
            &chunk.ciphertext,
        )
        .await?;

        current_offset += bytes_len as u64;
        bytes_since_last_update += bytes_len as u64;

        if let Some(ref bar) = pb {
            bar.inc(bytes_len as u64);
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

    send_end_of_stream(&mut established.stream).await?;
    let mut ack = [0; 8];
    tokio::time::timeout(
        crate::protocol::FINALIZATION_TIMEOUT,
        established.stream.read_exact(&mut ack),
    )
    .await
    .context("El receptor no confirmó la verificación del archivo a tiempo")??;
    if &ack != crate::protocol::COMPLETION_ACK {
        bail!("El receptor no confirmó la recepción íntegra del archivo");
    }
    let mut proof = [0; 32];
    tokio::time::timeout(crate::protocol::CHUNK_TIMEOUT, established.stream.read_exact(&mut proof)).await??;
    if !verify_proof_constant_time(&crate::protocol::completion_proof(&offer, &response, &auth_key)?, &proof) {
        bail!("Confirmación final de integridad no autenticada");
    }
    established.stream.close(false).await;

    if let Some(ref bar) = pb {
        bar.finish_with_message("Transferencia completada con éxito!");
    }

    let elapsed = start_time.elapsed().as_secs_f64();
    let avg_speed_mb = if elapsed > 0.0 {
        (remaining_bytes as f64 / (1024.0 * 1024.0)) / elapsed
    } else {
        0.0
    };

    println!(
        "\n{} Archivo enviado exitosamente ({:.2} MB/s)",
        "SUCCESS:".green().bold(),
        avg_speed_mb
    );

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Completed;
        p.transferred_bytes = file_size;
        p.status_text = format!("¡Transferencia completada a {:.2} MB/s!", avg_speed_mb);
        p.bytes_per_sec = 0.0;
    }

    Ok(())
}

