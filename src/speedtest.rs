//! Internet Speed Test module using Cloudflare Anycast Speed Test infrastructure.
//!
//! Provides non-blocking measurements of latency (ping), download speed and upload speed,
//! with live progress reporting, cancellation support, and headless process spawning on Windows.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::io::AsyncReadExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub const CF_PING_URL: &str = "https://speed.cloudflare.com/__down?bytes=0";
pub const CF_DOWNLOAD_URL_25MB: &str = "https://speed.cloudflare.com/__down?bytes=26214400"; // 25 MiB
pub const CF_UPLOAD_URL: &str = "https://speed.cloudflare.com/__up";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpeedTestResult {
    pub ping_ms: f64,
    pub download_mbps: f64,
    pub upload_mbps: f64,
    pub download_mbs: f64,
    pub upload_mbs: f64,
    pub timestamp: SystemTime,
}

impl SpeedTestResult {
    /// Calculate 85% recommended upload limit in MB/s (clamped to at least 1 MB/s)
    pub fn recommended_upload_limit_mb(&self) -> u32 {
        let lim = (self.upload_mbs * 0.85).round();
        lim.max(1.0) as u32
    }

    /// Calculate 85% recommended download limit in MB/s (clamped to at least 1 MB/s)
    pub fn recommended_download_limit_mb(&self) -> u32 {
        let lim = (self.download_mbs * 0.85).round();
        lim.max(1.0) as u32
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpeedTestPhase {
    Idle,
    MeasuringPing,
    MeasuringDownload { progress: f32, current_mbps: f64 },
    MeasuringUpload { progress: f32, current_mbps: f64 },
    Completed(SpeedTestResult),
    Failed(String),
}

/// Helper to configure a child process without creating a console window on Windows
fn new_command(binary: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(binary);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Measure latency to Cloudflare CDN using 3 consecutive small HTTP requests
async fn measure_latency(cancel: &Arc<AtomicBool>) -> Result<f64> {
    let mut samples = Vec::with_capacity(3);

    for _ in 0..3 {
        if cancel.load(Ordering::Relaxed) {
            bail!("Prueba cancelada por el usuario");
        }

        let mut cmd = new_command("curl.exe");
        cmd.args(["-s", "-o", "NUL", "-w", "%{time_connect}", CF_PING_URL]);

        let output = tokio::time::timeout(Duration::from_secs(5), cmd.output())
            .await
            .context("Timeout midiendo latencia")?
            .context("Error ejecutando curl para latencia")?;

        if !output.status.success() {
            bail!("Error conectando a servidor de prueba");
        }

        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Ok(sec) = text.parse::<f64>() {
            samples.push(sec * 1000.0);
        }
    }

    if samples.is_empty() {
        bail!("No se pudo obtener muestra de latencia");
    }

    // Return the minimum (best) latency sample rounded to 1 decimal place
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok((samples[0] * 10.0).round() / 10.0)
}

/// Measure download speed by streaming 25 MB from Cloudflare
async fn measure_download(
    state: &Arc<parking_lot::Mutex<SpeedTestPhase>>,
    cancel: &Arc<AtomicBool>,
) -> Result<(f64, f64)> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Prueba cancelada por el usuario");
    }

    let mut cmd = new_command("curl.exe");
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());
    cmd.args([
        "-s",
        "-o",
        "NUL",
        "-w",
        "%{speed_download}|%{time_total}|%{size_download}",
        CF_DOWNLOAD_URL_25MB,
    ]);

    let start = Instant::now();
    let mut child = cmd.spawn().context("Fallo al iniciar descarga de prueba")?;

    // Monitor progress periodically while process runs
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill().await;
            bail!("Prueba cancelada por el usuario");
        }

        tokio::select! {
            res = child.wait() => break res,
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                let elapsed = start.elapsed().as_secs_f32();
                let est_progress = (elapsed / 3.0).clamp(0.05, 0.95);
                let mut guard = state.lock();
                if let SpeedTestPhase::MeasuringDownload { ref mut progress, .. } = *guard {
                    *progress = est_progress;
                }
            }
        }
    }
    .context("Error esperando descarga")?;

    if !status.success() {
        bail!("Fallo en proceso de descarga");
    }

    let mut stdout_bytes = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_end(&mut stdout_bytes).await;
    }

    let text = String::from_utf8_lossy(&stdout_bytes).trim().to_string();
    let parts: Vec<&str> = text.split('|').collect();
    if parts.len() < 3 {
        bail!("Respuesta de descarga con formato inesperado: {}", text);
    }

    let speed_bps = parts[0].parse::<f64>().context("Error parseando velocidad de bajada")?;
    let mbps = (speed_bps * 8.0) / 1_000_000.0;
    let mbs = speed_bps / (1024.0 * 1024.0);

    let round_mbps = (mbps * 100.0).round() / 100.0;
    let round_mbs = (mbs * 100.0).round() / 100.0;

    Ok((round_mbps, round_mbs))
}

/// Create a temporary file of the specified size in bytes for the upload test
async fn create_temp_upload_file(size_bytes: usize) -> Result<PathBuf> {
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join(format!("heavydrop_speedtest_up_{}.tmp", std::process::id()));
    let payload = vec![0x37u8; size_bytes];
    tokio::fs::write(&path, payload).await?;
    Ok(path)
}

