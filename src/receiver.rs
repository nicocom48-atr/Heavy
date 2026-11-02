use anyhow::{bail, Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::connectivity::establish_receiver_connection;
use crate::discovery::DiscoveryService;
use crate::integrity::compute_file_blake3_async;
use crate::pairing::PairingCode;
use crate::part_file::{PartFileManager, PrepareResult};
use crate::protocol::{
    decrypt_chunk, derive_session_cipher, generate_keypair, receive_chunk_header,
    receive_handshake_offer, sanitize_file_name, send_handshake_response,
    send_stream_range_request, verify_proof_constant_time, HandshakeResponse, StreamRangeRequest,
    CHUNK_TIMEOUT, DEFAULT_PARALLEL_STREAMS, PROTOCOL_VERSION,
};
use crate::signaling::SignalingClient;
use crate::transfer_state::{SharedProgress, TransferRole, TransferStatus};

const MAX_RECONNECT_ATTEMPTS: u32 = 10;
const CHANNEL_BUFFER_CHUNKS: usize = 8; // Max 32 MiB in memory

pub struct ReceiverConfig {
    pub direct_only: bool,
    /// Explicit consent to the compatibility transport, whose handshake metadata is visible.
    pub allow_legacy_tcp: bool,
    pub code: PairingCode,
    pub output_dir: PathBuf,
    pub force: bool,
    pub signaling_server: Option<String>,
    pub shared_progress: Option<SharedProgress>,
    pub cancel_signal: Option<Arc<AtomicBool>>,
    pub discovery: Option<Arc<DiscoveryService>>,
    pub speed_limiter: Option<crate::speed_limiter::SpeedLimiter>,
}

impl ReceiverConfig {
    pub fn validate_transport(&self) -> Result<()> {
        if self.direct_only && self.allow_legacy_tcp {
            bail!("Solo P2P directo por QUIC no admite compatibilidad TCP");
        }
        if self.code.invitation().is_none() && !self.allow_legacy_tcp {
            bail!("Usa la invitación QUIC completa (heavy1_…). Los códigos cortos requieren activar compatibilidad TCP explícitamente (--legacy-tcp).");
        }
        Ok(())
    }
}

pub fn is_recoverable_network_error(err: &anyhow::Error) -> bool {
    let err_str = format!("{:#}", err).to_lowercase();

    // Explicit non-recoverable cases
    if err_str.contains("cancelada")
        || err_str.contains("cancelado")
        || err_str.contains("autenticación")
        || err_str.contains("código incorrecto")
        || err_str.contains("la invitación expiró")
        || err_str.contains("el archivo de destino ya existe")
        || err_str.contains("enlace simbólico")
        || err_str.contains("componentes de ruta")
        || err_str.contains("integridad global fallida")
    {
        return false;
    }

    // Check for io errors
    if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
        match io_err.kind() {
            std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::NotConnected => return true,
            _ => {}
        }
    }

    // Recoverable QUIC, chunk and network keywords
    err_str.contains("connection lost")
        || err_str.contains("too many gaps in stream buffer")
        || err_str.contains("aborted by peer")
        || err_str.contains("timed out")
        || err_str.contains("timeout")
        || err_str.contains("connection reset")
        || err_str.contains("broken pipe")
        || err_str.contains("unexpected eof")
        || err_str.contains("transport error")
        || err_str.contains("closed by peer")
        || err_str.contains("fallo de red")
        || err_str.contains("no respondió")
        || err_str.contains("el servidor temporal se cerró")
        || err_str.contains("internal error")
        || err_str.contains("aead decryption failed")
        || err_str.contains("verificación de integridad de bloque fallida")
        || err_str.contains("descifrado y verificación")
        || err_str.contains("error de sincronización en stream")
        || err_str.contains("tamaño de bloque cifrado inválido")
        || err_str.contains("terminó prematuramente")
}

