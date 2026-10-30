use eframe::egui;
use std::collections::VecDeque;
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

#[derive(PartialEq, Eq, Clone, Copy)]
enum ActiveTab {
    Send,
    Receive,
    History,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HistoryRecord {
    pub file_name: String,
    pub total_bytes: u64,
    pub role: String,
    pub status: String,
    pub timestamp_str: String,
    pub avg_speed_mb: f64,
    pub destination_path: Option<PathBuf>,
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

    // Services & Network
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

    // Settings Modal
    settings_open: bool,

    // Live Telemetry & Metrics
    speed_history: VecDeque<f32>,
    peak_speed_mb: f64,
    transfer_start_time: Option<Instant>,
    last_speed_sample: Instant,

    // History Tracking
    last_send_status: TransferStatus,
    last_recv_status: TransferStatus,
    history: Vec<HistoryRecord>,
    history_file_path: PathBuf,

    // Cached disk space
    cached_free_space: Option<(Instant, u64)>,
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

        let history_file_path = get_history_file_path();
        let history = load_history(&history_file_path);

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

            settings_open: false,

            speed_history: VecDeque::with_capacity(50),
            peak_speed_mb: 0.0,
            transfer_start_time: None,
            last_speed_sample: Instant::now(),

            last_send_status: TransferStatus::Idle,
            last_recv_status: TransferStatus::Idle,
            history,
            history_file_path,

            cached_free_space: None,
        }
    }

    fn apply_custom_theme(ctx: &egui::Context) {
        // 1. Install high-DPI Windows Fluent Fonts
        let mut fonts = egui::FontDefinitions::default();
        if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") {
            fonts.font_data.insert(
                "segoe_ui".to_owned(),
                egui::FontData::from_owned(data),
            );
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "segoe_ui".to_owned());
        }
        if let Ok(mono) = std::fs::read("C:\\Windows\\Fonts\\CascadiaMono.ttf")
            .or_else(|_| std::fs::read("C:\\Windows\\Fonts\\consola.ttf"))
        {
            fonts.font_data.insert(
                "custom_mono".to_owned(),
                egui::FontData::from_owned(mono),
            );
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .insert(0, "custom_mono".to_owned());
        }
        ctx.set_fonts(fonts);

        // 2. High-contrast, Deep Canvas Dark Visuals
        let mut visuals = egui::Visuals::dark();

        visuals.window_fill = egui::Color32::from_rgb(11, 12, 16);
        visuals.panel_fill = egui::Color32::from_rgb(11, 12, 16);
        visuals.extreme_bg_color = egui::Color32::from_rgb(15, 17, 24);
        visuals.faint_bg_color = egui::Color32::from_rgb(18, 21, 30);
        visuals.code_bg_color = egui::Color32::from_rgb(20, 23, 32);

        let border_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 22));

        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(16, 18, 26);
        visuals.widgets.noninteractive.bg_stroke = border_stroke;
        visuals.widgets.noninteractive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(244, 244, 245));
        visuals.widgets.noninteractive.rounding = egui::Rounding::same(12.0);

        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(20, 23, 34);
        visuals.widgets.inactive.bg_stroke = border_stroke;
        visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(228, 228, 231));
        visuals.widgets.inactive.rounding = egui::Rounding::same(12.0);

        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(30, 36, 52);
        visuals.widgets.hovered.bg_stroke =
            egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(56, 189, 248)); // Electric Cyan hover
        visuals.widgets.hovered.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
        visuals.widgets.hovered.rounding = egui::Rounding::same(12.0);

        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(14, 165, 233);
        visuals.widgets.active.bg_stroke =
            egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(125, 211, 252));
        visuals.widgets.active.fg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
        visuals.widgets.active.rounding = egui::Rounding::same(12.0);

        visuals.window_rounding = egui::Rounding::same(16.0);
        visuals.menu_rounding = egui::Rounding::same(12.0);

        ctx.set_visuals(visuals);

        let mut style = (*ctx.style()).clone();
        style.text_styles = [
            (egui::TextStyle::Heading, egui::FontId::proportional(22.0)),
            (egui::TextStyle::Name("Subheading".into()), egui::FontId::proportional(16.5)),
            (egui::TextStyle::Body, egui::FontId::proportional(13.5)),
            (egui::TextStyle::Monospace, egui::FontId::monospace(13.0)),
            (egui::TextStyle::Button, egui::FontId::proportional(13.5)),
            (egui::TextStyle::Small, egui::FontId::proportional(11.5)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
        ctx.set_style(style);
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

    fn get_free_disk_space(&mut self) -> Option<u64> {
        if let Some((time, bytes)) = self.cached_free_space {
            if time.elapsed().as_secs() < 3 {
                return Some(bytes);
            }
        }
        let bytes = get_available_disk_space(&self.recv_output_dir);
        if let Some(b) = bytes {
            self.cached_free_space = Some((Instant::now(), b));
        }
        bytes
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
        self.speed_history.clear();
        self.peak_speed_mb = 0.0;
        self.transfer_start_time = None;

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
            skip_prehash: true,
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
        self.speed_history.clear();
        self.peak_speed_mb = 0.0;
        self.transfer_start_time = None;
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
// Custom Vector Drawing Helpers
// ----------------------------------------------------------------------------

fn draw_card_with_top_highlight(
    painter: &egui::Painter,
    rect: egui::Rect,
    rounding: f32,
    bg_fill: egui::Color32,
    stroke: egui::Stroke,
) {
    // 1. Fill card background
    painter.rect_filled(rect, rounding, bg_fill);

    // 2. Stroke perimeter
    painter.rect_stroke(rect, rounding, stroke);

    // 3. Top-edge glass reflection highlight (1px crisp line)
    let pad = rounding.max(8.0);
    let highlight_stroke =
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 28));
    painter.line_segment(
        [
            egui::pos2(rect.min.x + pad, rect.min.y + 1.0),
            egui::pos2(rect.max.x - pad, rect.min.y + 1.0),
        ],
        highlight_stroke,
    );
}

fn draw_bolt_icon(ui: &mut egui::Ui, size: f32, color: egui::Color32) {
    let width = size * 0.65;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, size), egui::Sense::hover());
    let painter = ui.painter();

    // Subtle ambient glow behind bolt
    painter.circle_filled(
        rect.center(),
        size * 0.6,
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 40),
    );

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

