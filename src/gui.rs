use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Runtime;

use heavydrop::discovery::DiscoveryService;
use heavydrop::manager::TransferManager;
use heavydrop::network::get_available_local_ips;
use heavydrop::receiver::ReceiverConfig;
use heavydrop::sender::SenderConfig;
use heavydrop::transfer_state::{TransferRole, TransferStatus};

#[derive(PartialEq, Eq)]
enum ActiveTab {
    Send,
    Receive,
}

pub struct HeavyDropApp {
    active_tab: ActiveTab,

    // Send state
    send_file_path: Option<PathBuf>,
    send_port_str: String,
    send_token: String,
    send_skip_prehash: bool,

    // Receive state
    recv_address: String,
    recv_output_dir: PathBuf,
    recv_token: String,
    recv_force: bool,

    // Services
    manager: TransferManager,
    discovery: DiscoveryService,
    tokio_rt: Arc<Runtime>,

    // Network interfaces
    available_ips: Vec<(String, String)>,

    // UI feedback
    copied_toast_time: Option<Instant>,
    copied_toast_text: String,
}

impl HeavyDropApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: Arc<Runtime>) -> Self {
        Self::apply_custom_theme(&cc.egui_ctx);

        let available_ips = get_available_local_ips()
            .into_iter()
            .map(|(iface, ip)| (iface, ip.to_string()))
            .collect();

        let default_output_dir = dirs_next_output_dir();
        let manager = TransferManager::new();
        let discovery = DiscoveryService::new();

        Self {
            active_tab: ActiveTab::Send,
            send_file_path: None,
            send_port_str: "7878".to_string(),
            send_token: String::new(),
            send_skip_prehash: false,

            recv_address: String::new(),
            recv_output_dir: default_output_dir,
            recv_token: String::new(),
            recv_force: false,

            manager,
            discovery,
            tokio_rt: rt,
            available_ips,

            copied_toast_time: None,
            copied_toast_text: String::new(),
        }
    }

    fn apply_custom_theme(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = egui::Color32::from_rgb(15, 23, 42); // slate-900
        visuals.panel_fill = egui::Color32::from_rgb(15, 23, 42);
        visuals.extreme_bg_color = egui::Color32::from_rgb(2, 6, 23); // slate-950

        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(30, 41, 59); // slate-800
        visuals.widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(51, 65, 85));
        visuals.widgets.noninteractive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(226, 232, 240));

        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(30, 41, 59);
        visuals.widgets.inactive.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(71, 85, 105));
        visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(226, 232, 240));

        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(51, 65, 85);
        visuals.widgets.hovered.bg_stroke =
            egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(56, 189, 248)); // sky-400
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);

        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(2, 132, 199); // sky-600
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.5_f32, egui::Color32::WHITE);
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);

        ctx.set_visuals(visuals);
    }

    fn show_toast(&mut self, text: &str) {
        self.copied_toast_text = text.to_string();
        self.copied_toast_time = Some(Instant::now());
    }

    fn cancel_current_transfer(&mut self) {
        self.manager.cancel();
    }

    fn start_send_task(&mut self) {
        let file_path = match self.send_file_path.clone() {
            Some(p) => p,
            None => return,
        };

        let port = self.send_port_str.parse::<u16>().unwrap_or(7878);
        let token = if self.send_token.trim().is_empty() {
            None
        } else {
            Some(self.send_token.trim().to_string())
        };
        let skip_prehash = self.send_skip_prehash;

        let config = SenderConfig {
            file_path,
            bind_addr: "0.0.0.0".to_string(),
            port,
            chunk_size: 4 * 1024 * 1024,
            token,
            skip_prehash,
            shared_progress: None,
            cancel_signal: None,
        };

        let mgr = self.manager.clone();
        self.tokio_rt.spawn(async move {
            let _ = mgr.run_send(config).await;
        });
    }

    fn start_receive_task(&mut self) {
        let addr = self.recv_address.trim().to_string();
        if addr.is_empty() {
            return;
        }

        let token = if self.recv_token.trim().is_empty() {
            None
        } else {
            Some(self.recv_token.trim().to_string())
        };
        let output_dir = self.recv_output_dir.clone();
        let force = self.recv_force;

        let config = ReceiverConfig {
            peer_addr: addr,
            output_dir,
            token,
            force,
            shared_progress: None,
            cancel_signal: None,
        };

        let mgr = self.manager.clone();
        self.tokio_rt.spawn(async move {
            let _ = mgr.run_recv(config).await;
        });
    }
}

