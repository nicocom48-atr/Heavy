mod cli;
mod gui;
mod integrity;
mod network;
mod protocol;
mod receiver;
mod sender;
mod transfer_state;

use clap::Parser;
use colored::Colorize;
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    let args = cli::Cli::parse();

    // If no subcommand was passed or user explicitly chose `gui`, launch the native window GUI
    match args.command {
        None | Some(cli::Commands::Gui) => launch_gui(),
        Some(cmd) => {
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
            sender::run_sender(sender::SenderConfig {
                file_path: file,
                bind_addr: bind,
                port,
                chunk_size,
                token,
                skip_prehash: no_prehash,
                shared_progress: None,
            })
            .await
        }
        cli::Commands::Recv {
            address,
            output_dir,
            token,
            force,
        } => {
            receiver::run_receiver(receiver::ReceiverConfig {
                peer_addr: address,
                output_dir,
                token,
                force,
                shared_progress: None,
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
