use crate::audio::AudioEngine;
use crate::config::AppConfig;
use crate::detect::{run_hardware_scan, CompatibilityResult, HardwareScanReport};
use crate::rtmp::StreamSession;
use crate::win_enum::{get_available_windows, resolve_target_window, WindowItem};
use eframe::egui::{self, Color32, CornerRadius, FontId, Margin, RichText, Stroke};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

// ============================================================================
// UI-UX-PRO-MAX DESIGN TOKENS (OLED Dark Broadcast Studio Palette)
// ============================================================================
const BG_DEEP_OLED: Color32 = Color32::from_rgb(12, 14, 24); // #0C0E18
const BG_CARD: Color32 = Color32::from_rgb(22, 25, 43); // #16192B
const BG_CARD_ELEVATED: Color32 = Color32::from_rgb(31, 35, 59); // #1F233B
const BG_INPUT: Color32 = Color32::from_rgb(16, 18, 32); // #101220
const BORDER_SUBTLE: Color32 = Color32::from_rgb(45, 51, 84); // #2D3354
const BORDER_ACCENT: Color32 = Color32::from_rgb(99, 102, 241); // #6366F1

const TEXT_PRIMARY: Color32 = Color32::from_rgb(248, 250, 252); // #F8FAFC
const TEXT_SECONDARY: Color32 = Color32::from_rgb(148, 163, 184); // #94A3B8
const TEXT_MUTED: Color32 = Color32::from_rgb(100, 116, 139); // #64748B

const ACCENT_INDIGO: Color32 = Color32::from_rgb(79, 70, 229); // #4F46E5
const ACCENT_INDIGO_BRIGHT: Color32 = Color32::from_rgb(129, 140, 248); // #818CF8
const ACCENT_EMERALD: Color32 = Color32::from_rgb(34, 197, 94); // #22C55E
const ACCENT_EMERALD_DARK: Color32 = Color32::from_rgb(21, 128, 61); // #15803D
const ACCENT_AMBER: Color32 = Color32::from_rgb(245, 158, 11); // #F59E0B
const ACCENT_RED: Color32 = Color32::from_rgb(239, 68, 68); // #EF4444
const ACCENT_RED_DARK: Color32 = Color32::from_rgb(185, 28, 28); // #B91C1C

#[derive(Clone, Default)]
pub struct LiveTelemetry {
    pub is_streaming: Arc<AtomicBool>,
    pub current_bitrate_kbps: Arc<AtomicU32>,
    pub fps: Arc<AtomicU32>,
    pub dropped_frames: Arc<AtomicU64>,
    pub total_frames: Arc<AtomicU64>,
}

pub struct ObsLiteApp {
    pub config: AppConfig,
    pub applied_config: AppConfig,
    pub config_applied: bool,
    pub scan_report: HardwareScanReport,
    pub telemetry: LiveTelemetry,
    pub audio_engine: Option<AudioEngine>,
    pub show_stream_key: bool,
    pub stream_start_time: Option<Instant>,
    pub status_message: String,
    pub stop_signal: Arc<AtomicBool>,
    pub rt: Arc<Runtime>,
    pub initial_frames: u32,
    pub status_tx: mpsc::UnboundedSender<String>,
    pub status_rx: mpsc::UnboundedReceiver<String>,
    pub is_testing_sync: Arc<AtomicBool>,
    pub available_windows: Vec<WindowItem>,
    pub bitrate_history: VecDeque<u32>,
    pub last_history_sample: Instant,
}

impl ObsLiteApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_pro_max_theme(&cc.egui_ctx);

        tracing::info!("Step 1: Loading config...");
        let mut config = AppConfig::load();

        tracing::info!("Step 2: Starting hardware scan...");
        let scan_report = run_hardware_scan();

        // Sync initial "Auto" profile with detected hardware profile
        if let CompatibilityResult::Compatible { profile, .. } = &scan_report.compatibility {
            config.apply_quality_preset(
                profile.width,
                profile.height,
                profile.fps,
                profile.video_bitrate_kbps,
            );
        }

        tracing::info!("Step 3: Creating audio engine...");
        let audio_engine = AudioEngine::new().ok();
        if let Some(engine) = &audio_engine {
            engine
                .controls
                .desktop_volume
                .store(config.desktop_volume, Ordering::Relaxed);
            engine
                .controls
                .desktop_muted
                .store(config.desktop_muted, Ordering::Relaxed);
            engine
                .controls
                .mic_volume
                .store(config.mic_volume, Ordering::Relaxed);
            engine
                .controls
                .mic_muted
                .store(config.mic_muted, Ordering::Relaxed);
        }

        tracing::info!("Step 4: Building Tokio runtime...");
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed to initialize Tokio runtime"),
        );

        let (status_tx, status_rx) = mpsc::unbounded_channel();
        let available_windows = get_available_windows();

        // Resolve target window HWND if saved in Window mode
        if config.capture_mode == "Window" && !config.target_window_title.is_empty() {
            if let Some(win) =
                resolve_target_window(config.target_window_hwnd, &config.target_window_title)
            {
                config.target_window_hwnd = win.hwnd;
                config.target_window_title = win.title;
            }
        }

        let applied_config = config.clone();

        if let Some(window) = cc.winit_window() {
            window.set_visible(true);
            window.focus_window();
        }

        Self {
            config,
            applied_config,
            config_applied: false,
            scan_report,
            telemetry: LiveTelemetry::default(),
            audio_engine,
            show_stream_key: false,
            stream_start_time: None,
            status_message: "Configure capture source & destination, then click Apply.".to_string(),
            stop_signal: Arc::new(AtomicBool::new(false)),
            rt,
            initial_frames: 0,
            status_tx,
            status_rx,
            is_testing_sync: Arc::new(AtomicBool::new(false)),
            available_windows,
            bitrate_history: VecDeque::from(vec![0; 40]),
            last_history_sample: Instant::now(),
        }
    }

    fn hw_defaults(&self) -> (u32, u32, u32, u32) {
        match &self.scan_report.compatibility {
            CompatibilityResult::Compatible { profile, .. } => (
                profile.width,
                profile.height,
                profile.fps,
                profile.video_bitrate_kbps,
            ),
            _ => (1280, 720, 30, 2500),
        }
    }

    /// Validates, resolves window handles, saves, and locks the current configuration.
    fn apply_and_lock_settings(&mut self) -> bool {
        let (hw_w, hw_h, hw_fps, hw_br) = self.hw_defaults();
        self.config
            .apply_quality_preset(hw_w, hw_h, hw_fps, hw_br);
        self.config.sanitize();

        if self.config.capture_mode == "Window" {
            if self.config.target_window_title.trim().is_empty() {
                self.status_message =
                    "✖ Please select a target application window before clicking Apply.".to_string();
                return false;
            }
            self.available_windows = get_available_windows();
            if let Some(win) = resolve_target_window(
                self.config.target_window_hwnd,
                &self.config.target_window_title,
            ) {
                self.config.target_window_hwnd = win.hwnd;
                self.config.target_window_title = win.title;
            } else {
                self.status_message = format!(
                    "✖ Window '{}' was not found. Make sure it is open, click Refresh, and re-select it.",
                    self.config.target_window_title
                );
                return false;
            }
        } else {
            self.config.target_window_hwnd = 0;
        }

        if self.config.server_url.trim().is_empty() {
            self.status_message = "✖ Server Ingest URL cannot be empty.".to_string();
            return false;
        }

        let _ = self.config.save();
        self.applied_config = self.config.clone();
        self.config_applied = true;

        let source_desc = if self.applied_config.capture_mode == "Window" {
            format!(
                "Window [{}] (HWND 0x{:X})",
                self.applied_config.target_window_title, self.applied_config.target_window_hwnd
            )
        } else {
            "Whole Desktop (DXGI Zero-Copy)".to_string()
        };

        self.status_message = format!(
            "✔ Settings Applied & Locked! Source: {} | Output: {}x{} @ {}fps ({} kbps)",
            source_desc,
            self.applied_config.width,
            self.applied_config.height,
            self.applied_config.fps,
            self.applied_config.bitrate_kbps
        );
        true
    }
}

