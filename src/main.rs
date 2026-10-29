#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;
mod gui;

use clap::Parser;
use colored::Colorize;
use heavydrop::manager;
use heavydrop::receiver;
use heavydrop::sender;
use std::process::ExitCode;
use std::sync::Arc;

#[cfg(target_os = "windows")]
fn attach_console() {
    unsafe {
        extern "system" {
            fn AttachConsole(dwProcessId: u32) -> i32;
        }
        const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn main() -> ExitCode {
    let args = cli::Cli::parse();

    // If no subcommand was passed or user explicitly chose `gui`, launch the native window GUI
    match args.command {
        None | Some(cli::Commands::Gui) => launch_gui(),
        Some(cmd) => {
            #[cfg(target_os = "windows")]
            attach_console();

            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed to build Tokio runtime");

            rt.block_on(async { run_cli(cmd).await })
        }
    }
}

fn launch_gui() -> ExitCode {
    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to start async runtime for GUI"),
    );

    // Keep the Tokio runtime context entered on the main thread so any tokio calls work
    let _guard = rt.enter();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([720.0, 640.0])
            .with_min_inner_size([640.0, 520.0])
            .with_title("⚡ HeavyDrop - P2P File Transfer (50GB+)"),
        ..Default::default()
    };

    let rt_clone = rt.clone();
    let result = eframe::run_native(
        "HeavyDrop",
        options,
        Box::new(move |cc| Ok(Box::new(gui::HeavyDropApp::new(cc, rt_clone)))),
    );

    if let Err(e) = result {
        eprintln!("Failed to launch GUI: {}", e);
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

async fn run_cli(cmd: cli::Commands) -> ExitCode {
    let manager = manager::TransferManager::new();

    // Hook Ctrl+C for cooperative cancellation
    let mgr_cancel = manager.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            println!(
                "\n{}",
                "Received Ctrl+C, cancelling transfer cooperatively...".yellow()
            );
            mgr_cancel.cancel();
        }
    });

    let result = match cmd {
        cli::Commands::Send {
            file,
            port,
            bind,
            chunk_size_mb,
            token,
            no_prehash,
        } => {
            let chunk_size = chunk_size_mb.max(1) * 1024 * 1024;
            manager
                .run_send(sender::SenderConfig {
                    file_path: file,
                    bind_addr: bind,
                    port,
                    chunk_size,
                    token,
                    skip_prehash: no_prehash,
                    shared_progress: None,
                    cancel_signal: None,
                })
                .await
        }
        cli::Commands::Recv {
            address,
            output_dir,
            token,
            force,
        } => {
            manager
                .run_recv(receiver::ReceiverConfig {
                    peer_addr: address,
                    output_dir,
                    token,
                    force,
                    shared_progress: None,
                    cancel_signal: None,
                })
                .await
        }
        cli::Commands::Gui => unreachable!(),
    };

    if let Err(e) = result {
        eprintln!("\n{} {}", "ERROR:".red().bold(), e);
        for cause in e.chain().skip(1) {
            eprintln!("  {} {}", "Caused by:".yellow(), cause);
        }
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
