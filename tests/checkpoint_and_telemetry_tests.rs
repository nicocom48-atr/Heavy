use heavydrop::part_file::{PartFileManager, PrepareResult};
use heavydrop::protocol::*;
use heavydrop::transfer_state::SpeedWindow;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn temp_test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("heavydrop_ckpt_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn make_test_offer(file_name: &str, file_size: u64, chunk_size: u32) -> HandshakeOffer {
    HandshakeOffer {
        protocol_version: PROTOCOL_VERSION,
        session_id: "ckpt-sess-1".to_string(),
        sender_public_key: [0u8; 32],
        session_salt: [0u8; 32],
        file_name: file_name.to_string(),
        file_size,
        chunk_size,
        file_hash: "testfilehash000000000000000000000000000000000000000000000000000000".to_string(),
        sender_auth_proof: [0u8; 32],
        parallel_streams: 4,
    }
}

// 1. Checkpoint batching: metadata is NOT rewritten on every chunk, but persisted at threshold
#[tokio::test]
async fn test_batched_checkpoints_reduce_metadata_writes() {
    let dir = temp_test_dir("batch_ckpt");
    let chunk_size = 1024 * 1024; // 1 MiB chunks
    let total_size = 32 * 1024 * 1024; // 32 MiB total
    let offer = make_test_offer("batched.bin", total_size, chunk_size as u32);

    let prep = PartFileManager::prepare(&dir, &offer, true).await.unwrap();
    let (mut part_mgr, resume) = match prep {
        PrepareResult::Ready { part_manager, resume_offset } => (*part_manager, resume_offset),
        _ => panic!("Expected Ready"),
    };
    assert_eq!(resume, 0);

    let meta_path = dir.join("batched.bin.part.meta");

    // Write chunk 0 (1 MiB): threshold is 16 MiB or 2s, so metadata file should not exist yet
    let chunk_data = vec![0x55u8; chunk_size];
    let hash0 = *blake3::hash(&chunk_data).as_bytes();
    part_mgr.write_verified_chunk(0, &chunk_data, hash0).await.unwrap();

    // Verify metadata was NOT written yet (dirty in memory)
    assert!(!meta_path.exists(), "Metadata should not be written after only 1 MiB");

    // Write 14 more chunks (total 15 MiB < 16 MiB threshold) quickly
    for i in 1..15 {
        let offset = (i as u64) * (chunk_size as u64);
        let h = *blake3::hash(&chunk_data).as_bytes();
        part_mgr.write_verified_chunk(offset, &chunk_data, h).await.unwrap();
    }
    assert!(!meta_path.exists(), "Metadata should not be written before 16 MiB threshold");

    // Writing the 16th chunk crosses the 16 MiB threshold -> checkpoint should persist
    let h15 = *blake3::hash(&chunk_data).as_bytes();
    part_mgr.write_verified_chunk(15 * (chunk_size as u64), &chunk_data, h15).await.unwrap();
    assert!(meta_path.exists(), "Metadata MUST be persisted when crossing threshold");

    // Explicit flush ensures all subsequent chunks are committed
    part_mgr.flush_checkpoint().await.unwrap();
    assert_eq!(part_mgr.metadata.verified_bytes, 16 * (chunk_size as u64));
}