fn apply_pro_max_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        let mut visuals = egui::Visuals::dark();

        visuals.panel_fill = BG_DEEP_OLED;
        visuals.window_fill = BG_CARD;
        visuals.extreme_bg_color = BG_INPUT;
        visuals.faint_bg_color = BG_CARD_ELEVATED;
        visuals.override_text_color = Some(TEXT_PRIMARY);

        let radius = CornerRadius::same(8);
        visuals.widgets.noninteractive.bg_fill = BG_CARD;
        visuals.widgets.noninteractive.weak_bg_fill = BG_CARD;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
        visuals.widgets.noninteractive.corner_radius = radius;

        visuals.widgets.inactive.bg_fill = BG_CARD_ELEVATED;
        visuals.widgets.inactive.weak_bg_fill = BG_CARD_ELEVATED;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
        visuals.widgets.inactive.corner_radius = radius;

        visuals.widgets.hovered.bg_fill = Color32::from_rgb(48, 54, 92);
        visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(48, 54, 92);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.5, ACCENT_INDIGO_BRIGHT);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        visuals.widgets.hovered.corner_radius = radius;

        visuals.widgets.active.bg_fill = ACCENT_INDIGO;
        visuals.widgets.active.weak_bg_fill = ACCENT_INDIGO;
        visuals.widgets.active.bg_stroke = Stroke::new(1.5, Color32::WHITE);
        visuals.widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        visuals.widgets.active.corner_radius = radius;

        visuals.selection.bg_fill = ACCENT_INDIGO;
        visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);

        style.visuals = visuals;
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
    });
}

fn studio_card<R>(
    ui: &mut egui::Ui,
    border_color: Color32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::Frame::new()
        .fill(BG_CARD)
        .stroke(Stroke::new(1.0, border_color))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, add_contents)
        .inner
}

fn draw_badge(ui: &mut egui::Ui, text: &str, bg: Color32, fg: Color32) {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(fg).font(FontId::proportional(11.5)).strong());
        });
}

