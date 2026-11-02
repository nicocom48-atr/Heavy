use eframe::egui;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
    direct_only: bool,
    legacy_tcp: bool,
    verify_full_file: bool,

    // UI feedback
    copied_toast_time: Option<Instant>,
    copied_toast_text: String,

    // Bandwidth controls
    upload_limiter: heavydrop::speed_limiter::SpeedLimiter,
    download_limiter: heavydrop::speed_limiter::SpeedLimiter,
    upload_limit_mb: u32,
    download_limit_mb: u32,

    // Internet Speed Test
    speedtest_state: Arc<parking_lot::Mutex<heavydrop::speedtest::SpeedTestPhase>>,
    speedtest_cancel: Option<Arc<AtomicBool>>,

    history_expanded: bool,

    // Live Telemetry & Metrics
    send_metrics: TransferMetrics,
    recv_metrics: TransferMetrics,

    // History Tracking
    last_send_status: TransferStatus,
    last_recv_status: TransferStatus,
    history: Vec<HistoryRecord>,
    history_file_path: PathBuf,
    clear_history_confirm: bool,

    // Cached disk space
    cached_free_space: Option<(Instant, u64)>,
}

impl HeavyDropApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: Arc<Runtime>) -> Self {
        Self::with_context(&cc.egui_ctx, rt, get_history_file_path())
    }

    fn with_context(ctx: &egui::Context, rt: Arc<Runtime>, history_file_path: PathBuf) -> Self {
        Self::apply_custom_theme(ctx);

        let default_output_dir = dirs_next_output_dir();
        let send_manager = TransferManager::new();
        let recv_manager = TransferManager::new();
        let discovery = Arc::new(DiscoveryService::new());
        let upload_limiter = heavydrop::speed_limiter::SpeedLimiter::default();
        let download_limiter = heavydrop::speed_limiter::SpeedLimiter::default();

        let history = load_history(&history_file_path);

        Self {
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
            direct_only: false,
            legacy_tcp: false,
            verify_full_file: false,

            copied_toast_time: None,
            copied_toast_text: String::new(),

            upload_limiter,
            download_limiter,
            upload_limit_mb: 0,
            download_limit_mb: 0,

            speedtest_state: Arc::new(parking_lot::Mutex::new(heavydrop::speedtest::SpeedTestPhase::Idle)),
            speedtest_cancel: None,

            history_expanded: false,

            send_metrics: TransferMetrics::default(),
            recv_metrics: TransferMetrics::default(),

            last_send_status: TransferStatus::Idle,
            last_recv_status: TransferStatus::Idle,
            history,
            history_file_path,
            clear_history_confirm: false,

            cached_free_space: None,
        }
    }

    fn apply_custom_theme(ctx: &egui::Context) {
        // 1. Install high-DPI Windows Fluent Fonts
        let mut fonts = egui::FontDefinitions::default();
        if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") {
            fonts
                .font_data
                .insert("segoe_ui".to_owned(), egui::FontData::from_owned(data));
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "segoe_ui".to_owned());
        }
        if let Ok(mono) = std::fs::read("C:\\Windows\\Fonts\\CascadiaMono.ttf")
            .or_else(|_| std::fs::read("C:\\Windows\\Fonts\\consola.ttf"))
        {
            fonts
                .font_data
                .insert("custom_mono".to_owned(), egui::FontData::from_owned(mono));
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .insert(0, "custom_mono".to_owned());
        }
        ctx.set_fonts(fonts);

        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = CANVAS;
        visuals.panel_fill = CANVAS;
        visuals.extreme_bg_color = FIELD;
        visuals.faint_bg_color = SURFACE;
        visuals.code_bg_color = FIELD;
        visuals.override_text_color = Some(TEXT);
        visuals.selection.bg_fill = egui::Color32::from_rgb(16, 70, 88);
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT);
        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.bg_fill = FIELD;
            widget.weak_bg_fill = FIELD;
            widget.bg_stroke = egui::Stroke::NONE;
            widget.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
            widget.rounding = egui::Rounding::same(10.0);
            widget.expansion = 0.0;
        }
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(21, 43, 58);
        visuals.widgets.hovered.weak_bg_fill = visuals.widgets.hovered.bg_fill;
        visuals.widgets.hovered.fg_stroke.color = ACCENT;
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(19, 59, 75);
        visuals.widgets.active.weak_bg_fill = visuals.widgets.active.bg_fill;
        visuals.widgets.active.fg_stroke.color = TEXT;
        visuals.window_rounding = egui::Rounding::same(16.0);
        visuals.menu_rounding = egui::Rounding::same(10.0);
        ctx.set_visuals(visuals);

        let mut style = (*ctx.style()).clone();
        style.text_styles = [
            (egui::TextStyle::Heading, egui::FontId::proportional(22.0)),
            (
                egui::TextStyle::Name("Subheading".into()),
                egui::FontId::proportional(16.5),
            ),
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
        if self.send_manager.is_busy() {
            return;
        }
        let file_path = match self.send_file_path.clone() {
            Some(p) => p,
            None => {
                self.send_error = Some("Por favor selecciona un archivo primero.".to_string());
                return;
            }
        };

        if !file_path.is_file() {
            self.send_error =
                Some("El archivo ya no está disponible. Selecciona otro archivo.".into());
            return;
        }

        let session = match Session::new() {
            Ok(s) => s,
            Err(e) => {
                self.send_error = Some(format!("Error al crear sesión: {}", e));
                return;
            }
        };

        self.send_session = Some(session.clone());
        self.send_error = None;
        self.send_metrics = TransferMetrics::default();

        let config = SenderConfig {
            transport_mode: if self.direct_only {
                heavydrop::automatic::TransportMode::DirectOnly
            } else if self.legacy_tcp {
                heavydrop::automatic::TransportMode::LegacyTcp
            } else {
                heavydrop::automatic::TransportMode::Automatic
            },
            file_path,
            session,
            preferred_port: 7878,
            chunk_size: 4 * 1024 * 1024,
            skip_prehash: !self.verify_full_file,
            signaling_server: self.signal_server(),
            shared_progress: None,
            cancel_signal: None,
            speed_limiter: Some(self.upload_limiter.clone()),
        };

        let mgr = self.send_manager.clone();
        // Reserve the UI state before spawning so another click cannot enqueue a second task.
        *mgr.progress().lock() = TransferProgress {
            status: TransferStatus::Preparing,
            role: TransferRole::Sender,
            ..Default::default()
        };
        self.last_send_status = TransferStatus::Preparing;
        self.tokio_rt.spawn(async move {
            let _ = mgr.run_send(config).await;
        });
    }

    fn start_receive_task(&mut self) {
        if self.recv_manager.is_busy() {
            return;
        }
        let parsed_code = match PairingCode::parse(&self.recv_code_input) {
            Ok(c) => c,
            Err(e) => {
                self.recv_error = Some(e.to_string());
                return;
            }
        };

        self.recv_error = None;
        self.recv_metrics = TransferMetrics::default();
        let output_dir = self.recv_output_dir.clone();

        let config = ReceiverConfig { direct_only: self.direct_only,
            allow_legacy_tcp: self.legacy_tcp,
            code: parsed_code,
            output_dir,
            force: false,
            signaling_server: self.signal_server(),
            shared_progress: None,
            cancel_signal: None,
            discovery: Some(self.discovery.clone()),
            speed_limiter: Some(self.download_limiter.clone()),
        };
        if let Err(error) = config.validate_transport() {
            self.recv_error = Some(error.to_string());
            return;
        }

        let mgr = self.recv_manager.clone();
        *mgr.progress().lock() = TransferProgress {
            status: TransferStatus::Preparing,
            role: TransferRole::Receiver,
            ..Default::default()
        };
        self.last_recv_status = TransferStatus::Preparing;
        self.tokio_rt.spawn(async move {
            let _ = mgr.run_recv(config).await;
        });
    }
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