fn dirs_next_output_dir() -> PathBuf {
    if let Some(user_profile) = std::env::var_os("USERPROFILE") {
        let downloads = PathBuf::from(user_profile).join("Downloads");
        if downloads.exists() {
            return downloads;
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("descargas")
}

impl eframe::App for HeavyDropApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Continuous repaint during active transfers or when beacon discoveries arrive
        if self.manager.is_busy() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        // Handle Drag & Drop: drop any file into the window
        let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(file) = dropped_files.first() {
            if let Some(ref path) = file.path {
                if path.is_file() {
                    self.send_file_path = Some(path.clone());
                    self.active_tab = ActiveTab::Send;
                }
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // Header
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("⚡ HeavyDrop")
                        .size(24.0)
                        .strong()
                        .color(egui::Color32::from_rgb(56, 189, 248)),
                );
                ui.label(
                    egui::RichText::new("Transferencia P2P Masiva y Cifrada (50GB+)")
                        .size(13.0)
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );
            });
            ui.add_space(8.0);

            let is_busy = self.manager.is_busy();

            // Tabs: Enviar | Recibir
            ui.horizontal(|ui| {
                let send_btn = ui.selectable_label(self.active_tab == ActiveTab::Send, "📤 Enviar");
                if send_btn.clicked() && !is_busy {
                    self.active_tab = ActiveTab::Send;
                }

                let recv_btn =
                    ui.selectable_label(self.active_tab == ActiveTab::Receive, "📥 Recibir");
                if recv_btn.clicked() && !is_busy {
                    self.active_tab = ActiveTab::Receive;
                }
            });
            ui.separator();
            ui.add_space(8.0);

            // Toast notification
            if let Some(toast_time) = self.copied_toast_time {
                if toast_time.elapsed().as_secs() < 3 {
                    ui.label(
                        egui::RichText::new(&self.copied_toast_text)
                            .color(egui::Color32::from_rgb(34, 197, 94))
                            .strong(),
                    );
                } else {
                    self.copied_toast_time = None;
                }
            }

            // Main Content Area
            if is_busy || self.manager.progress().lock().status != TransferStatus::Idle {
                self.render_transfer_status(ui);
                ui.add_space(16.0);
            }

            match self.active_tab {
                ActiveTab::Send => self.render_send_tab(ui, is_busy),
                ActiveTab::Receive => self.render_receive_tab(ui, is_busy),
            }
        });
    }
}