pub async fn run_receiver(config: ReceiverConfig) -> Result<()> {
    // Enforce policy before creating directories, discovering peers, or contacting a server.
    config.validate_transport()?;
    let cancel = config
        .cancel_signal
        .clone()
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

    let mut attempt = 1u32;
    loop {
        tracing::info!(attempt = attempt, "Iniciando sesión de transferencia receptora");
        let result = run_receiver_session(&config, cancel.clone(), attempt).await;

        match result {
            Ok(()) => {
                tracing::info!("Transferencia receptora completada exitosamente");
                return Ok(());
            }
            Err(err) => {
                if cancel.load(Ordering::Relaxed) {
                    return Err(err);
                }
                if !is_recoverable_network_error(&err) {
                    tracing::error!("Error no recuperable en recepción: {:#}", err);
                    if let Some(ref sp) = config.shared_progress {
                        let mut p = sp.lock();
                        p.status = TransferStatus::Failed;
                        p.error = Some(format!("{:#}", err));
                    }
                    return Err(err);
                }
                if attempt >= MAX_RECONNECT_ATTEMPTS {
                    tracing::error!(
                        "Se agotaron los {} intentos de reconexión tras fallo de red: {:#}",
                        MAX_RECONNECT_ATTEMPTS,
                        err
                    );
                    if let Some(ref sp) = config.shared_progress {
                        let mut p = sp.lock();
                        p.status = TransferStatus::Failed;
                        p.error = Some(format!(
                            "Se agotaron los {} intentos de reconexión: {:#}",
                            MAX_RECONNECT_ATTEMPTS, err
                        ));
                    }
                    return Err(err.context(format!(
                        "Se agotaron los {} intentos de reconexión tras fallas de red",
                        MAX_RECONNECT_ATTEMPTS
                    )));
                }

                let backoff_secs = std::cmp::min(1u64 << (attempt - 1), 15);
                let backoff = Duration::from_secs(backoff_secs);
                let reconnect_msg = format!("Reconectando… intento {}/{}", attempt + 1, MAX_RECONNECT_ATTEMPTS);

                tracing::warn!(
                    attempt = attempt,
                    error = %err,
                    backoff_secs = backoff_secs,
                    "Conexión perdida. {}",
                    reconnect_msg
                );

                if let Some(ref sp) = config.shared_progress {
                    let mut p = sp.lock();
                    p.status = TransferStatus::Connecting;
                    p.status_text = reconnect_msg.clone();
                }
                println!(
                    "{}",
                    format!(
                        "⚠️ Conexión perdida ({}) - {} en {}s...",
                        err, reconnect_msg, backoff_secs
                    )
                    .yellow()
                );

                let start_sleep = Instant::now();
                while start_sleep.elapsed() < backoff {
                    if cancel.load(Ordering::Relaxed) {
                        bail!("Transferencia cancelada por el usuario");
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }

                attempt += 1;
            }
        }
    }
}

struct IncomingChunk {
    offset: u64,
    chunk_index: u64,
    plaintext_checksum: [u8; 32],
    ciphertext: Vec<u8>,
}

#[allow(clippy::too_many_arguments)]
async fn run_stream_reader<R: AsyncReadExt + Unpin>(
    mut reader: R,
    stream_id: u32,
    chunk_size: u64,
    file_size: u64,
    expected_start: u64,
    expected_end: u64,
    tx: tokio::sync::mpsc::Sender<Result<IncomingChunk>>,
    cancel: Arc<AtomicBool>,
) {
    if expected_start < expected_end {
        let mut current_offset = expected_start;
        while current_offset < expected_end {
            if cancel.load(Ordering::Relaxed) {
                let _ = tx.send(Err(anyhow::anyhow!("Transferencia cancelada por el usuario"))).await;
                return;
            }

            let header = match receive_chunk_header(&mut reader).await {
                Ok(h) => h,
                Err(e) => {
                    let _ = tx.send(Err(e.context(format!("Fallo al leer cabecera en stream {}", stream_id)))).await;
                    return;
                }
            };

            if header.is_end_of_stream() {
                if current_offset < expected_end {
                    let _ = tx.send(Err(anyhow::anyhow!(
                        "Stream {} terminó prematuramente en byte {}/{}",
                        stream_id, current_offset, expected_end
                    ))).await;
                }
                return;
            }

            if header.offset != current_offset {
                let _ = tx.send(Err(anyhow::anyhow!(
                    "Error de sincronización en stream {}: offset recibido {} != esperado {}",
                    stream_id, header.offset, current_offset
                ))).await;
                return;
            }

            let expected_len = (file_size - current_offset).min(chunk_size) as usize;
            if header.ciphertext_length as usize != expected_len + 16 {
                let _ = tx.send(Err(anyhow::anyhow!(
                    "Tamaño de bloque cifrado inválido en stream {}: {} != {}",
                    stream_id, header.ciphertext_length, expected_len + 16
                ))).await;
                return;
            }

            let mut ciphertext = vec![0u8; header.ciphertext_length as usize];
            if let Err(e) = tokio::time::timeout(CHUNK_TIMEOUT, reader.read_exact(&mut ciphertext)).await {
                let _ = tx.send(Err(anyhow::anyhow!("Timeout de red leyendo bloque en stream {}: {}", stream_id, e))).await;
                return;
            }

            let chunk_index = current_offset / chunk_size;
            let incoming = IncomingChunk {
                offset: current_offset,
                chunk_index,
                plaintext_checksum: header.plaintext_checksum,
                ciphertext,
            };

            current_offset += expected_len as u64;

            if tx.send(Ok(incoming)).await.is_err() {
                return;
            }
        }
    }

    // Wait for end-of-stream header if sent by sender
    if let Ok(end_header) = receive_chunk_header(&mut reader).await {
        if !end_header.is_end_of_stream() {
            tracing::warn!(stream_id = stream_id, "Se esperaba end-of-stream al final del rango");
        }
    }
}

async fn run_receiver_session(
    config: &ReceiverConfig,
    cancel: Arc<AtomicBool>,
    attempt: u32,
) -> Result<()> {
    let signal_client = SignalingClient::new(if config.direct_only {
        None
    } else {
        config.signaling_server.clone()
    });
    let discovery = config
        .discovery
        .clone()
        .unwrap_or_else(|| Arc::new(DiscoveryService::new()));

    // Automatically resolve and connect to peer
    let mut established = if config.code.invitation().is_some() {
        crate::automatic::connect_receiver_with_policy(
            &config.code,
            config.shared_progress.clone(),
            config.direct_only,
        )
        .await?
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
    let offer = receive_handshake_offer(&mut established.stream).await?;

    // Verify Sender Authentication Proof in constant time
    let auth_key = config.code.derive_auth_key();
    let expected_sender_proof = crate::protocol::offer_proof(&offer, &auth_key)?;

    if !verify_proof_constant_time(&expected_sender_proof, &offer.sender_auth_proof) {
        if let Some(ref sp) = config.shared_progress {
            let mut p = sp.lock();
            p.status = TransferStatus::Failed;
            p.error = Some("Autenticación fallida: emisor no coincide con el código".to_string());
        }
        bail!("Autenticación rechazada: el emisor no presentó una prueba válida del código.");
    }

    // Strict Path Traversal Sanitization
    if sanitize_file_name(&offer.file_name)? != offer.file_name {
        bail!("El nombre recibido contiene componentes de ruta");
    }

    if attempt == 1 {
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
            println!(
                "  BLAKE3  : {}",
                "Verificación continua por bloque (ChaCha20-Poly1305 + BLAKE3)".green()
            );
        }
    }

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.file_name = offer.file_name.clone();
        p.total_bytes = offer.file_size;
    }

    // Generate ephemeral X25519 keypair
    let (receiver_secret, receiver_public) = generate_keypair()?;
    let cipher = derive_session_cipher(
        &receiver_secret,
        &offer.sender_public_key,
        &auth_key,
        &offer.session_salt,
    )?;

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
            let mut resp = HandshakeResponse {
                protocol_version: PROTOCOL_VERSION,
                receiver_public_key: receiver_public,
                accepted: false,
                resume_offset: offer.file_size,
                receiver_auth_proof: [0; 32],
                message: Some("El archivo ya existe y su hash coincide perfectamente.".to_string()),
                parallel_streams: 1,
            };
            resp.receiver_auth_proof = crate::protocol::response_proof(&offer, &resp, &auth_key)?;
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

    let chunk_size = offer.chunk_size as u64;
    let total_chunks = offer.file_size.div_ceil(chunk_size);
    let start_chunk = resume_offset / chunk_size;
    let remaining_chunks = total_chunks.saturating_sub(start_chunk);

    let is_quic = established.quic_connection.is_some();
    let num_streams = if is_quic && remaining_chunks >= DEFAULT_PARALLEL_STREAMS as u64 {
        offer.parallel_streams.clamp(1, DEFAULT_PARALLEL_STREAMS)
    } else {
        1
    };

    // Accept handshake and send response
    let mut handshake_resp = HandshakeResponse {
        protocol_version: PROTOCOL_VERSION,
        receiver_public_key: receiver_public,
        accepted: true,
        resume_offset,
        receiver_auth_proof: [0; 32],
        message: None,
        parallel_streams: num_streams,
    };
    handshake_resp.receiver_auth_proof =
        crate::protocol::response_proof(&offer, &handshake_resp, &auth_key)?;
    send_handshake_response(&mut established.stream, &handshake_resp).await?;

    if let Some(ref sp) = config.shared_progress {
        let mut p = sp.lock();
        p.status = TransferStatus::Transferring;
        p.resume_offset = resume_offset;
        p.transferred_bytes = resume_offset;
        p.status_text = if num_streams > 1 {
            format!("Recibiendo bloques cifrados en {} streams paralelos...", num_streams)
        } else {
            "Recibiendo y verificando bloques cifrados...".to_string()
        };
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

    let active_streams = num_streams;

    tracing::info!(
        streams = active_streams,
        resume_offset = resume_offset,
        remaining_chunks = remaining_chunks,
        "Iniciando pipeline desacoplado de lectura y escritura"
    );

    // Decoupled reading channel: bounded mpsc (8 items max in transit)
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<IncomingChunk>>(CHANNEL_BUFFER_CHUNKS);

    // Split stream 0 into read and write halves
    let (reader_0, mut writer_0) = tokio::io::split(established.stream);

    let mut reader_join_set = tokio::task::JoinSet::new();

    if active_streams > 1 {
        let quic_conn = established
            .quic_connection
            .as_ref()
            .context("Falta conexión QUIC para multi-stream")?
            .clone();

        // Range for Stream 0
        let s0_chunk_start = start_chunk;
        let s0_chunk_end = start_chunk + remaining_chunks / active_streams as u64;
        let s0_start_offset = s0_chunk_start * chunk_size;
        let s0_end_offset = (s0_chunk_end * chunk_size).min(offer.file_size);

        let req_0 = StreamRangeRequest {
            stream_index: 0,
            start_offset: s0_start_offset,
            end_offset: s0_end_offset,
        };

        send_stream_range_request(&mut writer_0, &req_0).await?;

        let tx_0 = tx.clone();
        let cancel_0 = cancel.clone();
        reader_join_set.spawn(async move {
            run_stream_reader(
                reader_0,
                0,
                chunk_size,
                offer.file_size,
                s0_start_offset,
                s0_end_offset,
                tx_0,
                cancel_0,
            )
            .await;
        });

        // Ranges for auxiliary worker streams (1..active_streams)
        for s in 1..active_streams {
            let s_chunk_start = start_chunk + (s as u64 * remaining_chunks) / active_streams as u64;
            let s_chunk_end =
                start_chunk + ((s as u64 + 1) * remaining_chunks) / active_streams as u64;
            let s_start_offset = s_chunk_start * chunk_size;
            let s_end_offset = (s_chunk_end * chunk_size).min(offer.file_size);

            let req = StreamRangeRequest {
                stream_index: s,
                start_offset: s_start_offset,
                end_offset: s_end_offset,
            };

            let tx_clone = tx.clone();
            let cancel_clone = cancel.clone();

            let (mut send_stream, recv_stream) = quic_conn.open_bi().await?;
            send_stream_range_request(&mut send_stream, &req).await?;
            let _ = send_stream.finish();
            reader_join_set.spawn(async move {
                run_stream_reader(
                    recv_stream,
                    s,
                    chunk_size,
                    offer.file_size,
                    s_start_offset,
                    s_end_offset,
                    tx_clone,
                    cancel_clone,
                )
                .await;
            });
        }
    } else {
        // Single stream mode
        let tx_clone = tx.clone();
        let cancel_clone = cancel.clone();
        reader_join_set.spawn(async move {
            run_stream_reader(
                reader_0,
                0,
                chunk_size,
                offer.file_size,
                resume_offset,
                offer.file_size,
                tx_clone,
                cancel_clone,
            )
            .await;
        });
    }

    // Drop original sender so channel closes when all reader tasks finish
    drop(tx);

    let cipher_arc = Arc::new(cipher);
    let salt = offer.session_salt;

    let start_time = Instant::now();
    let mut last_speed_update = Instant::now();
    let mut bytes_since_last_update = 0u64;

    // Worker loop: reads decrypted chunks, verifies BLAKE3, writes to disk
    while let Some(chunk_res) = rx.recv().await {
        if cancel.load(Ordering::Relaxed) {
            reader_join_set.shutdown().await;
            bail!("Transferencia cancelada por el usuario");
        }

        let chunk = match chunk_res {
            Ok(c) => c,
            Err(e) => {
                reader_join_set.shutdown().await;
                return Err(e);
            }
        };

        let cipher_clone = cipher_arc.clone();

        // Offload decryption and BLAKE3 computation to blocking threadpool
        let (plaintext, hash) = tokio::task::spawn_blocking(move || {
            let pt = decrypt_chunk(&cipher_clone, &salt, chunk.chunk_index, &chunk.ciphertext)?;
            let h = *blake3::hash(&pt).as_bytes();
            if h != chunk.plaintext_checksum {
                bail!(
                    "Verificación de integridad de bloque fallida en byte {}",
                    chunk.offset
                );
            }
            Ok::<_, anyhow::Error>((pt, h))
        })
        .await
        .context("Error en tarea de descifrado y verificación")??;

        part_manager
            .write_verified_chunk(chunk.offset, &plaintext, hash)
            .await?;

        let bytes_len = plaintext.len() as u64;

        if let Some(ref limiter) = config.speed_limiter {
            limiter.consume(bytes_len as usize).await;
        }

        bytes_since_last_update += bytes_len;

        if let Some(ref bar) = pb {
            bar.inc(bytes_len);
        }

        let now = Instant::now();
        if now.duration_since(last_speed_update) >= Duration::from_millis(250) {
            let elapsed_sec = now.duration_since(last_speed_update).as_secs_f64();
            let bps = if elapsed_sec > 0.0 {
                bytes_since_last_update as f64 / elapsed_sec
            } else {
                0.0
            };

            if let Some(ref sp) = config.shared_progress {
                let mut p = sp.lock();
                p.transferred_bytes = part_manager.metadata.verified_bytes;
                p.bytes_per_sec = bps;
            }

            last_speed_update = now;
            bytes_since_last_update = 0;
        }
    }

    // Await reader tasks to ensure no errors were missed
    while let Some(res) = reader_join_set.join_next().await {
        if let Err(e) = res {
            tracing::warn!("Tarea de lectura finalizada con error: {:?}", e);
        }
    }

    if part_manager.metadata.verified_bytes < offer.file_size {
        bail!(
            "Faltan bloques por recibir ({} / {} bytes verificados)",
            part_manager.metadata.verified_bytes,
            offer.file_size
        );
    }

    if let Some(ref bar) = pb {
        bar.finish_with_message("Descarga finalizada. Verificando integridad global...");
    }

    part_manager.file.flush().await?;

    if offer.file_hash != "UNVERIFIED_PREHASH" {
        if let Some(ref shared) = config.shared_progress {
            shared.lock().status_text =
                "Verificando integridad completa del archivo en disco...".into();
        }
        let final_hash = compute_file_blake3_async(&part_manager.part_path).await?;
        if !final_hash.eq_ignore_ascii_case(&offer.file_hash) {
            bail!("Integridad global fallida: el hash final no coincide");
        }
    }

    let final_path = part_manager.finalize().await?;

    // Send completion ack and authenticated proof to sender on stream 0
    writer_0.write_all(crate::protocol::COMPLETION_ACK).await?;
    let proof = crate::protocol::completion_proof(&offer, &handshake_resp, &auth_key)?;
    writer_0.write_all(&proof).await?;
    writer_0.flush().await?;
    let _ = writer_0.shutdown().await;

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
