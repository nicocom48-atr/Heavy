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
use heavydrop::transfer_state::{ConnectionType, TransferProgress, TransferRole, TransferStatus};

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
    send_manager: TransferManager,

    // Receive state
    recv_code_input: String,
    recv_output_dir: PathBuf,
    recv_error: Option<String>,
    recv_manager: TransferManager,

    // Services
    discovery: Arc<DiscoveryService>,
    tokio_rt: Arc<Runtime>,
    signal_server_input: String,

    // UI feedback
    copied_toast_time: Option<Instant>,
    copied_toast_text: String,

    // Bandwidth Speed Limiters & Sidebar
    upload_limiter: heavydrop::speed_limiter::SpeedLimiter,
    download_limiter: heavydrop::speed_limiter::SpeedLimiter,
    speed_sidebar_open: bool,
    upload_limit_mb: u32,
    download_limit_mb: u32,
}

impl HeavyDropApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: Arc<Runtime>) -> Self {
        Self::apply_custom_theme(&cc.egui_ctx);

        let default_output_dir = dirs_next_output_dir();
        let send_manager = TransferManager::new();
        let recv_manager = TransferManager::new();
        let discovery = Arc::new(DiscoveryService::new());
        let upload_limiter = heavydrop::speed_limiter::SpeedLimiter::default();
        let download_limiter = heavydrop::speed_limiter::SpeedLimiter::default();

        Self {
            active_tab: ActiveTab::Send,
            send_file_path: None,
            send_session: None,
            send_error: None,
            send_manager,

            recv_code_input: String::new(),
            recv_output_dir: default_output_dir,
            recv_error: None,
            recv_manager,

            discovery,
            tokio_rt: rt,
            signal_server_input: heavydrop::signaling::SignalingClient::default_server()
                .unwrap_or_default(),

            copied_toast_time: None,
            copied_toast_text: String::new(),

            upload_limiter,
            download_limiter,
            speed_sidebar_open: false,
            upload_limit_mb: 0,
            download_limit_mb: 0,
        }
    }

    fn apply_custom_theme(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();

        // Deep OLED dark mode palette
        visuals.window_fill = egui::Color32::from_rgb(9, 9, 12);
        visuals.panel_fill = egui::Color32::from_rgb(9, 9, 12);
        visuals.extreme_bg_color = egui::Color32::from_rgb(14, 14, 18);

        // 1px solid rgba(255, 255, 255, 0.08)
        let subtle_border = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20);

        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(16, 16, 20);
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, subtle_border);
        visuals.widgets.noninteractive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(244, 244, 245));

        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(22, 22, 28);
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, subtle_border);
        visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(244, 244, 245));
        visuals.widgets.inactive.rounding = egui::Rounding::same(20.0);

        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(34, 34, 42);
        visuals.widgets.hovered.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(56, 189, 248)); // Cyan accent on hover
        visuals.widgets.hovered.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(255, 255, 255));
        visuals.widgets.hovered.rounding = egui::Rounding::same(20.0);

        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(45, 45, 56);
        visuals.widgets.active.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(14, 165, 233));
        visuals.widgets.active.rounding = egui::Rounding::same(20.0);

        ctx.set_visuals(visuals);
    }

    fn show_toast(&mut self, text: impl Into<String>) {
        self.copied_toast_text = text.into();
        self.copied_toast_time = Some(Instant::now());
    }

    fn signal_server(&self) -> Option<String> {
        let server = self.signal_server_input.trim();
        (!server.is_empty()).then(|| server.to_string())
    }

    fn cancel_send_transfer(&mut self) {
        self.send_manager.cancel();
        self.send_session = None;
    }

    fn cancel_recv_transfer(&mut self) {
        self.recv_manager.cancel();
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
            transport_mode: if self.signal_server().is_some() {
                heavydrop::automatic::TransportMode::LegacyTcp
            } else {
                heavydrop::automatic::TransportMode::Automatic
            },
            file_path,
            session,
            preferred_port: 7878,
            chunk_size: 4 * 1024 * 1024,
            skip_prehash: false,
            signaling_server: self.signal_server(),
            shared_progress: None,
            cancel_signal: None,
            speed_limiter: Some(self.upload_limiter.clone()),
        };

        let mgr = self.send_manager.clone();
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
            signaling_server: self.signal_server(),
            shared_progress: None,
            cancel_signal: None,
            discovery: Some(self.discovery.clone()),
            speed_limiter: Some(self.download_limiter.clone()),
        };

        let mgr = self.recv_manager.clone();
        self.tokio_rt.spawn(async move {
            let _ = mgr.run_recv(config).await;
        });
    }
}

// ----------------------------------------------------------------------------
// Custom Vector Drawing Helpers (Crisp on high-DPI, zero font dependencies)
// ----------------------------------------------------------------------------