// 2. Out-of-order chunks: verified_ranges tracks gaps, resume_offset uses contiguous prefix
#[tokio::test]
async fn test_out_of_order_chunks_and_gap_resume() {
    let dir = temp_test_dir("out_of_order");
    let chunk_size = 512 * 1024; // 512 KiB
    let total_size = 4 * chunk_size as u64; // 2 MiB (4 chunks)
    let offer = make_test_offer("ooo.bin", total_size, chunk_size as u32);

    let prep = PartFileManager::prepare(&dir, &offer, true).await.unwrap();
    let (mut part_mgr, _) = match prep {
        PrepareResult::Ready { part_manager, resume_offset } => (*part_manager, resume_offset),
        _ => panic!("Expected Ready"),
    };

    let data0 = vec![0x11u8; chunk_size];
    let data2 = vec![0x33u8; chunk_size];
    let data3 = vec![0x44u8; chunk_size];

    // Chunk 0 arrives
    let h0 = *blake3::hash(&data0).as_bytes();
    part_mgr.write_verified_chunk(0, &data0, h0).await.unwrap();

    // Stream 2 finishes chunk 2 before Stream 1 finishes chunk 1 (out of order arrival)
    let h2 = *blake3::hash(&data2).as_bytes();
    part_mgr.write_verified_chunk(2 * chunk_size as u64, &data2, h2).await.unwrap();

    // Stream 3 finishes chunk 3
    let h3 = *blake3::hash(&data3).as_bytes();
    part_mgr.write_verified_chunk(3 * chunk_size as u64, &data3, h3).await.unwrap();

    // Ranges should have a gap: [0..512KB] and [1024KB..2048KB]
    assert_eq!(part_mgr.metadata.verified_ranges.len(), 2);
    assert_eq!(part_mgr.metadata.verified_ranges[0], (0, chunk_size as u64));
    assert_eq!(part_mgr.metadata.verified_ranges[1], (2 * chunk_size as u64, 4 * chunk_size as u64));

    // Persist checkpoint and simulate interruption
    part_mgr.flush_checkpoint().await.unwrap();
    drop(part_mgr);

    // Resuming must NOT skip chunk 1! Resume offset must be exactly 512 KB
    let prep_resume = PartFileManager::prepare(&dir, &offer, false).await.unwrap();
    let (mut part_mgr2, resume_offset) = match prep_resume {
        PrepareResult::Ready { part_manager, resume_offset } => (*part_manager, resume_offset),
        _ => panic!("Expected Ready"),
    };
    assert_eq!(resume_offset, chunk_size as u64, "Resume offset must stop at the first missing gap!");

    // Now write the missing chunk 1
    let data1 = vec![0x22u8; chunk_size];
    let h1 = *blake3::hash(&data1).as_bytes();
    part_mgr2.write_verified_chunk(chunk_size as u64, &data1, h1).await.unwrap();

    // Gaps should now be fully merged into a single contiguous [0..2048KB] range
    assert_eq!(part_mgr2.metadata.verified_ranges.len(), 1);
    assert_eq!(part_mgr2.metadata.verified_ranges[0], (0, total_size));
    assert_eq!(part_mgr2.metadata.verified_bytes, total_size);

    // Finalize should succeed
    let final_path = part_mgr2.finalize().await.unwrap();
    assert!(final_path.exists());
    assert_eq!(std::fs::metadata(&final_path).unwrap().len(), total_size);
}

// 3. Safe resume and rollback when metadata is invalid, tampered or lagging
#[tokio::test]
async fn test_corrupt_metadata_gracefully_recovered() {
    let dir = temp_test_dir("corrupt_meta");
    let offer = make_test_offer("safe.bin", 1024 * 1024, 256 * 1024);

    let prep = PartFileManager::prepare(&dir, &offer, true).await.unwrap();
    let (mut part_mgr, _) = match prep {
        PrepareResult::Ready { part_manager, resume_offset } => (*part_manager, resume_offset),
        _ => panic!("Expected Ready"),
    };

    let chunk = vec![0x77u8; 256 * 1024];
    let h = *blake3::hash(&chunk).as_bytes();
    part_mgr.write_verified_chunk(0, &chunk, h).await.unwrap();
    part_mgr.flush_checkpoint().await.unwrap();
    drop(part_mgr);

    let meta_path = dir.join("safe.bin.part.meta");
    assert!(meta_path.exists());

    // Case A: Corrupt JSON bytes in metadata
    tokio::fs::write(&meta_path, b"GARBAGE_NOT_JSON{{{[[").await.unwrap();

    // Prepare should safely detect corrupt metadata and start fresh from 0 without failing
    let prep_corrupt = PartFileManager::prepare(&dir, &offer, false).await.unwrap();
    let (_, resume_corrupt) = match prep_corrupt {
        PrepareResult::Ready { part_manager, resume_offset } => (*part_manager, resume_offset),
        _ => panic!("Expected Ready"),
    };
    assert_eq!(resume_corrupt, 0, "Corrupted metadata must fall back to 0 bytes safely");

    // Case B: Incompatible file size in metadata
    let bad_meta = serde_json::json!({
        "file_name": "safe.bin",
        "file_size": 999999999u64, // wrong file size!
        "file_hash": "UNVERIFIED_PREHASH",
        "verified_ranges": [[0, 262144]],
        "last_chunk_hash": vec![0u8; 32],
        "verified_bytes": 262144
    });
    tokio::fs::write(&meta_path, serde_json::to_vec(&bad_meta).unwrap()).await.unwrap();

    let prep_mismatch = PartFileManager::prepare(&dir, &offer, false).await.unwrap();
    let (_, resume_mismatch) = match prep_mismatch {
        PrepareResult::Ready { part_manager, resume_offset } => (*part_manager, resume_offset),
        _ => panic!("Expected Ready"),
    };
    assert_eq!(resume_mismatch, 0, "Mismatched file size in metadata must fall back to 0 bytes");
}