/// Measure upload speed by uploading 25 MB to Cloudflare
async fn measure_upload(
    state: &Arc<parking_lot::Mutex<SpeedTestPhase>>,
    cancel: &Arc<AtomicBool>,
) -> Result<(f64, f64)> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Prueba cancelada por el usuario");
    }

    let upload_file = create_temp_upload_file(25 * 1024 * 1024).await?;

    let mut cmd = new_command("curl.exe");
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());
    let file_arg = format!("@{}", upload_file.display());
    cmd.args([
        "-s",
        "-o",
        "NUL",
        "-w",
        "%{speed_upload}|%{time_total}|%{size_upload}",
        "-X",
        "POST",
        "--data-binary",
        &file_arg,
        CF_UPLOAD_URL,
    ]);

    let start = Instant::now();
    let child_spawn = cmd.spawn();
    let mut child = match child_spawn {
        Ok(c) => c,
        Err(e) => {
            let _ = tokio::fs::remove_file(&upload_file).await;
            return Err(e).context("Fallo al iniciar subida de prueba");
        }
    };

    let status_res = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill().await;
            let _ = tokio::fs::remove_file(&upload_file).await;
            bail!("Prueba cancelada por el usuario");
        }

        tokio::select! {
            res = child.wait() => break res,
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                let elapsed = start.elapsed().as_secs_f32();
                let est_progress = (elapsed / 3.0).clamp(0.05, 0.95);
                let mut guard = state.lock();
                if let SpeedTestPhase::MeasuringUpload { ref mut progress, .. } = *guard {
                    *progress = est_progress;
                }
            }
        }
    };

    let _ = tokio::fs::remove_file(&upload_file).await;

    let status = status_res.context("Error esperando subida")?;
    if !status.success() {
        bail!("Fallo en proceso de subida");
    }

    let mut stdout_bytes = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_end(&mut stdout_bytes).await;
    }

    let text = String::from_utf8_lossy(&stdout_bytes).trim().to_string();
    let parts: Vec<&str> = text.split('|').collect();
    if parts.len() < 3 {
        bail!("Respuesta de subida con formato inesperado: {}", text);
    }

    let speed_bps = parts[0].parse::<f64>().context("Error parseando velocidad de subida")?;
    let mbps = (speed_bps * 8.0) / 1_000_000.0;
    let mbs = speed_bps / (1024.0 * 1024.0);

    let round_mbps = (mbps * 100.0).round() / 100.0;
    let round_mbs = (mbs * 100.0).round() / 100.0;

    Ok((round_mbps, round_mbs))
}

/// Execute the full speed test lifecycle updating the shared state
pub async fn run_speed_test(
    state: Arc<parking_lot::Mutex<SpeedTestPhase>>,
    cancel: Arc<AtomicBool>,
) {
    // 1. Measuring Ping
    {
        let mut guard = state.lock();
        *guard = SpeedTestPhase::MeasuringPing;
    }

    let ping_ms = match measure_latency(&cancel).await {
        Ok(ms) => ms,
        Err(e) => {
            let mut guard = state.lock();
            *guard = SpeedTestPhase::Failed(format!("Error midiendo latencia: {:#}", e));
            return;
        }
    };

    // 2. Measuring Download
    {
        let mut guard = state.lock();
        *guard = SpeedTestPhase::MeasuringDownload {
            progress: 0.1,
            current_mbps: 0.0,
        };
    }

    let (download_mbps, download_mbs) = match measure_download(&state, &cancel).await {
        Ok(res) => res,
        Err(e) => {
            let mut guard = state.lock();
            *guard = SpeedTestPhase::Failed(format!("Error midiendo bajada: {:#}", e));
            return;
        }
    };

    // 3. Measuring Upload
    {
        let mut guard = state.lock();
        *guard = SpeedTestPhase::MeasuringUpload {
            progress: 0.1,
            current_mbps: 0.0,
        };
    }

    let (upload_mbps, upload_mbs) = match measure_upload(&state, &cancel).await {
        Ok(res) => res,
        Err(e) => {
            let mut guard = state.lock();
            *guard = SpeedTestPhase::Failed(format!("Error midiendo subida: {:#}", e));
            return;
        }
    };

    // 4. Completed
    let result = SpeedTestResult {
        ping_ms,
        download_mbps,
        upload_mbps,
        download_mbs,
        upload_mbs,
        timestamp: SystemTime::now(),
    };

    let mut guard = state.lock();
    *guard = SpeedTestPhase::Completed(result);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recommended_limits_85_percent() {
        let result = SpeedTestResult {
            ping_ms: 10.0,
            download_mbps: 400.0,
            upload_mbps: 100.0,
            download_mbs: 50.0,
            upload_mbs: 12.0,
            timestamp: SystemTime::now(),
        };

        // 85% of 12 MB/s = 10.2 -> 10 MB/s
        assert_eq!(result.recommended_upload_limit_mb(), 10);
        // 85% of 50 MB/s = 42.5 -> 43 MB/s
        assert_eq!(result.recommended_download_limit_mb(), 43);
    }
}