fn draw_cloud_upload_hero(ui: &mut egui::Ui, hovered: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(76.0, 58.0), egui::Sense::hover());
    let p = |x: f32, y: f32| rect.min + egui::vec2(x + 3.0, y + 4.0);
    let stroke = egui::Stroke::new(if hovered { 2.4_f32 } else { 2.0_f32 }, ACCENT);
    for points in [
        [p(16.0, 40.0), p(-2.0, 40.0), p(-2.0, 17.0), p(15.0, 18.0)],
        [p(15.0, 18.0), p(17.0, -2.0), p(40.0, -2.0), p(46.0, 16.0)],
        [p(46.0, 16.0), p(68.0, 12.0), p(72.0, 40.0), p(54.0, 40.0)],
    ] {
        ui.painter()
            .add(egui::epaint::CubicBezierShape::from_points_stroke(
                points,
                false,
                egui::Color32::TRANSPARENT,
                stroke,
            ));
    }
    for line in [
        [p(16.0, 40.0), p(24.0, 40.0)],
        [p(43.0, 40.0), p(54.0, 40.0)],
        [p(34.0, 47.0), p(34.0, 22.0)],
        [p(25.0, 31.0), p(34.0, 22.0)],
        [p(43.0, 31.0), p(34.0, 22.0)],
    ] {
        ui.painter().line_segment(line, stroke);
    }
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
        ext_upper.chars().take(5).collect()
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

fn draw_speed_sparkline(ui: &mut egui::Ui, history: &VecDeque<f32>, peak_speed: f64) {
    let desired_size = egui::vec2(ui.available_width(), 58.0);
    let (rect, _) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    let painter = ui.painter();

    // Background box
    painter.rect_filled(rect, 10.0, egui::Color32::from_rgb(14, 16, 24));
    painter.rect_stroke(
        rect,
        10.0,
        egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14),
        ),
    );

    if history.len() < 2 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Midiendo velocidad…",
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
        painter.circle_filled(
            last_p,
            5.0,
            egui::Color32::from_rgba_unmultiplied(56, 189, 248, 90),
        );
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

const CANVAS: egui::Color32 = egui::Color32::from_rgb(8, 14, 23);
const SURFACE: egui::Color32 = egui::Color32::from_rgb(11, 22, 35);
const FIELD: egui::Color32 = egui::Color32::from_rgb(15, 30, 45);
const BORDER: egui::Color32 = egui::Color32::from_rgb(25, 45, 61);
const TEXT: egui::Color32 = egui::Color32::from_rgb(238, 245, 252);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(25, 211, 237);
const MUTED: egui::Color32 = egui::Color32::from_rgb(143, 166, 188);
const GREEN: egui::Color32 = egui::Color32::from_rgb(66, 224, 181);
const RED: egui::Color32 = egui::Color32::from_rgb(248, 113, 113);

struct TransferMetrics {
    samples: VecDeque<f32>,
    peak: f64,
    started: Option<Instant>,
    last_sample: Instant,
}

impl Default for TransferMetrics {
    fn default() -> Self {
        Self {
            samples: VecDeque::with_capacity(40),
            peak: 0.0,
            started: None,
            last_sample: Instant::now(),
        }
    }
}

impl TransferMetrics {
    fn sample(&mut self, progress: &TransferProgress) {
        if progress.status != TransferStatus::Transferring {
            return;
        }
        self.started.get_or_insert_with(Instant::now);
        if self.last_sample.elapsed().as_millis() >= 350 {
            self.last_sample = Instant::now();
            let speed = progress.bytes_per_sec / (1024.0 * 1024.0);
            self.peak = self.peak.max(speed);
            if self.samples.len() == 40 {
                self.samples.pop_front();
            }
            self.samples.push_back(speed as f32);
        }
    }

    fn average(&self, progress: &TransferProgress) -> f64 {
        self.started
            .map(|start| {
                progress
                    .transferred_bytes
                    .saturating_sub(progress.resume_offset) as f64
                    / start.elapsed().as_secs_f64().max(0.1)
                    / (1024.0 * 1024.0)
            })
            .unwrap_or(0.0)
    }
}

fn terminal(status: TransferStatus) -> bool {
    matches!(
        status,
        TransferStatus::Completed | TransferStatus::Failed | TransferStatus::Cancelled
    )
}

fn card() -> egui::Frame {
    egui::Frame::none()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .rounding(14.0)
        .inner_margin(16.0)
}

fn text_action(ui: &mut egui::Ui, title: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(title).color(ACCENT).size(12.0))
            .frame(false)
            .min_size(egui::vec2(0.0, 28.0)),
    )
}

// Own the text field surface so platform/default widget strokes never leak into the design.
fn text_field(
    ui: &mut egui::Ui,
    value: &mut String,
    placeholder: &str,
    enabled: bool,
    mono: bool,
) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| {
        let frame = egui::Frame::none()
            .fill(FIELD)
            .rounding(10.0)
            .inner_margin(egui::Margin::symmetric(14.0, 13.0));
        let response = frame.show(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(value)
                    .hint_text(placeholder)
                    .font(if mono {
                        egui::TextStyle::Monospace
                    } else {
                        egui::TextStyle::Body
                    })
                    .desired_width(f32::INFINITY)
                    .frame(false)
                    .char_limit(heavydrop::pairing::MAX_INVITATION_LEN),
            )
        });
        if response.inner.has_focus() {
            let rect = response.response.rect;
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left() + 12.0, rect.bottom() - 1.0),
                    egui::pos2(rect.right() - 12.0, rect.bottom() - 1.0),
                ],
                egui::Stroke::new(1.5_f32, ACCENT),
            );
        }
        response.inner
    })
    .inner
}