impl HeavyDropApp {
    fn render_send_tab(&mut self, ui: &mut egui::Ui, is_busy: bool) {
        ui.heading("Enviar Archivo");
        ui.add_space(4.0);

        // Drag and drop zone
        ui.group(|ui| {
            ui.vertical_centered(|ui| {
                if let Some(ref path) = self.send_file_path {
                    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    ui.label(
                        egui::RichText::new(format!(
                            "📄 {}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ))
                        .size(16.0)
                        .strong()
                        .color(egui::Color32::from_rgb(248, 250, 252)),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "Tamaño: {} ({:.2} GB)",
                            indicatif::HumanBytes(file_size),
                            file_size as f64 / (1024.0 * 1024.0 * 1024.0)
                        ))
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                    );
                    if !is_busy && ui.button("Cambiar archivo...").clicked() {
                        if let Some(file) = rfd::FileDialog::new().pick_file() {
                            self.send_file_path = Some(file);
                        }
                    }
                } else {
                    let drop_btn = ui.add(
                        egui::Button::new(
                            egui::RichText::new(
                                "📁 Arrastra tu archivo aquí\no haz clic para seleccionar...",
                            )
                            .size(15.0)
                            .color(egui::Color32::from_rgb(148, 163, 184)),
                        )
                        .min_size(egui::vec2(ui.available_width() - 20.0, 90.0)),
                    );
                    if drop_btn.clicked() && !is_busy {
                        if let Some(file) = rfd::FileDialog::new().pick_file() {
                            self.send_file_path = Some(file);
                        }
                    }
                }
            });
        });

        ui.add_space(10.0);

        // IP address sharing info
        ui.group(|ui| {
            ui.label(egui::RichText::new("Direcciones IP de este equipo para compartir:").strong());
            let ips = self.available_ips.clone();
            let port_str = self.send_port_str.clone();
            let mut copied_toast = None;

            for (iface, ip) in &ips {
                ui.horizontal(|ui| {
                    ui.label(format!("• {}: {}:{}", iface, ip, port_str));
                    if ui.button("📋 Copiar").clicked() {
                        ui.output_mut(|o| o.copied_text = format!("{}:{}", ip, port_str));
                        copied_toast = Some(format!("¡Copiado {}:{}!", ip, port_str));
                    }
                });
            }

            if let Some(msg) = copied_toast {
                self.show_toast(&msg);
            }
        });

        ui.add_space(8.0);

        // Collapsible Advanced settings
        ui.collapsing("⚙️ Configuración avanzada", |ui| {
            ui.horizontal(|ui| {
                ui.label("Puerto TCP:");
                ui.add_enabled(
                    !is_busy,
                    egui::TextEdit::singleline(&mut self.send_port_str).desired_width(70.0),
                );
                ui.add_space(12.0);
                ui.label("Contraseña / Token:");
                ui.add_enabled(
                    !is_busy,
                    egui::TextEdit::singleline(&mut self.send_token)
                        .password(true)
                        .hint_text("Opcional")
                        .desired_width(120.0),
                );
            });
            ui.checkbox(
                &mut self.send_skip_prehash,
                "Omitir pre-cálculo de hash BLAKE3 (inicio instantáneo)",
            );
        });

        ui.add_space(12.0);

        // Action Buttons
        ui.horizontal(|ui| {
            let can_start = self.send_file_path.is_some() && !is_busy;
            let start_btn = egui::Button::new(
                egui::RichText::new("▶ Iniciar Envío")
                    .size(15.0)
                    .strong()
                    .color(egui::Color32::WHITE),
            )
            .fill(if can_start {
                egui::Color32::from_rgb(2, 132, 199)
            } else {
                egui::Color32::from_rgb(51, 65, 85)
            })
            .min_size(egui::vec2(160.0, 40.0));

            if ui.add_enabled(can_start, start_btn).clicked() {
                self.start_send_task();
            }

            if is_busy {
                let cancel_btn = egui::Button::new(
                    egui::RichText::new("🛑 Cancelar")
                        .size(15.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                )
                .fill(egui::Color32::from_rgb(225, 29, 72))
                .min_size(egui::vec2(130.0, 40.0));

                if ui.add(cancel_btn).clicked() {
                    self.cancel_current_transfer();
                }
            }
        });
    }

    fn render_receive_tab(&mut self, ui: &mut egui::Ui, is_busy: bool) {
        ui.heading("Recibir Archivo");
        ui.add_space(4.0);

        // Local mDNS / UDP Beacon Peer Discovery
        let active_peers = self.discovery.get_active_peers();
        if !active_peers.is_empty() {
            ui.group(|ui| {
                ui.label(
                    egui::RichText::new("📡 Emisores detectados en la red local:")
                        .strong()
                        .color(egui::Color32::from_rgb(56, 189, 248)),
                );
                for peer in active_peers {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "💻 {} — {} ({})",
                            peer.announcement.device_name,
                            peer.announcement.file_name,
                            indicatif::HumanBytes(peer.announcement.file_size)
                        ));
                        if ui.button("⚡ Conectar").clicked() && !is_busy {
                            self.recv_address = peer.address.clone();
                            self.start_receive_task();
                        }
                    });
                }
            });
            ui.add_space(8.0);
        }

        // Manual address input
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label("Dirección IP del Emisor:");
                ui.add_enabled(
                    !is_busy,
                    egui::TextEdit::singleline(&mut self.recv_address)
                        .hint_text("192.168.1.X:7878"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Guardar en:");
                ui.label(
                    egui::RichText::new(self.recv_output_dir.display().to_string())
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );
                if !is_busy && ui.button("Examinar...").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.recv_output_dir = dir;
                    }
                }
            });
        });

        ui.add_space(8.0);

        // Collapsible Advanced settings
        ui.collapsing("⚙️ Configuración avanzada", |ui| {
            ui.horizontal(|ui| {
                ui.label("Contraseña / Token requerida:");
                ui.add_enabled(
                    !is_busy,
                    egui::TextEdit::singleline(&mut self.recv_token)
                        .password(true)
                        .desired_width(140.0),
                );
            });
            ui.checkbox(
                &mut self.recv_force,
                "Sobrescribir archivo (ignorar la reanudación si ya existe)",
            );
        });

        ui.add_space(12.0);

        // Action Buttons
        ui.horizontal(|ui| {
            let can_start = !self.recv_address.trim().is_empty() && !is_busy;
            let start_btn = egui::Button::new(
                egui::RichText::new("▶ Iniciar Descarga")
                    .size(15.0)
                    .strong()
                    .color(egui::Color32::WHITE),
            )
            .fill(if can_start {
                egui::Color32::from_rgb(2, 132, 199)
            } else {
                egui::Color32::from_rgb(51, 65, 85)
            })
            .min_size(egui::vec2(160.0, 40.0));

            if ui.add_enabled(can_start, start_btn).clicked() {
                self.start_receive_task();
            }

            if is_busy {
                let cancel_btn = egui::Button::new(
                    egui::RichText::new("🛑 Cancelar")
                        .size(15.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                )
                .fill(egui::Color32::from_rgb(225, 29, 72))
                .min_size(egui::vec2(130.0, 40.0));

                if ui.add(cancel_btn).clicked() {
                    self.cancel_current_transfer();
                }
            }
        });
    }

    fn render_transfer_status(&mut self, ui: &mut egui::Ui) {
        let p = self.manager.progress().lock().clone();

        ui.group(|ui| {
            ui.horizontal(|ui| {
                let (badge_text, badge_color) = match p.status {
                    TransferStatus::Idle => ("LISTO", egui::Color32::GRAY),
                    TransferStatus::Preparing => {
                        ("PREPARANDO", egui::Color32::from_rgb(234, 179, 8))
                    }
                    TransferStatus::WaitingForPeer => {
                        ("ESPERANDO RECEPTOR", egui::Color32::from_rgb(56, 189, 248))
                    }
                    TransferStatus::Connected => {
                        ("CONECTADO", egui::Color32::from_rgb(34, 197, 94))
                    }
                    TransferStatus::Transferring => {
                        ("TRANSFIRIENDO", egui::Color32::from_rgb(2, 132, 199))
                    }
                    TransferStatus::Completed => {
                        ("COMPLETADO", egui::Color32::from_rgb(34, 197, 94))
                    }
                    TransferStatus::Failed => ("ERROR", egui::Color32::from_rgb(239, 68, 68)),
                    TransferStatus::Cancelled => {
                        ("CANCELADO", egui::Color32::from_rgb(244, 63, 94))
                    }
                };

                ui.label(egui::RichText::new(badge_text).strong().color(badge_color));
                ui.label(match p.role {
                    TransferRole::Sender => "(Emisor)",
                    TransferRole::Receiver => "(Receptor)",
                });
                ui.label(
                    egui::RichText::new("🔐 Cifrado ChaCha20-Poly1305")
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );
            });

            if !p.file_name.is_empty() {
                ui.label(format!(
                    "Archivo: {} ({})",
                    p.file_name,
                    indicatif::HumanBytes(p.total_bytes)
                ));
            }
            if !p.peer_address.is_empty() {
                ui.label(format!("Dispositivo: {}", p.peer_address));
            }

            // Progress bar
            let progress_ratio = if p.total_bytes > 0 {
                (p.transferred_bytes as f32 / p.total_bytes as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };

            ui.add(
                egui::ProgressBar::new(progress_ratio)
                    .show_percentage()
                    .animate(p.status == TransferStatus::Transferring),
            );

            // Speed & ETA
            ui.horizontal(|ui| {
                let speed_mb = p.bytes_per_sec / (1024.0 * 1024.0);
                ui.label(format!("Velocidad: {:.1} MB/s", speed_mb));

                if p.bytes_per_sec > 1000.0 && p.total_bytes > p.transferred_bytes {
                    let remaining_bytes = p.total_bytes - p.transferred_bytes;
                    let remaining_secs = remaining_bytes as f64 / p.bytes_per_sec;
                    let eta_mins = (remaining_secs / 60.0).floor() as u64;
                    let eta_secs = (remaining_secs % 60.0).floor() as u64;
                    ui.label(format!("• ETA: {}m {}s", eta_mins, eta_secs));
                }
            });

            if !p.status_text.is_empty() {
                ui.label(
                    egui::RichText::new(&p.status_text)
                        .color(egui::Color32::from_rgb(226, 232, 240)),
                );
            }

            if let Some(ref err) = p.error {
                ui.label(
                    egui::RichText::new(format!("Error: {}", err))
                        .color(egui::Color32::from_rgb(239, 68, 68)),
                );
            }
        });
    }
}