fn draw_cloud_upload_hero(ui: &mut egui::Ui, is_hovered: bool) {
    let size = egui::vec2(76.0, 58.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    let center = rect.center();

    let accent_color = if is_hovered {
        egui::Color32::from_rgb(56, 189, 248)
    } else {
        egui::Color32::from_rgb(14, 165, 233)
    };

    // Soft ambient glow halo
    painter.circle_filled(
        center,
        34.0,
        if is_hovered {
            egui::Color32::from_rgba_unmultiplied(56, 189, 248, 45)
        } else {
            egui::Color32::from_rgba_unmultiplied(14, 165, 233, 22)
        },
    );

    let stroke = egui::Stroke::new(2.0_f32, accent_color);
    let fill = if is_hovered {
        egui::Color32::from_rgb(18, 30, 48)
    } else {
        egui::Color32::from_rgb(18, 23, 34)
    };

    // Cloud bubbles
    painter.circle(center + egui::vec2(-16.0, 3.0), 13.0, fill, stroke);
    painter.circle(center + egui::vec2(16.0, 3.0), 11.0, fill, stroke);
    painter.circle(center + egui::vec2(1.0, -8.0), 17.0, fill, stroke);

    // Inner bridge
    painter.rect_filled(
        egui::Rect::from_center_size(center + egui::vec2(0.0, 3.0), egui::vec2(34.0, 20.0)),
        0.0,
        fill,
    );

    // Upward arrow
    let arrow_stroke = egui::Stroke::new(2.4_f32, egui::Color32::WHITE);
    let arrow_top = center.y - 13.0;
    let arrow_bottom = center.y + 11.0;
    let cx = center.x;

    painter.line_segment([egui::pos2(cx, arrow_bottom), egui::pos2(cx, arrow_top)], arrow_stroke);
    painter.line_segment([egui::pos2(cx - 5.5, arrow_top + 6.0), egui::pos2(cx, arrow_top)], arrow_stroke);
    painter.line_segment([egui::pos2(cx + 5.5, arrow_top + 6.0), egui::pos2(cx, arrow_top)], arrow_stroke);
}

fn draw_upload_icon(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.8_f32, color);

    let y_tray = rect.max.y - 2.0;
    painter.line_segment([egui::pos2(rect.min.x + 2.0, y_tray - 3.0), egui::pos2(rect.min.x + 2.0, y_tray)], stroke);
    painter.line_segment([egui::pos2(rect.min.x + 2.0, y_tray), egui::pos2(rect.max.x - 2.0, y_tray)], stroke);
    painter.line_segment([egui::pos2(rect.max.x - 2.0, y_tray), egui::pos2(rect.max.x - 2.0, y_tray - 3.0)], stroke);

    let cx = rect.center().x;
    painter.line_segment([egui::pos2(cx, y_tray - 2.0), egui::pos2(cx, rect.min.y + 2.0)], stroke);
    painter.line_segment([egui::pos2(cx - 3.5, rect.min.y + 5.5), egui::pos2(cx, rect.min.y + 2.0)], stroke);
    painter.line_segment([egui::pos2(cx + 3.5, rect.min.y + 5.5), egui::pos2(cx, rect.min.y + 2.0)], stroke);
}

fn draw_download_icon(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.8_f32, color);

    let y_tray = rect.max.y - 2.0;
    painter.line_segment([egui::pos2(rect.min.x + 2.0, y_tray - 3.0), egui::pos2(rect.min.x + 2.0, y_tray)], stroke);
    painter.line_segment([egui::pos2(rect.min.x + 2.0, y_tray), egui::pos2(rect.max.x - 2.0, y_tray)], stroke);
    painter.line_segment([egui::pos2(rect.max.x - 2.0, y_tray), egui::pos2(rect.max.x - 2.0, y_tray - 3.0)], stroke);

    let cx = rect.center().x;
    painter.line_segment([egui::pos2(cx, rect.min.y + 2.0), egui::pos2(cx, y_tray - 2.0)], stroke);
    painter.line_segment([egui::pos2(cx - 3.5, y_tray - 5.5), egui::pos2(cx, y_tray - 2.0)], stroke);
    painter.line_segment([egui::pos2(cx + 3.5, y_tray - 5.5), egui::pos2(cx, y_tray - 2.0)], stroke);
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
    painter.line_segment([egui::pos2(rect.min.x + 3.0, y1), egui::pos2(rect.max.x - 3.0, y1)], egui::Stroke::new(1.0_f32, line_color));
    painter.line_segment([egui::pos2(rect.min.x + 3.0, y2), egui::pos2(rect.max.x - 4.5, y2)], egui::Stroke::new(1.0_f32, line_color));
}

fn draw_folder_icon(ui: &mut egui::Ui, size: f32, color: egui::Color32) {
    let width = size * 1.25;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, size), egui::Sense::hover());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.4_f32, color);

    let p0 = egui::pos2(rect.min.x + 1.0, rect.min.y + 2.0);
    let p1 = egui::pos2(rect.min.x + width * 0.44, rect.min.y + 2.0);
    let p2 = egui::pos2(rect.min.x + width * 0.54, rect.min.y + 4.5);
    painter.line_segment([p0, p1], stroke);
    painter.line_segment([p1, p2], stroke);

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

    painter.line_segment([egui::pos2(rect.min.x + 1.5, top_y), egui::pos2(rect.max.x - 1.5, top_y)], stroke);
    painter.line_segment([egui::pos2(rect.min.x + 1.5, top_y), egui::pos2(rect.min.x + 1.5, mid_y)], stroke);
    painter.line_segment([egui::pos2(rect.max.x - 1.5, top_y), egui::pos2(rect.max.x - 1.5, mid_y)], stroke);
    painter.line_segment([egui::pos2(rect.min.x + 1.5, mid_y), egui::pos2(cx, bot_y)], stroke);
    painter.line_segment([egui::pos2(rect.max.x - 1.5, mid_y), egui::pos2(cx, bot_y)], stroke);
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

    // Top
    let mut x = r.min.x + pad;
    while x < r.max.x - pad {
        let x_end = (x + dash_len).min(r.max.x - pad);
        painter.line_segment([egui::pos2(x, r.min.y), egui::pos2(x_end, r.min.y)], stroke);
        x += dash_len + gap_len;
    }
    // Bottom
    let mut x = r.min.x + pad;
    while x < r.max.x - pad {
        let x_end = (x + dash_len).min(r.max.x - pad);
        painter.line_segment([egui::pos2(x, r.max.y), egui::pos2(x_end, r.max.y)], stroke);
        x += dash_len + gap_len;
    }
    // Left
    let mut y = r.min.y + pad;
    while y < r.max.y - pad {
        let y_end = (y + dash_len).min(r.max.y - pad);
        painter.line_segment([egui::pos2(r.min.x, y), egui::pos2(r.min.x, y_end)], stroke);
        y += dash_len + gap_len;
    }
    // Right
    let mut y = r.min.y + pad;
    while y < r.max.y - pad {
        let y_end = (y + dash_len).min(r.max.y - pad);
        painter.line_segment([egui::pos2(r.max.x, y), egui::pos2(r.max.x, y_end)], stroke);
        y += dash_len + gap_len;
    }
}

fn draw_file_type_badge(ui: &mut egui::Ui, extension: &str) {
    let ext_upper = extension.to_uppercase();
    let (bg, fg, border) = match ext_upper.as_str() {
        "ISO" | "IMG" | "BIN" => (
            egui::Color32::from_rgb(28, 26, 68),
            egui::Color32::from_rgb(199, 210, 254),
            egui::Color32::from_rgb(99, 102, 241),
        ),
        "ZIP" | "RAR" | "7Z" | "TAR" | "GZ" => (
            egui::Color32::from_rgb(45, 26, 14),
            egui::Color32::from_rgb(254, 215, 170),
            egui::Color32::from_rgb(249, 115, 22),
        ),
        "MP4" | "MKV" | "AVI" | "MOV" => (
            egui::Color32::from_rgb(52, 14, 46),
            egui::Color32::from_rgb(251, 207, 232),
            egui::Color32::from_rgb(236, 72, 153),
        ),
        "EXE" | "MSI" | "DMG" => (
            egui::Color32::from_rgb(14, 34, 56),
            egui::Color32::from_rgb(186, 230, 253),
            egui::Color32::from_rgb(14, 165, 233),
        ),
        _ => (
            egui::Color32::from_rgb(24, 28, 38),
            egui::Color32::from_rgb(226, 232, 240),
            egui::Color32::from_rgb(100, 116, 139),
        ),
    };

    let label = if ext_upper.is_empty() {
        "FILE".to_string()
    } else if ext_upper.len() > 5 {
        ext_upper[..5].to_string()
    } else {
        ext_upper
    };

    let (rect, _) = ui.allocate_exact_size(egui::vec2(52.0, 52.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 12.0, bg);
    painter.rect_stroke(rect, 12.0, egui::Stroke::new(1.2_f32, border));

    // Centered label
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(12.0),
        fg,
    );
}

