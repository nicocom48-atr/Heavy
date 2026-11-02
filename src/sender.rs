use anyhow::{bail, Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::automatic::{SenderServer, TransportMode};
use crate::connectivity::{establish_sender_connection, EstablishedConnection};
use crate::integrity::compute_file_blake3_async;
use crate::protocol::{
    derive_session_cipher, encrypt_chunk, generate_keypair, receive_stream_range_request,
    send_end_of_stream, send_handshake_offer, send_prepared_chunk, verify_proof_constant_time,
    HandshakeOffer, DEFAULT_PARALLEL_STREAMS, FINALIZATION_TIMEOUT, PROTOCOL_VERSION,
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

enum TransferOutcome {
    Completed,
    Rejected(String),
}

pub async fn run_sender(config: SenderConfig) -> Result<()> {
    if !(crate::protocol::MIN_CHUNK_SIZE..=crate::protocol::MAX_CHUNK_SIZE)
        .contains(&config.chunk_size)
    {
        bail!("Tamaño de bloque fuera de los límites permitidos");
    }
    let cancel = config
        .cancel_signal
        .clone()
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
        p.status_text = if config.skip_prehash {
            "Abriendo servidor temporal...".into()
        } else {
            "Verificando archivo para permitir reanudación segura...".into()
        };
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

    // Blake3 prehash for verifiable resume
    let file_hash = if config.skip_prehash {
        "UNVERIFIED_PREHASH".into()
    } else {
        compute_file_blake3_async(&config.file_path).await?
    };

    if config.transport_mode == TransportMode::LegacyTcp {
        config.session.start_waiting();
        if let Some(ref shared) = config.shared_progress {
            let mut p = shared.lock();
            p.status = TransferStatus::WaitingForPeer;
            p.pairing_code = Some(code_formatted.clone());
        }

        let signal_client = SignalingClient::new(config.signaling_server.clone());
        println!("  Código LAN/manual: {code_formatted}");
        let mut established = establish_sender_connection(
            &config.session,
            config.preferred_port,
            &signal_client,
            cancel.clone(),
        )
        .await?;

        let outcome = run_sender_session(
            &config,
            &file_hash,
            file_size,
            &file_name,
            &mut established,
            cancel.clone(),
        )
        .await?;

        match outcome {
            TransferOutcome::Completed => {
                config.session.consume()?;
                Ok(())
            }
            TransferOutcome::Rejected(msg) => {
                if let Some(ref sp) = config.shared_progress {
                    let mut p = sp.lock();
                    p.status = TransferStatus::Completed;
                    p.status_text = msg.clone();
                }
                println!("ℹ {}", msg.yellow());
                Ok(())
            }
        }
    } else {
        // Persistent QUIC server that survives receptor reconnections
        let server = SenderServer::bind(
            &config.session,
            config.transport_mode.clone(),
            config.shared_progress.clone(),
        )
        .await?;

        loop {
            if cancel.load(Ordering::Relaxed) {
                bail!("Transferencia cancelada por el usuario");
            }
            if config.session.is_expired() {
                bail!("La invitación expiró");
            }

            let mut established = match server.accept_connection(&config.session).await {
                Ok(conn) => conn,
                Err(e) => {
                    if cancel.load(Ordering::Relaxed) {
                        bail!("Transferencia cancelada por el usuario");
                    }
                    return Err(e);
                }
            };

            let outcome = run_sender_session(
                &config,
                &file_hash,
                file_size,
                &file_name,
                &mut established,
                cancel.clone(),
            )
            .await;

            match outcome {
                Ok(TransferOutcome::Completed) => {
                    config.session.consume()?;
                    return Ok(());
                }
                Ok(TransferOutcome::Rejected(msg)) => {
                    if let Some(ref sp) = config.shared_progress {
                        let mut p = sp.lock();
                        p.status = TransferStatus::Completed;
                        p.status_text = msg.clone();
                    }
                    println!("ℹ {}", msg.yellow());
                    return Ok(());
                }
                Err(err) => {
                    if cancel.load(Ordering::Relaxed) {
                        bail!("Transferencia cancelada por el usuario");
                    }
                    tracing::warn!(
                        "Error durante transferencia con receptor: {:#}. Esperando reconexión...",
                        err
                    );
                    if let Some(ref sp) = config.shared_progress {
                        let mut p = sp.lock();
                        p.status = TransferStatus::WaitingForPeer;
                        p.status_text =
                            "Conexión perdida con el receptor. Esperando reconexión...".into();
                    }
                    println!(
                        "{}",
                        "⚠️ Conexión perdida con el receptor. Esperando reconexión...".yellow()
                    );
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_stream_sender<W: AsyncWriteExt + Unpin>(
    mut writer: W,
    _stream_id: u32,
    file_path: PathBuf,
    cipher: Arc<chacha20poly1305::ChaCha20Poly1305>,
    salt: [u8; 32],
    chunk_size: u64,
    file_size: u64,
    start_offset: u64,
    end_offset: u64,
    total_transferred: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    speed_limiter: Option<crate::speed_limiter::SpeedLimiter>,
) -> Result<()> {
    if start_offset >= end_offset {
        send_end_of_stream(&mut writer).await?;
        let _ = writer.flush().await;
        let _ = writer.shutdown().await;
        return Ok(());
    }

    let mut file = File::open(&file_path).await?;
    file.seek(SeekFrom::Start(start_offset)).await?;

    let mut current_offset = start_offset;
    let mut buf = vec![0u8; chunk_size as usize];

    while current_offset < end_offset {
        if cancel.load(Ordering::Relaxed) {
            bail!("Transferencia cancelada por el usuario");
        }

        let to_read = std::cmp::min(chunk_size, file_size - current_offset) as usize;
        let chunk_end = current_offset + to_read as u64;
        let actual_to_read = if chunk_end > end_offset {
            (end_offset - current_offset) as usize
        } else {
            to_read
        };

        file.read_exact(&mut buf[..actual_to_read]).await?;

        let chunk_index = current_offset / chunk_size;
        let ciphertext = encrypt_chunk(&cipher, &salt, chunk_index, &buf[..actual_to_read])?;
        let checksum = *blake3::hash(&buf[..actual_to_read]).as_bytes();

        if let Some(ref limiter) = speed_limiter {
            limiter.consume(actual_to_read).await;
        }

        send_prepared_chunk(&mut writer, current_offset, &checksum, &ciphertext).await?;

        total_transferred.fetch_add(actual_to_read as u64, Ordering::Relaxed);
        current_offset += actual_to_read as u64;
    }

    send_end_of_stream(&mut writer).await?;
    let _ = writer.flush().await;
    let _ = writer.shutdown().await;
    Ok(())
}

async fn run_sender_session(
    config: &SenderConfig,
    file_hash: &str,
    file_size: u64,
    file_name: &str,
    established: &mut EstablishedConnection,
    cancel: Arc<AtomicBool>,
) -> Result<TransferOutcome> {
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

    let can_multi_stream = established.quic_connection.is_some();
    let offer_parallel_streams = if can_multi_stream {
        DEFAULT_PARALLEL_STREAMS
    } else {
        1
    };

    let mut offer = HandshakeOffer {
        protocol_version: PROTOCOL_VERSION,
        session_id: config.session.id().to_string(),
        sender_public_key: sender_public,
        session_salt: *config.session.salt(),
        file_name: file_name.to_string(),
        file_size,
        chunk_size: config.chunk_size,
        file_hash: file_hash.to_string(),
        sender_auth_proof: [0; 32],
        parallel_streams: offer_parallel_streams,
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

    if !response.accepted {
        let msg = response
            .message
            .unwrap_or_else(|| "El receptor rechazó la transferencia".to_string());
        established.stream.close(false).await;
        return Ok(TransferOutcome::Rejected(msg));
    }

    // Derive symmetric ChaCha20-Poly1305 session cipher
    let cipher = Arc::new(derive_session_cipher(
        &sender_secret,
        &response.receiver_public_key,
        &auth_key,
        config.session.salt(),
    )?);

    let resume_offset = response.resume_offset;
    if resume_offset > file_size
        || (resume_offset != file_size && resume_offset % config.chunk_size as u64 != 0)
    {
        bail!("Offset de reanudación inválido");
    }

    let chunk_size = config.chunk_size as u64;
    let total_chunks = file_size.div_ceil(chunk_size);
    let start_chunk = resume_offset / chunk_size;
    let remaining_chunks = total_chunks.saturating_sub(start_chunk);

    let is_quic = established.quic_connection.is_some();
    let num_streams = if is_quic && remaining_chunks >= DEFAULT_PARALLEL_STREAMS as u64 {
        response.parallel_streams.min(offer_parallel_streams).max(1)
    } else {
        1
    };

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Transferring;
        p.resume_offset = resume_offset;
        p.transferred_bytes = resume_offset;
        p.status_text = if num_streams > 1 {
            format!("Transfiriendo archivo en {} streams paralelos...", num_streams)
        } else {
            "Transfiriendo archivo cifrado...".to_string()
        };
    }

    if resume_offset > 0 {
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

    let total_transferred = Arc::new(AtomicU64::new(resume_offset));
    let start_time = Instant::now();

    // Spawn progress monitoring task
    let progress_cancel = cancel.clone();
    let progress_total = total_transferred.clone();
    let shared_progress_clone = config.shared_progress.clone();
    let pb_clone = pb.clone();

    let progress_task = tokio::spawn(async move {
        let mut last_instant = Instant::now();
        let mut last_bytes = progress_total.load(Ordering::Relaxed);

        while !progress_cancel.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let current = progress_total.load(Ordering::Relaxed);
            let delta = current.saturating_sub(last_bytes);
            let now = Instant::now();
            let elapsed_sec = now.duration_since(last_instant).as_secs_f64();
            let bps = if elapsed_sec > 0.0 {
                delta as f64 / elapsed_sec
            } else {
                0.0
            };

            if let Some(ref bar) = pb_clone {
                bar.set_position(current);
            }

            if let Some(ref sp) = shared_progress_clone {
                let mut p = sp.lock();
                p.transferred_bytes = current;
                p.bytes_per_sec = bps;
            }

            last_instant = now;
            last_bytes = current;
        }
    });

    if num_streams > 1 {
        // Multi-stream parallel QUIC transfer
        let quic_conn = established
            .quic_connection
            .as_ref()
            .context("Falta conexión QUIC para multi-stream")?
            .clone();

        // Stream 0 reads its StreamRangeRequest from established.stream
        let req_0 = receive_stream_range_request(&mut established.stream).await?;
        tracing::info!(stream_id = 0, start = req_0.start_offset, end = req_0.end_offset, "Stream 0 asignado");

        let mut join_set = tokio::task::JoinSet::new();

        // Accept auxiliary worker streams (1..num_streams)
        for _ in 1..num_streams {
            let (send_stream, mut recv_stream) = quic_conn.accept_bi().await?;
            let req = receive_stream_range_request(&mut recv_stream).await?;
            tracing::info!(stream_id = req.stream_index, start = req.start_offset, end = req.end_offset, "Worker stream asignado");

            let file_path = config.file_path.clone();
            let cipher_worker = cipher.clone();
            let salt = *config.session.salt();
            let chunk_size = config.chunk_size as u64;
            let trans_worker = total_transferred.clone();
            let cancel_worker = cancel.clone();
            let limiter_worker = config.speed_limiter.clone();

            join_set.spawn(async move {
                run_stream_sender(
                    send_stream,
                    req.stream_index,
                    file_path,
                    cipher_worker,
                    salt,
                    chunk_size,
                    file_size,
                    req.start_offset,
                    req.end_offset,
                    trans_worker,
                    cancel_worker,
                    limiter_worker,
                )
                .await
            });
        }

        let stream_0_fut = run_stream_sender(
            &mut established.stream,
            0,
            config.file_path.clone(),
            cipher.clone(),
            *config.session.salt(),
            config.chunk_size as u64,
            file_size,
            req_0.start_offset,
            req_0.end_offset,
            total_transferred.clone(),
            cancel.clone(),
            config.speed_limiter.clone(),
        );

        let workers_fut = async {
            while let Some(res) = join_set.join_next().await {
                match res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        join_set.shutdown().await;
                        return Err(e);
                    }
                    Err(e) => {
                        join_set.shutdown().await;
                        bail!("Tarea de envío falló: {:?}", e);
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        };

        let (res_0, res_workers) = tokio::join!(stream_0_fut, workers_fut);
        if let Err(e) = res_0 {
            join_set.shutdown().await;
            progress_task.abort();
            return Err(e);
        }
        if let Err(e) = res_workers {
            progress_task.abort();
            return Err(e);
        }
    } else {
        // Single stream transfer
        let mut file = File::open(&config.file_path).await?;
        if resume_offset > 0 {
            file.seek(SeekFrom::Start(resume_offset)).await?;
        }

        let mut current_offset = resume_offset;
        let mut idx = current_offset / (config.chunk_size as u64);
        let mut buf = vec![0u8; config.chunk_size as usize];

        while current_offset < file_size {
            if cancel.load(Ordering::Relaxed) {
                progress_task.abort();
                bail!("Transferencia cancelada por el usuario");
            }

            let to_read =
                std::cmp::min(config.chunk_size as u64, file_size - current_offset) as usize;
            file.read_exact(&mut buf[..to_read]).await?;

            let ciphertext = encrypt_chunk(&cipher, config.session.salt(), idx, &buf[..to_read])?;
            let checksum = *blake3::hash(&buf[..to_read]).as_bytes();

            if let Some(ref limiter) = config.speed_limiter {
                limiter.consume(to_read).await;
            }

            send_prepared_chunk(
                &mut established.stream,
                current_offset,
                &checksum,
                &ciphertext,
            )
            .await?;

            total_transferred.fetch_add(to_read as u64, Ordering::Relaxed);
            current_offset += to_read as u64;
            idx += 1;
        }

        send_end_of_stream(&mut established.stream).await?;
    }

    progress_task.abort();

    // Await completion acknowledgment and integrity proof on stream 0
    let mut ack = [0; 8];
    tokio::time::timeout(
        FINALIZATION_TIMEOUT,
        established.stream.read_exact(&mut ack),
    )
    .await
    .context("El receptor no confirmó la verificación del archivo a tiempo")??;

    if &ack != crate::protocol::COMPLETION_ACK {
        bail!("El receptor no confirmó la recepción íntegra del archivo");
    }

    let mut proof = [0; 32];
    tokio::time::timeout(
        crate::protocol::CHUNK_TIMEOUT,
        established.stream.read_exact(&mut proof),
    )
    .await??;

    if !verify_proof_constant_time(
        &crate::protocol::completion_proof(&offer, &response, &auth_key)?,
        &proof,
    ) {
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

    Ok(TransferOutcome::Completed)
}