fn note(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.add(egui::Label::new(egui::RichText::new(text).size(12.0).color(MUTED)).wrap());
}

fn parse_friendly_error(text: &str) -> (String, Option<String>) {
    let lower = text.to_lowercase();
    if lower.contains("too many gaps in stream buffer") {
        (
            "Fallo de transporte QUIC: saturación de fragmentos de red. HeavyDrop optimizó los búferes y reintentará automáticamente.".to_string(),
            Some(text.to_string()),
        )
    } else if lower.contains("aead decryption failed") || lower.contains("integridad de bloque fallida") {
        (
            "Fallo temporal de bloque en tránsito. HeavyDrop reanudará automáticamente el bloque...".to_string(),
            Some(text.to_string()),
        )
    } else if lower.contains("connection lost") || lower.contains("aborted by peer") {
        (
            "Conexión perdida con el interlocutor. Reanudando desde el último bloque verificado...".to_string(),
            Some(text.to_string()),
        )
    } else if lower.contains("autenticación") || lower.contains("código incorrecto") || lower.contains("autenticacion") || lower.contains("codigo incorrecto") {
        (
            "Error de autenticación: la clave de sesión o invitación no coincide.".to_string(),
            Some(text.to_string()),
        )
    } else if lower.contains("cancelada por el usuario") {
        ("Transferencia cancelada.".to_string(), None)
    } else if lower.contains("ya existe") {
        (
            "El archivo ya existe en destino. Activá 'Sobrescribir' o seleccioná otra carpeta.".to_string(),
            None,
        )
    } else if lower.contains("integridad") {
        (
            "Fallo en la verificación de integridad criptográfica de los datos recibidos.".to_string(),
            Some(text.to_string()),
        )
    } else if text.len() > 80 || lower.contains("error:") || lower.contains("failed") {
        (
            "Ocurrió un error en la transferencia.".to_string(),
            Some(text.to_string()),
        )
    } else {
        (text.to_string(), None)
    }
}

fn error_message(ui: &mut egui::Ui, text: &str) {
    let (friendly, technical) = parse_friendly_error(text);
    ui.add(egui::Label::new(egui::RichText::new(&friendly).color(RED).strong()).wrap());
    if let Some(tech) = technical {
        ui.collapsing("Detalles técnicos", |ui| {
            ui.add(egui::Label::new(egui::RichText::new(tech).monospace().size(11.0).color(MUTED)).wrap());
        });
    }
}

fn primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    // Explicit colors retain a readable disabled state without egui's generic gray button skin.
    let fill = if enabled {
        ACCENT
    } else {
        egui::Color32::from_rgb(25, 49, 65)
    };
    let foreground = if enabled { CANVAS } else { MUTED };
    ui.add_enabled_ui(enabled, |ui| {
        ui.add_sized(
            [ui.available_width(), 44.0],
            egui::Button::new(egui::RichText::new(text).strong().color(foreground))
                .fill(fill)
                .stroke(egui::Stroke::NONE)
                .rounding(10.0),
        )
    })
    .inner
}

fn section_title(ui: &mut egui::Ui, title: &str, send: bool) {
    ui.horizontal(|ui| {
        egui::Frame::none()
            .fill(FIELD)
            .rounding(12.0)
            .inner_margin(12.0)
            .show(ui, |ui| {
                if send {
                    draw_upload_icon(ui, ACCENT);
                } else {
                    draw_download_icon(ui, ACCENT);
                }
            });
        ui.label(egui::RichText::new(title).size(21.0).strong());
    });
    ui.add_space(6.0);
}

impl eframe::App for HeavyDropApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.render(ctx);
    }
}

impl HeavyDropApp {
    fn render(&mut self, ctx: &egui::Context) {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
        let send = self.send_manager.progress().lock().clone();
        let recv = self.recv_manager.progress().lock().clone();
        self.send_metrics.sample(&send);
        self.recv_metrics.sample(&recv);
        self.record_history(&send, true);
        self.record_history(&recv, false);
        let send_busy = self.send_manager.is_busy();
        let recv_busy = self.recv_manager.is_busy();
        if !send_busy && terminal(send.status) {
            self.send_session = None;
        }
        if let Some(path) = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()))
        {
            if !send_busy && path.is_file() {
                self.send_file_path = Some(path);
                self.send_error = None;
            }
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(CANVAS).inner_margin(22.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().id_salt("workspace_scroll")
                    .auto_shrink([false, false]).show(ui, |ui| {
                        let width = ui.available_width().min(1120.0);
                        ui.horizontal(|ui| {
                            let margin = ((ui.available_width() - width) / 2.0).max(0.0);
                            if margin > 0.0 { ui.add_space(margin); }
                            ui.allocate_ui_with_layout(egui::vec2(width, 0.0),
                                egui::Layout::top_down(egui::Align::LEFT), |ui| {
                                self.render_top_bar(ui);
                                ui.add_space(18.0);
                                card().inner_margin(if width < 700.0 { 20.0 } else { 26.0 })
                                    .rounding(18.0).show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        if ui.available_width() >= 740.0 {
                                            let top = ui.cursor().top();
                                            let mid = ui.max_rect().center().x;
                                            ui.spacing_mut().item_spacing.x = 36.0;
                                            ui.columns(2, |columns| {
                                                for col in columns.iter_mut() { col.spacing_mut().item_spacing = egui::vec2(8.0, 9.0); }
                                                self.render_send_panel(&mut columns[0], send_busy);
                                                self.render_receive_panel(&mut columns[1], recv_busy);
                                            });
                                            ui.painter().line_segment([
                                                egui::pos2(mid, top), egui::pos2(mid, ui.min_rect().bottom())
                                            ], egui::Stroke::new(1.0_f32, BORDER));
                                        } else {
                                            self.render_send_panel(ui, send_busy);
                                            ui.add_space(20.0);
                                            ui.separator();
                                            ui.add_space(20.0);
                                            self.render_receive_panel(ui, recv_busy);
                                        }
                                    });
                                ui.add_space(16.0);
                                self.render_lower_panels(ui);
                                ui.add_space(14.0);
                                ui.horizontal_wrapped(|ui| {
                                    draw_shield_icon(ui, 14.0, GREEN);
                                    note(ui, "Cifrado de extremo a extremo · Sin almacenamiento en la nube");
                                });
                            });
                        });
                    });
            });
        if self
            .copied_toast_time
            .is_some_and(|t| t.elapsed().as_secs() < 3)
        {
            egui::Area::new(egui::Id::new("feedback"))
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -18.0])
                .order(egui::Order::Foreground)
                .interactable(false)
                .show(ctx, |ui| {
                    card().show(ui, |ui| {
                        ui.label(egui::RichText::new(&self.copied_toast_text).color(ACCENT));
                    });
                });
        }
    }

    fn render_lower_panels(&mut self, ui: &mut egui::Ui) {
        if ui.available_width() >= 920.0 {
            ui.spacing_mut().item_spacing.x = 14.0;
            ui.columns(3, |columns| {
                for col in columns.iter_mut() {
                    col.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                }
                card().show(&mut columns[0], |ui| {
                    ui.set_width(ui.available_width());
                    self.render_history_panel(ui);
                });
                card().show(&mut columns[1], |ui| {
                    ui.set_width(ui.available_width());
                    self.render_speed_panel(ui);
                });
                card().show(&mut columns[2], |ui| {
                    ui.set_width(ui.available_width());
                    self.render_settings_panel(ui);
                });
            });
        } else {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.render_history_panel(ui);
            });
            ui.add_space(10.0);
            if ui.available_width() >= 580.0 {
                ui.columns(2, |columns| {
                    card().show(&mut columns[0], |ui| {
                        ui.set_width(ui.available_width());
                        self.render_speed_panel(ui);
                    });
                    card().show(&mut columns[1], |ui| {
                        ui.set_width(ui.available_width());
                        self.render_settings_panel(ui);
                    });
                });
            } else {
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.render_speed_panel(ui);
                });
                ui.add_space(10.0);
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.render_settings_panel(ui);
                });
            }
        }
    }
}