fn draw_speed_sparkline(
    ui: &mut egui::Ui,
    history: &VecDeque<f32>,
    peak_speed: f64,
) {
    let desired_size = egui::vec2(ui.available_width(), 58.0);
    let (rect, _) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    let painter = ui.painter();

    // Background box
    painter.rect_filled(rect, 10.0, egui::Color32::from_rgb(14, 16, 24));
    painter.rect_stroke(
        rect,
        10.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14)),
    );

    if history.len() < 2 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "⚡ Muestreando telemetría de transferencia en tiempo real...",
            egui::FontId::proportional(11.5),
            egui::Color32::from_rgb(148, 163, 184),
        );
        return;
    }

    let max_val = (peak_speed as f32).max(5.0);
    let pad_x = 10.0;
    let pad_y = 8.0;
    let w = rect.width() - pad_x * 2.0;
    let h = rect.height() - pad_y * 2.0;

    let count = history.len();
    let step_x = w / (count - 1) as f32;

    let mut points: Vec<egui::Pos2> = Vec::with_capacity(count);
    for (i, &val) in history.iter().enumerate() {
        let x = rect.min.x + pad_x + (i as f32) * step_x;
        let norm_y = (val / max_val).clamp(0.0, 1.0);
        let y = rect.max.y - pad_y - norm_y * h;
        points.push(egui::pos2(x, y));
    }

    // Gradient fill under the curve
    for i in 0..(points.len() - 1) {
        let p1 = points[i];
        let p2 = points[i + 1];
        let b1 = egui::pos2(p1.x, rect.max.y - pad_y);
        let b2 = egui::pos2(p2.x, rect.max.y - pad_y);

        let col_top = egui::Color32::from_rgba_unmultiplied(14, 165, 233, 45);
        let col_bot = egui::Color32::from_rgba_unmultiplied(14, 165, 233, 4);

        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(p1, col_top);
        mesh.colored_vertex(p2, col_top);
        mesh.colored_vertex(b2, col_bot);
        mesh.colored_vertex(b1, col_bot);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
    }

    // Draw curve
    for i in 0..(points.len() - 1) {
        painter.line_segment(
            [points[i], points[i + 1]],
            egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(56, 189, 248)),
        );
    }

    // Latest dot with glow
    if let Some(&last_p) = points.last() {
        painter.circle_filled(last_p, 5.0, egui::Color32::from_rgba_unmultiplied(56, 189, 248, 90));
        painter.circle_filled(last_p, 2.5, egui::Color32::WHITE);
    }

    painter.text(
        egui::pos2(rect.max.x - 12.0, rect.min.y + 8.0),
        egui::Align2::RIGHT_TOP,
        format!("Pico: {:.1} MB/s", peak_speed),
        egui::FontId::monospace(10.5),
        egui::Color32::from_rgb(148, 163, 184),
    );
}

// ----------------------------------------------------------------------------
// Application Main UI
// ----------------------------------------------------------------------------

