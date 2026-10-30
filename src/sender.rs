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
    derive_session_cipher, encrypt_chunk, generate_keypair, send_encrypted_chunk,
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

    let code_formatted = config.session.code().formatted();

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.role = TransferRole::Sender;
        p.status = TransferStatus::Preparing;
        p.file_name = file_name.clone();
        p.total_bytes = file_size;
        p.pairing_code = None;
        p.code_expires_in = Some(config.session.format_remaining());
        p.status_text = "Calculando comprobación de integridad BLAKE3...".to_string();
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
    println!("  Estado  : Preparando el archivo antes de abrir el servidor temporal...");
    println!(
        "{}",
        "============================================================\n"
            .bold()
            .cyan()
    );

    // Calculate full file hash
    let file_hash = if config.skip_prehash {
        "UNVERIFIED_PREHASH".to_string()
    } else {
        compute_file_blake3_async(&config.file_path)
            .await
            .context("Failed to compute file BLAKE3 checksum")?
    };

    if config.transport_mode == TransportMode::LegacyTcp {
        config.session.start_waiting();
    }
    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status_text = "Abriendo servidor temporal...".into();
        if config.transport_mode == TransportMode::LegacyTcp {
            p.status = TransferStatus::WaitingForPeer;
            p.pairing_code = Some(code_formatted.clone());
        }
    }

    let signal_client = SignalingClient::new(config.signaling_server);

    // Establish direct or relay connection
    let mut established = if config.transport_mode == TransportMode::LegacyTcp {
        println!("  Código LAN/manual: {code_formatted}");
        establish_sender_connection(
            &config.session,
            config.preferred_port,
            &signal_client,
            cancel.clone(),
        )
        .await?
    } else {
        crate::automatic::listen_sender(
            &config.session,
            config.transport_mode,
            config.shared_progress.clone(),
        )
        .await?
    };

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Connected;
        p.connection_type = established.conn_type;
        p.peer_address = established.peer_description.clone();
        p.status_text = format!(
            "Receptor conectado ({})! Iniciando autenticación mutua...",
            established.conn_type.label()
        );
    }

    println!(
        "✓ Receptor conectado ({})! Realizando autenticación criptográfica...",
        established.conn_type.label().green()
    );

    // Generate ephemeral X25519 keypair
    let (sender_secret, sender_public) = generate_keypair()?;

    // Compute mutual authentication proof
    let sender_auth_proof = config
        .session
        .code()
        .compute_sender_proof(config.session.salt(), &sender_public);

    let offer = HandshakeOffer {
        protocol_version: PROTOCOL_VERSION,
        session_id: config.session.id().to_string(),
        sender_public_key: sender_public,
        session_salt: *config.session.salt(),
        file_name: file_name.clone(),
        file_size,
        chunk_size: config.chunk_size,
        file_hash,
        sender_auth_proof,
    };

    // Send Handshake Offer to receiver
    send_handshake_offer(&mut established.stream, &offer).await?;

    // Receive Handshake Response from receiver
    let response = crate::protocol::receive_handshake_response(&mut established.stream).await?;

    // Verify Receiver Authentication Proof in constant time
    let expected_receiver_proof = config
        .session
        .code()
        .compute_receiver_proof(config.session.salt(), &response.receiver_public_key);

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
    );

    let mut resume_offset = response.resume_offset;
    if resume_offset > file_size {
        resume_offset = 0;
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

    let mut chunk_buf = vec![0u8; config.chunk_size as usize];
    let mut current_offset = resume_offset;
    let mut chunk_index = current_offset / (config.chunk_size as u64);
    let start_time = Instant::now();
    let mut last_speed_update = Instant::now();
    let mut bytes_since_last_update = 0u64;

    while current_offset < file_size {
        if cancel.load(Ordering::Relaxed) {
            bail!("Transferencia cancelada por el usuario");
        }

        let to_read = std::cmp::min(config.chunk_size as u64, file_size - current_offset) as usize;
        file.read_exact(&mut chunk_buf[..to_read]).await?;

        let ciphertext = encrypt_chunk(
            &cipher,
            config.session.salt(),
            chunk_index,
            &chunk_buf[..to_read],
        )?;

        send_encrypted_chunk(
            &mut established.stream,
            current_offset,
            &chunk_buf[..to_read],
            &ciphertext,
        )
        .await?;

        current_offset += to_read as u64;
        chunk_index += 1;
        bytes_since_last_update += to_read as u64;

        if let Some(ref bar) = pb {
            bar.inc(to_read as u64);
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
        crate::protocol::CHUNK_TIMEOUT,
        established.stream.read_exact(&mut ack),
    )
    .await
    .context("El receptor no confirmó la verificación del archivo a tiempo")??;
    if &ack != crate::protocol::COMPLETION_ACK {
        bail!("El receptor no confirmó la recepción íntegra del archivo");
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
