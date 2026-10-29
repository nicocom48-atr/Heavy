use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Runtime;

use heavydrop::discovery::DiscoveryService;
use heavydrop::manager::TransferManager;
use heavydrop::pairing::PairingCode;
use heavydrop::receiver::ReceiverConfig;
use heavydrop::sender::SenderConfig;
use heavydrop::session::Session;
use heavydrop::transfer_state::{ConnectionType, TransferRole, TransferStatus};

#[derive(PartialEq, Eq)]
enum ActiveTab {
    Send,
    Receive,
}

pub struct HeavyDropApp {
    active_tab: ActiveTab,

    // Send state
    send_file_path: Option<PathBuf>,
    send_session: Option<Session>,
    send_error: Option<String>,

    // Receive state
    recv_code_input: String,
    recv_output_dir: PathBuf,
    recv_error: Option<String>,

    // Services
    manager: TransferManager,
    discovery: Arc<DiscoveryService>,
    tokio_rt: Arc<Runtime>,

    // UI feedback
    copied_toast_time: Option<Instant>,
    copied_toast_text: String,
}

impl HeavyDropApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: Arc<Runtime>) -> Self {
        Self::apply_custom_theme(&cc.egui_ctx);

        let default_output_dir = dirs_next_output_dir();
        let manager = TransferManager::new();
        let discovery = Arc::new(DiscoveryService::new());

        Self {
            active_tab: ActiveTab::Send,
            send_file_path: None,
            send_session: None,
            send_error: None,

            recv_code_input: String::new(),
            recv_output_dir: default_output_dir,
            recv_error: None,

            manager,
            discovery,
            tokio_rt: rt,

            copied_toast_time: None,
            copied_toast_text: String::new(),
        }
    }

    fn apply_custom_theme(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = egui::Color32::from_rgb(10, 15, 29); // slate-950 deep dark
        visuals.panel_fill = egui::Color32::from_rgb(10, 15, 29);
        visuals.extreme_bg_color = egui::Color32::from_rgb(3, 7, 18);

        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(20, 27, 45);
        visuals.widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(38, 48, 74));
        visuals.widgets.noninteractive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(226, 232, 240));

        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(24, 33, 54);
        visuals.widgets.inactive.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(51, 65, 85));
        visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(226, 232, 240));

        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(45, 59, 90);
        visuals.widgets.hovered.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(56, 189, 248));
        visuals.widgets.hovered.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(255, 255, 255));

        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(2, 132, 199);
        visuals.widgets.active.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(56, 189, 248));

        ctx.set_visuals(visuals);
    }

    fn show_toast(&mut self, text: impl Into<String>) {
        self.copied_toast_text = text.into();
        self.copied_toast_time = Some(Instant::now());
    }

    fn cancel_current_transfer(&mut self) {
        self.manager.cancel();
        self.send_session = None;
    }

    fn start_send_task(&mut self) {
        let file_path = match self.send_file_path.clone() {
            Some(p) => p,
            None => {
                self.send_error = Some("Por favor selecciona un archivo primero.".to_string());
                return;
            }
        };

        let session = match Session::new() {
            Ok(s) => s,
            Err(e) => {
                self.send_error = Some(format!("Error al crear sesión: {}", e));
                return;
            }
        };

        self.send_session = Some(session.clone());
        self.send_error = None;

        let config = SenderConfig {
            file_path,
            session,
            preferred_port: 7878,
            chunk_size: 4 * 1024 * 1024,
            skip_prehash: false,
            signaling_server: heavydrop::signaling::SignalingClient::default_server(),
            shared_progress: None,
            cancel_signal: None,
        };

        let mgr = self.manager.clone();
        self.tokio_rt.spawn(async move {
            let _ = mgr.run_send(config).await;
        });
    }

    fn start_receive_task(&mut self) {
        let parsed_code = match PairingCode::parse(&self.recv_code_input) {
            Ok(c) => c,
            Err(e) => {
                self.recv_error = Some(e.to_string());
                return;
            }
        };

        self.recv_error = None;
        let output_dir = self.recv_output_dir.clone();

        let config = ReceiverConfig {
            code: parsed_code,
            output_dir,
            force: false,
            signaling_server: heavydrop::signaling::SignalingClient::default_server(),
            shared_progress: None,
            cancel_signal: None,
            discovery: Some(self.discovery.clone()),
        };

        let mgr = self.manager.clone();
        self.tokio_rt.spawn(async move {
            let _ = mgr.run_recv(config).await;
        });
    }
}