fn draw_bolt_icon(ui: &mut egui::Ui, size: f32, color: egui::Color32) {
    let width = size * 0.65;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, size), egui::Sense::hover());
    let painter = ui.painter();
    let points = [
        egui::pos2(rect.min.x + width * 0.58, rect.min.y),
        egui::pos2(rect.min.x + width * 0.05, rect.min.y + size * 0.52),
        egui::pos2(rect.min.x + width * 0.46, rect.min.y + size * 0.52),
        egui::pos2(rect.min.x + width * 0.38, rect.max.y),
        egui::pos2(rect.max.x - width * 0.05, rect.min.y + size * 0.42),
        egui::pos2(rect.min.x + width * 0.52, rect.min.y + size * 0.42),
    ];
    painter.add(egui::Shape::convex_polygon(
        points.to_vec(),
        color,
        egui::Stroke::NONE,
    ));
}

fn draw_upload_icon(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.8_f32, color);

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
    let line_color = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 120);
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

fn draw_folder_icon(ui: &mut egui::Ui, size: f32, color: egui::Color32) {
    let width = size * 1.25;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, size), egui::Sense::hover());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.4_f32, color);

    // Tab at top
    let p0 = egui::pos2(rect.min.x + 1.0, rect.min.y + 2.0);
    let p1 = egui::pos2(rect.min.x + width * 0.44, rect.min.y + 2.0);
    let p2 = egui::pos2(rect.min.x + width * 0.54, rect.min.y + 4.5);
    painter.line_segment([p0, p1], stroke);
    painter.line_segment([p1, p2], stroke);

    // Folder body
    let body_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + 1.0, rect.min.y + 4.5),
        egui::pos2(rect.max.x - 1.0, rect.max.y - 1.0),
    );
    painter.rect_stroke(body_rect, 2.0, stroke);
}