impl eframe::App for ObsLiteApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.initial_frames == 0 {
            tracing::info!("ObsLite v0.5 UI first frame presented!");
        }
        if self.initial_frames < 3 {
            self.initial_frames += 1;
            ui.ctx().request_repaint();
        }

        while let Ok(msg) = self.status_rx.try_recv() {
            self.status_message = msg;
        }

        let streaming = self.telemetry.is_streaming.load(Ordering::Relaxed);
        let testing = self.is_testing_sync.load(Ordering::Relaxed);

        if streaming || testing {
            ui.ctx()
                .request_repaint_after(Duration::from_millis(80));
        } else {
            ui.ctx()
                .request_repaint_after(Duration::from_millis(120));
        }

        // Sample bitrate history once per second
        if self.last_history_sample.elapsed() >= Duration::from_secs(1) {
            self.last_history_sample = Instant::now();
            let br = if streaming {
                self.telemetry.current_bitrate_kbps.load(Ordering::Relaxed)
            } else {
                0
            };
            if self.bitrate_history.len() >= 40 {
                self.bitrate_history.pop_front();
            }
            self.bitrate_history.push_back(br);
        }

        // Settings are locked whenever streaming is active, OR when user has clicked Apply
        let controls_editable = !streaming && !self.config_applied;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG_DEEP_OLED)
                    .inner_margin(Margin::symmetric(18, 14)),
            )
            .show(ui, |ui| {
                // ============================================================
                // TOP STUDIO HEADER BAR
                // ============================================================
                studio_card(
                    ui,
                    if streaming {
                        ACCENT_EMERALD
                    } else {
                        BORDER_SUBTLE
                    },
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("OBSLITE")
                                    .font(FontId::proportional(21.0))
                                    .color(TEXT_PRIMARY)
                                    .strong(),
                            );
                            draw_badge(
                                ui,
                                "v0.5 STABLE",
                                ACCENT_INDIGO,
                                Color32::WHITE,
                            );

                            if let CompatibilityResult::Compatible { profile, .. } =
                                &self.scan_report.compatibility
                            {
                                draw_badge(
                                    ui,
                                    &format!(
                                        "{} • SCORE {}/100",
                                        profile.tier_badge, profile.hardware_score
                                    ),
                                    BG_CARD_ELEVATED,
                                    ACCENT_INDIGO_BRIGHT,
                                );
                            }

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if streaming {
                                        let secs = self
                                            .stream_start_time
                                            .map(|t| t.elapsed().as_secs())
                                            .unwrap_or(0);
                                        let timer = format!(
                                            "● ON AIR  {:02}:{:02}:{:02}",
                                            secs / 3600,
                                            (secs % 3600) / 60,
                                            secs % 60
                                        );
                                        draw_badge(
                                            ui,
                                            &timer,
                                            ACCENT_RED,
                                            Color32::WHITE,
                                        );
                                        draw_badge(
                                            ui,
                                            "LOCKED FOR BROADCAST",
                                            BG_CARD_ELEVATED,
                                            ACCENT_AMBER,
                                        );
                                    } else if self.config_applied {
                                        draw_badge(
                                            ui,
                                            "✔ CONFIG APPLIED & LOCKED",
                                            ACCENT_EMERALD_DARK,
                                            Color32::WHITE,
                                        );
                                    } else {
                                        draw_badge(
                                            ui,
                                            "STANDBY • UNAPPLIED",
                                            BG_CARD_ELEVATED,
                                            TEXT_SECONDARY,
                                        );
                                    }
                                },
                            );
                        });
                    },
                );

                ui.add_space(6.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // ============================================================
                        // SECTION 1: HARDWARE PRE-FLIGHT & STREAM PROFILE
                        // ============================================================
                        studio_card(ui, BORDER_SUBTLE, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("HARDWARE PRE-FLIGHT & ENCODING PROFILE")
                                        .font(FontId::proportional(13.0))
                                        .color(ACCENT_INDIGO_BRIGHT)
                                        .strong(),
                                );

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add_enabled(
                                                !streaming,
                                                egui::Button::new(
                                                    RichText::new("Re-Probe Device").small(),
                                                ),
                                            )
                                            .clicked()
                                        {
                                            self.scan_report = run_hardware_scan();
                                            let (w, h, fps, br) = self.hw_defaults();
                                            self.config.apply_quality_preset(w, h, fps, br);
                                        }
                                    },
                                );
                            });

                            ui.add_space(4.0);

                            // Hardware specs grid
                            ui.columns(3, |cols| {
                                egui::Frame::new()
                                    .fill(BG_INPUT)
                                    .corner_radius(CornerRadius::same(6))
                                    .inner_margin(Margin::same(8))
                                    .show(&mut cols[0], |ui| {
                                        ui.label(
                                            RichText::new("PROCESSOR & MEMORY")
                                                .color(TEXT_MUTED)
                                                .small()
                                                .strong(),
                                        );
                                        ui.label(
                                            RichText::new(&self.scan_report.cpu.brand)
                                                .color(TEXT_PRIMARY)
                                                .small(),
                                        );
                                        ui.label(
                                            RichText::new(format!(
                                                "{} Physical / {} Threads • {:.1} GB RAM",
                                                self.scan_report.cpu.physical_cores,
                                                self.scan_report.cpu.logical_cores,
                                                (self.scan_report.cpu.total_memory_mb as f64)
                                                    / 1024.0
                                            ))
                                            .color(TEXT_SECONDARY)
                                            .small(),
                                        );
                                    });

                                egui::Frame::new()
                                    .fill(BG_INPUT)
                                    .corner_radius(CornerRadius::same(6))
                                    .inner_margin(Margin::same(8))
                                    .show(&mut cols[1], |ui| {
                                        ui.label(
                                            RichText::new("GRAPHICS ADAPTER")
                                                .color(TEXT_MUTED)
                                                .small()
                                                .strong(),
                                        );
                                        if let Some(gpu) = &self.scan_report.gpu {
                                            ui.label(
                                                RichText::new(&gpu.name)
                                                    .color(TEXT_PRIMARY)
                                                    .small(),
                                            );
                                            ui.label(
                                                RichText::new(format!(
                                                    "{} MB VRAM (+{} MB Shared) • {}",
                                                    gpu.dedicated_vram_mb,
                                                    gpu.shared_memory_mb,
                                                    gpu.d3d11_feature_level
                                                ))
                                                .color(TEXT_SECONDARY)
                                                .small(),
                                            );
                                        } else {
                                            ui.label(
                                                RichText::new("No D3D11 Adapter")
                                                    .color(ACCENT_RED)
                                                    .small(),
                                            );
                                        }
                                    });

                                egui::Frame::new()
                                    .fill(BG_INPUT)
                                    .corner_radius(CornerRadius::same(6))
                                    .inner_margin(Margin::same(8))
                                    .show(&mut cols[2], |ui| {
                                        ui.label(
                                            RichText::new("DEVICE BENCHMARK")
                                                .color(TEXT_MUTED)
                                                .small()
                                                .strong(),
                                        );
                                        match &self.scan_report.compatibility {
                                            CompatibilityResult::Compatible { profile, .. } => {
                                                ui.label(
                                                    RichText::new(&profile.tier_name)
                                                        .color(ACCENT_EMERALD)
                                                        .small()
                                                        .strong(),
                                                );
                                                ui.label(
                                                    RichText::new(format!(
                                                        "Score: {}/100 • Capacity: ~{} FPS",
                                                        profile.hardware_score,
                                                        profile.measured_fps_capacity
                                                    ))
                                                    .color(TEXT_SECONDARY)
                                                    .small(),
                                                );
                                            }
                                            CompatibilityResult::Incompatible { reasons } => {
                                                ui.label(
                                                    RichText::new("Incompatible Device")
                                                        .color(ACCENT_RED)
                                                        .small()
                                                        .strong(),
                                                );
                                                if let Some(first) = reasons.first() {
                                                    ui.label(
                                                        RichText::new(first)
                                                            .color(TEXT_SECONDARY)
                                                            .small(),
                                                    );
                                                }
                                            }
                                        }
                                    });
                            });

                            ui.add_space(4.0);

                            // Stream Quality Preset Selector
                            ui.add_enabled_ui(controls_editable, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("Quality Profile:")
                                            .color(TEXT_SECONDARY)
                                            .strong(),
                                    );

                                    let presets = [
                                        ("Auto", "Auto (Device Recommended)"),
                                        ("1080p60", "1080p @ 60fps (6000 kbps)"),
                                        ("1080p30", "1080p @ 30fps (4000 kbps)"),
                                        ("720p60", "720p @ 60fps (3500 kbps)"),
                                        ("720p30", "720p @ 30fps (2500 kbps)"),
                                        ("540p30", "540p @ 30fps (1500 kbps)"),
                                    ];

                                    let current_label = presets
                                        .iter()
                                        .find(|(k, _)| *k == self.config.quality_preset)
                                        .map(|(_, v)| *v)
                                        .unwrap_or("Auto (Device Recommended)");

                                    egui::ComboBox::from_id_salt("quality_preset_combo")
                                        .width(240.0)
                                        .selected_text(current_label)
                                        .show_ui(ui, |ui| {
                                            for (key, label) in presets {
                                                let selected = self.config.quality_preset == key;
                                                if ui.selectable_label(selected, label).clicked() {
                                                    self.config.quality_preset = key.to_string();
                                                    let (w, h, fps, br) = self.hw_defaults();
                                                    self.config
                                                        .apply_quality_preset(w, h, fps, br);
                                                    self.config_applied = false;
                                                }
                                            }
                                        });

                                    ui.separator();
                                    ui.label(
                                        RichText::new(format!(
                                            "Active Output: {}x{} @ {} FPS • {} kbps",
                                            self.config.width,
                                            self.config.height,
                                            self.config.fps,
                                            self.config.bitrate_kbps
                                        ))
                                        .color(ACCENT_INDIGO_BRIGHT)
                                        .small()
                                        .strong(),
                                    );
                                });
                            });
                        });

                        ui.add_space(6.0);

                        // ============================================================
                        // SECTION 2: CAPTURE SOURCE (DESKTOP VS WINDOW MODE)
                        // ============================================================
                        studio_card(
                            ui,
                            if self.config_applied {
                                ACCENT_EMERALD_DARK
                            } else {
                                BORDER_SUBTLE
                            },
                            |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("CAPTURE SOURCE")
                                            .font(FontId::proportional(13.0))
                                            .color(ACCENT_INDIGO_BRIGHT)
                                            .strong(),
                                    );
                                    ui.separator();

                                    ui.add_enabled_ui(controls_editable, |ui| {
                                        let is_desktop = self.config.capture_mode == "Desktop";
                                        if ui
                                            .selectable_label(
                                                is_desktop,
                                                "  Whole Desktop Display  ",
                                            )
                                            .clicked()
                                            && !is_desktop
                                        {
                                            self.config.capture_mode = "Desktop".to_string();
                                            self.config_applied = false;
                                        }

                                        let is_window = self.config.capture_mode == "Window";
                                        if ui
                                            .selectable_label(
                                                is_window,
                                                "  Application / Game Window  ",
                                            )
                                            .clicked()
                                            && !is_window
                                        {
                                            self.config.capture_mode = "Window".to_string();
                                            self.available_windows = get_available_windows();
                                            if self.config.target_window_title.is_empty() {
                                                if let Some(first) = self.available_windows.first()
                                                {
                                                    self.config.target_window_title =
                                                        first.title.clone();
                                                    self.config.target_window_hwnd = first.hwnd;
                                                }
                                            }
                                            self.config_applied = false;
                                        }
                                    });

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if streaming {
                                                draw_badge(
                                                    ui,
                                                    "LOCKED (STREAM ACTIVE)",
                                                    BG_CARD_ELEVATED,
                                                    ACCENT_AMBER,
                                                );
                                            } else if self.config_applied {
                                                draw_badge(
                                                    ui,
                                                    "SOURCE APPLIED",
                                                    ACCENT_EMERALD_DARK,
                                                    Color32::WHITE,
                                                );
                                            } else {
                                                draw_badge(
                                                    ui,
                                                    "NEEDS APPLY",
                                                    BG_CARD_ELEVATED,
                                                    ACCENT_AMBER,
                                                );
                                            }
                                        },
                                    );
                                });

                                ui.add_space(4.0);

                                if self.config.capture_mode == "Window" {
                                    ui.add_enabled_ui(controls_editable, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new("Target Window:")
                                                    .color(TEXT_SECONDARY)
                                                    .strong(),
                                            );

                                            let display_title =
                                                if self.config.target_window_title.is_empty() {
                                                    "Select an open game or application window..."
                                                        .to_string()
                                                } else {
                                                    self.config.target_window_title.clone()
                                                };

                                            egui::ComboBox::from_id_salt("win_picker_combo")
                                                .width(420.0)
                                                .selected_text(display_title)
                                                .show_ui(ui, |ui| {
                                                    if self.available_windows.is_empty() {
                                                        ui.label(
                                                            RichText::new(
                                                                "No eligible application windows found. Click Refresh.",
                                                            )
                                                            .color(TEXT_MUTED),
                                                        );
                                                    }
                                                    for win in &self.available_windows {
                                                        let is_cur = (self.config.target_window_hwnd
                                                            != 0
                                                            && self.config.target_window_hwnd
                                                                == win.hwnd)
                                                            || (self.config.target_window_hwnd == 0
                                                                && self.config.target_window_title
                                                                    == win.title);
                                                        let label = if win.is_minimized {
                                                            format!(
                                                                "{}  [Minimized • {}x{}]",
                                                                win.title, win.rect.2, win.rect.3
                                                            )
                                                        } else {
                                                            format!(
                                                                "{}  [{}x{}]",
                                                                win.title, win.rect.2, win.rect.3
                                                            )
                                                        };
                                                        if ui
                                                            .selectable_label(is_cur, label)
                                                            .clicked()
                                                        {
                                                            self.config.target_window_title =
                                                                win.title.clone();
                                                            self.config.target_window_hwnd =
                                                                win.hwnd;
                                                            self.config_applied = false;
                                                        }
                                                    }
                                                });

                                            if ui.button("Refresh List").clicked() {
                                                self.available_windows = get_available_windows();
                                                if let Some(found) = resolve_target_window(
                                                    self.config.target_window_hwnd,
                                                    &self.config.target_window_title,
                                                ) {
                                                    self.config.target_window_hwnd = found.hwnd;
                                                    self.config.target_window_title =
                                                        found.title.clone();
                                                    self.status_message = format!(
                                                        "✔ Refreshed window list ({} windows). Target: '{}' (HWND 0x{:X})",
                                                        self.available_windows.len(),
                                                        found.title,
                                                        found.hwnd
                                                    );
                                                } else if let Some(first) =
                                                    self.available_windows.first()
                                                {
                                                    self.config.target_window_hwnd = first.hwnd;
                                                    self.config.target_window_title =
                                                        first.title.clone();
                                                    self.status_message = format!(
                                                        "✔ Refreshed window list ({} windows). Selected '{}'.",
                                                        self.available_windows.len(),
                                                        first.title
                                                    );
                                                }
                                            }
                                        });
                                    });

                                    ui.horizontal(|ui| {
                                        if let Some(target) = self
                                            .available_windows
                                            .iter()
                                            .find(|w| {
                                                (self.config.target_window_hwnd != 0
                                                    && w.hwnd == self.config.target_window_hwnd)
                                                    || w.title == self.config.target_window_title
                                            })
                                        {
                                            let state_str = if target.is_minimized {
                                                "Minimized (Will capture window directly when restored)"
                                            } else {
                                                "Active & Ready for Isolated Window Capture"
                                            };
                                            ui.label(
                                                RichText::new(format!(
                                                    "✔ Locked Target: '{}' (HWND 0x{:X} • {}x{}) — {}",
                                                    target.title,
                                                    target.hwnd,
                                                    target.rect.2,
                                                    target.rect.3,
                                                    state_str
                                                ))
                                                .color(ACCENT_EMERALD)
                                                .small(),
                                            );
                                        } else if !self.config.target_window_title.is_empty() {
                                            ui.label(
                                                RichText::new(format!(
                                                    "Target '{}' selected — click Refresh List if the window was reopened.",
                                                    self.config.target_window_title
                                                ))
                                                .color(ACCENT_AMBER)
                                                .small(),
                                            );
                                        }
                                    });
                                } else {
                                    ui.label(
                                        RichText::new(
                                            "Capturing primary monitor via Direct3D 11 DXGI Desktop Duplication (Single-Pass I420 Pipeline)",
                                        )
                                        .color(TEXT_SECONDARY)
                                        .small(),
                                    );
                                }
                            },
                        );

                        ui.add_space(6.0);

                        // ============================================================
                        // SECTION 3: STREAMING DESTINATION & APPLY LOCK
                        // ============================================================
                        studio_card(
                            ui,
                            if self.config_applied {
                                ACCENT_EMERALD_DARK
                            } else {
                                BORDER_ACCENT
                            },
                            |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("STREAMING DESTINATION")
                                            .font(FontId::proportional(13.0))
                                            .color(ACCENT_INDIGO_BRIGHT)
                                            .strong(),
                                    );
                                    ui.separator();

                                    ui.add_enabled_ui(controls_editable, |ui| {
                                        let platforms = [
                                            (
                                                "Facebook",
                                                "Facebook Live",
                                                "rtmps://live-api-s.facebook.com:443/rtmp/",
                                            ),
                                            ("Twitch", "Twitch", "rtmp://live.twitch.tv/app"),
                                            (
                                                "YouTube",
                                                "YouTube Live",
                                                "rtmp://a.rtmp.youtube.com/live2",
                                            ),
                                            (
                                                "Kick",
                                                "Kick (RTMPS)",
                                                "rtmps://fa723fc1b171.global-contribute.live-video.net/app",
                                            ),
                                            (
                                                "Custom",
                                                "Custom / Local",
                                                "rtmp://localhost:1935/live",
                                            ),
                                        ];

                                        for (id, label, default_url) in platforms {
                                            if ui
                                                .selectable_label(
                                                    self.config.platform_preset == id,
                                                    label,
                                                )
                                                .clicked()
                                            {
                                                self.config.platform_preset = id.to_string();
                                                self.config.server_url = default_url.to_string();
                                                self.config_applied = false;
                                            }
                                        }
                                    });
                                });

                                ui.add_space(4.0);

                                ui.add_enabled_ui(controls_editable, |ui| {
                                    egui::Grid::new("stream_dest_grid")
                                        .num_columns(2)
                                        .spacing([10.0, 8.0])
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new("Ingest Server URL:")
                                                    .color(TEXT_SECONDARY)
                                                    .strong(),
                                            );
                                            let url_res = ui.add(
                                                egui::TextEdit::singleline(
                                                    &mut self.config.server_url,
                                                )
                                                .desired_width(510.0),
                                            );
                                            if url_res.changed() {
                                                self.config_applied = false;
                                            }
                                            ui.end_row();

                                            ui.label(
                                                RichText::new("Stream Key:")
                                                    .color(TEXT_SECONDARY)
                                                    .strong(),
                                            );
                                            ui.horizontal(|ui| {
                                                let key_res = ui.add(
                                                    egui::TextEdit::singleline(
                                                        &mut self.config.stream_key,
                                                    )
                                                    .password(!self.show_stream_key)
                                                    .desired_width(435.0),
                                                );
                                                if key_res.changed() {
                                                    self.config_applied = false;
                                                }
                                                if ui
                                                    .button(if self.show_stream_key {
                                                        "Hide"
                                                    } else {
                                                        "Show"
                                                    })
                                                    .clicked()
                                                {
                                                    self.show_stream_key = !self.show_stream_key;
                                                }
                                            });
                                            ui.end_row();
                                        });
                                });

                                ui.add_space(6.0);

                                // Action Row: APPLY BUTTON + UNLOCK/EDIT (only when offline) + TEST STREAM SYNC
                                ui.horizontal(|ui| {
                                    if !self.config_applied {
                                        let apply_btn = ui.add_enabled(
                                            !streaming,
                                            egui::Button::new(
                                                RichText::new("✔ Apply Source & Destination")
                                                    .color(Color32::WHITE)
                                                    .strong(),
                                            )
                                            .fill(ACCENT_EMERALD_DARK),
                                        );
                                        if apply_btn.clicked() {
                                            self.apply_and_lock_settings();
                                        }
                                    } else {
                                        ui.add_enabled(
                                            false,
                                            egui::Button::new(
                                                RichText::new("✔ Applied & Locked")
                                                    .color(Color32::WHITE)
                                                    .strong(),
                                            )
                                            .fill(ACCENT_EMERALD_DARK),
                                        );

                                        // Allow unlocking to edit ONLY before "Start Streaming" is clicked!
                                        if !streaming {
                                            if ui
                                                .button(
                                                    RichText::new("Modify Settings")
                                                        .color(TEXT_SECONDARY),
                                                )
                                                .clicked()
                                            {
                                                self.config_applied = false;
                                                self.status_message =
                                                    "Settings unlocked for editing. Click 'Apply Source & Destination' when done."
                                                        .to_string();
                                            }
                                        }
                                    }

                                    ui.separator();

                                    let test_btn = ui.add_enabled(
                                        !testing && !streaming,
                                        egui::Button::new(
                                            RichText::new("Test Stream Sync")
                                                .color(Color32::WHITE)
                                                .strong(),
                                        )
                                        .fill(ACCENT_INDIGO),
                                    );

                                    if test_btn.clicked() {
                                        self.is_testing_sync.store(true, Ordering::Relaxed);
                                        let url = self.config.server_url.clone();
                                        let key = self.config.stream_key.clone();
                                        let testing_flag = self.is_testing_sync.clone();
                                        let status_tx = self.status_tx.clone();

                                        self.rt.spawn(async move {
                                            let res =
                                                crate::rtmp::test_stream_sync(&url, &key).await;
                                            testing_flag.store(false, Ordering::Relaxed);
                                            let msg = match res {
                                                Ok(ok_msg) => ok_msg,
                                                Err(err_msg) => err_msg,
                                            };
                                            let _ = status_tx.send(msg);
                                        });
                                    }

                                    if testing {
                                        ui.spinner();
                                        ui.label(
                                            RichText::new("Handshaking with ingest server...")
                                                .color(ACCENT_INDIGO_BRIGHT)
                                                .italics()
                                                .small(),
                                        );
                                    }
                                });
                            },
                        );

                        ui.add_space(6.0);

                        // ============================================================
                        // SECTION 4 & 5: STUDIO AUDIO MIXER + LIVE TELEMETRY (2 COLUMNS)
                        // ============================================================
                        ui.columns(2, |cols| {
                            // LEFT COLUMN: STUDIO AUDIO MIXER
                            studio_card(&mut cols[0], BORDER_SUBTLE, |ui| {
                                ui.label(
                                    RichText::new("STUDIO AUDIO MIXER")
                                        .font(FontId::proportional(13.0))
                                        .color(ACCENT_INDIGO_BRIGHT)
                                        .strong(),
                                );
                                ui.add_space(4.0);

                                let (desk_peak, mic_peak) = match &self.audio_engine {
                                    Some(eng) => (
                                        eng.controls.desktop_peak_pct.load(Ordering::Relaxed),
                                        eng.controls.mic_peak_pct.load(Ordering::Relaxed),
                                    ),
                                    None => (0, 0),
                                };

                                // Desktop Audio Channel
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("Desktop Audio")
                                            .color(TEXT_PRIMARY)
                                            .strong(),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let mute_label = if self.config.desktop_muted {
                                                "MUTED"
                                            } else {
                                                "ACTIVE"
                                            };
                                            let mute_color = if self.config.desktop_muted {
                                                ACCENT_RED_DARK
                                            } else {
                                                BG_CARD_ELEVATED
                                            };
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        RichText::new(mute_label)
                                                            .color(Color32::WHITE)
                                                            .small(),
                                                    )
                                                    .fill(mute_color),
                                                )
                                                .clicked()
                                            {
                                                self.config.desktop_muted =
                                                    !self.config.desktop_muted;
                                                if let Some(engine) = &self.audio_engine {
                                                    engine.controls.desktop_muted.store(
                                                        self.config.desktop_muted,
                                                        Ordering::Relaxed,
                                                    );
                                                }
                                                let _ = self.config.save();
                                            }
                                        },
                                    );
                                });

                                let mut desk_vol = self.config.desktop_volume as f32;
                                if ui
                                    .add(
                                        egui::Slider::new(&mut desk_vol, 0.0..=100.0)
                                            .suffix("%"),
                                    )
                                    .changed()
                                {
                                    self.config.desktop_volume = desk_vol as u32;
                                    if let Some(engine) = &self.audio_engine {
                                        engine
                                            .controls
                                            .desktop_volume
                                            .store(self.config.desktop_volume, Ordering::Relaxed);
                                    }
                                    let _ = self.config.save();
                                }
                                draw_vu_meter(
                                    ui,
                                    if self.config.desktop_muted {
                                        0
                                    } else {
                                        desk_peak
                                    },
                                );

                                ui.add_space(6.0);

                                // Microphone Channel
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("Microphone Input")
                                            .color(TEXT_PRIMARY)
                                            .strong(),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let mute_label = if self.config.mic_muted {
                                                "MUTED"
                                            } else {
                                                "ACTIVE"
                                            };
                                            let mute_color = if self.config.mic_muted {
                                                ACCENT_RED_DARK
                                            } else {
                                                BG_CARD_ELEVATED
                                            };
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        RichText::new(mute_label)
                                                            .color(Color32::WHITE)
                                                            .small(),
                                                    )
                                                    .fill(mute_color),
                                                )
                                                .clicked()
                                            {
                                                self.config.mic_muted = !self.config.mic_muted;
                                                if let Some(engine) = &self.audio_engine {
                                                    engine.controls.mic_muted.store(
                                                        self.config.mic_muted,
                                                        Ordering::Relaxed,
                                                    );
                                                }
                                                let _ = self.config.save();
                                            }
                                        },
                                    );
                                });

                                let mut mic_vol = self.config.mic_volume as f32;
                                if ui
                                    .add(
                                        egui::Slider::new(&mut mic_vol, 0.0..=100.0).suffix("%"),
                                    )
                                    .changed()
                                {
                                    self.config.mic_volume = mic_vol as u32;
                                    if let Some(engine) = &self.audio_engine {
                                        engine
                                            .controls
                                            .mic_volume
                                            .store(self.config.mic_volume, Ordering::Relaxed);
                                    }
                                    let _ = self.config.save();
                                }
                                draw_vu_meter(
                                    ui,
                                    if self.config.mic_muted { 0 } else { mic_peak },
                                );
                            });

                            // RIGHT COLUMN: LIVE STREAM TELEMETRY
                            studio_card(&mut cols[1], BORDER_SUBTLE, |ui| {
                                ui.label(
                                    RichText::new("LIVE BROADCAST TELEMETRY")
                                        .font(FontId::proportional(13.0))
                                        .color(ACCENT_INDIGO_BRIGHT)
                                        .strong(),
                                );
                                ui.add_space(4.0);

                                let current_bitrate =
                                    self.telemetry.current_bitrate_kbps.load(Ordering::Relaxed);
                                let current_fps = self.telemetry.fps.load(Ordering::Relaxed);
                                let dropped =
                                    self.telemetry.dropped_frames.load(Ordering::Relaxed);
                                let total = self.telemetry.total_frames.load(Ordering::Relaxed);

                                ui.columns(2, |mcols| {
                                    egui::Frame::new()
                                        .fill(BG_INPUT)
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(Margin::same(8))
                                        .show(&mut mcols[0], |ui| {
                                            ui.label(
                                                RichText::new("BITRATE")
                                                    .color(TEXT_MUTED)
                                                    .small()
                                                    .strong(),
                                            );
                                            ui.label(
                                                RichText::new(format!("{} kbps", current_bitrate))
                                                    .font(FontId::proportional(17.0))
                                                    .color(if streaming {
                                                        ACCENT_EMERALD
                                                    } else {
                                                        TEXT_PRIMARY
                                                    })
                                                    .strong(),
                                            );
                                            ui.label(
                                                RichText::new(format!(
                                                    "Target: {} kbps",
                                                    self.applied_config.bitrate_kbps
                                                ))
                                                .color(TEXT_SECONDARY)
                                                .small(),
                                            );
                                        });

                                    egui::Frame::new()
                                        .fill(BG_INPUT)
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(Margin::same(8))
                                        .show(&mut mcols[1], |ui| {
                                            ui.label(
                                                RichText::new("FRAME RATE & DROPS")
                                                    .color(TEXT_MUTED)
                                                    .small()
                                                    .strong(),
                                            );
                                            let fps_color = if !streaming {
                                                TEXT_PRIMARY
                                            } else if current_fps >= self.applied_config.fps.saturating_sub(4) {
                                                ACCENT_EMERALD
                                            } else {
                                                ACCENT_AMBER
                                            };
                                            ui.label(
                                                RichText::new(format!(
                                                    "{} / {} FPS",
                                                    current_fps, self.applied_config.fps
                                                ))
                                                .font(FontId::proportional(17.0))
                                                .color(fps_color)
                                                .strong(),
                                            );
                                            ui.label(
                                                RichText::new(format!(
                                                    "Dropped: {} ({} sent)",
                                                    dropped, total
                                                ))
                                                .color(if dropped == 0 {
                                                    TEXT_SECONDARY
                                                } else {
                                                    ACCENT_AMBER
                                                })
                                                .small(),
                                            );
                                        });
                                });

                                ui.add_space(6.0);
                                draw_bitrate_sparkline(
                                    ui,
                                    &self.bitrate_history,
                                    self.applied_config.bitrate_kbps.max(1000),
                                );
                            });
                        });

                        ui.add_space(8.0);

                        // ============================================================
                        // SECTION 6: PRIMARY BROADCAST CONTROL DOCK
                        // ============================================================
                        let is_compatible = matches!(
                            self.scan_report.compatibility,
                            CompatibilityResult::Compatible { .. }
                        );

                        studio_card(
                            ui,
                            if streaming {
                                ACCENT_RED
                            } else if self.config_applied {
                                ACCENT_EMERALD
                            } else {
                                BORDER_SUBTLE
                            },
                            |ui| {
                                ui.vertical_centered(|ui| {
                                    if streaming {
                                        let stop_btn = ui.add_sized(
                                            [310.0, 42.0],
                                            egui::Button::new(
                                                RichText::new("■  STOP LIVE BROADCAST")
                                                    .color(Color32::WHITE)
                                                    .size(15.5)
                                                    .strong(),
                                            )
                                            .fill(ACCENT_RED),
                                        );

                                        if stop_btn.clicked() {
                                            self.stop_signal.store(false, Ordering::Relaxed);
                                            self.telemetry
                                                .is_streaming
                                                .store(false, Ordering::Relaxed);
                                            self.stream_start_time = None;
                                            self.status_message =
                                                "Broadcast stopped. Click 'Modify Settings' if you wish to change source or destination."
                                                    .to_string();
                                        }
                                    } else {
                                        let can_start = is_compatible && self.config_applied;
                                        let btn_label = if !self.config_applied {
                                            "▶  START STREAMING (Click 'Apply' Above First)"
                                        } else {
                                            "▶  START LIVE STREAMING"
                                        };

                                        let start_btn = ui.add_enabled(
                                            can_start,
                                            egui::Button::new(
                                                RichText::new(btn_label)
                                                    .color(Color32::WHITE)
                                                    .size(15.5)
                                                    .strong(),
                                            )
                                            .min_size(egui::vec2(340.0, 42.0))
                                            .fill(if can_start {
                                                ACCENT_EMERALD_DARK
                                            } else {
                                                BG_CARD_ELEVATED
                                            }),
                                        );

                                        if start_btn.clicked() {
                                            if !is_compatible {
                                                self.status_message =
                                                    "✖ Cannot start stream: Hardware is incompatible."
                                                        .to_string();
                                            } else if self
                                                .applied_config
                                                .stream_key
                                                .trim()
                                                .is_empty()
                                            {
                                                self.config_applied = false;
                                                self.status_message =
                                                    "✖ Stream Key is empty. Please enter your Stream Key above and click Apply."
                                                        .to_string();
                                            } else {
                                                if let Some(engine) = &mut self.audio_engine {
                                                    engine.start_streams();
                                                }
                                                self.stop_signal =
                                                    Arc::new(AtomicBool::new(true));
                                                self.telemetry
                                                    .is_streaming
                                                    .store(true, Ordering::Relaxed);
                                                self.telemetry
                                                    .dropped_frames
                                                    .store(0, Ordering::Relaxed);
                                                self.telemetry
                                                    .total_frames
                                                    .store(0, Ordering::Relaxed);
                                                self.stream_start_time = Some(Instant::now());
                                                self.status_message =
                                                    "Connecting to ingest server...".to_string();

                                                let cfg = self.applied_config.clone();
                                                let session = StreamSession::new(
                                                    cfg.server_url,
                                                    cfg.stream_key,
                                                    cfg.width,
                                                    cfg.height,
                                                    cfg.fps,
                                                    cfg.bitrate_kbps,
                                                    cfg.capture_mode,
                                                    cfg.target_window_title,
                                                    cfg.target_window_hwnd,
                                                    self.telemetry.clone(),
                                                    self.status_tx.clone(),
                                                );
                                                let stop = self.stop_signal.clone();
                                                self.rt.spawn(async move {
                                                    let _ = session.run(stop).await;
                                                });
                                            }
                                        }
                                    }

                                    ui.add_space(4.0);

                                    let is_error = self.status_message.contains("✖")
                                        || self.status_message.contains("rejected")
                                        || self.status_message.contains("failed")
                                        || self.status_message.contains("error");
                                    let is_success = self.status_message.contains("✔")
                                        || self.status_message.contains("LIVE:");

                                    let text_color = if is_error {
                                        ACCENT_RED
                                    } else if is_success {
                                        ACCENT_EMERALD
                                    } else {
                                        TEXT_SECONDARY
                                    };

                                    ui.label(
                                        RichText::new(&self.status_message)
                                            .color(text_color)
                                            .small()
                                            .strong(),
                                    );
                                });
                            },
                        );
                    });
            });
    }
}

