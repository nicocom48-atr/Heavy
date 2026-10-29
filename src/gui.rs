use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

use crate::network::get_available_local_ips;
use crate::receiver::{run_receiver, ReceiverConfig};
use crate::sender::{run_sender, SenderConfig};
use crate::transfer_state::{new_shared_progress, SharedProgress, TransferRole, TransferStatus};

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
    send_show_advanced: bool,

    // Receive state
    recv_address: String,
    recv_output_dir: PathBuf,
    recv_token: String,
    recv_force: bool,
    recv_show_advanced: bool,

    // Runtime & Transfer task
    tokio_rt: Arc<Runtime>,
    current_task: Option<JoinHandle<()>>,
    shared_progress: SharedProgress,

    // Network interfaces
    available_ips: Vec<(String, String)>,

    // UI feedback
    copied_toast_time: Option<Instant>,
    copied_toast_text: String,
}

impl HeavyDropApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: Arc<Runtime>) -> Self {
        // Apply custom modern dark theme
        Self::apply_custom_theme(&cc.egui_ctx);

        let available_ips = get_available_local_ips()
            .into_iter()
            .map(|(iface, ip)| (iface, ip.to_string()))
            .collect();

        let default_output_dir = dirs_next_output_dir();

        Self {
            active_tab: ActiveTab::Send,
            send_file_path: None,
            send_port_str: "7878".to_string(),
            send_token: String::new(),
            send_skip_prehash: false,
            send_show_advanced: false,

            recv_address: String::new(),
            recv_output_dir: default_output_dir,
            recv_token: String::new(),
            recv_force: false,
            recv_show_advanced: false,

            tokio_rt: rt,
            current_task: None,
            shared_progress: new_shared_progress(),
            available_ips,

            copied_toast_time: None,
            copied_toast_text: String::new(),
        }
    }

    fn apply_custom_theme(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        // Background colors
        visuals.window_fill = egui::Color32::from_rgb(15, 23, 42); // slate-900
        visuals.panel_fill = egui::Color32::from_rgb(15, 23, 42);
        visuals.extreme_bg_color = egui::Color32::from_rgb(2, 6, 23); // slate-950

        // Widgets
        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(30, 41, 59); // slate-800
        visuals.widgets.noninteractive.rounding = egui::Rounding::same(8.0);
        visuals.widgets.noninteractive.fg_stroke.color = egui::Color32::from_rgb(226, 232, 240); // slate-200

        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(30, 41, 59);
        visuals.widgets.inactive.rounding = egui::Rounding::same(8.0);
        visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(203, 213, 225);

        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(51, 65, 85);
        visuals.widgets.hovered.rounding = egui::Rounding::same(8.0);
        visuals.widgets.hovered.fg_stroke.color = egui::Color32::WHITE;

        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(14, 165, 233); // sky-500
        visuals.widgets.active.rounding = egui::Rounding::same(8.0);
        visuals.widgets.active.fg_stroke.color = egui::Color32::WHITE;

        visuals.selection.bg_fill = egui::Color32::from_rgb(2, 132, 199);
        visuals.selection.stroke.color = egui::Color32::WHITE;

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.spacing.button_padding = egui::vec2(12.0, 8.0);
        ctx.set_visuals(visuals);
        ctx.set_style(style);
    }

    fn cancel_current_transfer(&mut self) {
        if let Some(task) = self.current_task.take() {
            task.abort();
        }
        let mut p = self.shared_progress.lock();
        p.status = TransferStatus::Cancelled;
        p.status_text = "Transferencia cancelada por el usuario.".to_string();
        p.bytes_per_sec = 0.0;
    }

    fn is_running(&self) -> bool {
        let p = self.shared_progress.lock();
        matches!(
            p.status,
            TransferStatus::Preparing
                | TransferStatus::WaitingForPeer
                | TransferStatus::Connected
                | TransferStatus::Transferring
        )
    }

    fn copy_to_clipboard(&mut self, text: String, ui: &mut egui::Ui) {
        ui.output_mut(|o| o.copied_text = text.clone());
        self.copied_toast_text = format!("¡Copiado: {}!", text);
        self.copied_toast_time = Some(Instant::now());
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
        // Redraw at ~30 FPS during active transfers
        if self.is_running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }

        // Handle Drag & Drop of files anywhere in the window
        ctx.input(|i| {
            if let Some(dropped) = i.raw.dropped_files.first() {
                if let Some(ref path) = dropped.path {
                    if path.is_file() {
                        self.active_tab = ActiveTab::Send;
                        self.send_file_path = Some(path.clone());
                    }
                }
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // Header with App Title & Subtitle
            self.render_header(ui);

            ui.add_space(8.0);

            // Tab Bar (Segmented Control style)
            self.render_tab_selector(ui);

            ui.add_space(10.0);

            let is_busy = self.is_running();

            // Main Content Area in a ScrollArea to avoid cutoffs
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                match self.active_tab {
                    ActiveTab::Send => self.render_send_view(ui, is_busy),
                    ActiveTab::Receive => self.render_receive_view(ui, is_busy),
                }

                ui.add_space(14.0);
                self.render_progress_card(ui);
                ui.add_space(14.0);
            });
        });
    }
}