impl HeavyDropApp {
    fn record_history(&mut self, progress: &TransferProgress, sender: bool) {
        let last = if sender {
            self.last_send_status
        } else {
            self.last_recv_status
        };
        if terminal(progress.status) && progress.status != last {
            let name = if !progress.file_name.is_empty() {
                progress.file_name.clone()
            } else if sender {
                self.send_file_path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Archivo enviado".into())
            } else {
                "Conexión de recepción".into()
            };
            // Failed/cancelled downloads may only exist as .part: open the destination folder.
            let destination = if sender {
                self.send_file_path.clone()
            } else if progress.status == TransferStatus::Completed && !progress.file_name.is_empty()
            {
                Some(self.recv_output_dir.join(&progress.file_name))
            } else {
                Some(self.recv_output_dir.clone())
            };
            self.history.insert(
                0,
                HistoryRecord {
                    file_name: name,
                    total_bytes: progress.total_bytes,
                    role: if sender { "Enviado" } else { "Recibido" }.into(),
                    status: match progress.status {
                        TransferStatus::Completed => "Completado",
                        TransferStatus::Cancelled => "Cancelado",
                        _ => "Error",
                    }
                    .into(),
                    timestamp_str: current_timestamp_str(),
                    avg_speed_mb: if sender {
                        self.send_metrics.average(progress)
                    } else {
                        self.recv_metrics.average(progress)
                    },
                    destination_path: destination,
                },
            );
            save_history(&self.history_file_path, &self.history);
        }
        if sender {
            self.last_send_status = progress.status;
        } else {
            self.last_recv_status = progress.status;
        }
    }

