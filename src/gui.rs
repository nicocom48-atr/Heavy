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
        // Pitch black background matching the mockup
        visuals.window_fill = egui::Color32::from_rgb(0, 0, 0);
        visuals.panel_fill = egui::Color32::from_rgb(0, 0, 0);
        visuals.extreme_bg_color = egui::Color32::from_rgb(10, 10, 12);

        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(18, 18, 22);
        visuals.widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(39, 39, 42)); // zinc-800
        visuals.widgets.noninteractive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(244, 244, 245));

        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(24, 24, 28);
        visuals.widgets.inactive.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(39, 39, 42));
        visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(244, 244, 245));
        visuals.widgets.inactive.rounding = egui::Rounding::same(20.0);

        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(39, 39, 44);
        visuals.widgets.hovered.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(63, 63, 70));
        visuals.widgets.hovered.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(255, 255, 255));
        visuals.widgets.hovered.rounding = egui::Rounding::same(20.0);

        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(63, 63, 70);
        visuals.widgets.active.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(113, 113, 122));
        visuals.widgets.active.rounding = egui::Rounding::same(20.0);

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

// ----------------------------------------------------------------------------
// Custom Vector Drawing Helpers (Crisp on all screens, zero font dependencies)
// ----------------------------------------------------------------------------

fn draw_upload_icon(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.8_f32, color);

    // Tray at bottom
    let y_tray = rect.max.y - 2.0;
    painter.line_segment(
        [
            egui::pos2(rect.min.x + 2.0, y_tray - 3.0),
            egui::pos2(rect.min.x + 2.0, y_tray),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.min.x + 2.0, y_tray),
            egui::pos2(rect.max.x - 2.0, y_tray),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.max.x - 2.0, y_tray),
            egui::pos2(rect.max.x - 2.0, y_tray - 3.0),
        ],
        stroke,
    );

    // Arrow up
    let cx = rect.center().x;
    painter.line_segment(
        [
            egui::pos2(cx, y_tray - 2.0),
            egui::pos2(cx, rect.min.y + 2.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(cx - 3.5, rect.min.y + 5.5),
            egui::pos2(cx, rect.min.y + 2.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(cx + 3.5, rect.min.y + 5.5),
            egui::pos2(cx, rect.min.y + 2.0),
        ],
        stroke,
    );
}

fn draw_download_icon(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.8_f32, color);

    // Tray at bottom
    let y_tray = rect.max.y - 2.0;
    painter.line_segment(
        [
            egui::pos2(rect.min.x + 2.0, y_tray - 3.0),
            egui::pos2(rect.min.x + 2.0, y_tray),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.min.x + 2.0, y_tray),
            egui::pos2(rect.max.x - 2.0, y_tray),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.max.x - 2.0, y_tray),
            egui::pos2(rect.max.x - 2.0, y_tray - 3.0),
        ],
        stroke,
    );

    // Arrow down
    let cx = rect.center().x;
    painter.line_segment(
        [
            egui::pos2(cx, rect.min.y + 2.0),
            egui::pos2(cx, y_tray - 2.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(cx - 3.5, y_tray - 5.5),
            egui::pos2(cx, y_tray - 2.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(cx + 3.5, y_tray - 5.5),
            egui::pos2(cx, y_tray - 2.0),
        ],
        stroke,
    );
}

fn draw_file_icon(ui: &mut egui::Ui, size: f32, color: egui::Color32) {
    let width = size * 0.75;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, size), egui::Sense::hover());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.4_f32, color);
    let fold = size * 0.28;

    let p0 = rect.min;
    let p1 = egui::pos2(rect.max.x - fold, rect.min.y);
    let p2 = egui::pos2(rect.max.x, rect.min.y + fold);
    let p3 = rect.max;
    let p4 = egui::pos2(rect.min.x, rect.max.y);

    painter.line_segment([p0, p1], stroke);
    painter.line_segment([p1, p2], stroke);
    painter.line_segment([p2, p3], stroke);
    painter.line_segment([p3, p4], stroke);
    painter.line_segment([p4, p0], stroke);

    let p_fold = egui::pos2(rect.max.x - fold, rect.min.y + fold);
    painter.line_segment([p1, p_fold], egui::Stroke::new(1.0_f32, color));
    painter.line_segment([p_fold, p2], egui::Stroke::new(1.0_f32, color));

    let y1 = rect.min.y + size * 0.52;
    let y2 = rect.min.y + size * 0.72;
    let line_color = egui::Color32::from_rgb(113, 113, 122);
    painter.line_segment(
        [
            egui::pos2(rect.min.x + 3.0, y1),
            egui::pos2(rect.max.x - 3.0, y1),
        ],
        egui::Stroke::new(1.0_f32, line_color),
    );
    painter.line_segment(
        [
            egui::pos2(rect.min.x + 3.0, y2),
            egui::pos2(rect.max.x - 4.5, y2),
        ],
        egui::Stroke::new(1.0_f32, line_color),
    );
}