impl HeavyDropApp {
    fn render_header(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(ui.available_width() / 2.0 - 150.0);
                ui.label(
                    egui::RichText::new("⚡")
                        .size(32.0)
                        .color(egui::Color32::from_rgb(56, 189, 248)),
                );
                ui.heading(
                    egui::RichText::new("HeavyDrop")
                        .size(28.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                );
                ui.label(
                    egui::RichText::new("v0.1 P2P")
                        .size(11.0)
                        .background_color(egui::Color32::from_rgb(30, 41, 59))
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );
            });

            ui.label(
                egui::RichText::new("Envía y recibe archivos pesados (50 GB+) punto a punto sin intermediarios")
                    .size(12.5)
                    .color(egui::Color32::from_rgb(148, 163, 184)),
            );
        });
    }

    fn render_tab_selector(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let total_w = ui.available_width();
            let btn_w = (total_w - 12.0) / 2.0;

            let send_active = self.active_tab == ActiveTab::Send;
            let recv_active = self.active_tab == ActiveTab::Receive;

            let (send_bg, send_fg) = if send_active {
                (egui::Color32::from_rgb(14, 165, 233), egui::Color32::WHITE)
            } else {
                (egui::Color32::from_rgb(30, 41, 59), egui::Color32::from_rgb(148, 163, 184))
            };

            let send_btn = egui::Button::new(
                egui::RichText::new("📤  ENVIAR ARCHIVO").size(14.0).strong().color(send_fg),
            )
            .fill(send_bg)
            .min_size(egui::vec2(btn_w, 40.0));

            if ui.add(send_btn).clicked() {
                self.active_tab = ActiveTab::Send;
            }

            let (recv_bg, recv_fg) = if recv_active {
                (egui::Color32::from_rgb(16, 185, 129), egui::Color32::WHITE)
            } else {
                (egui::Color32::from_rgb(30, 41, 59), egui::Color32::from_rgb(148, 163, 184))
            };

            let recv_btn = egui::Button::new(
                egui::RichText::new("📥  RECIBIR ARCHIVO").size(14.0).strong().color(recv_fg),
            )
            .fill(recv_bg)
            .min_size(egui::vec2(btn_w, 40.0));

            if ui.add(recv_btn).clicked() {
                self.active_tab = ActiveTab::Receive;
            }
        });
    }

    fn render_send_view(&mut self, ui: &mut egui::Ui, is_busy: bool) {
        let mut new_picked_file = None;

        // Step 1: File Selection Card / Dropzone
        egui::Frame::none()
            .fill(egui::Color32::from_rgb(30, 41, 59))
            .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(51, 65, 85)))
            .rounding(egui::Rounding::same(10.0))
            .inner_margin(egui::Margin::same(16.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("1").strong().size(16.0).color(egui::Color32::from_rgb(56, 189, 248)));
                    ui.label(egui::RichText::new("Selecciona el archivo a transferir").size(15.0).strong().color(egui::Color32::WHITE));
                });
                ui.add_space(6.0);

                if let Some(ref path) = self.send_file_path {
                    // File is selected: show rich card
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(15, 23, 42))
                        .rounding(egui::Rounding::same(8.0))
                        .inner_margin(egui::Margin::same(12.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("📁").size(28.0));
                                ui.vertical(|ui| {
                                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                                    ui.label(egui::RichText::new(name).size(14.0).strong().color(egui::Color32::WHITE));

                                    let size_str = match std::fs::metadata(path) {
                                        Ok(m) => {
                                            let gb = m.len() as f64 / (1024.0 * 1024.0 * 1024.0);
                                            if gb >= 1.0 {
                                                format!("{:.2} GB ({} bytes)", gb, m.len())
                                            } else {
                                                format!("{:.2} MB", m.len() as f64 / (1024.0 * 1024.0))
                                            }
                                        }
                                        Err(_) => "Tamaño desconocido".to_string(),
                                    };
                                    ui.label(egui::RichText::new(size_str).size(12.0).color(egui::Color32::from_rgb(56, 189, 248)));
                                });

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if !is_busy {
                                        if ui.button(egui::RichText::new("Cambiar").size(12.0)).clicked() {
                                            new_picked_file = rfd::FileDialog::new().pick_file();
                                        }
                                    }
                                });
                            });
                        });
                } else {
                    // Empty dropzone area
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(15, 23, 42))
                        .stroke(egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(71, 85, 105)))
                        .rounding(egui::Rounding::same(8.0))
                        .inner_margin(egui::Margin::same(24.0))
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(egui::RichText::new("📥").size(32.0));
                                ui.label(
                                    egui::RichText::new("Arrastra y suelta tu archivo aquí")
                                        .size(15.0)
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                );
                                ui.label(
                                    egui::RichText::new("o haz clic en el botón para explorarlo en tu computadora")
                                        .size(12.0)
                                        .color(egui::Color32::from_rgb(148, 163, 184)),
                                );
                                ui.add_space(8.0);

                                if ui.add_enabled(!is_busy, egui::Button::new(
                                    egui::RichText::new("📁  Explorar Archivo...").size(13.0).strong()
                                ).fill(egui::Color32::from_rgb(30, 41, 59))).clicked() {
                                    new_picked_file = rfd::FileDialog::new().pick_file();
                                }
                            });
                        });
                }
            });

        if let Some(picked) = new_picked_file {
            self.send_file_path = Some(picked);
        }

        ui.add_space(10.0);

        let mut to_copy = None;

        // Step 2: Connection sharing info
        egui::Frame::none()
            .fill(egui::Color32::from_rgb(30, 41, 59))
            .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(51, 65, 85)))
            .rounding(egui::Rounding::same(10.0))
            .inner_margin(egui::Margin::same(16.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("2").strong().size(16.0).color(egui::Color32::from_rgb(56, 189, 248)));
                    ui.label(egui::RichText::new("Tu dirección para compartir con el receptor").size(15.0).strong().color(egui::Color32::WHITE));
                });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Comparte una de estas direcciones con quien va a descargar el archivo:")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );
                ui.add_space(6.0);

                let port = self.send_port_str.clone();
                if self.available_ips.is_empty() {
                    let full_addr = format!("127.0.0.1:{}", port);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("• Local: {}", full_addr)).strong());
                        if ui.button("📋 Copiar").clicked() {
                            to_copy = Some(full_addr);
                        }
                    });
                } else {
                    for (iface, ip) in &self.available_ips {
                        let full_addr = format!("{}:{}", ip, port);
                        let badge = if iface.to_lowercase().contains("tailscale") || ip.starts_with("100.") {
                            "VPN / Tailscale"
                        } else if iface.to_lowercase().contains("wi-fi") || iface.to_lowercase().contains("wlan") {
                            "Wi-Fi LAN"
                        } else {
                            "Red Local"
                        };

                        egui::Frame::none()
                            .fill(egui::Color32::from_rgb(15, 23, 42))
                            .rounding(egui::Rounding::same(6.0))
                            .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("[{}]", badge))
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(56, 189, 248)),
                                    );
                                    ui.label(egui::RichText::new(&full_addr).size(13.0).strong().color(egui::Color32::WHITE));

                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.button(egui::RichText::new("📋 Copiar").size(12.0)).clicked() {
                                            to_copy = Some(full_addr.clone());
                                        }
                                    });
                                });
                            });
                        ui.add_space(2.0);
                    }
                }

                // Show toast notification if copied
                if let Some(t) = self.copied_toast_time {
                    if t.elapsed().as_secs() < 3 {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(format!("✓ {}", self.copied_toast_text))
                                .size(12.0)
                                .color(egui::Color32::from_rgb(52, 211, 153)),
                        );
                    }
                }
            });

        if let Some(addr) = to_copy {
            self.copy_to_clipboard(addr, ui);
        }

        ui.add_space(8.0);

        // Collapsible Advanced Settings
        egui::CollapsingHeader::new(egui::RichText::new("⚙️  Opciones avanzadas (Opcional)").size(13.0).color(egui::Color32::from_rgb(148, 163, 184)))
            .default_open(self.send_show_advanced)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Puerto TCP:");
                    ui.add_enabled(!is_busy, egui::TextEdit::singleline(&mut self.send_port_str).desired_width(70.0));

                    ui.add_space(16.0);
                    ui.label("Contraseña / Token:");
                    ui.add_enabled(!is_busy, egui::TextEdit::singleline(&mut self.send_token).password(true).hint_text("Opcional").desired_width(120.0));
                });

                ui.checkbox(&mut self.send_skip_prehash, "Envío instantáneo (no esperar el cálculo del hash BLAKE3 inicial)");
            });

        ui.add_space(14.0);

        // Main Action Button
        ui.horizontal(|ui| {
            let can_start = !is_busy && self.send_file_path.is_some();
            let start_btn = egui::Button::new(
                egui::RichText::new("🚀  EMPEZAR A ENVIAR")
                    .size(15.0)
                    .strong()
                    .color(if can_start { egui::Color32::WHITE } else { egui::Color32::GRAY }),
            )
            .fill(if can_start { egui::Color32::from_rgb(14, 165, 233) } else { egui::Color32::from_rgb(30, 41, 59) })
            .min_size(egui::vec2(220.0, 44.0));

            if ui.add_enabled(can_start, start_btn).clicked() {
                self.start_send_task();
            }

            if is_busy {
                let cancel_btn = egui::Button::new(
                    egui::RichText::new("🛑  Cancelar").size(14.0).strong().color(egui::Color32::WHITE),
                )
                .fill(egui::Color32::from_rgb(225, 29, 72))
                .min_size(egui::vec2(130.0, 44.0));

                if ui.add(cancel_btn).clicked() {
                    self.cancel_current_transfer();
                }
            }
        });
    }

    fn render_receive_view(&mut self, ui: &mut egui::Ui, is_busy: bool) {
        // Step 1: Peer Address Input Card
        egui::Frame::none()
            .fill(egui::Color32::from_rgb(30, 41, 59))
            .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(51, 65, 85)))
            .rounding(egui::Rounding::same(10.0))
            .inner_margin(egui::Margin::same(16.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("1").strong().size(16.0).color(egui::Color32::from_rgb(16, 185, 129)));
                    ui.label(egui::RichText::new("Dirección del emisor").size(15.0).strong().color(egui::Color32::WHITE));
                });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Escribe o pega la IP y puerto que te compartió quien envía el archivo:")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    ui.add_enabled(
                        !is_busy,
                        egui::TextEdit::singleline(&mut self.recv_address)
                            .hint_text("Ejemplo: 192.168.1.45:7878 o 100.85.20.12:7878")
                            .desired_width(ui.available_width()),
                    );
                });
            });

        ui.add_space(10.0);

        // Step 2: Download Destination Folder Card
        egui::Frame::none()
            .fill(egui::Color32::from_rgb(30, 41, 59))
            .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(51, 65, 85)))
            .rounding(egui::Rounding::same(10.0))
            .inner_margin(egui::Margin::same(16.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("2").strong().size(16.0).color(egui::Color32::from_rgb(16, 185, 129)));
                    ui.label(egui::RichText::new("Carpeta de descarga").size(15.0).strong().color(egui::Color32::WHITE));
                });
                ui.add_space(4.0);

                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(15, 23, 42))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("📁").size(20.0));
                            ui.label(
                                egui::RichText::new(self.recv_output_dir.display().to_string())
                                    .size(13.0)
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            );

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if !is_busy {
                                    if ui.button(egui::RichText::new("Cambiar carpeta...").size(12.0)).clicked() {
                                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                            self.recv_output_dir = folder;
                                        }
                                    }
                                }
                            });
                        });
                    });
            });

        ui.add_space(8.0);

        // Collapsible Advanced Settings
        egui::CollapsingHeader::new(egui::RichText::new("⚙️  Opciones avanzadas (Opcional)").size(13.0).color(egui::Color32::from_rgb(148, 163, 184)))
            .default_open(self.recv_show_advanced)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Contraseña / Token requerida:");
                    ui.add_enabled(!is_busy, egui::TextEdit::singleline(&mut self.recv_token).password(true).desired_width(140.0));
                });
                ui.checkbox(&mut self.recv_force, "Sobrescribir archivo (ignorar la reanudación automática si ya existe)");
            });

        ui.add_space(14.0);

        // Action Button
        ui.horizontal(|ui| {
            let can_start = !is_busy && !self.recv_address.trim().is_empty();
            let start_btn = egui::Button::new(
                egui::RichText::new("📥  INICIAR DESCARGA")
                    .size(15.0)
                    .strong()
                    .color(if can_start { egui::Color32::WHITE } else { egui::Color32::GRAY }),
            )
            .fill(if can_start { egui::Color32::from_rgb(16, 185, 129) } else { egui::Color32::from_rgb(30, 41, 59) })
            .min_size(egui::vec2(220.0, 44.0));

            if ui.add_enabled(can_start, start_btn).clicked() {
                self.start_receive_task();
            }

            if is_busy {
                let cancel_btn = egui::Button::new(
                    egui::RichText::new("🛑  Cancelar").size(14.0).strong().color(egui::Color32::WHITE),
                )
                .fill(egui::Color32::from_rgb(225, 29, 72))
                .min_size(egui::vec2(130.0, 44.0));

                if ui.add(cancel_btn).clicked() {
                    self.cancel_current_transfer();
                }
            }
        });
    }

    fn render_progress_card(&self, ui: &mut egui::Ui) {
        let p = self.shared_progress.lock().clone();

        egui::Frame::none()
            .fill(egui::Color32::from_rgb(24, 32, 47))
            .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(51, 65, 85)))
            .rounding(egui::Rounding::same(12.0))
            .inner_margin(egui::Margin::same(16.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("📊").size(18.0));
                    ui.label(egui::RichText::new("Estado de la Transferencia").size(16.0).strong().color(egui::Color32::WHITE));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (badge_text, badge_color) = match p.status {
                            TransferStatus::Idle => ("INACTIVO", egui::Color32::from_rgb(100, 116, 139)),
                            TransferStatus::Preparing => ("PREPARANDO", egui::Color32::from_rgb(234, 179, 8)),
                            TransferStatus::WaitingForPeer => ("ESPERANDO RECEPTOR", egui::Color32::from_rgb(245, 158, 11)),
                            TransferStatus::Connected => ("CONECTADO", egui::Color32::from_rgb(56, 189, 248)),
                            TransferStatus::Transferring => ("TRANSFIRIENDO", egui::Color32::from_rgb(14, 165, 233)),
                            TransferStatus::Completed => ("COMPLETADO", egui::Color32::from_rgb(34, 197, 94)),
                            TransferStatus::Failed => ("ERROR", egui::Color32::from_rgb(239, 68, 68)),
                            TransferStatus::Cancelled => ("CANCELADO", egui::Color32::from_rgb(244, 63, 94)),
                        };

                        ui.label(
                            egui::RichText::new(badge_text)
                                .size(11.0)
                                .strong()
                                .background_color(badge_color)
                                .color(egui::Color32::BLACK),
                        );
                    });
                });

                ui.add_space(6.0);

                if !p.file_name.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("Archivo: {}", p.file_name))
                            .size(13.0)
                            .strong()
                            .color(egui::Color32::WHITE),
                    );
                }

                if !p.status_text.is_empty() {
                    ui.label(
                        egui::RichText::new(&p.status_text)
                            .size(12.5)
                            .color(egui::Color32::from_rgb(203, 213, 225)),
                    );
                }

                if let Some(ref err) = p.error {
                    ui.label(
                        egui::RichText::new(format!("⚠️ {}", err))
                            .size(12.5)
                            .color(egui::Color32::from_rgb(248, 113, 113)),
                    );
                }

                ui.add_space(6.0);

                // Progress Bar
                let progress_ratio = if p.total_bytes > 0 {
                    (p.transferred_bytes as f32) / (p.total_bytes as f32)
                } else {
                    0.0
                };

                let mb_sec = p.bytes_per_sec / (1024.0 * 1024.0);
                let transferred_gb = p.transferred_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                let total_gb = p.total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

                // ETA calculation
                let eta_str = if p.bytes_per_sec > 1024.0 && p.total_bytes > p.transferred_bytes {
                    let remaining_bytes = p.total_bytes - p.transferred_bytes;
                    let remaining_secs = (remaining_bytes as f64 / p.bytes_per_sec) as u64;
                    let mins = remaining_secs / 60;
                    let secs = remaining_secs % 60;
                    format!(" - Restante: {:02}:{:02}", mins, secs)
                } else {
                    String::new()
                };

                let bar_text = format!(
                    "{:.1}%  ({:.2} GB / {:.2} GB)  |  {:.1} MB/s{}",
                    progress_ratio * 100.0,
                    transferred_gb,
                    total_gb,
                    mb_sec,
                    eta_str
                );

                ui.add(
                    egui::ProgressBar::new(progress_ratio.clamp(0.0, 1.0))
                        .text(bar_text)
                        .animate(p.status == TransferStatus::Transferring),
                );

                if p.resume_offset > 0 {
                    let resumed_gb = p.resume_offset as f64 / (1024.0 * 1024.0 * 1024.0);
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "🔄 ¡Reanudación activa! Se conservaron {:.2} GB ya descargados previamente.",
                            resumed_gb
                        ))
                        .size(12.0)
                        .color(egui::Color32::from_rgb(251, 191, 36)),
                    );
                }
            });
    }

    fn start_send_task(&mut self) {
        let Some(file_path) = self.send_file_path.clone() else {
            return;
        };

        let port = self.send_port_str.parse::<u16>().unwrap_or(7878);
        let token = if self.send_token.trim().is_empty() {
            None
        } else {
            Some(self.send_token.trim().to_string())
        };
        let skip_prehash = self.send_skip_prehash;

        let sp = self.shared_progress.clone();
        {
            let mut p = sp.lock();
            p.status = TransferStatus::Preparing;
            p.role = TransferRole::Sender;
            p.transferred_bytes = 0;
            p.error = None;
            p.status_text = "Iniciando servidor de envío...".to_string();
        }

        let config = SenderConfig {
            file_path,
            bind_addr: "0.0.0.0".to_string(),
            port,
            chunk_size: 4 * 1024 * 1024,
            token,
            skip_prehash,
            shared_progress: Some(sp.clone()),
        };

        let handle = self.tokio_rt.spawn(async move {
            if let Err(e) = run_sender(config).await {
                let mut p = sp.lock();
                p.status = TransferStatus::Failed;
                p.error = Some(e.to_string());
            }
        });

        self.current_task = Some(handle);
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

        let sp = self.shared_progress.clone();
        {
            let mut p = sp.lock();
            p.status = TransferStatus::Preparing;
            p.role = TransferRole::Receiver;
            p.transferred_bytes = 0;
            p.error = None;
            p.status_text = "Conectando al emisor...".to_string();
        }

        let config = ReceiverConfig {
            peer_addr: addr,
            output_dir,
            token,
            force,
            shared_progress: Some(sp.clone()),
        };

        let handle = self.tokio_rt.spawn(async move {
            if let Err(e) = run_receiver(config).await {
                let mut p = sp.lock();
                p.status = TransferStatus::Failed;
                p.error = Some(e.to_string());
            }
        });

        self.current_task = Some(handle);
    }
}