impl eframe::App for HeavyDropApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let is_send_busy = self.send_manager.is_busy();
        let is_recv_busy = self.recv_manager.is_busy();

        if is_send_busy || is_recv_busy || self.send_session.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }

        // Live Telemetry Sampling
        let active_progress = if is_send_busy {
            Some(self.send_manager.progress().lock().clone())
        } else if is_recv_busy {
            Some(self.recv_manager.progress().lock().clone())
        } else {
            None
        };

        if let Some(p) = active_progress {
            if p.status == TransferStatus::Transferring {
                if self.transfer_start_time.is_none() {
                    self.transfer_start_time = Some(Instant::now());
                    self.peak_speed_mb = 0.0;
                    self.speed_history.clear();
                }
                if self.last_speed_sample.elapsed().as_millis() >= 350 {
                    self.last_speed_sample = Instant::now();
                    let current_mb = (p.bytes_per_sec / (1024.0 * 1024.0)) as f32;
                    if current_mb as f64 > self.peak_speed_mb {
                        self.peak_speed_mb = current_mb as f64;
                    }
                    if self.speed_history.len() >= 40 {
                        self.speed_history.pop_front();
                    }
                    self.speed_history.push_back(current_mb);
                }
            }
        }

        // History Transition Checks
        let send_p = self.send_manager.progress().lock().clone();
        if send_p.status != self.last_send_status {
            if (send_p.status == TransferStatus::Completed || send_p.status == TransferStatus::Cancelled || send_p.status == TransferStatus::Failed)
                && (self.last_send_status == TransferStatus::Transferring || self.last_send_status == TransferStatus::Preparing)
            {
                let status_str = match send_p.status {
                    TransferStatus::Completed => "Completado",
                    TransferStatus::Cancelled => "Cancelado",
                    _ => "Error",
                };
                let elapsed = self.transfer_start_time.map(|t| t.elapsed().as_secs_f64()).unwrap_or(1.0).max(0.1);
                let avg_speed = (send_p.transferred_bytes as f64 / elapsed) / (1024.0 * 1024.0);
                let rec = HistoryRecord {
                    file_name: if !send_p.file_name.is_empty() {
                        send_p.file_name.clone()
                    } else if let Some(ref p) = self.send_file_path {
                        p.file_name().unwrap_or_default().to_string_lossy().to_string()
                    } else {
                        "Archivo enviado".to_string()
                    },
                    total_bytes: send_p.total_bytes,
                    role: "Enviado".to_string(),
                    status: status_str.to_string(),
                    timestamp_str: current_timestamp_str(),
                    avg_speed_mb: avg_speed,
                    destination_path: self.send_file_path.clone(),
                };
                self.history.insert(0, rec);
                save_history(&self.history_file_path, &self.history);
            }
            self.last_send_status = send_p.status;
        }

        let recv_p = self.recv_manager.progress().lock().clone();
        if recv_p.status != self.last_recv_status {
            if (recv_p.status == TransferStatus::Completed || recv_p.status == TransferStatus::Cancelled || recv_p.status == TransferStatus::Failed)
                && (self.last_recv_status == TransferStatus::Transferring || self.last_recv_status == TransferStatus::Connecting)
            {
                let status_str = match recv_p.status {
                    TransferStatus::Completed => "Completado",
                    TransferStatus::Cancelled => "Cancelado",
                    _ => "Error",
                };
                let elapsed = self.transfer_start_time.map(|t| t.elapsed().as_secs_f64()).unwrap_or(1.0).max(0.1);
                let avg_speed = (recv_p.transferred_bytes as f64 / elapsed) / (1024.0 * 1024.0);
                let dest = self.recv_output_dir.join(&recv_p.file_name);
                let rec = HistoryRecord {
                    file_name: if !recv_p.file_name.is_empty() {
                        recv_p.file_name.clone()
                    } else {
                        "Archivo recibido".to_string()
                    },
                    total_bytes: recv_p.total_bytes,
                    role: "Recibido".to_string(),
                    status: status_str.to_string(),
                    timestamp_str: current_timestamp_str(),
                    avg_speed_mb: avg_speed,
                    destination_path: Some(dest),
                };
                self.history.insert(0, rec);
                save_history(&self.history_file_path, &self.history);
            }
            self.last_recv_status = recv_p.status;
        }

        // Drag & Drop anywhere in window
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

        // Speed Limits Sidebar
        if self.speed_sidebar_open {
            egui::SidePanel::right("speed_sidebar")
                .resizable(false)
                .default_width(310.0)
                .frame(
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(13, 14, 20))
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

        // Settings Modal
        self.render_settings_window(ctx);

        // Main Center Area
        egui::CentralPanel::default().show(ctx, |ui| {
            let total_width = ui.available_width();
            let total_height = ui.available_height();

            let max_card_width = 840.0_f32;
            let content_width = total_width.min(max_card_width);
            let h_margin = ((total_width - content_width) / 2.0).max(0.0);
            let v_padding = ((total_height - 560.0) / 2.0).clamp(8.0, 36.0);

            ui.add_space(v_padding);

            ui.horizontal(|ui| {
                if h_margin > 0.0 {
                    ui.add_space(h_margin);
                }

                ui.allocate_ui_with_layout(
                    egui::vec2(content_width, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        // 1. Sleek Top Bar
                        self.render_top_bar(ui);
                        ui.add_space(16.0);

                        // 2. Toast feedback
                        self.render_toast(ui);

                        // 3. Main Hero Card
                        let card_rect = {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 0.0),
                                egui::Sense::hover(),
                            );
                            rect
                        };
                        let _ = card_rect;

                        egui::Frame::group(ui.style())
                            .fill(egui::Color32::from_rgb(15, 17, 24))
                            .stroke(egui::Stroke::new(
                                1.0_f32,
                                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 22),
                            ))
                            .rounding(18.0)
                            .inner_margin(24.0)
                            .show(ui, |ui| {
                                // Draw top highlight on the card
                                let painter = ui.painter();
                                let max_r = ui.max_rect();
                                painter.line_segment(
                                    [
                                        egui::pos2(max_r.min.x + 18.0, max_r.min.y + 1.0),
                                        egui::pos2(max_r.max.x - 18.0, max_r.min.y + 1.0),
                                    ],
                                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
                                );

                                match self.active_tab {
                                    ActiveTab::Send => self.render_send_panel(ui, is_send_busy),
                                    ActiveTab::Receive => self.render_receive_panel(ui, is_recv_busy),
                                    ActiveTab::History => self.render_history_panel(ui),
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
    fn render_top_bar(&mut self, ui: &mut egui::Ui) {
        let is_send_busy = self.send_manager.is_busy();
        let is_recv_busy = self.recv_manager.is_busy();

        ui.horizontal(|ui| {
            // Left: Logo & Tech Badge
            draw_bolt_icon(ui, 22.0, egui::Color32::from_rgb(14, 165, 233));
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("HeavyDrop")
                    .size(20.0)
                    .strong()
                    .color(egui::Color32::from_rgb(250, 250, 250)),
            );
            ui.add_space(8.0);

            egui::Frame::group(ui.style())
                .fill(egui::Color32::from_rgb(20, 23, 34))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
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

            // Center: Segmented Navigation Tabs
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add_space(14.0);
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgb(17, 19, 28))
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                    ))
                    .rounding(20.0)
                    .inner_margin(egui::Margin::symmetric(4.0, 3.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // Tab 1: Enviar
                            let send_active = self.active_tab == ActiveTab::Send;
                            let send_title = if is_send_busy {
                                "📤 Enviar (Activo)"
                            } else {
                                "📤 Enviar"
                            };
                            let send_btn = ui.add(
                                egui::Button::new(
                                    egui::RichText::new(send_title)
                                        .size(12.5)
                                        .strong()
                                        .color(if send_active {
                                            egui::Color32::WHITE
                                        } else {
                                            egui::Color32::from_rgb(156, 163, 175)
                                        }),
                                )
                                .fill(if send_active {
                                    egui::Color32::from_rgb(14, 165, 233)
                                } else {
                                    egui::Color32::TRANSPARENT
                                })
                                .stroke(if send_active {
                                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60))
                                } else {
                                    egui::Stroke::NONE
                                })
                                .rounding(16.0),
                            );
                            if send_btn.clicked() {
                                self.active_tab = ActiveTab::Send;
                            }

                            // Tab 2: Recibir
                            let recv_active = self.active_tab == ActiveTab::Receive;
                            let recv_title = if is_recv_busy {
                                "📥 Recibir (Activo)"
                            } else {
                                "📥 Recibir"
                            };
                            let recv_btn = ui.add(
                                egui::Button::new(
                                    egui::RichText::new(recv_title)
                                        .size(12.5)
                                        .strong()
                                        .color(if recv_active {
                                            egui::Color32::WHITE
                                        } else {
                                            egui::Color32::from_rgb(156, 163, 175)
                                        }),
                                )
                                .fill(if recv_active {
                                    egui::Color32::from_rgb(14, 165, 233)
                                } else {
                                    egui::Color32::TRANSPARENT
                                })
                                .stroke(if recv_active {
                                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60))
                                } else {
                                    egui::Stroke::NONE
                                })
                                .rounding(16.0),
                            );
                            if recv_btn.clicked() {
                                self.active_tab = ActiveTab::Receive;
                            }

                            // Tab 3: Historial
                            let hist_active = self.active_tab == ActiveTab::History;
                            let hist_title = if !self.history.is_empty() {
                                format!("🕒 Historial ({})", self.history.len())
                            } else {
                                "🕒 Historial".to_string()
                            };
                            let hist_btn = ui.add(
                                egui::Button::new(
                                    egui::RichText::new(hist_title)
                                        .size(12.5)
                                        .strong()
                                        .color(if hist_active {
                                            egui::Color32::WHITE
                                        } else {
                                            egui::Color32::from_rgb(156, 163, 175)
                                        }),
                                )
                                .fill(if hist_active {
                                    egui::Color32::from_rgb(14, 165, 233)
                                } else {
                                    egui::Color32::TRANSPARENT
                                })
                                .stroke(if hist_active {
                                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60))
                                } else {
                                    egui::Stroke::NONE
                                })
                                .rounding(16.0),
                            );
                            if hist_btn.clicked() {
                                self.active_tab = ActiveTab::History;
                            }
                        });
                    });
            });

            // Right: Settings, Speed Limits & Network Status
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Settings button (⚙)
                let settings_btn = ui.add(
                    egui::Button::new(
                        egui::RichText::new("⚙")
                            .size(14.0)
                            .color(if self.settings_open {
                                egui::Color32::from_rgb(56, 189, 248)
                            } else {
                                egui::Color32::from_rgb(156, 163, 175)
                            }),
                    )
                    .fill(if self.settings_open {
                        egui::Color32::from_rgb(24, 32, 48)
                    } else {
                        egui::Color32::from_rgb(20, 23, 32)
                    })
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                    ))
                    .rounding(10.0),
                );
                if settings_btn.clicked() {
                    self.settings_open = !self.settings_open;
                }

                ui.add_space(4.0);

                // Speed limiter toggle
                let speed_active = self.upload_limit_mb > 0 || self.download_limit_mb > 0;
                let speed_text = if speed_active {
                    let mut s = String::from("⚡ ");
                    if self.upload_limit_mb > 0 {
                        s.push_str(&format!("↑{}M ", self.upload_limit_mb));
                    }
                    if self.download_limit_mb > 0 {
                        s.push_str(&format!("↓{}M", self.download_limit_mb));
                    }
                    s
                } else if self.speed_sidebar_open {
                    "⚡ Límites ▾".to_string()
                } else {
                    "⚡ Velocidad".to_string()
                };

                let speed_btn = ui.add(
                    egui::Button::new(
                        egui::RichText::new(speed_text)
                            .size(11.0)
                            .strong()
                            .color(if self.speed_sidebar_open || speed_active {
                                egui::Color32::from_rgb(56, 189, 248)
                            } else {
                                egui::Color32::from_rgb(212, 212, 216)
                            }),
                    )
                    .fill(if self.speed_sidebar_open {
                        egui::Color32::from_rgb(20, 36, 54)
                    } else {
                        egui::Color32::from_rgb(20, 23, 32)
                    })
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                    ))
                    .rounding(10.0),
                );
                if speed_btn.clicked() {
                    self.speed_sidebar_open = !self.speed_sidebar_open;
                }

                ui.add_space(4.0);

                // Network indicator badge
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgb(14, 26, 20))
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_unmultiplied(34, 197, 94, 60),
                    ))
                    .rounding(10.0)
                    .inner_margin(egui::Margin::symmetric(9.0, 4.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
                            ui.painter().circle_filled(dot_rect.center(), 3.0, egui::Color32::from_rgb(34, 197, 94));
                            ui.add_space(3.0);
                            ui.label(
                                egui::RichText::new("P2P Listo")
                                    .size(11.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(34, 197, 94)),
                            );
                        });
                    });
            });
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

        ui.horizontal(|ui| {
            draw_upload_icon(ui, egui::Color32::from_rgb(56, 189, 248));
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Enviar archivo a través de túnel P2P")
                    .size(16.5)
                    .strong()
                    .color(egui::Color32::from_rgb(244, 244, 245)),
            );
        });
        ui.label(
            egui::RichText::new("Transferencia directa de alta velocidad cifrada extremo a extremo sin almacenamiento en la nube.")
                .size(11.5)
                .color(egui::Color32::from_rgb(148, 163, 184)),
        );
        ui.add_space(12.0);

        // 1. Dropzone or Selected File Inspector
        if let Some(ref path) = self.send_file_path {
            let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let file_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let extension = path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            // High-Definition Selected File Inspector Card
            egui::Frame::group(ui.style())
                .fill(egui::Color32::from_rgb(18, 22, 32))
                .stroke(egui::Stroke::new(
                    1.2_f32,
                    egui::Color32::from_rgb(56, 189, 248),
                ))
                .rounding(16.0)
                .inner_margin(18.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        draw_file_type_badge(ui, &extension);
                        ui.add_space(10.0);

                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(&file_name)
                                    .size(15.5)
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            );

                            ui.add_space(2.0);
                            ui.horizontal(|ui| {
                                // Size tag
                                egui::Frame::group(ui.style())
                                    .fill(egui::Color32::from_rgb(14, 34, 24))
                                    .stroke(egui::Stroke::new(
                                        1.0_f32,
                                        egui::Color32::from_rgba_unmultiplied(34, 197, 94, 70),
                                    ))
                                    .rounding(8.0)
                                    .inner_margin(egui::Margin::symmetric(7.0, 2.0))
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} ({:.2} GB)",
                                                indicatif::HumanBytes(file_size),
                                                file_size as f64 / (1024.0 * 1024.0 * 1024.0)
                                            ))
                                            .size(11.0)
                                            .strong()
                                            .color(egui::Color32::from_rgb(74, 222, 128)),
                                        );
                                    });

                                ui.label(
                                    egui::RichText::new("• Listo para transmitir")
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(148, 163, 184)),
                                );
                            });
                        });

                        if !is_send_busy && session_opt.is_none() {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .button(
                                        egui::RichText::new("✕ Quitar")
                                            .size(11.5)
                                            .color(egui::Color32::from_rgb(239, 68, 68)),
                                    )
                                    .clicked()
                                {
                                    self.send_file_path = None;
                                }

                                ui.add_space(6.0);

                                if ui
                                    .button(
                                        egui::RichText::new("🔄 Cambiar")
                                            .size(11.5)
                                            .color(egui::Color32::from_rgb(228, 228, 231)),
                                    )
                                    .clicked()
                                {
                                    if let Some(file) = rfd::FileDialog::new().pick_file() {
                                        self.send_file_path = Some(file);
                                    }
                                }
                            });
                        }
                    });
                });
        } else {
            // Interactive Hero Dropzone
            let is_window_drag = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
            let drop_box_rect = {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 160.0),
                    egui::Sense::hover(),
                );
                rect
            };

            let is_hovered = ui.rect_contains_pointer(drop_box_rect) || is_window_drag;

            let bg_color = if is_hovered {
                egui::Color32::from_rgb(16, 28, 46)
            } else {
                egui::Color32::from_rgb(16, 19, 28)
            };

            let border_stroke = if is_hovered {
                egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(56, 189, 248))
            } else {
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 28),
                )
            };

            let painter = ui.painter();
            draw_card_with_top_highlight(painter, drop_box_rect, 16.0, bg_color, border_stroke);

            if !is_hovered {
                draw_dashed_border(painter, drop_box_rect, 16.0, border_stroke, 8.0, 5.0);
            }

            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(drop_box_rect), |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(14.0);
                    draw_cloud_upload_hero(ui, is_hovered);
                    ui.add_space(4.0);

                    let title_text = if is_window_drag {
                        "¡Soltá el archivo aquí!"
                    } else {
                        "Arrastrá y soltá tu archivo aquí"
                    };

                    ui.label(
                        egui::RichText::new(title_text)
                            .size(16.0)
                            .strong()
                            .color(if is_hovered {
                                egui::Color32::from_rgb(56, 189, 248)
                            } else {
                                egui::Color32::from_rgb(250, 250, 250)
                            }),
                    );

                    ui.label(
                        egui::RichText::new("Optimizado para archivos pesados (50GB+ hasta 1TB+ sin límite)")
                            .size(11.5)
                            .color(egui::Color32::from_rgb(148, 163, 184)),
                    );

                    ui.add_space(8.0);
                    let pick_btn = ui.add(
                        egui::Button::new(
                            egui::RichText::new("📁 Explorar archivos en este equipo")
                                .size(12.0)
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(26, 32, 46))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgba_unmultiplied(56, 189, 248, 90),
                        ))
                        .rounding(14.0),
                    );
                    if pick_btn.clicked() {
                        if let Some(file) = rfd::FileDialog::new().pick_file() {
                            self.send_file_path = Some(file);
                        }
                    }

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("🔒 Cifrado E2EE ChaCha20  •  ⚡ Pipeline QUIC 64MB  •  ✓ BLAKE3 por bloque")
                                .size(10.5)
                                .color(egui::Color32::from_rgb(100, 116, 139)),
                        );
                    });
                });
            });
        }

        ui.add_space(14.0);

        // 2. Connection Code Card
        let has_session = progress.pairing_code.is_some()
            && (progress.status == TransferStatus::WaitingForPeer
                || progress.status == TransferStatus::Preparing);

        ui.label(
            egui::RichText::new("Código de invitación segura")
                .size(12.0)
                .strong()
                .color(egui::Color32::from_rgb(203, 213, 225)),
        );
        ui.add_space(4.0);

        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(17, 20, 28))
            .stroke(egui::Stroke::new(
                1.0_f32,
                if has_session {
                    egui::Color32::from_rgb(56, 189, 248)
                } else {
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18)
                },
            ))
            .rounding(14.0)
            .inner_margin(egui::Margin::symmetric(16.0, 12.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(ref invitation) =
                        progress.pairing_code.as_ref().filter(|_| has_session)
                    {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(if invitation.starts_with("heavy1_") {
                                    "Invitación P2P Cifrada Lista"
                                } else {
                                    invitation
                                })
                                .size(15.0)
                                .strong()
                                .color(egui::Color32::WHITE),
                            );
                            ui.label(
                                egui::RichText::new("Copia y comparte este token para conectar al receptor")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(148, 163, 184)),
                            );
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let recently_copied = self
                                .copied_toast_time
                                .map(|t| t.elapsed().as_secs() < 2)
                                .unwrap_or(false);

                            let (btn_fill, btn_color, label_text) = if recently_copied {
                                (
                                    egui::Color32::from_rgb(22, 101, 52),
                                    egui::Color32::from_rgb(74, 222, 128),
                                    "✓ ¡Copiado!",
                                )
                            } else {
                                (
                                    egui::Color32::from_rgb(14, 165, 233),
                                    egui::Color32::WHITE,
                                    "📋 COPIAR INVITACIÓN",
                                )
                            };

                            let copy_btn = ui.add(
                                egui::Button::new(
                                    egui::RichText::new(label_text)
                                        .size(12.0)
                                        .strong()
                                        .color(btn_color),
                                )
                                .fill(btn_fill)
                                .stroke(egui::Stroke::new(
                                    1.0_f32,
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40),
                                ))
                                .rounding(12.0),
                            );

                            if copy_btn.clicked() {
                                ui.output_mut(|o| o.copied_text = invitation.to_string());
                                self.show_toast("¡Invitación copiada al portapapeles!");
                            }
                        });
                    } else if is_send_busy {
                        ui.label(
                            egui::RichText::new("⚡ Estableciendo túnel P2P y generando clave efímera...")
                                .size(13.5)
                                .color(egui::Color32::from_rgb(56, 189, 248)),
                        );
                    } else {
                        ui.horizontal(|ui| {
                            draw_shield_icon(ui, 16.0, egui::Color32::from_rgb(148, 163, 184));
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new("Se generará una clave efímera X25519 de un solo uso para conectar al receptor.")
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(148, 163, 184)),
                            );
                        });
                    }
                });
            });

        ui.add_space(10.0);

        // 3. Primary Actions (Hero Button)
        if let Some(ref session) = session_opt {
            ui.horizontal(|ui| {
                draw_clock_icon(ui, egui::Color32::from_rgb(148, 163, 184));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(if has_session {
                        format!("Expira en {}", session.format_remaining())
                    } else if is_send_busy {
                        "Esperando conexión con el receptor...".into()
                    } else {
                        "Servidor cerrado".into()
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
                egui::Color32::from_rgb(14, 165, 233)
            } else {
                egui::Color32::from_rgb(24, 30, 44)
            };
            let text_color = egui::Color32::WHITE;

            let button_text = if let Some(ref path) = self.send_file_path {
                let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                format!(
                    "⚡ CREAR TRANSFERENCIA SEGURA ({:.2} GB)",
                    file_size as f64 / (1024.0 * 1024.0 * 1024.0)
                )
            } else {
                "⚡ Seleccionar archivo para transferir".to_string()
            };

            let start_btn = ui.add_sized(
                [ui.available_width(), 46.0],
                egui::Button::new(
                    egui::RichText::new(button_text)
                        .size(14.5)
                        .strong()
                        .color(text_color),
                )
                .fill(btn_fill)
                .stroke(egui::Stroke::new(
                    1.2_f32,
                    if has_file {
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60)
                    } else {
                        egui::Color32::from_rgb(56, 189, 248)
                    },
                ))
                .rounding(23.0),
            );

            if start_btn.clicked() {
                if has_file {
                    self.start_send_task();
                } else if let Some(file) = rfd::FileDialog::new().pick_file() {
                    self.send_file_path = Some(file);
                    self.start_send_task();
                }
            }
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

        ui.horizontal(|ui| {
            draw_download_icon(ui, egui::Color32::from_rgb(56, 189, 248));
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Recibir archivo")
                    .size(16.5)
                    .strong()
                    .color(egui::Color32::from_rgb(244, 244, 245)),
            );
        });
        ui.label(
            egui::RichText::new("Pega la invitación de transferencia para conectar con el emisor y descargar el archivo directamente.")
                .size(11.5)
                .color(egui::Color32::from_rgb(148, 163, 184)),
        );
        ui.add_space(12.0);

        // 1. Code Input Field (Command Bar Style)
        ui.label(
            egui::RichText::new("Invitación o código de conexión")
                .size(12.0)
                .strong()
                .color(egui::Color32::from_rgb(203, 213, 225)),
        );
        ui.add_space(4.0);

        let has_code = PairingCode::parse(&self.recv_code_input).is_ok();

        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(17, 20, 28))
            .stroke(egui::Stroke::new(
                1.2_f32,
                if has_code {
                    egui::Color32::from_rgb(56, 189, 248)
                } else {
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 22)
                },
            ))
            .rounding(16.0)
            .inner_margin(egui::Margin::symmetric(14.0, 10.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let edit = ui.add_sized(
                        [ui.available_width() - 40.0, 26.0],
                        egui::TextEdit::singleline(&mut self.recv_code_input)
                            .hint_text("Pega el código o invitación completa (heavy1_…)")
                            .font(egui::TextStyle::Monospace)
                            .char_limit(heavydrop::pairing::MAX_INVITATION_LEN)
                            .frame(false),
                    );
                    let _ = edit;

                    if !self.recv_code_input.is_empty() {
                        if ui.button(egui::RichText::new("✕").size(12.0)).clicked() {
                            self.recv_code_input.clear();
                        }
                    }
                });
            });

        ui.add_space(10.0);

        // 2. Action Button
        if !is_recv_busy {
            let btn_fill = if has_code {
                egui::Color32::from_rgb(14, 165, 233)
            } else {
                egui::Color32::from_rgb(24, 30, 44)
            };
            let text_color = egui::Color32::WHITE;

            let connect_btn = ui.add_sized(
                [ui.available_width(), 46.0],
                egui::Button::new(
                    egui::RichText::new("📥 INICIAR CONEXIÓN Y DESCARGA")
                        .size(14.5)
                        .strong()
                        .color(text_color),
                )
                .fill(btn_fill)
                .stroke(egui::Stroke::new(
                    1.2_f32,
                    if has_code {
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60)
                    } else {
                        egui::Color32::from_rgb(56, 189, 248)
                    },
                ))
                .rounding(23.0),
            );

            if connect_btn.clicked() {
                self.start_receive_task();
            }
        } else {
            let cancel_btn = ui.add_sized(
                [ui.available_width(), 46.0],
                egui::Button::new(
                    egui::RichText::new("🛑 CANCELAR CONEXIÓN")
                        .size(14.0)
                        .strong()
                        .color(egui::Color32::from_rgb(239, 68, 68)),
                )
                .fill(egui::Color32::from_rgb(34, 20, 20))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgb(239, 68, 68),
                ))
                .rounding(23.0),
            );
            if cancel_btn.clicked() {
                self.cancel_recv_transfer();
            }
        }

        ui.add_space(12.0);

        // 3. Destination Folder Selector & Visual Disk Space Meter
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(17, 20, 28))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18),
            ))
            .rounding(14.0)
            .inner_margin(egui::Margin::symmetric(14.0, 10.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    draw_folder_icon(ui, 15.0, egui::Color32::from_rgb(56, 189, 248));
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Guardar en:")
                            .size(11.5)
                            .color(egui::Color32::from_rgb(148, 163, 184)),
                    );

                    let dir_str = self.recv_output_dir.display().to_string();
                    let truncated = if dir_str.chars().count() > 28 {
                        let suffix: String = dir_str
                            .chars()
                            .rev()
                            .take(24)
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
                            .size(11.5)
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
                                .fill(egui::Color32::from_rgb(28, 30, 40))
                                .rounding(10.0),
                            );
                            if change_btn.clicked() {
                                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                    self.recv_output_dir = dir;
                                    self.cached_free_space = None;
                                }
                            }
                        });
                    }
                });

                // Disk space indicator
                if let Some(free_bytes) = self.get_free_disk_space() {
                    ui.add_space(4.0);
                    let free_gb = free_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                    let (disk_col, disk_text) = if free_gb >= 50.0 {
                        (
                            egui::Color32::from_rgb(34, 197, 94),
                            format!("{:.1} GB disponibles en disco (Óptimo para 50GB+)", free_gb),
                        )
                    } else if free_gb >= 15.0 {
                        (
                            egui::Color32::from_rgb(234, 179, 8),
                            format!("{:.1} GB disponibles (Espacio moderado)", free_gb),
                        )
                    } else {
                        (
                            egui::Color32::from_rgb(239, 68, 68),
                            format!("{:.1} GB disponibles (¡Espacio bajo en disco!)", free_gb),
                        )
                    };

                    ui.horizontal(|ui| {
                        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot_rect.center(), 2.8, disk_col);
                        ui.add_space(2.0);
                        ui.label(egui::RichText::new(disk_text).size(10.5).color(disk_col));
                    });
                }
            });

        ui.add_space(12.0);

        // 4. Live Dashboard or Ready Status
        if is_recv_active {
            self.render_transfer_dashboard(ui, &progress, false);
        } else {
            egui::Frame::group(ui.style())
                .fill(egui::Color32::from_rgb(17, 20, 28))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14),
                ))
                .rounding(14.0)
                .inner_margin(egui::Margin::symmetric(16.0, 14.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        draw_file_icon(ui, 24.0, egui::Color32::from_rgb(100, 116, 139));
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
                                    "Pega la invitación de arriba para establecer el túnel P2P seguro",
                                )
                                .size(11.0)
                                .color(egui::Color32::from_rgb(113, 113, 122)),
                            );
                        });
                    });

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        draw_shield_icon(ui, 13.0, egui::Color32::from_rgb(34, 197, 94));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Conexión cifrada E2EE ChaCha20-Poly1305 • P2P directo")
                                .size(11.0)
                                .color(egui::Color32::from_rgb(148, 163, 184)),
                        );
                    });
                });
        }

        if let Some(ref err) = self.recv_error {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("Error: {}", err))
                    .size(12.0)
                    .color(egui::Color32::from_rgb(239, 68, 68)),
            );
        }
    }

    // Modern Integrated Transfer Progress Dashboard & Live Telemetry
    fn render_transfer_dashboard(
        &mut self,
        ui: &mut egui::Ui,
        progress: &TransferProgress,
        is_sender: bool,
    ) {
        let is_completed = progress.status == TransferStatus::Completed;

        egui::Frame::group(ui.style())
            .fill(if is_completed {
                egui::Color32::from_rgb(14, 26, 20)
            } else {
                egui::Color32::from_rgb(16, 20, 30)
            })
            .stroke(egui::Stroke::new(
                1.2_f32,
                if is_completed {
                    egui::Color32::from_rgb(34, 197, 94)
                } else {
                    egui::Color32::from_rgb(56, 189, 248)
                },
            ))
            .rounding(16.0)
            .inner_margin(16.0)
            .show(ui, |ui| {
                // Header of Dashboard
                ui.horizontal(|ui| {
                    if !progress.file_name.is_empty() {
                        draw_file_icon(ui, 16.0, egui::Color32::from_rgb(56, 189, 248));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(&progress.file_name)
                                .size(14.0)
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                    }

                    // Connectivity badge
                    let (badge_color, badge_text) = match progress.connection_type {
                        ConnectionType::DirectLocal => (egui::Color32::from_rgb(34, 197, 94), "● LAN Directo"),
                        ConnectionType::DirectInternet => (egui::Color32::from_rgb(56, 189, 248), "● P2P Directo"),
                        ConnectionType::Relay => (egui::Color32::from_rgb(234, 179, 8), "● Relay Cifrado"),
                        ConnectionType::None => (egui::Color32::from_rgb(148, 163, 184), "○ Conectando..."),
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

                // Live Speed Sparkline
                if progress.status == TransferStatus::Transferring {
                    draw_speed_sparkline(ui, &self.speed_history, self.peak_speed_mb);
                    ui.add_space(8.0);
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

                ui.add_space(8.0);

                // Stats Grid: Velocidad, Bytes transferidos, ETA
                ui.horizontal(|ui| {
                    let speed_mb = progress.bytes_per_sec / (1024.0 * 1024.0);
                    ui.label(
                        egui::RichText::new(format!("⚡ {:.2} MB/s", speed_mb))
                            .size(12.0)
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
                            .size(12.0)
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
                                .size(12.0)
                                .color(egui::Color32::from_rgb(56, 189, 248)),
                            );
                        }
                    }
                });

                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(&progress.status_text)
                        .size(11.0)
                        .color(if is_completed {
                            egui::Color32::from_rgb(74, 222, 128)
                        } else {
                            egui::Color32::from_rgb(148, 163, 184)
                        }),
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
                                    .size(11.5)
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
                    } else {
                        if !is_sender && is_completed {
                            let target_file = self.recv_output_dir.join(&progress.file_name);
                            if ui
                                .button(
                                    egui::RichText::new("📁 Abrir en carpeta")
                                        .size(11.5)
                                        .strong()
                                        .color(egui::Color32::from_rgb(56, 189, 248)),
                                )
                                .clicked()
                            {
                                open_in_file_manager(&target_file);
                            }
                            ui.add_space(6.0);
                        }

                        if ui
                            .button(
                                egui::RichText::new("Listo / Cerrar")
                                    .size(11.5)
                                    .strong(),
                            )
                            .clicked()
                        {
                            if is_sender {
                                self.send_manager.reset_cancel();
                                self.send_session = None;
                            } else {
                                self.recv_manager.reset_cancel();
                            }
                        }
                    }
                });
            });
    }

    // History Panel
    fn render_history_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Historial de Transferencias")
                    .size(16.5)
                    .strong()
                    .color(egui::Color32::from_rgb(244, 244, 245)),
            );

            if !self.history.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(
                            egui::RichText::new("🗑 Limpiar historial")
                                .size(11.0)
                                .color(egui::Color32::from_rgb(156, 163, 175)),
                        )
                        .clicked()
                    {
                        self.history.clear();
                        save_history(&self.history_file_path, &self.history);
                    }
                });
            }
        });

        ui.add_space(12.0);

        if self.history.is_empty() {
            egui::Frame::group(ui.style())
                .fill(egui::Color32::from_rgb(17, 20, 28))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14),
                ))
                .rounding(14.0)
                .inner_margin(24.0)
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(10.0);
                        draw_clock_icon(ui, egui::Color32::from_rgb(100, 116, 139));
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Sin transferencias recientes")
                                .size(14.5)
                                .strong()
                                .color(egui::Color32::from_rgb(228, 228, 231)),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Los archivos que envíes o recibas quedarán registrados aquí con sus métricas.",
                            )
                            .size(11.5)
                            .color(egui::Color32::from_rgb(148, 163, 184)),
                        );
                        ui.add_space(10.0);
                    });
                });
        } else {
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    for item in self.history.clone().iter() {
                        egui::Frame::group(ui.style())
                            .fill(egui::Color32::from_rgb(17, 20, 28))
                            .stroke(egui::Stroke::new(
                                1.0_f32,
                                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 16),
                            ))
                            .rounding(12.0)
                            .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if item.role == "Enviado" {
                                        draw_upload_icon(ui, egui::Color32::from_rgb(56, 189, 248));
                                    } else {
                                        draw_download_icon(ui, egui::Color32::from_rgb(34, 197, 94));
                                    }
                                    ui.add_space(6.0);

                                    ui.vertical(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(&item.file_name)
                                                    .size(13.5)
                                                    .strong()
                                                    .color(egui::Color32::WHITE),
                                            );
                                            let (status_color, status_text) = match item.status.as_str() {
                                                "Completado" => (egui::Color32::from_rgb(34, 197, 94), "✓ Completado"),
                                                "Cancelado" => (egui::Color32::from_rgb(234, 179, 8), "○ Cancelado"),
                                                _ => (egui::Color32::from_rgb(239, 68, 68), "✕ Error"),
                                            };
                                            ui.label(
                                                egui::RichText::new(status_text)
                                                    .size(10.5)
                                                    .strong()
                                                    .color(status_color),
                                            );
                                        });

                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{} • {:.1} MB/s prom. • {}",
                                                    indicatif::HumanBytes(item.total_bytes),
                                                    item.avg_speed_mb,
                                                    item.timestamp_str
                                                ))
                                                .size(11.0)
                                                .color(egui::Color32::from_rgb(148, 163, 184)),
                                            );
                                        });
                                    });

                                    if let Some(ref path) = item.destination_path {
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if ui.button(egui::RichText::new("📁 Abrir").size(10.5)).clicked() {
                                                open_in_file_manager(path);
                                            }
                                        });
                                    }
                                });
                            });
                        ui.add_space(6.0);
                    }
                });
        }
    }

    // Settings Window Modal
    fn render_settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }

        let is_busy = self.send_manager.is_busy() || self.recv_manager.is_busy();

        egui::Window::new("⚙ Ajustes y Configuración de Red")
            .collapsible(false)
            .resizable(false)
            .fixed_size(egui::vec2(440.0, 360.0))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(egui::Color32::from_rgb(14, 16, 24))
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30),
                    ))
                    .rounding(16.0)
                    .inner_margin(20.0),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Ajustes de Conectividad")
                            .size(16.0)
                            .strong()
                            .color(egui::Color32::WHITE),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("✕").size(14.0)).clicked() {
                            self.settings_open = false;
                        }
                    });
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);

                // 1. Signaling & Relay Server
                ui.label(
                    egui::RichText::new("Servidor Manual de Señalización (Opcional)")
                        .size(12.5)
                        .strong()
                        .color(egui::Color32::from_rgb(228, 228, 231)),
                );
                ui.add_space(3.0);
                ui.label(
                    egui::RichText::new(
                        "Por defecto HeavyDrop descubre pares locales automáticamente y usa relay QUIC cifrado. Si usas tu propio servidor de señalización, ingrésalo aquí:",
                    )
                    .size(11.0)
                    .color(egui::Color32::from_rgb(148, 163, 184)),
                );
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.add_enabled(
                        !is_busy,
                        egui::TextEdit::singleline(&mut self.signal_server_input)
                            .hint_text("servidor:7880")
                            .desired_width(280.0),
                    );
                    if ui.button("Por defecto").clicked() {
                        self.signal_server_input =
                            heavydrop::signaling::SignalingClient::default_server().unwrap_or_default();
                    }
                });

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(10.0);

                // 2. Protocol & Network Diagnostics
                ui.label(
                    egui::RichText::new("Información de Protocolo & Red")
                        .size(12.5)
                        .strong()
                        .color(egui::Color32::from_rgb(228, 228, 231)),
                );
                ui.add_space(4.0);

                let local_ip = local_ip_address::local_ip()
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|_| "No detectada".into());

                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("• IP Local:").size(11.0).color(egui::Color32::from_rgb(148, 163, 184)));
                    ui.label(egui::RichText::new(local_ip).size(11.0).monospace().strong().color(egui::Color32::WHITE));
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("• Descubrimiento LAN:").size(11.0).color(egui::Color32::from_rgb(148, 163, 184)));
                    ui.label(egui::RichText::new("Activo (UDP Broadcast / mDNS)").size(11.0).color(egui::Color32::from_rgb(34, 197, 94)));
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("• Cifrado E2EE:").size(11.0).color(egui::Color32::from_rgb(148, 163, 184)));
                    ui.label(egui::RichText::new("ChaCha20-Poly1305 + X25519").size(11.0).color(egui::Color32::from_rgb(56, 189, 248)));
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("• Integridad:").size(11.0).color(egui::Color32::from_rgb(148, 163, 184)));
                    ui.label(egui::RichText::new("BLAKE3 multihilo por chunks").size(11.0).color(egui::Color32::from_rgb(56, 189, 248)));
                });

                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(egui::RichText::new("Listo").strong()).clicked() {
                        self.settings_open = false;
                    }
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

        // Upload Section
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

        // Download Section
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

        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(18, 22, 32))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 15),
            ))
            .rounding(10.0)
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("🚀 Optimización QUIC")
                        .size(12.0)
                        .strong()
                        .color(egui::Color32::from_rgb(56, 189, 248)),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("• Ventana BDP: 64 MB ampliados\n• Pipeline I/O paralelo y cifrado AEAD\n• Escritura diferida en disco (32 MB chunks)")
                        .size(10.5)
                        .color(egui::Color32::from_rgb(148, 163, 184)),
                );
            });
    }
}

