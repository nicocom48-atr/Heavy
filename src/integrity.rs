use anyhow::{Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Fast BLAKE3 file hasher with visual progress bar for large files (e.g., 50GB).
pub fn compute_file_blake3<P: AsRef<Path>>(path: P, show_progress: bool) -> Result<String> {
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let file_size = file.metadata()?.len();

    let pb = if show_progress {
        let pb = ProgressBar::new(file_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {msg}")
                .expect("Failed to create progress template")
                .progress_chars("#>-"),
        );
        pb.set_message(format!("{}", "Computing BLAKE3 integrity hash...".cyan()));
        Some(pb)
    } else {
        None
    };

    let mut hasher = blake3::Hasher::new();
    let mut reader = std::io::BufReader::with_capacity(8 * 1024 * 1024, file);
    let mut buffer = vec![0u8; 8 * 1024 * 1024]; // 8 MB buffer for fast hashing

    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        if let Some(ref pb) = pb {
            pb.inc(n as u64);
        }
    }

    if let Some(pb) = pb {
        pb.finish_with_message(
            "Integrity hash calculated successfully!"
                .green()
                .to_string(),
        );
    }

    let hash = hasher.finalize();
    Ok(hash.to_hex().to_string())
}

/// Verify if a file on disk matches the expected BLAKE3 hash
#[allow(dead_code)]
pub fn verify_file_blake3<P: AsRef<Path>>(path: P, expected_hash: &str) -> Result<bool> {
    let actual_hash = compute_file_blake3(path, true)?;
    Ok(actual_hash.eq_ignore_ascii_case(expected_hash))
}