fn draw_shield_icon(ui: &mut egui::Ui, size: f32, color: egui::Color32) {
    let width = size * 0.85;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, size), egui::Sense::hover());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.3_f32, color);
    let cx = rect.center().x;
    let top_y = rect.min.y + 1.0;
    let mid_y = rect.min.y + size * 0.45;
    let bot_y = rect.max.y - 1.0;

    painter.line_segment(
        [
            egui::pos2(rect.min.x + 1.5, top_y),
            egui::pos2(rect.max.x - 1.5, top_y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.min.x + 1.5, top_y),
            egui::pos2(rect.min.x + 1.5, mid_y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.max.x - 1.5, top_y),
            egui::pos2(rect.max.x - 1.5, mid_y),
        ],
        stroke,
    );
    painter.line_segment(
        [egui::pos2(rect.min.x + 1.5, mid_y), egui::pos2(cx, bot_y)],
        stroke,
    );
    painter.line_segment(
        [egui::pos2(rect.max.x - 1.5, mid_y), egui::pos2(cx, bot_y)],
        stroke,
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

fn draw_dashed_border(
    painter: &egui::Painter,
    rect: egui::Rect,
    rounding: f32,
    stroke: egui::Stroke,
    dash_len: f32,
    gap_len: f32,
) {
    let r = rect;
    let pad = rounding.max(6.0);

    // Top edge
    let mut x = r.min.x + pad;
    while x < r.max.x - pad {
        let x_end = (x + dash_len).min(r.max.x - pad);
        painter.line_segment([egui::pos2(x, r.min.y), egui::pos2(x_end, r.min.y)], stroke);
        x += dash_len + gap_len;
    }

    // Bottom edge
    let mut x = r.min.x + pad;
    while x < r.max.x - pad {
        let x_end = (x + dash_len).min(r.max.x - pad);
        painter.line_segment([egui::pos2(x, r.max.y), egui::pos2(x_end, r.max.y)], stroke);
        x += dash_len + gap_len;
    }

    // Left edge
    let mut y = r.min.y + pad;
    while y < r.max.y - pad {
        let y_end = (y + dash_len).min(r.max.y - pad);
        painter.line_segment([egui::pos2(r.min.x, y), egui::pos2(r.min.x, y_end)], stroke);
        y += dash_len + gap_len;
    }

    // Right edge
    let mut y = r.min.y + pad;
    while y < r.max.y - pad {
        let y_end = (y + dash_len).min(r.max.y - pad);
        painter.line_segment([egui::pos2(r.max.x, y), egui::pos2(r.max.x, y_end)], stroke);
        y += dash_len + gap_len;
    }
}

// ----------------------------------------------------------------------------
// Application Main UI
// ----------------------------------------------------------------------------

impl eframe::App for HeavyDropApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let is_send_busy = self.send_manager.is_busy();
        let is_recv_busy = self.recv_manager.is_busy();
        if is_send_busy || is_recv_busy || self.send_session.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        // Handle Drag & Drop: drop any file into the window
        let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(file) = dropped_files.first() {
            if let Some(ref path) = file.path {
                if path.is_file() && !is_send_busy {
                    self.send_file_path = Some(path.clone());
                    self.send_session = None;
                    self.active_tab = ActiveTab::Send;
                }
            }
        }

        egui::TopBottomPanel::bottom("internet_settings").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Servidor manual (opcional):");
                ui.add_enabled(
                    !is_send_busy && !is_recv_busy,
                    egui::TextEdit::singleline(&mut self.signal_server_input)
                        .hint_text("servidor:7880")
                        .desired_width(240.0),
                );
            });
            ui.small(if self.signal_server().is_some() {
                "Ambos equipos deben usar el mismo servidor de señalización y relay."
            } else {
                "Conexión automática: crea una transferencia y comparte la invitación completa."
            });
        });

        if self.speed_sidebar_open {
            egui::SidePanel::right("speed_sidebar")
                .resizable(false)
                .default_width(310.0)
                .frame(
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(13, 13, 17))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 22),
                        ))
                        .inner_margin(egui::Margin::symmetric(18.0, 20.0)),
                )
                .show(ctx, |ui| {
                    self.render_speed_sidebar(ui);
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let total_width = ui.available_width();
            let total_height = ui.available_height();

            // Balanced container width (max 940px)
            let max_card_width = 940.0_f32;
            let content_width = total_width.min(max_card_width);
            let h_margin = ((total_width - content_width) / 2.0).max(0.0);

            // Subtle vertical centering so the card sits comfortably in the window
            let v_padding = ((total_height - 540.0) / 2.0).clamp(8.0, 48.0);

            ui.add_space(v_padding);

            ui.horizontal(|ui| {
                if h_margin > 0.0 {
                    ui.add_space(h_margin);
                }

                ui.allocate_ui_with_layout(
                    egui::vec2(content_width, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        // 1. Clean Top Header: Logo + Title + Discrete Network State
                        self.render_header(ui);
                        ui.add_space(14.0);

                        // 2. Toast Feedback notification (if active)
                        self.render_toast(ui);

                        // 3. Main Center Card ("Enviar" y "Recibir")
                        egui::Frame::group(ui.style())
                            .fill(egui::Color32::from_rgb(12, 12, 16))
                            .stroke(egui::Stroke::new(
                                1.0_f32,
                                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                            ))
                            .rounding(16.0)
                            .inner_margin(22.0)
                            .show(ui, |ui| {
                                let inner_width = ui.available_width();
                                let is_wide = inner_width >= 620.0;

                                if is_wide {
                                    let col_width = (inner_width - 28.0) / 2.0;

                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::TOP),
                                        |ui| {
                                            // Left Column: Enviar
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(col_width, 0.0),
                                                egui::Layout::top_down(egui::Align::LEFT),
                                                |ui| {
                                                    self.render_send_panel(ui, is_send_busy);
                                                },
                                            );

                                            ui.add_space(28.0);

                                            // Right Column: Recibir
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(col_width, 0.0),
                                                egui::Layout::top_down(egui::Align::LEFT),
                                                |ui| {
                                                    self.render_receive_panel(ui, is_recv_busy);
                                                },
                                            );
                                        },
                                    );
                                } else {
                                    // Responsive fallback for compact windows
                                    ui.horizontal(|ui| {
                                        let send_active = self.active_tab == ActiveTab::Send;
                                        if ui.selectable_label(send_active, "Enviar").clicked()
                                            && !is_send_busy
                                        {
                                            self.active_tab = ActiveTab::Send;
                                        }
                                        let recv_active = self.active_tab == ActiveTab::Receive;
                                        if ui.selectable_label(recv_active, "Recibir").clicked()
                                            && !is_recv_busy
                                        {
                                            self.active_tab = ActiveTab::Receive;
                                        }
                                    });
                                    ui.separator();
                                    ui.add_space(8.0);

                                    match self.active_tab {
                                        ActiveTab::Send => self.render_send_panel(ui, is_send_busy),
                                        ActiveTab::Receive => {
                                            self.render_receive_panel(ui, is_recv_busy)
                                        }
                                    }
                                }
                            });
                    },
                );

                if h_margin > 0.0 {
                    ui.add_space(h_margin);
                }
            });
        });
    }
}

impl HeavyDropApp {
    fn render_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Left: HeavyDrop Brand + Tech Badge
            draw_bolt_icon(ui, 20.0, egui::Color32::from_rgb(14, 165, 233)); // Electric Cyan
            ui.add_space(5.0);
            ui.label(
                egui::RichText::new("HeavyDrop")
                    .size(20.0)
                    .strong()
                    .color(egui::Color32::from_rgb(250, 250, 250)),
            );
            ui.add_space(8.0);