// ----------------------------------------------------------------------------
// Utilities & Platform Helpers
// ----------------------------------------------------------------------------

fn dirs_next_output_dir() -> PathBuf {
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let downloads = PathBuf::from(user_profile).join("Downloads");
        if downloads.exists() {
            return downloads;
        }
    }
    PathBuf::from(".")
}

fn open_in_file_manager(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    {
        if path.is_file() {
            let _ = std::process::Command::new("explorer")
                .arg(format!("/select,\"{}\"", path.display()))
                .spawn();
        } else {
            let _ = std::process::Command::new("explorer")
                .arg(path.as_os_str())
                .spawn();
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

#[cfg(target_os = "windows")]
fn get_available_disk_space(path: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let path_str: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free_bytes_available: u64 = 0;
    let mut total_number_of_bytes: u64 = 0;
    let mut total_number_of_free_bytes: u64 = 0;

    extern "system" {
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
    }

    let res = unsafe {
        GetDiskFreeSpaceExW(
            path_str.as_ptr(),
            &mut free_bytes_available,
            &mut total_number_of_bytes,
            &mut total_number_of_free_bytes,
        )
    };
    if res != 0 {
        Some(free_bytes_available)
    } else {
        None
    }
}

#[cfg(not(target_os = "windows"))]
fn get_available_disk_space(_path: &std::path::Path) -> Option<u64> {
    None
}

fn current_timestamp_str() -> String {
    let now = std::time::SystemTime::now();
    let duration = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = duration.as_secs();
    format_timestamp_secs(secs)
}

fn format_timestamp_secs(secs: u64) -> String {
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = (time_secs / 3600) % 24;
    let mins = (time_secs / 60) % 60;

    let mut y = 1970;
    let mut d = days;
    loop {
        let leap = if (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0) { 1 } else { 0 };
        let days_in_year = 365 + leap;
        if d >= days_in_year {
            d -= days_in_year;
            y += 1;
        } else {
            break;
        }
    }
    let leap = if (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0) { 1 } else { 0 };
    let month_days = [31, 28 + leap, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 1;
    for (idx, &md) in month_days.iter().enumerate() {
        if d >= md {
            d -= md;
        } else {
            m = idx + 1;
            break;
        }
    }
    let day = d + 1;
    format!("{:04}-{:02}-{:02} {:02}:{:02}", y, m, day, hours, mins)
}

fn get_history_file_path() -> PathBuf {
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let app_dir = PathBuf::from(user_profile).join(".heavydrop");
        let _ = std::fs::create_dir_all(&app_dir);
        return app_dir.join("history.json");
    }
    PathBuf::from("heavydrop_history.json")
}

fn load_history(path: &std::path::Path) -> Vec<HistoryRecord> {
    if let Ok(data) = std::fs::read_to_string(path) {
        if let Ok(items) = serde_json::from_str::<Vec<HistoryRecord>>(&data) {
            return items;
        }
    }
    Vec::new()
}

fn save_history(path: &std::path::Path, items: &[HistoryRecord]) {
    if let Ok(data) = serde_json::to_string_pretty(items) {
        let _ = std::fs::write(path, data);
    }
}
