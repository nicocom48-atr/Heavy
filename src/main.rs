#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod cli;
mod gui;

use clap::Parser;
use colored::Colorize;
use heavydrop::pairing::PairingCode;
use heavydrop::receiver;
use heavydrop::sender;
use heavydrop::server::SignalingServer;
use heavydrop::session::Session;
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
    // Attach before clap parses flags so --help and argument errors remain visible.
    #[cfg(target_os = "windows")]
    if std::env::args_os().nth(1).is_some_and(|arg| arg != "gui") {
        attach_console();
    }
    let args = cli::Cli::parse();
    heavydrop::init_logging(args.verbose);

    // If no subcommand was passed or user explicitly chose `gui`, launch the native window GUI
    match args.command {
        None | Some(cli::Commands::Gui) => launch_gui(),
        Some(cmd) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed to build Tokio runtime");

            rt.block_on(async { run_cli(cmd, args.verbose).await })
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

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 720.0])
            .with_min_inner_size([900.0, 560.0])
            .with_title("HeavyDrop — P2P File Transfer"),
        ..Default::default()
    };

    let result = eframe::run_native(
        "HeavyDrop",
        native_options,
        Box::new(|cc| Ok(Box::new(gui::HeavyDropApp::new(cc, rt.clone())))),
    );

    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error starting HeavyDrop GUI: {}", e);
            ExitCode::FAILURE
        }
    }
}

async fn run_cli(command: cli::Commands, verbose: bool) -> ExitCode {
    match command {
        cli::Commands::Send {
            lan_only,
            direct_only,
            legacy_tcp,
            file,
            no_prehash,
            port,
            signal_server,
        } => {
            let session = match Session::new() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error creating session: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            let config = sender::SenderConfig {
                transport_mode: if direct_only {
                    heavydrop::automatic::TransportMode::DirectOnly
                } else if lan_only || legacy_tcp {
                    heavydrop::automatic::TransportMode::LegacyTcp
                } else {
                    heavydrop::automatic::TransportMode::Automatic
                },
                file_path: file,
                session,
                preferred_port: port,
                chunk_size: 4 * 1024 * 1024,
                skip_prehash: no_prehash,
                signaling_server: signal_server,
                shared_progress: None,
                cancel_signal: None,
                speed_limiter: None,
            };

            match sender::run_sender(config).await {
                Ok(_) => ExitCode::SUCCESS,
                Err(e) => {
                    if verbose {
                        eprintln!("\n{} Fallo en la transferencia: {:#?}", "ERROR:".red().bold(), e);
                    } else {
                        eprintln!("\n{} Fallo en la transferencia: {:#}", "ERROR:".red().bold(), e);
                        eprintln!("(Usa --verbose o -v para ver el detalle técnico del error)");
                    }
                    ExitCode::FAILURE
                }
            }
        }
        cli::Commands::Recv {
            legacy_tcp,
            direct_only,
            code,
            output_dir,
            force,
            signal_server,
        } => {
            let parsed_code = match PairingCode::parse(&code) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Invalid pairing code or invitation: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            let config = receiver::ReceiverConfig {
                allow_legacy_tcp: legacy_tcp,
                direct_only,
                code: parsed_code,
                output_dir,
                force,
                signaling_server: signal_server,
                shared_progress: None,
                cancel_signal: None,
                discovery: None,
                speed_limiter: None,
            };

            match receiver::run_receiver(config).await {
                Ok(_) => ExitCode::SUCCESS,
                Err(e) => {
                    if verbose {
                        eprintln!("\n{} Fallo en la transferencia: {:#?}", "ERROR:".red().bold(), e);
                    } else {
                        eprintln!("\n{} Fallo en la transferencia: {:#}", "ERROR:".red().bold(), e);
                        eprintln!("(Usa --verbose o -v para ver el detalle técnico del error)");
                    }
                    ExitCode::FAILURE
                }
            }
        }
        cli::Commands::Gui => ExitCode::SUCCESS,
        cli::Commands::Server { port } => {
            let server = SignalingServer::new(port);
            match server.run().await {
                Ok(_) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("Signaling server error: {:#}", e);
                    ExitCode::FAILURE
                }
            }
        }
    }
}