            // Subtle tag
            egui::Frame::group(ui.style())
                .fill(egui::Color32::from_rgb(18, 18, 24))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                ))
                .rounding(10.0)
                .inner_margin(egui::Margin::symmetric(8.0, 3.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("P2P 50GB+")
                            .size(10.5)
                            .strong()
                            .color(egui::Color32::from_rgb(148, 163, 184)),
                    );
                });

            // Right: Speed Limiter Toggle + Discrete Network Status Badge
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Button to toggle Speed Limiter Sidebar
                let btn_text = if self.upload_limit_mb > 0 || self.download_limit_mb > 0 {
                    let mut s = String::from("⚡ ");
                    if self.upload_limit_mb > 0 {
                        s.push_str(&format!("↑{}MB ", self.upload_limit_mb));
                    }
                    if self.download_limit_mb > 0 {
                        s.push_str(&format!("↓{}MB", self.download_limit_mb));
                    }
                    s
                } else if self.speed_sidebar_open {
                    "⚡ Límites ▾".to_string()
                } else {
                    "⚡ Velocidad".to_string()
                };

                let (btn_fill, btn_color) = if self.speed_sidebar_open {
                    (
                        egui::Color32::from_rgb(14, 165, 233),
                        egui::Color32::WHITE,
                    )
                } else if self.upload_limit_mb > 0 || self.download_limit_mb > 0 {
                    (
                        egui::Color32::from_rgb(20, 35, 50),
                        egui::Color32::from_rgb(56, 189, 248),
                    )
                } else {
                    (
                        egui::Color32::from_rgb(22, 22, 28),
                        egui::Color32::from_rgb(212, 212, 216),
                    )
                };

                let speed_btn = ui.add(
                    egui::Button::new(
                        egui::RichText::new(btn_text)
                            .size(11.0)
                            .strong()
                            .color(btn_color),
                    )
                    .fill(btn_fill)
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 25),
                    ))
                    .rounding(12.0),
                );

                if speed_btn.clicked() {
                    self.speed_sidebar_open = !self.speed_sidebar_open;
                }

                ui.add_space(8.0);

                // Discrete Network Status Badge
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgb(14, 22, 18))
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_unmultiplied(34, 197, 94, 60),
                    ))
                    .rounding(12.0)
                    .inner_margin(egui::Margin::symmetric(10.0, 4.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (dot_rect, _) =
                                ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                            ui.painter().circle_filled(
                                dot_rect.center(),
                                3.5,
                                egui::Color32::from_rgb(34, 197, 94),
                            );
                            ui.add_space(3.0);
                            ui.label(
                                egui::RichText::new("Red P2P Activa")
                                    .size(11.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(34, 197, 94)),
                            );
                        });
                    });
            });
        });
    }

    fn render_speed_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            draw_bolt_icon(ui, 18.0, egui::Color32::from_rgb(14, 165, 233));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Límites de Velocidad")
                    .size(16.0)
                    .strong()
                    .color(egui::Color32::from_rgb(250, 250, 250)),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(
                        egui::RichText::new("✕")
                            .size(13.0)
                            .color(egui::Color32::from_rgb(156, 163, 175)),
                    )
                    .clicked()
                {
                    self.speed_sidebar_open = false;
                }
            });
        });

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Controla el ancho de banda máximo en tiempo real.")
                .size(11.5)
                .color(egui::Color32::from_rgb(148, 163, 184)),
        );

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(10.0);

        // 1. Upload Section
        ui.horizontal(|ui| {
            draw_upload_icon(ui, egui::Color32::from_rgb(56, 189, 248));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Velocidad de Subida")
                    .size(13.5)
                    .strong()
                    .color(egui::Color32::from_rgb(244, 244, 245)),
            );
        });

        let mut up_limited = self.upload_limit_mb > 0;
        let mut up_changed = false;
        if ui.checkbox(&mut up_limited, "Limitar subida").changed() {
            if up_limited {
                if self.upload_limit_mb == 0 {
                    self.upload_limit_mb = 10;
                }
            } else {
                self.upload_limit_mb = 0;
            }
            up_changed = true;
        }

        if up_limited {
            ui.add_space(4.0);
            if ui
                .add(
                    egui::Slider::new(&mut self.upload_limit_mb, 1..=500)
                        .suffix(" MB/s")
                        .logarithmic(true)
                        .text("Máximo"),
                )
                .changed()
            {
                up_changed = true;
            }

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for preset in [5, 10, 25, 50, 100] {
                    let active = self.upload_limit_mb == preset;
                    if ui.selectable_label(active, format!("{} MB/s", preset)).clicked() {
                        self.upload_limit_mb = preset;
                        up_changed = true;
                    }
                }
            });
        } else {
            ui.label(
                egui::RichText::new("⚡ Sin límite (máxima velocidad)")
                    .size(11.0)
                    .color(egui::Color32::from_rgb(74, 222, 128)),
            );
        }

        if up_changed {
            self.upload_limiter.set_limit_mbps(self.upload_limit_mb as u64);
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(10.0);

        // 2. Download Section
        ui.horizontal(|ui| {
            draw_download_icon(ui, egui::Color32::from_rgb(34, 197, 94));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Velocidad de Bajada")
                    .size(13.5)
                    .strong()
                    .color(egui::Color32::from_rgb(244, 244, 245)),
            );
        });

        let mut down_limited = self.download_limit_mb > 0;
        let mut down_changed = false;
        if ui.checkbox(&mut down_limited, "Limitar bajada").changed() {
            if down_limited {
                if self.download_limit_mb == 0 {
                    self.download_limit_mb = 10;
                }
            } else {
                self.download_limit_mb = 0;
            }
            down_changed = true;
        }

        if down_limited {
            ui.add_space(4.0);
            if ui
                .add(
                    egui::Slider::new(&mut self.download_limit_mb, 1..=500)
                        .suffix(" MB/s")
                        .logarithmic(true)
                        .text("Máximo"),
                )
                .changed()
            {
                down_changed = true;
            }

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for preset in [5, 10, 25, 50, 100] {
                    let active = self.download_limit_mb == preset;
                    if ui.selectable_label(active, format!("{} MB/s", preset)).clicked() {
                        self.download_limit_mb = preset;
                        down_changed = true;
                    }
                }
            });
        } else {
            ui.label(
                egui::RichText::new("⚡ Sin límite (máxima velocidad)")
                    .size(11.0)
                    .color(egui::Color32::from_rgb(74, 222, 128)),
            );
        }

        if down_changed {
            self.download_limiter.set_limit_mbps(self.download_limit_mb as u64);
        }

        ui.add_space(20.0);
        ui.separator();
        ui.add_space(10.0);

        // 3. Performance & Protocol Info Card
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(18, 18, 24))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 15),
            ))
            .rounding(10.0)
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("🚀 Rendimiento Mejorado")
                        .size(12.0)
                        .strong()
                        .color(egui::Color32::from_rgb(56, 189, 248)),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("• Ventana QUIC BDP: 64 MB ampliados\n• Pipeline I/O asíncrono y cifrado paralelo\n• Escritura diferida en disco (32 MB chunks)")
                        .size(10.5)
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );
            });
    }

    fn render_toast(&mut self, ui: &mut egui::Ui) {
        if let Some(toast_time) = self.copied_toast_time {
            if toast_time.elapsed().as_secs() < 3 {
                ui.horizontal(|ui| {
                    draw_check_icon(ui, egui::Color32::from_rgb(34, 197, 94));
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(&self.copied_toast_text)
                            .color(egui::Color32::from_rgb(74, 222, 128))
                            .strong(),
                    );
                });
                ui.add_space(6.0);
            } else {
                self.copied_toast_time = None;
            }
        }
    }

    fn render_send_panel(&mut self, ui: &mut egui::Ui, is_send_busy: bool) {
        let session_opt = self.send_session.clone();
        let progress = self.send_manager.progress().lock().clone();
        let is_sending_active =
            progress.status != TransferStatus::Idle && progress.role == TransferRole::Sender;

        // Card Header: Upload Icon + Enviar
        ui.horizontal(|ui| {
            draw_upload_icon(ui, egui::Color32::from_rgb(56, 189, 248));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Enviar archivo")
                    .size(16.0)
                    .strong()
                    .color(egui::Color32::from_rgb(244, 244, 245)),
            );
        });
        ui.add_space(12.0);

        // 1. Interactive Drag & Drop Area
        let is_window_drag = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let drop_desired_h = if self.send_file_path.is_some() {
            110.0
        } else {
            120.0
        };

        let drop_box_rect = {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), drop_desired_h),
                egui::Sense::hover(),
            );
            rect
        };

        let is_hovered = ui.rect_contains_pointer(drop_box_rect) || is_window_drag;

        let bg_color = if is_hovered {
            egui::Color32::from_rgb(16, 26, 38)
        } else {
            egui::Color32::from_rgb(16, 16, 22)
        };

        let border_stroke = if is_hovered {
            egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(56, 189, 248))
        } else {
            egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 22),
            )
        };

        // Draw background and dashed border
        let painter = ui.painter();
        painter.rect_filled(drop_box_rect, 12.0, bg_color);
        draw_dashed_border(painter, drop_box_rect, 12.0, border_stroke, 8.0, 5.0);

        // Render dropzone content inside the allocated rect
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(drop_box_rect), |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(14.0);

                if let Some(ref path) = self.send_file_path {
                    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    let file_name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();

                    draw_file_icon(ui, 24.0, egui::Color32::from_rgb(56, 189, 248));
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(file_name)
                            .size(14.5)
                            .strong()
                            .color(egui::Color32::from_rgb(244, 244, 245)),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "{} ({:.2} GB)",
                            indicatif::HumanBytes(file_size),
                            file_size as f64 / (1024.0 * 1024.0 * 1024.0)
                        ))
                        .size(12.0)
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                    );

                    if !is_send_busy && session_opt.is_none() {
                        ui.add_space(4.0);
                        let change_btn = ui.add(
                            egui::Button::new(
                                egui::RichText::new("Cambiar archivo")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(161, 161, 170)),
                            )
                            .fill(egui::Color32::from_rgb(26, 26, 32))
                            .rounding(16.0),
                        );
                        if change_btn.clicked() {
                            if let Some(file) = rfd::FileDialog::new().pick_file() {
                                self.send_file_path = Some(file);
                            }
                        }
                    }
                } else {
                    let icon_color = if is_hovered {
                        egui::Color32::from_rgb(56, 189, 248)
                    } else {
                        egui::Color32::from_rgb(113, 113, 122)
                    };
                    draw_file_icon(ui, 26.0, icon_color);
                    ui.add_space(6.0);

                    let title_text = if is_window_drag {
                        "¡Soltá el archivo aquí!"
                    } else {
                        "Arrastrá un archivo aquí"
                    };

                    ui.label(egui::RichText::new(title_text).size(14.0).strong().color(
                        if is_hovered {
                            egui::Color32::from_rgb(56, 189, 248)
                        } else {
                            egui::Color32::from_rgb(244, 244, 245)
                        },
                    ));
                    ui.label(
                        egui::RichText::new("o hacé clic para explorar (hasta 1 TB+)")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(113, 113, 122)),
                    );

                    ui.add_space(6.0);
                    let pick_btn = ui.add(
                        egui::Button::new(
                            egui::RichText::new("Seleccionar archivo")
                                .size(11.5)
                                .color(egui::Color32::from_rgb(228, 228, 231)),
                        )
                        .fill(egui::Color32::from_rgb(30, 30, 36))
                        .rounding(16.0),
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

        // 2. Connection Code section
        ui.label(
            egui::RichText::new("Código de conexión")
                .size(12.0)
                .color(egui::Color32::from_rgb(148, 163, 184)),
        );
        ui.add_space(4.0);

        // 3. Large Code Display Box with Integrated Copy Button
        let has_session =
            progress.pairing_code.is_some() && progress.status == TransferStatus::WaitingForPeer;
        let code_border_stroke = if has_session {
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(56, 189, 248))
        } else {
            egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18),
            )
        };

        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(20, 20, 26))
            .stroke(code_border_stroke)
            .rounding(16.0)
            .inner_margin(egui::Margin::symmetric(14.0, 10.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if let Some(ref invitation) =
                        progress.pairing_code.as_ref().filter(|_| has_session)
                    {
                        ui.label(
                            egui::RichText::new(if invitation.starts_with("heavy1_") {
                                "Invitación lista".to_string()
                            } else {
                                invitation.to_string()
                            })
                            .size(16.0)
                            .monospace()
                            .strong()
                            .color(egui::Color32::from_rgb(250, 250, 250)),
                        );

                        // Integrated Copy Button to the right
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let recently_copied = self
                                .copied_toast_time
                                .map(|t| t.elapsed().as_secs() < 2)
                                .unwrap_or(false);

                            let (btn_fill, btn_color, label_text) = if recently_copied {
                                (
                                    egui::Color32::from_rgb(22, 101, 52),  // Emerald 800
                                    egui::Color32::from_rgb(74, 222, 128), // Emerald 400
                                    "✓ ¡Copiado!",
                                )
                            } else {
                                (
                                    egui::Color32::from_rgb(32, 32, 38),
                                    egui::Color32::from_rgb(228, 228, 231),
                                    "📋 Copiar",
                                )
                            };

                            let copy_btn = ui.add(
                                egui::Button::new(
                                    egui::RichText::new(label_text)
                                        .size(11.5)
                                        .strong()
                                        .color(btn_color),
                                )
                                .fill(btn_fill)
                                .stroke(egui::Stroke::new(
                                    1.0_f32,
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                                ))
                                .rounding(12.0),
                            );

                            if copy_btn.clicked() {
                                ui.output_mut(|o| o.copied_text = invitation.to_string());
                                self.show_toast("¡Código copiado al portapapeles!");
                            }
                        });
                    } else {
                        ui.label(
                            egui::RichText::new("•••• - •••• - ••••")
                                .size(19.0)
                                .monospace()
                                .strong()
                                .color(egui::Color32::from_rgb(90, 90, 100)),
                        );
                    }
                });
            });

        ui.add_space(10.0);

        // 4. Primary Actions (Crear transferencia o cuenta regresiva)
        if let Some(ref session) = session_opt {
            ui.horizontal(|ui| {
                draw_clock_icon(ui, egui::Color32::from_rgb(148, 163, 184));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(if has_session {
                        format!("Expira en {}", session.format_remaining())
                    } else if is_send_busy {
                        "Preparando transferencia...".into()
                    } else {
                        "Servidor temporal cerrado".into()
                    })
                    .size(12.0)
                    .color(egui::Color32::from_rgb(148, 163, 184)),
                );

                if !is_send_busy {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(
                                egui::RichText::new("Cancelar sesión")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            )
                            .clicked()
                        {
                            self.cancel_send_transfer();
                        }
                    });
                }
            });
        } else {
            let has_file = self.send_file_path.is_some();
            let btn_fill = if has_file {
                egui::Color32::from_rgb(14, 165, 233) // Electric cyan/blue
            } else {
                egui::Color32::from_rgb(30, 30, 36)
            };
            let text_color = if has_file {
                egui::Color32::WHITE
            } else {
                egui::Color32::from_rgb(148, 163, 184)
            };

            let start_btn = ui.add_sized(
                [ui.available_width(), 42.0],
                egui::Button::new(
                    egui::RichText::new("⚡ Crear transferencia")
                        .size(14.0)
                        .strong()
                        .color(text_color),
                )
                .fill(btn_fill)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    if has_file {
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12)
                    },
                ))
                .rounding(21.0),
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
                        .size(11.5)
                        .color(egui::Color32::from_rgb(113, 113, 122)),
                );
            });
        }

        // Live transfer monitor in Send panel when active
        if is_sending_active {
            ui.add_space(14.0);
            self.render_transfer_dashboard(ui, &progress, true);
        }

        if let Some(ref err) = self.send_error {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("Error: {}", err))
                    .size(12.0)
                    .color(egui::Color32::from_rgb(239, 68, 68)),
            );
        }
    }

    fn render_receive_panel(&mut self, ui: &mut egui::Ui, is_recv_busy: bool) {
        let progress = self.recv_manager.progress().lock().clone();
        let is_recv_active =
            progress.status != TransferStatus::Idle && progress.role == TransferRole::Receiver;

        // Card Header: Download Icon + Recibir
        ui.horizontal(|ui| {
            draw_download_icon(ui, egui::Color32::from_rgb(56, 189, 248));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Recibir archivo")
                    .size(16.0)
                    .strong()
                    .color(egui::Color32::from_rgb(244, 244, 245)),
            );
        });
        ui.add_space(12.0);

        // 1. Label
        ui.label(
            egui::RichText::new("Ingresá el código del emisor")
                .size(12.0)
                .color(egui::Color32::from_rgb(148, 163, 184)),
        );
        ui.add_space(4.0);

        // 2. Rounded Pill Input Field with auto-uppercase
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(20, 20, 26))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
            ))
            .rounding(20.0)
            .inner_margin(egui::Margin::symmetric(14.0, 8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let edit = ui.add_sized(
                    [ui.available_width(), 26.0],
                    egui::TextEdit::singleline(&mut self.recv_code_input)
                        .hint_text("Pega la invitación completa (heavy1_…)")
                        .font(egui::TextStyle::Monospace)
                        .char_limit(heavydrop::pairing::MAX_INVITATION_LEN)
                        .frame(false),
                );
                let _ = edit;
            });

        ui.add_space(10.0);

        // 3. Button "Usar código" (Only shows Cancelar conexión when the receiver itself is busy!)
        if !is_recv_busy {
            let has_code = PairingCode::parse(&self.recv_code_input).is_ok();
            let btn_fill = if has_code {
                egui::Color32::from_rgb(14, 165, 233) // Electric cyan/blue
            } else {
                egui::Color32::from_rgb(30, 30, 36)
            };
            let text_color = if has_code {
                egui::Color32::WHITE
            } else {
                egui::Color32::from_rgb(148, 163, 184)
            };

            let connect_btn = ui.add_sized(
                [ui.available_width(), 42.0],
                egui::Button::new(
                    egui::RichText::new("Usar código")
                        .size(14.0)
                        .strong()
                        .color(text_color),
                )
                .fill(btn_fill)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    if has_code {
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12)
                    },
                ))
                .rounding(21.0),
            );

            if connect_btn.clicked() {
                self.start_receive_task();
            }
        } else {
            let cancel_btn = ui.add_sized(
                [ui.available_width(), 42.0],
                egui::Button::new(
                    egui::RichText::new("Cancelar conexión")
                        .size(14.0)
                        .strong()
                        .color(egui::Color32::from_rgb(239, 68, 68)),
                )
                .fill(egui::Color32::from_rgb(32, 24, 24))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgb(239, 68, 68),
                ))
                .rounding(21.0),
            );
            if cancel_btn.clicked() {
                self.cancel_recv_transfer();
            }
        }

        ui.add_space(14.0);

        // 4. State: Either Active Transfer Dashboard OR Clean Empty State
        if is_recv_active {
            self.render_transfer_dashboard(ui, &progress, false);
        } else {
            // Modern Clean Empty State Box
            egui::Frame::group(ui.style())
                .fill(egui::Color32::from_rgb(16, 16, 22))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14),
                ))
                .rounding(14.0)
                .inner_margin(egui::Margin::symmetric(16.0, 14.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        draw_file_icon(ui, 24.0, egui::Color32::from_rgb(100, 100, 115));
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new("Listo para recibir")
                                    .size(13.5)
                                    .strong()
                                    .color(egui::Color32::from_rgb(244, 244, 245)),
                            );
                            ui.label(
                                egui::RichText::new(
                                    "Ingresá el código del emisor arriba para iniciar la descarga",
                                )
                                .size(11.0)
                                .color(egui::Color32::from_rgb(113, 113, 122)),
                            );
                        });
                    });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        draw_shield_icon(ui, 13.0, egui::Color32::from_rgb(34, 197, 94));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Cifrado E2EE ChaCha20-Poly1305 • P2P directo")
                                .size(11.0)
                                .color(egui::Color32::from_rgb(148, 163, 184)),
                        );
                    });
                });
        }

        ui.add_space(14.0);

        // 5. Destination folder selector as a compact styled pill
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(18, 18, 24))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18),
            ))
            .rounding(14.0)
            .inner_margin(egui::Margin::symmetric(12.0, 8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    draw_folder_icon(ui, 14.0, egui::Color32::from_rgb(56, 189, 248));
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Guardar en:")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(148, 163, 184)),
                    );

                    let dir_str = self.recv_output_dir.display().to_string();
                    let truncated = if dir_str.chars().count() > 24 {
                        let suffix: String = dir_str
                            .chars()
                            .rev()
                            .take(20)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        format!("...{}", suffix)
                    } else {
                        dir_str
                    };
                    ui.label(
                        egui::RichText::new(truncated)
                            .size(11.0)
                            .monospace()
                            .color(egui::Color32::from_rgb(228, 228, 231)),
                    );

                    if !is_recv_busy {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let change_btn = ui.add(
                                egui::Button::new(
                                    egui::RichText::new("Cambiar")
                                        .size(10.5)
                                        .color(egui::Color32::from_rgb(161, 161, 170)),
                                )
                                .fill(egui::Color32::from_rgb(28, 28, 34))
                                .rounding(10.0),
                            );
                            if change_btn.clicked() {
                                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                    self.recv_output_dir = dir;
                                }
                            }
                        });
                    }
                });
            });

        if let Some(ref err) = self.recv_error {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("Error: {}", err))
                    .size(12.0)
                    .color(egui::Color32::from_rgb(239, 68, 68)),
            );
        }
    }

    // Modern Integrated Transfer Progress Dashboard
    fn render_transfer_dashboard(
        &mut self,
        ui: &mut egui::Ui,
        progress: &TransferProgress,
        is_sender: bool,
    ) {
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(18, 20, 28))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(56, 189, 248),
            ))
            .rounding(14.0)
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if !progress.file_name.is_empty() {
                        draw_file_icon(ui, 16.0, egui::Color32::from_rgb(56, 189, 248));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(&progress.file_name)
                                .size(13.5)
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                    }

                    // Connectivity badge
                    let (badge_color, badge_text) = match progress.connection_type {
                        ConnectionType::DirectLocal => {
                            (egui::Color32::from_rgb(34, 197, 94), "● LAN Directo")
                        }
                        ConnectionType::DirectInternet => {
                            (egui::Color32::from_rgb(56, 189, 248), "● P2P Directo")
                        }
                        ConnectionType::Relay => {
                            (egui::Color32::from_rgb(234, 179, 8), "● Relay Cifrado")
                        }
                        ConnectionType::None => {
                            (egui::Color32::from_rgb(148, 163, 184), "○ Conectando...")
                        }
                    };

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(badge_text)
                                .size(11.0)
                                .strong()
                                .color(badge_color),
                        );
                    });
                });

                ui.add_space(8.0);

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

                ui.add_space(8.0);

                // Stats: Velocidad, Bytes transferidos, ETA
                ui.horizontal(|ui| {
                    let speed_mb = progress.bytes_per_sec / (1024.0 * 1024.0);
                    ui.label(
                        egui::RichText::new(format!("⚡ {:.2} MB/s", speed_mb))
                            .size(11.5)
                            .strong()
                            .color(egui::Color32::from_rgb(244, 244, 245)),
                    );

                    if progress.total_bytes > 0 {
                        ui.label(
                            egui::RichText::new(format!(
                                "• {} / {}",
                                indicatif::HumanBytes(progress.transferred_bytes),
                                indicatif::HumanBytes(progress.total_bytes)
                            ))
                            .size(11.5)
                            .color(egui::Color32::from_rgb(148, 163, 184)),
                        );

                        if progress.bytes_per_sec > 0.0 {
                            let rem_bytes = progress
                                .total_bytes
                                .saturating_sub(progress.transferred_bytes);
                            let eta_secs = (rem_bytes as f64 / progress.bytes_per_sec) as u64;
                            ui.label(
                                egui::RichText::new(format!(
                                    "• ETA: {}m {}s",
                                    eta_secs / 60,
                                    eta_secs % 60
                                ))
                                .size(11.5)
                                .color(egui::Color32::from_rgb(56, 189, 248)),
                            );
                        }
                    }
                });

                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(&progress.status_text)
                        .size(11.0)
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );

                if let Some(ref err) = progress.error {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("Error: {}", err))
                            .size(11.5)
                            .color(egui::Color32::from_rgb(239, 68, 68)),
                    );
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let is_busy = if is_sender {
                        self.send_manager.is_busy()
                    } else {
                        self.recv_manager.is_busy()
                    };

                    if is_busy {
                        if ui
                            .button(
                                egui::RichText::new("🛑 Cancelar transferencia")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(239, 68, 68)),
                            )
                            .clicked()
                        {
                            if is_sender {
                                self.cancel_send_transfer();
                            } else {
                                self.cancel_recv_transfer();
                            }
                        }
                    } else if ui.button("Listo / Cerrar").clicked() {
                        if is_sender {
                            self.send_manager.reset_cancel();
                            self.send_session = None;
                        } else {
                            self.recv_manager.reset_cancel();
                        }
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