fn draw_check_icon(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(2.0_f32, color);
    let p1 = egui::pos2(rect.min.x + 2.0, rect.min.y + 7.0);
    let p2 = egui::pos2(rect.min.x + 5.5, rect.min.y + 11.0);
    let p3 = egui::pos2(rect.min.x + 12.0, rect.min.y + 3.5);
    painter.line_segment([p1, p2], stroke);
    painter.line_segment([p2, p3], stroke);
}

fn draw_clock_icon(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.5_f32, color);
    let center = rect.center();
    painter.circle_stroke(center, 5.5, stroke);
    painter.line_segment([center, egui::pos2(center.x, center.y - 3.0)], stroke);
    painter.line_segment([center, egui::pos2(center.x + 2.2, center.y)], stroke);
}

// ----------------------------------------------------------------------------
// Application Main UI
// ----------------------------------------------------------------------------

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
                ui.add_space(4.0);

                // Window Header (matching mockup title)
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Cómo debería funcionar la interfaz")
                            .size(17.0)
                            .strong()
                            .color(egui::Color32::from_rgb(244, 244, 245)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("⚡ HeavyDrop")
                                .size(14.0)
                                .strong()
                                .color(egui::Color32::from_rgb(56, 189, 248)),
                        );
                    });
                });
                ui.add_space(10.0);

                // Toast notification
                if let Some(toast_time) = self.copied_toast_time {
                    if toast_time.elapsed().as_secs() < 3 {
                        ui.horizontal(|ui| {
                            draw_check_icon(ui, egui::Color32::from_rgb(34, 197, 94));
                            ui.label(
                                egui::RichText::new(&self.copied_toast_text)
                                    .color(egui::Color32::from_rgb(34, 197, 94))
                                    .strong(),
                            );
                        });
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

                // Main Outer Card (Exact layout to the user's mockup)
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgb(10, 10, 12)) // Deep black charcoal
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgb(39, 39, 42), // zinc-800
                    ))
                    .rounding(12.0)
                    .inner_margin(18.0)
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

                                ui.add_space(24.0);

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
                            // Responsive fallback for narrow windows
                            ui.horizontal(|ui| {
                                let send_active = self.active_tab == ActiveTab::Send;
                                if ui.selectable_label(send_active, "Enviar").clicked() && !is_busy {
                                    self.active_tab = ActiveTab::Send;
                                }
                                let recv_active = self.active_tab == ActiveTab::Receive;
                                if ui.selectable_label(recv_active, "Recibir").clicked() && !is_busy {
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

                        // Conceptual disclaimer at the bottom of the card (matching mockup)
                        ui.add_space(14.0);
                        ui.label(
                            egui::RichText::new(
                                "Diseño conceptual, no funcional: el código es ilustrativo y no está conectado a una sesión real.",
                            )
                            .size(11.0)
                            .color(egui::Color32::from_rgb(113, 113, 122)),
                        );
                    });

                // Architecture Recommendation Section (Matching Mockup bottom half)
                ui.add_space(20.0);
                self.render_architecture_section(ui);
                ui.add_space(12.0);
            });
        });
    }
}

impl HeavyDropApp {
    fn render_send_panel(&mut self, ui: &mut egui::Ui, is_busy: bool) {
        let session_opt = self.send_session.clone();

        // Card Header: Upload Icon + Enviar
        ui.horizontal(|ui| {
            draw_upload_icon(ui, egui::Color32::from_rgb(244, 244, 245));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Enviar")
                    .size(16.0)
                    .strong()
                    .color(egui::Color32::from_rgb(244, 244, 245)),
            );
        });
        ui.add_space(10.0);