    fn render_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            draw_bolt_icon(ui, 30.0, ACCENT);
            ui.add_space(4.0);
            ui.label(egui::RichText::new("HeavyDrop").size(26.0).strong());
            ui.add_space(6.0);
            ui.label(egui::RichText::new("P2P 50 GB+").size(11.0).color(MUTED));
        });
    }

    fn render_send_panel(&mut self, ui: &mut egui::Ui, busy: bool) {
        section_title(ui, "Enviar archivo", true);
        note(ui, "Compartí archivos grandes de forma directa y segura.");
        ui.add_space(10.0);
        let progress = self.send_manager.progress().lock().clone();
        let dragging = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let dropzone = egui::Frame::none()
            .fill(egui::Color32::from_rgb(10, 26, 40))
            .rounding(12.0)
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.add_space(4.0);
                    if let Some(path) = self.send_file_path.clone() {
                        draw_file_type_badge(
                            ui,
                            &path.extension().unwrap_or_default().to_string_lossy(),
                        );
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(
                                    path.file_name().unwrap_or_default().to_string_lossy(),
                                )
                                .strong(),
                            )
                            .truncate(),
                        )
                        .on_hover_text(path.display().to_string());
                        note(
                            ui,
                            std::fs::metadata(&path)
                                .map(|m| indicatif::HumanBytes(m.len()).to_string())
                                .unwrap_or_else(|_| "Archivo no disponible".into()),
                        );
                        ui.add_enabled_ui(!busy, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                if text_action(ui, "Cambiar archivo").clicked() {
                                    if let Some(file) = rfd::FileDialog::new().pick_file() {
                                        self.send_file_path = Some(file);
                                        self.send_error = None;
                                    }
                                }
                                if text_action(ui, "Quitar").clicked() {
                                    self.send_file_path = None;
                                    self.send_error = None;
                                }
                            });
                        });
                    } else {
                        draw_cloud_upload_hero(ui, dragging);
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(if dragging {
                                "Soltá el archivo aquí"
                            } else {
                                "Arrastrá un archivo aquí"
                            })
                            .size(16.0)
                            .strong(),
                        );
                        note(ui, "Archivos grandes, hasta 1 TB y más.");
                        if text_action(ui, "Explorar archivos").clicked() {
                            if let Some(file) = rfd::FileDialog::new().pick_file() {
                                self.send_file_path = Some(file);
                                self.send_error = None;
                            }
                        }
                    }
                    ui.add_space(4.0);
                });
            });
        let rect = dropzone.response.rect;
        let stroke = egui::Stroke::new(
            1.0_f32,
            if dragging {
                ACCENT
            } else {
                egui::Color32::from_rgb(35, 92, 115)
            },
        );
        // Size the dropzone from its content, including the browse action.
        for (a, b) in [
            (
                egui::pos2(rect.left() + 12.0, rect.top()),
                egui::pos2(rect.right() - 12.0, rect.top()),
            ),
            (
                egui::pos2(rect.left() + 12.0, rect.bottom()),
                egui::pos2(rect.right() - 12.0, rect.bottom()),
            ),
            (
                egui::pos2(rect.left(), rect.top() + 12.0),
                egui::pos2(rect.left(), rect.bottom() - 12.0),
            ),
            (
                egui::pos2(rect.right(), rect.top() + 12.0),
                egui::pos2(rect.right(), rect.bottom() - 12.0),
            ),
        ] {
            let delta = b - a;
            let length = delta.length();
            let direction = delta.normalized();
            let mut offset = 0.0;
            while offset < length {
                ui.painter().line_segment(
                    [
                        a + direction * offset,
                        a + direction * (offset + 5.0).min(length),
                    ],
                    stroke,
                );
                offset += 10.0;
            }
        }
        ui.add_space(10.0);
        note(ui, "Código de conexión");
        let invitation = progress.pairing_code.as_ref().filter(|_| {
            busy && matches!(
                progress.status,
                TransferStatus::Preparing | TransferStatus::WaitingForPeer
            )
        });
        egui::Frame::none()
            .fill(FIELD)
            .rounding(10.0)
            .inner_margin(egui::Margin::symmetric(14.0, 9.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let code = invitation.map(|s| s.as_str());
                    let label = match code {
                        Some(value) if value.starts_with(heavydrop::pairing::INVITATION_PREFIX) => {
                            "Invitación P2P lista"
                        }
                        Some(value) => value,
                        None => "XXXX-XXXX-XXXX",
                    };
                    ui.label(
                        egui::RichText::new(label)
                            .monospace()
                            .size(14.0)
                            .color(if code.is_some() { TEXT } else { MUTED }),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled_ui(code.is_some(), |ui| {
                            if text_action(ui, "Copiar").clicked() {
                                if let Some(value) = code {
                                    ui.output_mut(|o| o.copied_text = value.into());
                                    self.show_toast("Invitación copiada");
                                }
                            }
                        });
                    });
                });
            });
        ui.add_space(4.0);
        note(
            ui,
            if busy {
                "Comparte la invitación completa con el receptor."
            } else {
                "Se genera al crear la transferencia."
            },
        );
        ui.add_space(8.0);
        if busy {
            if ui
                .add_sized(
                    [ui.available_width(), 44.0],
                    egui::Button::new(egui::RichText::new("Cancelar envío").color(RED))
                        .stroke(egui::Stroke::NONE),
                )
                .clicked()
            {
                self.cancel_send_transfer();
            }
        } else if primary_button(ui, "Crear transferencia", true).clicked() {
            if self.send_file_path.is_none() {
                self.send_file_path = rfd::FileDialog::new().pick_file();
            }
            if self.send_file_path.is_some() {
                self.start_send_task();
            }
        }
        ui.add_space(4.0);
        if let Some(session) = &self.send_session {
            note(
                ui,
                if invitation.is_some() {
                    format!("Expira en {}", session.format_remaining())
                } else {
                    "Preparando conexión segura…".into()
                },
            );
        } else {
            note(ui, "Invitación de un solo uso · Validez de 5 minutos");
        }
        if progress.status != TransferStatus::Idle {
            ui.add_space(10.0);
            self.render_transfer_dashboard(ui, &progress, true);
        }
        if let Some(error) = &self.send_error {
            error_message(ui, error);
        }
    }

    fn render_receive_panel(&mut self, ui: &mut egui::Ui, busy: bool) {
        section_title(ui, "Recibir archivo", false);
        note(ui, "Pegá la invitación completa del emisor para iniciar la descarga.");
        ui.add_space(10.0);
        note(ui, "Invitación del emisor");
        let hint = if self.legacy_tcp { "heavy1_… o código corto TCP" } else { "Pegá la invitación completa heavy1_…" };
        let edit = text_field(
            ui,
            &mut self.recv_code_input,
            hint,
            !busy,
            true,
        );
        if edit.changed() {
            self.recv_error = None;
        }
        let parsed = PairingCode::parse(&self.recv_code_input).ok();
        let blocked_legacy = parsed.as_ref().is_some_and(|code| code.invitation().is_none() && !self.legacy_tcp);
        let valid = parsed.is_some() && !blocked_legacy;
        ui.add_space(8.0);
        if busy {
            if ui
                .add_sized(
                    [ui.available_width(), 44.0],
                    egui::Button::new(egui::RichText::new("Cancelar recepción").color(RED))
                        .stroke(egui::Stroke::NONE),
                )
                .clicked()
            {
                self.cancel_recv_transfer();
            }
        } else if primary_button(ui, "Conectar", valid).clicked()
            || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && valid)
        {
            self.start_receive_task();
        }
        if !busy && !self.recv_code_input.trim().is_empty() && !valid {
            note(ui, if blocked_legacy { "Este es un código TCP antiguo. Pedí una invitación completa o habilitá compatibilidad en Ajustes." } else { "Revisa la invitación y pegala completa." });
        }
        ui.add_space(12.0);
        let progress = self.recv_manager.progress().lock().clone();
        if progress.status != TransferStatus::Idle {
            self.render_transfer_dashboard(ui, &progress, false);
        } else {
            egui::Frame::none()
                .fill(FIELD)
                .rounding(12.0)
                .inner_margin(16.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (dot, _) =
                            ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 3.0, ACCENT);
                        ui.label(egui::RichText::new("Listo para recibir").strong());
                    });
                    note(ui, "Pegá la invitación completa para conectar de forma segura.");
                });
        }
        ui.add_space(12.0);
        egui::Frame::none()
            .fill(FIELD)
            .rounding(12.0)
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    draw_folder_icon(ui, 20.0, ACCENT);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let change = ui
                            .add_enabled_ui(!busy, |ui| text_action(ui, "Cambiar"))
                            .inner
                            .clicked();
                        if change {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.recv_output_dir = dir;
                                self.cached_free_space = None;
                            }
                        }
                        let width = ui.available_width();
                        ui.allocate_ui_with_layout(
                            egui::vec2(width, 0.0),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(
                                            self.recv_output_dir
                                                .file_name()
                                                .unwrap_or_else(|| self.recv_output_dir.as_os_str())
                                                .to_string_lossy(),
                                        )
                                        .strong(),
                                    )
                                    .truncate(),
                                );
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(
                                            self.recv_output_dir.display().to_string(),
                                        )
                                        .size(11.0)
                                        .color(MUTED),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(self.recv_output_dir.display().to_string());
                            },
                        );
                    });
                });
            });
        if let Some(bytes) = self.get_free_disk_space() {
            note(
                ui,
                format!("{} disponibles en disco", indicatif::HumanBytes(bytes)),
            );
        }
        ui.add_space(12.0);
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            draw_shield_icon(ui, 14.0, GREEN);
            note(ui, "Cifrado de extremo a extremo");
        });
        if let Some(error) = &self.recv_error {
            error_message(ui, error);
        }
    }

    fn render_transfer_dashboard(
        &mut self,
        ui: &mut egui::Ui,
        progress: &TransferProgress,
        sender: bool,
    ) {
        let completed = progress.status == TransferStatus::Completed;
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let color = match progress.status {
                TransferStatus::Completed => GREEN,
                TransferStatus::Failed => RED,
                _ => ACCENT,
            };
            let title = match progress.status {
                TransferStatus::Idle => "Listo",
                TransferStatus::Preparing => "Preparando transferencia",
                TransferStatus::WaitingForPeer => "Esperando al receptor",
                TransferStatus::Connecting => "Buscando al emisor",
                TransferStatus::Connected => "Conexión establecida",
                TransferStatus::Transferring => "Transferencia en curso",
                TransferStatus::Completed => "Transferencia completada",
                TransferStatus::Cancelled => "Transferencia cancelada",
                TransferStatus::Failed => "No se pudo transferir",
            };
            ui.add(egui::Label::new(egui::RichText::new(title).strong().color(color)).wrap());
            if !progress.file_name.is_empty() {
                ui.add(egui::Label::new(&progress.file_name).truncate())
                    .on_hover_text(&progress.file_name);
            }
            if progress.connection_type != ConnectionType::None {
                note(ui, progress.connection_type.label());
            }
            if progress.status == TransferStatus::Transferring || completed {
                let fraction = if completed {
                    1.0
                } else if progress.total_bytes > 0 {
                    (progress.transferred_bytes as f64 / progress.total_bytes as f64)
                        .clamp(0.0, 1.0) as f32
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .show_percentage()
                        .fill(color),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!(
                        "{:.1} MB/s",
                        progress.bytes_per_sec / (1024.0 * 1024.0)
                    ));
                    note(
                        ui,
                        format!(
                            "{} / {}",
                            indicatif::HumanBytes(progress.transferred_bytes),
                            indicatif::HumanBytes(progress.total_bytes)
                        ),
                    );
                    if !completed && progress.bytes_per_sec > 0.0 {
                        let seconds = (progress
                            .total_bytes
                            .saturating_sub(progress.transferred_bytes)
                            as f64
                            / progress.bytes_per_sec) as u64;
                        note(ui, format!("Restante: {}m {}s", seconds / 60, seconds % 60));
                    }
                });
                if !completed {
                    let metrics = if sender {
                        &self.send_metrics
                    } else {
                        &self.recv_metrics
                    };
                    draw_speed_sparkline(ui, &metrics.samples, metrics.peak);
                }
            }
            note(ui, &progress.status_text);
            if let Some(error) = &progress.error {
                error_message(ui, error);
            }
            if completed && !sender && ui.button("Mostrar archivo recibido").clicked() {
                open_in_file_manager(&self.recv_output_dir.join(&progress.file_name));
            }
        });
    }

    fn render_history_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Historial").size(17.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.history.is_empty()
                    && text_action(
                        ui,
                        if self.history_expanded {
                            "Ver menos"
                        } else {
                            "Ver todo"
                        },
                    )
                    .clicked()
                {
                    self.history_expanded = !self.history_expanded;
                }
            });
        });
        if self.history.is_empty() {
            ui.add_space(20.0);
            note(ui, "Todavía no hay transferencias");
            note(ui, "Tus envíos y descargas aparecerán aquí.");
            ui.add_space(20.0);
            return;
        }
        let count = if self.history_expanded {
            self.history.len()
        } else {
            self.history.len().min(3)
        };
        egui::ScrollArea::vertical()
            .id_salt("history_list")
            .max_height(if self.history_expanded { 320.0 } else { 230.0 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (index, item) in self.history.iter().take(count).enumerate() {
                    if index > 0 {
                        ui.separator();
                    }
                    ui.add(
                        egui::Label::new(egui::RichText::new(&item.file_name).strong()).truncate(),
                    )
                    .on_hover_text(&item.file_name);
                    ui.horizontal_wrapped(|ui| {
                        let color = match item.status.as_str() {
                            "Completado" => GREEN,
                            "Cancelado" => MUTED,
                            _ => RED,
                        };
                        ui.label(egui::RichText::new(&item.status).size(11.0).color(color));
                        note(
                            ui,
                            format!(
                                "{} · {}",
                                indicatif::HumanBytes(item.total_bytes),
                                item.role
                            ),
                        );
                    });
                    note(
                        ui,
                        format!("{} UTC · {:.1} MB/s", item.timestamp_str, item.avg_speed_mb),
                    );
                    if let Some(path) = &item.destination_path {
                        ui.add_enabled_ui(path.exists(), |ui| {
                            if text_action(ui, "Mostrar archivo").clicked() {
                                open_in_file_manager(path);
                            }
                        });
                    }
                    ui.add_space(4.0);
                }
            });
        if self.history_expanded {
            if text_action(ui, "Limpiar historial").clicked() {
                self.clear_history_confirm = true;
            }
            if self.clear_history_confirm {
                note(ui, "¿Borrar los registros? Los archivos se conservan.");
                ui.horizontal_wrapped(|ui| {
                    if text_action(ui, "Borrar registros").clicked() {
                        self.history.clear();
                        save_history(&self.history_file_path, &self.history);
                        self.clear_history_confirm = false;
                    }
                    if text_action(ui, "Conservar").clicked() {
                        self.clear_history_confirm = false;
                    }
                });
            }
        }
    }

    fn render_settings_panel(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Ajustes").size(17.0).strong());
        ui.add_space(8.0);
        note(ui, if self.legacy_tcp { "Compatibilidad TCP activada" } else if self.direct_only { "QUIC · Solo conexión directa" } else { "QUIC automático · Recomendado" });
        let busy = self.send_manager.is_busy() || self.recv_manager.is_busy();
        ui.add_enabled_ui(!busy, |ui| {
            if ui.checkbox(&mut self.direct_only, "Solo P2P directo (sin relay)").changed() && self.direct_only {
                self.legacy_tcp = false;
            }
            ui.add_enabled_ui(!self.direct_only, |ui| {
                ui.checkbox(&mut self.legacy_tcp, "Compatibilidad TCP (metadatos visibles)");
            });
            ui.checkbox(&mut self.verify_full_file, "Verificar archivo completo y reanudar");
        });
        note(ui, if self.verify_full_file { "El código se genera después del hash previo." } else { "Modo rápido: verifica bloques y reinicia descargas parciales." });
        if self.legacy_tcp {
            note(ui, "Servidor de compatibilidad TCP");
            text_field(
                ui,
                &mut self.signal_server_input,
                "Opcional · servidor:7880",
                !busy,
                false,
            );
            if busy {
                note(ui, "Disponible al terminar la transferencia.");
            } else if text_action(ui, "Restablecer").clicked() {
                self.signal_server_input =
                    heavydrop::signaling::SignalingClient::default_server().unwrap_or_default();
            }
        }
        ui.collapsing("Acerca de la conexión", |ui| {
            note(ui, "Las invitaciones heavy1_ priorizan P2P directo y atraviesan NAT cuando es posible. El relay es un respaldo cifrado. Solo P2P directo no usa relays, pero puede fallar con NAT restrictivo. El servidor manual se usa únicamente con códigos cortos TCP.");
            note(ui, "Se aplica a esta ejecución; no se guarda al cerrar.");
        });
        ui.add_space(8.0);
        ui.separator();
        ui.label(
            egui::RichText::new("Cifrado siempre activo")
                .size(11.0)
                .color(GREEN),
        );
        note(ui, "ChaCha20-Poly1305 · BLAKE3");
    }

    fn render_speed_panel(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Velocidad").size(17.0).strong());
        ui.add_space(8.0);
        speed_control(
            ui,
            "Subida",
            &mut self.upload_limit_mb,
            &self.upload_limiter,
        );
        ui.add_space(10.0);
        speed_control(
            ui,
            "Bajada",
            &mut self.download_limit_mb,
            &self.download_limiter,
        );
        ui.add_space(8.0);
        note(ui, "0 = sin límite · Cambios en tiempo real");

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("⚡ Test de Velocidad de Internet").size(15.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                note(ui, "Cloudflare Anycast");
            });
        });
        ui.add_space(6.0);

        let phase = self.speedtest_state.lock().clone();
        match phase {
            heavydrop::speedtest::SpeedTestPhase::Idle => {
                note(
                    ui,
                    "Medí la velocidad real de tu conexión para auto-calibrar tus límites al 85%.",
                );
                ui.add_space(8.0);
                if ui
                    .button(
                        egui::RichText::new("⚡ Iniciar Test de Velocidad")
                            .color(ACCENT)
                            .size(13.0)
                            .strong(),
                    )
                    .clicked()
                {
                    self.start_speedtest();
                }
            }
            heavydrop::speedtest::SpeedTestPhase::MeasuringPing => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new("Midiendo latencia (ping) a nodos Anycast...")
                            .color(ACCENT)
                            .size(13.0),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("Cancelar").color(RED)).clicked() {
                            self.cancel_speedtest();
                        }
                    });
                });
            }
            heavydrop::speedtest::SpeedTestPhase::MeasuringDownload { progress, current_mbps } => {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("📥 Medición de bajada:")
                            .size(13.0)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new(format!("{:.1} Mbps", current_mbps))
                            .color(GREEN)
                            .size(13.0)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("Cancelar").color(RED)).clicked() {
                            self.cancel_speedtest();
                        }
                    });
                });
                ui.add_space(4.0);
                ui.add(egui::ProgressBar::new(progress).show_percentage());
            }
            heavydrop::speedtest::SpeedTestPhase::MeasuringUpload { progress, current_mbps } => {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("📤 Medición de subida:")
                            .size(13.0)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new(format!("{:.1} Mbps", current_mbps))
                            .color(GREEN)
                            .size(13.0)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("Cancelar").color(RED)).clicked() {
                            self.cancel_speedtest();
                        }
                    });
                });
                ui.add_space(4.0);
                ui.add(egui::ProgressBar::new(progress).show_percentage());
            }
            heavydrop::speedtest::SpeedTestPhase::Completed(ref res) => {
                let rec_up = res.recommended_upload_limit_mb();
                let rec_down = res.recommended_download_limit_mb();

                egui::Grid::new("speedtest_results_grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Latencia (Ping):").size(12.0));
                        ui.label(
                            egui::RichText::new(format!("{:.1} ms", res.ping_ms))
                                .color(GREEN)
                                .strong(),
                        );
                        ui.end_row();

                        ui.label(egui::RichText::new("Bajada (Download):").size(12.0));
                        ui.label(
                            egui::RichText::new(format!("{:.1} Mbps ({:.2} MB/s)", res.download_mbps, res.download_mbs))
                                .color(ACCENT)
                                .strong(),
                        );
                        ui.end_row();

                        ui.label(egui::RichText::new("Subida (Upload):").size(12.0));
                        ui.label(
                            egui::RichText::new(format!("{:.1} Mbps ({:.2} MB/s)", res.upload_mbps, res.upload_mbs))
                                .color(ACCENT)
                                .strong(),
                        );
                        ui.end_row();
                    });

                ui.add_space(10.0);

                let apply_label = format!(
                    "🎯 Aplicar límites al 85% (Subida: {} MB/s | Bajada: {} MB/s)",
                    rec_up, rec_down
                );
                if ui
                    .button(
                        egui::RichText::new(apply_label)
                            .color(GREEN)
                            .size(13.0)
                            .strong(),
                    )
                    .clicked()
                {
                    self.upload_limit_mb = rec_up;
                    self.download_limit_mb = rec_down;
                    self.upload_limiter.set_limit_mbps(rec_up as u64);
                    self.download_limiter.set_limit_mbps(rec_down as u64);
                }

                ui.add_space(4.0);
                if ui
                    .button(
                        egui::RichText::new("⚡ Repetir prueba")
                            .color(MUTED)
                            .size(12.0),
                    )
                    .clicked()
                {
                    self.start_speedtest();
                }
            }
            heavydrop::speedtest::SpeedTestPhase::Failed(ref err) => {
                ui.label(
                    egui::RichText::new(format!("Error en el test: {}", err))
                        .color(RED)
                        .size(12.0),
                );
                ui.add_space(6.0);
                if ui
                    .button(
                        egui::RichText::new("⚡ Reintentar prueba")
                            .color(ACCENT)
                            .size(12.0),
                    )
                    .clicked()
                {
                    self.start_speedtest();
                }
            }
        }
    }

    fn start_speedtest(&mut self) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.speedtest_cancel = Some(cancel.clone());
        let state = self.speedtest_state.clone();
        self.tokio_rt.spawn(async move {
            heavydrop::speedtest::run_speed_test(state, cancel).await;
        });
    }

    fn cancel_speedtest(&mut self) {
        if let Some(ref cancel) = self.speedtest_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        let mut state = self.speedtest_state.lock();
        *state = heavydrop::speedtest::SpeedTestPhase::Idle;
    }
}