impl eframe::App for HeavyDropApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let is_busy = self.manager.is_busy();
        if is_busy || self.send_session.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        // Handle Drag & Drop: drop any file into the window
        let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(file) = dropped_files.first() {
            if let Some(ref path) = file.path {
                if path.is_file() {
                    self.send_file_path = Some(path.clone());
                    self.send_session = None;
                    self.active_tab = ActiveTab::Send;
                }
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                // Header Bar
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("⚡ HeavyDrop")
                            .size(22.0)
                            .strong()
                            .color(egui::Color32::from_rgb(56, 189, 248)),
                    );
                    ui.label(
                        egui::RichText::new("Transferencia P2P Masiva por Códigos Temporales")
                            .size(13.0)
                            .color(egui::Color32::from_rgb(148, 163, 184)),
                    );
                });
                ui.add_space(8.0);

                // Toast notification
                if let Some(toast_time) = self.copied_toast_time {
                    if toast_time.elapsed().as_secs() < 3 {
                        ui.label(
                            egui::RichText::new(&self.copied_toast_text)
                                .color(egui::Color32::from_rgb(34, 197, 94))
                                .strong(),
                        );
                        ui.add_space(4.0);
                    } else {
                        self.copied_toast_time = None;
                    }
                }

                // Transfer Monitor area (when active transfer is in progress)
                let progress = self.manager.progress().lock().clone();
                if is_busy || progress.status != TransferStatus::Idle {
                    self.render_transfer_status(ui, &progress);
                    ui.add_space(14.0);
                }

                // Main Content Frame (Mockup-inspired dual card layout)
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgb(15, 23, 42)) // slate-900
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgb(30, 41, 59),
                    ))
                    .rounding(10.0)
                    .inner_margin(16.0)
                    .show(ui, |ui| {
                        let total_width = ui.available_width();
                        let is_wide = total_width >= 620.0;

                        if is_wide {
                            let col_width = (total_width - 24.0) / 2.0;

                            ui.horizontal(|ui| {
                                // Left Column: Enviar
                                ui.allocate_ui_with_layout(
                                    egui::vec2(col_width, 10.0),
                                    egui::Layout::top_down(egui::Align::LEFT),
                                    |ui| {
                                        self.render_send_panel(ui, is_busy);
                                    },
                                );

                                ui.add_space(20.0);

                                // Right Column: Recibir
                                ui.allocate_ui_with_layout(
                                    egui::vec2(col_width, 10.0),
                                    egui::Layout::top_down(egui::Align::LEFT),
                                    |ui| {
                                        self.render_receive_panel(ui, is_busy);
                                    },
                                );
                            });
                        } else {
                            // Responsive fallback tabs for narrow screens
                            ui.horizontal(|ui| {
                                let send_active = self.active_tab == ActiveTab::Send;
                                if ui.selectable_label(send_active, "📤 Enviar").clicked() && !is_busy {
                                    self.active_tab = ActiveTab::Send;
                                }
                                let recv_active = self.active_tab == ActiveTab::Receive;
                                if ui.selectable_label(recv_active, "📥 Recibir").clicked() && !is_busy {
                                    self.active_tab = ActiveTab::Receive;
                                }
                            });
                            ui.separator();
                            ui.add_space(8.0);

                            match self.active_tab {
                                ActiveTab::Send => self.render_send_panel(ui, is_busy),
                                ActiveTab::Receive => self.render_receive_panel(ui, is_busy),
                            }
                        }
                    });

                // Architecture and Security Footer note
                ui.add_space(12.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(
                            "🔒 Cifrado ChaCha20-Poly1305 + X25519 • P2P directo LAN/WAN con fallback a Relay cifrado",
                        )
                        .size(11.0)
                        .color(egui::Color32::from_rgb(100, 116, 139)),
                    );
                });
            });
        });
    }
}