// 4. Drop persists dirty metadata on sudden exit / cancellation
#[tokio::test]
async fn test_drop_persists_uncommitted_checkpoint() {
    let dir = temp_test_dir("drop_persist");
    let offer = make_test_offer("drop.bin", 1024 * 1024, 256 * 1024);

    let prep = PartFileManager::prepare(&dir, &offer, true).await.unwrap();
    let (mut part_mgr, _) = match prep {
        PrepareResult::Ready { part_manager, resume_offset } => (*part_manager, resume_offset),
        _ => panic!("Expected Ready"),
    };

    let chunk = vec![0x99u8; 256 * 1024];
    let h = *blake3::hash(&chunk).as_bytes();
    part_mgr.write_verified_chunk(0, &chunk, h).await.unwrap();

    // Notice: we do NOT call flush_checkpoint! We simulate a crash/panic/cancel drop:
    drop(part_mgr);

    let meta_path = dir.join("drop.bin.part.meta");
    assert!(meta_path.exists(), "Drop implementation MUST persist dirty metadata");

    // Reopen and check that verified bytes were preserved
    let prep2 = PartFileManager::prepare(&dir, &offer, false).await.unwrap();
    let (_, resume) = match prep2 {
        PrepareResult::Ready { part_manager, resume_offset } => (*part_manager, resume_offset),
        _ => panic!("Expected Ready"),
    };
    assert_eq!(resume, 256 * 1024);
}

// 5. SpeedWindow telemetry: steady flow, multi-stream burst smoothing, and pause decay
#[test]
fn test_speed_window_burst_smoothing_and_pause_decay() {
    let window_duration = Duration::from_millis(1500);
    let mut sw = SpeedWindow::new(window_duration);
    let base = Instant::now();

    // Simulate 4-stream multi-stream download:
    // Every 500 ms, 4 streams deliver a 4 MB block at once (+16 MB burst)
    // Over 2 seconds (4 bursts of 16 MB = 64 MB), average speed is 32 MB/s.
    let mut total_bytes = 0u64;

    // t = 0 ms: initial connection
    sw.record(base, 0);

    // Burst 1 at 500 ms
    total_bytes += 16 * 1024 * 1024;
    sw.record(base + Duration::from_millis(500), total_bytes);

    // Burst 2 at 1000 ms
    total_bytes += 16 * 1024 * 1024;
    sw.record(base + Duration::from_millis(1000), total_bytes);

    // Burst 3 at 1500 ms
    total_bytes += 16 * 1024 * 1024;
    sw.record(base + Duration::from_millis(1500), total_bytes);

    // Speed at 1500 ms: window covers [0 ms .. 1500 ms], 48 MB / 1.5s = 32 MB/s
    let speed_mb = sw.current_speed(base + Duration::from_millis(1500)) / (1024.0 * 1024.0);
    assert!(
        (speed_mb - 32.0).abs() < 1.0,
        "Burst smoothed speed should be ~32 MB/s, got: {:.2} MB/s",
        speed_mb
    );

    // Now simulate network stall / pause: no bytes arrive for 2000 ms
    // At t = 3100 ms (1600 ms after last packet, exceeding 1500 ms window)
    let speed_stalled = sw.current_speed(base + Duration::from_millis(3100));
    assert_eq!(
        speed_stalled, 0.0,
        "Speed must decay to 0.0 on stalls exceeding the window duration"
    );
}