        // 1. File Selection / Drop Card (width 100%)
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(18, 18, 22))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(39, 39, 42),
            ))
            .rounding(10.0)
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    if let Some(ref path) = self.send_file_path {
                        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                        let file_name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();

                        draw_file_icon(ui, 24.0, egui::Color32::from_rgb(161, 161, 170));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(file_name)
                                .size(15.0)
                                .strong()
                                .color(egui::Color32::from_rgb(244, 244, 245)),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{} ({:.2} GB)",
                                indicatif::HumanBytes(file_size),
                                file_size as f64 / (1024.0 * 1024.0 * 1024.0)
                            ))
                            .size(13.0)
                            .color(egui::Color32::from_rgb(161, 161, 170)),
                        );

                        if !is_busy && session_opt.is_none() {
                            ui.add_space(6.0);
                            let change_btn = ui.add(
                                egui::Button::new(
                                    egui::RichText::new("Cambiar archivo")
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(161, 161, 170)),
                                )
                                .fill(egui::Color32::from_rgb(28, 28, 32))
                                .rounding(20.0),
                            );
                            if change_btn.clicked() {
                                if let Some(file) = rfd::FileDialog::new().pick_file() {
                                    self.send_file_path = Some(file);
                                }
                            }
                        }
                    } else {
                        draw_file_icon(ui, 24.0, egui::Color32::from_rgb(113, 113, 122));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Arrastrá un archivo aquí")
                                .size(14.0)
                                .strong()
                                .color(egui::Color32::from_rgb(244, 244, 245)),
                        );
                        ui.label(
                            egui::RichText::new("o hacé clic para seleccionar (hasta 1 TB+)")
                                .size(11.0)
                                .color(egui::Color32::from_rgb(113, 113, 122)),
                        );
                        ui.add_space(8.0);
                        let pick_btn = ui.add(
                            egui::Button::new(
                                egui::RichText::new("Seleccionar archivo")
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(228, 228, 231)),
                            )
                            .fill(egui::Color32::from_rgb(34, 34, 38))
                            .rounding(20.0),
                        );
                        if pick_btn.clicked() {
                            if let Some(file) = rfd::FileDialog::new().pick_file() {
                                self.send_file_path = Some(file);
                            }
                        }
                    }
                });
            });

        ui.add_space(14.0);

        // 2. Section Label
        ui.label(
            egui::RichText::new("Código de conexión")
                .size(12.0)
                .color(egui::Color32::from_rgb(161, 161, 170)),
        );
        ui.add_space(4.0);

        // 3. Large Code Display Box (Pill shape matching mockup)
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(26, 26, 30)) // Dark pill fill
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(39, 39, 42),
            ))
            .rounding(20.0)
            .inner_margin(egui::Margin::symmetric(14.0, 10.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    if let Some(ref session) = session_opt {
                        ui.label(
                            egui::RichText::new(session.code().formatted())
                                .size(22.0)
                                .monospace()
                                .strong()
                                .color(egui::Color32::from_rgb(244, 244, 245)),
                        );
                    } else {
                        // Symmetrical placeholder matching mockup
                        ui.label(
                            egui::RichText::new("KD7M-9Q2P-W8RX")
                                .size(22.0)
                                .monospace()
                                .strong()
                                .color(egui::Color32::from_rgb(82, 82, 91)),
                        );
                    }
                });
            });

        ui.add_space(10.0);

        // 4. Action Button (Pill shaped)
        if let Some(ref session) = session_opt {
            // White Pill Button with pure black text (EXACTLY matching mockup)
            let copy_btn = ui.add_sized(
                [ui.available_width(), 40.0],
                egui::Button::new(
                    egui::RichText::new("Copiar código")
                        .size(14.0)
                        .strong()
                        .color(egui::Color32::from_rgb(9, 9, 11)),
                )
                .fill(egui::Color32::from_rgb(255, 255, 255))
                .rounding(20.0),
            );

            if copy_btn.clicked() {
                ui.output_mut(|o| o.copied_text = session.code().formatted());
                self.show_toast("¡Código copiado al portapapeles!");
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                draw_clock_icon(ui, egui::Color32::from_rgb(161, 161, 170));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!("Expira en {}", session.format_remaining()))
                        .size(12.0)
                        .color(egui::Color32::from_rgb(161, 161, 170)),
                );

                if !is_busy {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(
                                egui::RichText::new("Cancelar")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            )
                            .clicked()
                        {
                            self.cancel_current_transfer();
                        }
                    });
                }
            });
        } else {
            // "Crear transferencia" button
            let has_file = self.send_file_path.is_some();
            let btn_fill = if has_file {
                egui::Color32::from_rgb(2, 132, 199) // Sky blue if ready
            } else {
                egui::Color32::from_rgb(34, 34, 38)
            };
            let text_color = if has_file {
                egui::Color32::WHITE
            } else {
                egui::Color32::from_rgb(161, 161, 170)
            };

            let start_btn = ui.add_sized(
                [ui.available_width(), 40.0],
                egui::Button::new(
                    egui::RichText::new("⚡ Crear transferencia")
                        .size(14.0)
                        .strong()
                        .color(text_color),
                )
                .fill(btn_fill)
                .rounding(20.0),
            );

            if start_btn.clicked() {
                if has_file {
                    self.start_send_task();
                } else if let Some(file) = rfd::FileDialog::new().pick_file() {
                    self.send_file_path = Some(file);
                    self.start_send_task();
                }
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                draw_clock_icon(ui, egui::Color32::from_rgb(113, 113, 122));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Expira en 5 minutos tras crearse")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(113, 113, 122)),
                );
            });
        }

        if let Some(ref err) = self.send_error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("Error: {}", err))
                    .color(egui::Color32::from_rgb(239, 68, 68)),
            );
        }
    }

    fn render_receive_panel(&mut self, ui: &mut egui::Ui, is_busy: bool) {
        // Card Header: Download Icon + Recibir
        ui.horizontal(|ui| {
            draw_download_icon(ui, egui::Color32::from_rgb(244, 244, 245));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Recibir")
                    .size(16.0)
                    .strong()
                    .color(egui::Color32::from_rgb(244, 244, 245)),
            );
        });
        ui.add_space(10.0);

        // 1. Label
        ui.label(
            egui::RichText::new("Ingresá el código del emisor")
                .size(12.0)
                .color(egui::Color32::from_rgb(161, 161, 170)),
        );
        ui.add_space(4.0);

        // 2. Rounded Pill Input Field (matching mockup)
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(18, 18, 22))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(39, 39, 42),
            ))
            .rounding(20.0)
            .inner_margin(egui::Margin::symmetric(14.0, 8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let edit = ui.add_sized(
                    [ui.available_width(), 26.0],
                    egui::TextEdit::singleline(&mut self.recv_code_input)
                        .hint_text("XXXX-XXXX-XXXX")
                        .font(egui::TextStyle::Monospace)
                        .char_limit(16)
                        .frame(false),
                );
                if edit.changed() {
                    self.recv_code_input = self.recv_code_input.to_ascii_uppercase();
                }
            });

        ui.add_space(10.0);

        // 3. Button "Usar código" (Dark pill button matching mockup)
        if !is_busy {
            let has_code = PairingCode::parse(&self.recv_code_input).is_ok();
            let btn_fill = if has_code {
                egui::Color32::from_rgb(2, 132, 199) // Sky blue if valid code
            } else {
                egui::Color32::from_rgb(28, 28, 32) // Dark pill
            };

            let connect_btn = ui.add_sized(
                [ui.available_width(), 40.0],
                egui::Button::new(
                    egui::RichText::new("Usar código")
                        .size(14.0)
                        .strong()
                        .color(egui::Color32::from_rgb(244, 244, 245)),
                )
                .fill(btn_fill)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgb(45, 45, 50),
                ))
                .rounding(20.0),
            );

            if connect_btn.clicked() {
                self.start_receive_task();
            }
        } else {
            let cancel_btn = ui.add_sized(
                [ui.available_width(), 40.0],
                egui::Button::new(
                    egui::RichText::new("Cancelar conexión")
                        .size(14.0)
                        .strong()
                        .color(egui::Color32::from_rgb(239, 68, 68)),
                )
                .fill(egui::Color32::from_rgb(28, 28, 32))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgb(239, 68, 68),
                ))
                .rounding(20.0),
            );
            if cancel_btn.clicked() {
                self.cancel_current_transfer();
            }
        }

        ui.add_space(16.0);

        // 4. Status Items (matching mockup clean list)
        ui.horizontal(|ui| {
            draw_file_icon(ui, 18.0, egui::Color32::from_rgb(161, 161, 170));
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new("Archivo pendiente")
                        .size(13.0)
                        .strong()
                        .color(egui::Color32::from_rgb(244, 244, 245)),
                );
                ui.label(
                    egui::RichText::new("Se muestra tras conectarse")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(113, 113, 122)),
                );
            });
        });

        ui.add_space(10.0);

        ui.horizontal(|ui| {
            draw_check_icon(ui, egui::Color32::from_rgb(34, 197, 94));
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Conexión cifrada")
                    .size(12.0)
                    .color(egui::Color32::from_rgb(161, 161, 170)),
            );
        });

        ui.add_space(12.0);

        // 5. Destination folder selector
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Carpeta:")
                    .size(11.0)
                    .color(egui::Color32::from_rgb(113, 113, 122)),
            );
            let dir_str = self.recv_output_dir.display().to_string();
            let truncated = if dir_str.len() > 24 {
                format!("...{}", &dir_str[dir_str.len() - 20..])
            } else {
                dir_str
            };
            ui.label(
                egui::RichText::new(truncated)
                    .size(11.0)
                    .color(egui::Color32::from_rgb(56, 189, 248)),
            );
            if !is_busy {
                let change_btn = ui.add(
                    egui::Button::new(
                        egui::RichText::new("Cambiar")
                            .size(10.0)
                            .color(egui::Color32::from_rgb(161, 161, 170)),
                    )
                    .fill(egui::Color32::from_rgb(28, 28, 32))
                    .rounding(10.0),
                );
                if change_btn.clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.recv_output_dir = dir;
                    }
                }
            }
        });

        if let Some(ref err) = self.recv_error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("Error: {}", err))
                    .color(egui::Color32::from_rgb(239, 68, 68)),
            );
        }
    }

    fn render_architecture_section(&self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Recomendación de arquitectura")
                .size(15.0)
                .strong()
                .color(egui::Color32::from_rgb(244, 244, 245)),
        );
        ui.add_space(8.0);

        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(10, 10, 12))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(39, 39, 42),
            ))
            .rounding(12.0)
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    // Node 1: Emisor
                    ui.label(
                        egui::RichText::new("HeavyDrop emisor")
                            .size(14.0)
                            .strong()
                            .color(egui::Color32::from_rgb(244, 244, 245)),
                    );
                    ui.label(
                        egui::RichText::new("Genera sesión y código temporal")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(113, 113, 122)),
                    );

                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("⇅  Registro y resolución de código")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(113, 113, 122)),
                    );
                    ui.add_space(4.0);

                    // Node 2: Servidor de señalización (Center Box)
                    egui::Frame::group(ui.style())
                        .fill(egui::Color32::from_rgb(18, 18, 22))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgb(39, 39, 42),
                        ))
                        .rounding(8.0)
                        .inner_margin(egui::Margin::symmetric(30.0, 10.0))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("Servidor de señalización")
                                    .size(14.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(244, 244, 245)),
                            );
                            ui.label(
                                egui::RichText::new("Coordina conexión sin almacenar archivos")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(113, 113, 122)),
                            );
                        });

                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("⇅  Intercambio de información de conexión")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(113, 113, 122)),
                    );
                    ui.add_space(4.0);

                    // Node 3: Receptor
                    ui.label(
                        egui::RichText::new("HeavyDrop receptor")
                            .size(14.0)
                            .strong()
                            .color(egui::Color32::from_rgb(244, 244, 245)),
                    );
                    ui.label(
                        egui::RichText::new("Introduce el código y acepta el archivo")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(113, 113, 122)),
                    );

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // Node 4: Preferred Data Route
                    ui.label(
                        egui::RichText::new("Ruta de datos preferida")
                            .size(12.0)
                            .strong()
                            .color(egui::Color32::from_rgb(161, 161, 170)),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("💻 ⇆ 💻")
                            .size(16.0)
                            .color(egui::Color32::from_rgb(34, 197, 94)),
                    );
                    ui.label(
                        egui::RichText::new("P2P directo; relay cifrado como alternativa")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(161, 161, 170)),
                    );
                });
            });
    }

    fn render_transfer_status(
        &mut self,
        ui: &mut egui::Ui,
        progress: &heavydrop::transfer_state::TransferProgress,
    ) {
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(18, 18, 22))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(56, 189, 248),
            ))
            .rounding(10.0)
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