impl HeavyDropApp {
    fn render_send_panel(&mut self, ui: &mut egui::Ui, is_busy: bool) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("📤 Enviar")
                    .size(17.0)
                    .strong()
                    .color(egui::Color32::WHITE),
            );
        });
        ui.add_space(8.0);

        // 1. File selection card / drop area
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(24, 33, 54))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(47, 63, 94),
            ))
            .rounding(8.0)
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    if let Some(ref path) = self.send_file_path {
                        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                        let file_name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();

                        ui.label(egui::RichText::new("📄").size(24.0));
                        ui.label(
                            egui::RichText::new(file_name)
                                .size(16.0)
                                .strong()
                                .color(egui::Color32::from_rgb(248, 250, 252)),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{} ({:.2} GB)",
                                indicatif::HumanBytes(file_size),
                                file_size as f64 / (1024.0 * 1024.0 * 1024.0)
                            ))
                            .size(13.0)
                            .color(egui::Color32::from_rgb(148, 163, 184)),
                        );

                        if !is_busy && self.send_session.is_none() {
                            ui.add_space(6.0);
                            if ui.button("Cambiar archivo").clicked() {
                                if let Some(file) = rfd::FileDialog::new().pick_file() {
                                    self.send_file_path = Some(file);
                                }
                            }
                        }
                    } else {
                        ui.label(egui::RichText::new("📁").size(24.0));
                        ui.label(
                            egui::RichText::new("Arrastrá un archivo aquí")
                                .size(15.0)
                                .strong()
                                .color(egui::Color32::from_rgb(226, 232, 240)),
                        );
                        ui.label(
                            egui::RichText::new("o hacé clic para seleccionar (hasta 1 TB+)")
                                .size(12.0)
                                .color(egui::Color32::from_rgb(148, 163, 184)),
                        );
                        ui.add_space(6.0);
                        if ui.button("Seleccionar archivo").clicked() {
                            if let Some(file) = rfd::FileDialog::new().pick_file() {
                                self.send_file_path = Some(file);
                            }
                        }
                    }
                });
            });

        ui.add_space(10.0);

        // 2. Action: Create transfer OR display pairing code
        let session_clone = self.send_session.clone();
        if let Some(ref session) = session_clone {
            ui.label(
                egui::RichText::new("Código de conexión")
                    .size(13.0)
                    .color(egui::Color32::from_rgb(148, 163, 184)),
            );
            ui.add_space(4.0);

            // Large Code Display Box
            egui::Frame::group(ui.style())
                .fill(egui::Color32::from_rgb(8, 12, 22))
                .stroke(egui::Stroke::new(
                    1.5_f32,
                    egui::Color32::from_rgb(56, 189, 248),
                ))
                .rounding(8.0)
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new(session.code().formatted())
                                .size(24.0)
                                .monospace()
                                .strong()
                                .color(egui::Color32::from_rgb(56, 189, 248)),
                        );
                    });
                });

            ui.add_space(8.0);

            let copy_btn = ui.add_sized(
                [ui.available_width(), 38.0],
                egui::Button::new(
                    egui::RichText::new("📋 Copiar código")
                        .size(14.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                )
                .fill(egui::Color32::from_rgb(2, 132, 199)),
            );
            if copy_btn.clicked() {
                ui.output_mut(|o| o.copied_text = session.code().formatted());
                self.show_toast("¡Código copiado al portapapeles!");
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("⏱ Expira en {}", session.format_remaining()))
                        .size(12.0)
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );

                if !is_busy {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(
                                egui::RichText::new("Cancelar")
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            )
                            .clicked()
                        {
                            self.cancel_current_transfer();
                        }
                    });
                }
            });
        } else if self.send_file_path.is_some() && !is_busy {
            let start_btn = ui.add_sized(
                [ui.available_width(), 42.0],
                egui::Button::new(
                    egui::RichText::new("⚡ Crear transferencia")
                        .size(15.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                )
                .fill(egui::Color32::from_rgb(2, 132, 199)),
            );

            if start_btn.clicked() {
                self.start_send_task();
            }
        }

        if let Some(ref err) = self.send_error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("❌ {}", err))
                    .color(egui::Color32::from_rgb(239, 68, 68)),
            );
        }
    }

    fn render_receive_panel(&mut self, ui: &mut egui::Ui, is_busy: bool) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("📥 Recibir")
                    .size(17.0)
                    .strong()
                    .color(egui::Color32::WHITE),
            );
        });
        ui.add_space(8.0);

        ui.label(
            egui::RichText::new("Ingresá el código del emisor")
                .size(13.0)
                .color(egui::Color32::from_rgb(148, 163, 184)),
        );
        ui.add_space(4.0);

        // Input Field for Code
        let code_edit = ui.add_sized(
            [ui.available_width(), 38.0],
            egui::TextEdit::singleline(&mut self.recv_code_input)
                .hint_text("XXXX-XXXX-XXXX")
                .font(egui::TextStyle::Monospace)
                .char_limit(16),
        );

        if code_edit.changed() {
            self.recv_code_input = self.recv_code_input.to_ascii_uppercase();
        }

        ui.add_space(8.0);

        if !is_busy {
            let connect_btn = ui.add_sized(
                [ui.available_width(), 40.0],
                egui::Button::new(
                    egui::RichText::new("🔗 Usar código")
                        .size(14.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                )
                .fill(egui::Color32::from_rgb(2, 132, 199)),
            );

            if connect_btn.clicked() {
                self.start_receive_task();
            }
        } else {
            if ui
                .add_sized(
                    [ui.available_width(), 38.0],
                    egui::Button::new(
                        egui::RichText::new("🛑 Cancelar conexión")
                            .color(egui::Color32::from_rgb(239, 68, 68)),
                    ),
                )
                .clicked()
            {
                self.cancel_current_transfer();
            }
        }

        ui.add_space(10.0);

        // File status preview box (matching mockup)
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(24, 33, 54))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(47, 63, 94),
            ))
            .rounding(8.0)
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("📄");
                    ui.label(
                        egui::RichText::new("Archivo pendiente")
                            .strong()
                            .color(egui::Color32::from_rgb(226, 232, 240)),
                    );
                });
                ui.label(
                    egui::RichText::new("Se muestra tras conectarse")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("✓")
                            .color(egui::Color32::from_rgb(34, 197, 94))
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new("Conexión cifrada")
                            .size(12.0)
                            .color(egui::Color32::from_rgb(148, 163, 184)),
                    );
                });
            });

        ui.add_space(8.0);

        // Target folder selector
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Carpeta:")
                    .size(11.0)
                    .color(egui::Color32::from_rgb(148, 163, 184)),
            );
            let dir_str = self.recv_output_dir.display().to_string();
            let truncated_dir = if dir_str.len() > 28 {
                format!("...{}", &dir_str[dir_str.len() - 25..])
            } else {
                dir_str
            };
            ui.label(
                egui::RichText::new(truncated_dir)
                    .size(11.0)
                    .color(egui::Color32::from_rgb(56, 189, 248)),
            );
            if !is_busy && ui.small_button("Cambiar").clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    self.recv_output_dir = dir;
                }
            }
        });

        if let Some(ref err) = self.recv_error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("❌ {}", err))
                    .color(egui::Color32::from_rgb(239, 68, 68)),
            );
        }
    }

    fn render_transfer_status(
        &mut self,
        ui: &mut egui::Ui,
        progress: &heavydrop::transfer_state::TransferProgress,
    ) {
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(24, 33, 54))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(56, 189, 248),
            ))
            .rounding(8.0)
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let role_tag = match progress.role {
                        TransferRole::Sender => "📤 Emisor",
                        TransferRole::Receiver => "📥 Receptor",
                    };
                    ui.label(
                        egui::RichText::new(role_tag)
                            .size(13.0)
                            .strong()
                            .color(egui::Color32::from_rgb(56, 189, 248)),
                    );

                    // Connectivity badge
                    let (badge_color, badge_text) = match progress.connection_type {
                        ConnectionType::DirectLocal => (
                            egui::Color32::from_rgb(34, 197, 94),
                            "● Conexión directa local (LAN)",
                        ),
                        ConnectionType::DirectInternet => (
                            egui::Color32::from_rgb(56, 189, 248),
                            "● Conexión directa por Internet (P2P)",
                        ),
                        ConnectionType::Relay => (
                            egui::Color32::from_rgb(234, 179, 8),
                            "● Conexión cifrada mediante relay",
                        ),
                        ConnectionType::None => {
                            (egui::Color32::from_rgb(148, 163, 184), "○ Conectando...")
                        }
                    };
                    ui.label(
                        egui::RichText::new(badge_text)
                            .size(12.0)
                            .color(badge_color),
                    );
                });

                ui.add_space(4.0);

                if !progress.file_name.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("Archivo: {}", progress.file_name))
                            .size(15.0)
                            .strong()
                            .color(egui::Color32::WHITE),
                    );
                }

                // Progress Bar
                let fraction = if progress.total_bytes > 0 {
                    (progress.transferred_bytes as f32 / progress.total_bytes as f32)
                        .clamp(0.0, 1.0)
                } else {
                    0.0
                };

                let percent = (fraction * 100.0) as u32;
                let pb_text = format!("{}%", percent);
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .text(pb_text)
                        .animate(progress.status == TransferStatus::Transferring),
                );

                ui.add_space(6.0);

                // Stats: MB/s, bytes, ETA
                ui.horizontal(|ui| {
                    let speed_mb = progress.bytes_per_sec / (1024.0 * 1024.0);
                    ui.label(format!("Velocidad: {:.2} MB/s", speed_mb));

                    if progress.total_bytes > 0 {
                        ui.label(format!(
                            "• {} / {}",
                            indicatif::HumanBytes(progress.transferred_bytes),
                            indicatif::HumanBytes(progress.total_bytes)
                        ));

                        if progress.bytes_per_sec > 0.0 {
                            let rem_bytes = progress
                                .total_bytes
                                .saturating_sub(progress.transferred_bytes);
                            let eta_secs = (rem_bytes as f64 / progress.bytes_per_sec) as u64;
                            ui.label(format!("• ETA: {}m {}s", eta_secs / 60, eta_secs % 60));
                        }
                    }
                });

                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(&progress.status_text)
                        .size(12.0)
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );

                if let Some(ref err) = progress.error {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("Error: {}", err))
                            .color(egui::Color32::from_rgb(239, 68, 68)),
                    );
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if self.manager.is_busy() {
                        if ui.button("🛑 Cancelar transferencia").clicked() {
                            self.cancel_current_transfer();
                        }
                    } else if ui.button("Listo / Cerrar").clicked() {
                        self.manager.reset_cancel();
                        self.send_session = None;
                    }
                });
            });
    }
}

fn dirs_next_output_dir() -> PathBuf {
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let downloads = PathBuf::from(user_profile).join("Downloads");
        if downloads.exists() {
            return downloads;
        }
    }
    PathBuf::from(".")
}