fn draw_vu_meter(ui: &mut egui::Ui, level_pct: u32) {
    let desired_size = egui::vec2(ui.available_width(), 8.0);
    let (rect, _) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    let painter = ui.painter();

    painter.rect_filled(rect, CornerRadius::same(4), BG_INPUT);

    let pct = (level_pct.min(100) as f32) / 100.0;
    if pct > 0.01 {
        let fill_w = rect.width() * pct;
        let fill_rect =
            egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height()));
        let color = if level_pct > 85 {
            ACCENT_RED
        } else if level_pct > 65 {
            ACCENT_AMBER
        } else {
            ACCENT_EMERALD
        };
        painter.rect_filled(fill_rect, CornerRadius::same(4), color);
    }
}

fn draw_bitrate_sparkline(ui: &mut egui::Ui, history: &VecDeque<u32>, target_kbps: u32) {
    let desired_size = egui::vec2(ui.available_width(), 38.0);
    let (rect, _) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    let painter = ui.painter();

    painter.rect_filled(rect, CornerRadius::same(6), BG_INPUT);
    painter.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0, BORDER_SUBTLE),
        egui::StrokeKind::Inside,
    );

    if history.is_empty() {
        return;
    }

    let max_val = (target_kbps as f32 * 1.35).max(1000.0);
    let n = history.len();
    let bar_w = (rect.width() / (n as f32)).max(2.0);

    for (i, &val) in history.iter().enumerate() {
        let ratio = ((val as f32) / max_val).clamp(0.0, 1.0);
        let h = (rect.height() - 4.0) * ratio;
        if h > 1.0 {
            let x = rect.left() + (i as f32) * bar_w;
            let bar_rect = egui::Rect::from_min_max(
                egui::pos2(x + 1.0, rect.bottom() - 2.0 - h),
                egui::pos2((x + bar_w - 1.0).max(x + 1.5), rect.bottom() - 2.0),
            );
            painter.rect_filled(bar_rect, CornerRadius::same(2), ACCENT_INDIGO_BRIGHT);
        }
    }
}

pub fn launch_gui() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([860.0, 740.0])
            .with_min_inner_size([760.0, 640.0])
            .with_title("ObsLite v0.5 — Broadcast Studio")
            .with_visible(true)
            .with_active(true)
            .with_taskbar(true)
            .with_decorations(true)
            .with_transparent(false),
        centered: true,
        ..Default::default()
    };

    tracing::info!("Launching ObsLite v0.5 native GUI window...");
    eframe::run_native(
        "ObsLite v0.5",
        options,
        Box::new(|cc| {
            tracing::info!("eframe creation context initialized, constructing ObsLiteApp v0.5");
            Ok(Box::new(ObsLiteApp::new(cc)))
        }),
    )
}
