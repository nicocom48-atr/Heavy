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
        cli::Commands::Speedtest => {
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let state = std::sync::Arc::new(parking_lot::Mutex::new(heavydrop::speedtest::SpeedTestPhase::Idle));
            println!("{}", "⚡ Iniciando prueba de velocidad de Internet (Cloudflare Anycast)...".cyan().bold());

            let pb = indicatif::ProgressBar::new_spinner();
            pb.set_style(
                indicatif::ProgressStyle::default_spinner()
                    .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
                    .template("{spinner:.green} {msg}")
                    .unwrap(),
            );
            pb.enable_steady_tick(std::time::Duration::from_millis(80));

            let state_monitor = state.clone();
            let pb_clone = pb.clone();
            let monitor_task = tokio::spawn(async move {
                let mut last_phase = heavydrop::speedtest::SpeedTestPhase::Idle;
                loop {
                    let phase = state_monitor.lock().clone();
                    if phase != last_phase {
                        match &phase {
                            heavydrop::speedtest::SpeedTestPhase::Idle => {
                                pb_clone.set_message("Preparando prueba...");
                            }
                            heavydrop::speedtest::SpeedTestPhase::MeasuringPing => {
                                pb_clone.set_message("Midiendo latencia (ping)...");
                            }
                            heavydrop::speedtest::SpeedTestPhase::MeasuringDownload { progress, current_mbps } => {
                                pb_clone.set_message(format!("Midiendo bajada ({:.0}%)... {:.1} Mbps", progress * 100.0, current_mbps));
                            }
                            heavydrop::speedtest::SpeedTestPhase::MeasuringUpload { progress, current_mbps } => {
                                pb_clone.set_message(format!("Midiendo subida ({:.0}%)... {:.1} Mbps", progress * 100.0, current_mbps));
                            }
                            heavydrop::speedtest::SpeedTestPhase::Completed(_) => {
                                pb_clone.finish_and_clear();
                                break;
                            }
                            heavydrop::speedtest::SpeedTestPhase::Failed(err) => {
                                pb_clone.finish_with_message(format!("Fallo: {}", err));
                                break;
                            }
                        }
                        last_phase = phase;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            });

            heavydrop::speedtest::run_speed_test(state.clone(), cancel).await;
            let _ = monitor_task.await;

            let final_phase = state.lock().clone();
            match final_phase {
                heavydrop::speedtest::SpeedTestPhase::Completed(res) => {
                    println!("\n{}", "================ [ RESULTADOS DE VELOCIDAD ] ================".bold().cyan());
                    println!("  Latencia (Ping) : {}", format!("{:.1} ms", res.ping_ms).green().bold());
                    println!(
                        "  Bajada (Download): {} ({:.2} MB/s)",
                        format!("{:.1} Mbps", res.download_mbps).green().bold(),
                        res.download_mbs
                    );
                    println!(
                        "  Subida (Upload)  : {} ({:.2} MB/s)",
                        format!("{:.1} Mbps", res.upload_mbps).green().bold(),
                        res.upload_mbs
                    );
                    println!("{}", "------------------------------------------------------------".cyan());
                    println!(
                        "  💡 Límite recomendado al 85%: Subida: {} MB/s | Bajada: {} MB/s",
                        res.recommended_upload_limit_mb(),
                        res.recommended_download_limit_mb()
                    );
                    println!("{}\n", "============================================================".bold().cyan());
                    ExitCode::SUCCESS
                }
                heavydrop::speedtest::SpeedTestPhase::Failed(err) => {
                    eprintln!("\n{} Error en la prueba de velocidad: {}", "ERROR:".red().bold(), err);
                    ExitCode::FAILURE
                }
                _ => ExitCode::FAILURE,
            }
        }
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