fn speed_control(
    ui: &mut egui::Ui,
    title: &str,
    limit: &mut u32,
    limiter: &heavydrop::speed_limiter::SpeedLimiter,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).size(12.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            note(
                ui,
                if *limit == 0 {
                    "Sin límite".into()
                } else {
                    format!("{} MB/s", limit)
                },
            );
        });
    });
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = ui.available_width();
        ui.visuals_mut().widgets.inactive.bg_fill = BORDER;
        ui.visuals_mut().widgets.inactive.fg_stroke.color = ACCENT;
        ui.visuals_mut().selection.bg_fill = ACCENT;
        if ui
            .add(
                egui::Slider::new(limit, 0..=500)
                    .logarithmic(true)
                    .trailing_fill(true)
                    .show_value(false),
            )
            .changed()
        {
            limiter.set_limit_mbps(*limit as u64);
        }
    });
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
    let path_str: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
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
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
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
        let leap = if (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0) {
            1
        } else {
            0
        };
        let days_in_year = 365 + leap;
        if d >= days_in_year {
            d -= days_in_year;
            y += 1;
        } else {
            break;
        }
    }
    let leap = if (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0) {
        1
    } else {
        0
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app(ctx: &egui::Context, name: &str) -> HeavyDropApp {
        let rt = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        HeavyDropApp::with_context(
            ctx,
            rt,
            std::env::temp_dir().join(format!("heavydrop_gui_{name}_{}.json", std::process::id())),
        )
    }

    #[test]
    fn default_gui_rejects_short_codes_without_starting_a_transfer() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx, "quic_policy");
        app.recv_code_input = "KD7M-9Q2P-W8RX".into();
        app.start_receive_task();
        assert!(app.recv_error.as_ref().is_some_and(|error| error.contains("QUIC")));
        assert!(!app.recv_manager.is_busy());
        assert_eq!(app.recv_manager.progress().lock().status, TransferStatus::Idle);
    }

    #[test]
    fn panels_fit_narrow_columns_with_long_names_and_errors() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx, "layout");
        app.recv_output_dir =
            PathBuf::from(format!("C:/descargas/{}/", "carpeta_larga_".repeat(20)));
        app.send_file_path = Some(PathBuf::from(format!(
            "{}.日本語",
            "archivo_largo_".repeat(20)
        )));
        app.send_error = Some("No se pudo abrir el archivo. ".repeat(20));
        let progress = TransferProgress {
            status: TransferStatus::Transferring,
            file_name: "archivo_largo_".repeat(30),
            total_bytes: 1_000_000,
            transferred_bytes: 500_000,
            bytes_per_sec: 400_000.0,
            error: Some("Error de conexión detallado. ".repeat(20)),
            ..Default::default()
        };
        app.history.push(HistoryRecord {
            file_name: progress.file_name.clone(),
            total_bytes: progress.total_bytes,
            role: "Enviado".into(),
            status: "Completado".into(),
            timestamp_str: "2026-10-31 18:30".into(),
            avg_speed_mb: 100.0,
            destination_path: None,
        });
        for width in [280.0, 340.0, 420.0, 680.0, 1040.0] {
            for panel in 0..6 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 720.0),
                    )),
                    ..Default::default()
                };
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let right = ui.max_rect().right();
                        match panel {
                            0 => app.render_send_panel(ui, false),
                            1 => app.render_receive_panel(ui, false),
                            2 => app.render_transfer_dashboard(ui, &progress, true),
                            3 => app.render_history_panel(ui),
                            4 => app.render_speed_panel(ui),
                            _ => app.render_settings_panel(ui),
                        }
                        assert!(
                            ui.min_rect().right() <= right + 1.0,
                            "panel {panel} overflowed at width {width}: {:?} > {right}",
                            ui.min_rect()
                        );
                    });
                });
            }
        }
    }

    #[test]
    fn history_records_terminal_transitions_once_even_when_frames_skip_states() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx, "history");
        app.history.clear();
        for previous in [
            TransferStatus::Preparing,
            TransferStatus::WaitingForPeer,
            TransferStatus::Connected,
        ] {
            app.last_send_status = previous;
            let progress = TransferProgress {
                status: TransferStatus::Cancelled,
                ..Default::default()
            };
            let before = app.history.len();
            app.record_history(&progress, true);
            app.record_history(&progress, true);
            assert_eq!(app.history.len(), before + 1);
        }
        app.last_recv_status = TransferStatus::Preparing;
        app.record_history(
            &TransferProgress {
                role: TransferRole::Receiver,
                status: TransferStatus::Failed,
                ..Default::default()
            },
            false,
        );
        assert_eq!(
            app.history[0].destination_path,
            Some(app.recv_output_dir.clone())
        );
        std::fs::remove_file(&app.history_file_path).unwrap();
    }

    #[test]
    fn metrics_keep_directions_separate_and_exclude_resumed_bytes() {
        let mut send = TransferMetrics::default();
        let recv = TransferMetrics::default();
        send.started = Some(Instant::now() - std::time::Duration::from_secs(1));
        send.last_sample = Instant::now() - std::time::Duration::from_secs(1);
        let progress = TransferProgress {
            status: TransferStatus::Transferring,
            transferred_bytes: 101 * 1024 * 1024,
            resume_offset: 100 * 1024 * 1024,
            bytes_per_sec: 5.0 * 1024.0 * 1024.0,
            ..Default::default()
        };
        send.sample(&progress);
        assert_eq!(send.peak, 5.0);
        assert!(recv.samples.is_empty());
        assert!(send.average(&progress) <= 1.0);
        assert_eq!(recv.average(&progress), 0.0);
    }

    #[test]
    fn workspace_switches_between_columns_and_stacked_panels() {
        for width in [680.0, 1040.0, 1160.0] {
            let ctx = egui::Context::default();
            let mut app = test_app(&ctx, "workspace");
            for _ in 0..3 {
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 720.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        app.render(ctx);
                        assert!(
                            ctx.used_size().x <= width + 1.0,
                            "width {width}, used {:?}",
                            ctx.used_size()
                        );
                    },
                );
                let text_position = |label: &str| {
                    output
                        .shapes
                        .iter()
                        .find_map(|shape| {
                            if let egui::Shape::Text(text) = &shape.shape {
                                (text.galley.text() == label).then_some(text.pos)
                            } else {
                                None
                            }
                        })
                        .unwrap_or_else(|| panic!("missing {label}"))
                };
                let send = text_position("Enviar archivo");
                let recv = text_position("Recibir archivo");
                if width >= 1040.0 {
                    for title in ["Historial", "Velocidad", "Ajustes"] {
                        let occurrences = output.shapes.iter().filter(|shape| {
                            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == title)
                        }).count();
                        assert_eq!(
                            occurrences, 1,
                            "{title} must appear once, in the integrated lower row"
                        );
                        assert!(text_position(title).y > send.y + 300.0);
                    }
                }
                if width >= 1040.0 {
                    assert!((send.y - recv.y).abs() < 1.0);
                    assert!(recv.x > width / 2.0);
                } else {
                    assert!((send.x - recv.x).abs() < 1.0);
                    assert!(recv.y > send.y + 300.0);
                }
            }
        }
    }
}

