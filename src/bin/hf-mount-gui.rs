#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, Once};
use std::thread::{self, JoinHandle};

use eframe::egui::{self, RichText, TextEdit};
use hf_mount::setup::{CacheMode, MountOptions, Source};

static BACKEND_INIT: Once = Once::new();

fn main() {
    if handle_cli_info() {
        return;
    }

    BACKEND_INIT.call_once(|| {
        hf_mount::setup::raise_fd_limit();
        hf_mount::setup::init_tracing(false);
    });

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 520.0])
            .with_min_inner_size([620.0, 420.0]),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "hf-mount",
        native_options,
        Box::new(|cc| {
            apply_codex_theme(&cc.egui_ctx);
            Ok(Box::new(MountGuiApp::default()))
        }),
    ) {
        eprintln!("failed to start hf-mount GUI: {e}");
        std::process::exit(1);
    }
}

fn handle_cli_info() -> bool {
    let mut args = std::env::args().skip(1);
    let Some(arg) = args.next() else {
        return false;
    };

    match arg.as_str() {
        "-h" | "--help" => {
            print_help();
            true
        }
        "-V" | "--version" => {
            println!("hf-mount-gui {}", env!("CARGO_PKG_VERSION"));
            true
        }
        other => {
            eprintln!("unknown argument: {other}");
            print_help();
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!(
        "hf-mount-gui {version}\n\
         Native GUI for mounting Hugging Face repos and buckets through the NFS backend.\n\n\
         USAGE:\n\
           hf-mount-gui\n\
           hf-mount-gui --help\n\
           hf-mount-gui --version\n\n\
         Windows requires Client for NFS and an Administrator session.",
        version = env!("CARGO_PKG_VERSION")
    );
}

fn apply_codex_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let mut visuals = egui::Visuals::dark();
    let rounding = egui::Rounding::same(8.0);

    visuals.panel_fill = app_bg();
    visuals.window_fill = panel_bg();
    visuals.extreme_bg_color = input_bg();
    visuals.faint_bg_color = egui::Color32::from_rgb(28, 30, 36);
    visuals.code_bg_color = egui::Color32::from_rgb(24, 26, 31);
    visuals.selection.bg_fill = accent();
    visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    visuals.hyperlink_color = egui::Color32::from_rgb(137, 180, 250);
    visuals.warn_fg_color = warning_fg();
    visuals.error_fg_color = error_fg();
    visuals.window_rounding = rounding;
    visuals.menu_rounding = rounding;

    visuals.widgets.noninteractive.rounding = rounding;
    visuals.widgets.inactive.rounding = rounding;
    visuals.widgets.hovered.rounding = rounding;
    visuals.widgets.active.rounding = rounding;
    visuals.widgets.open.rounding = rounding;

    visuals.widgets.noninteractive.bg_fill = panel_bg();
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, border());
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, text_primary());
    visuals.widgets.inactive.bg_fill = input_bg();
    visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(32, 34, 40);
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border());
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, text_primary());
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(43, 46, 54);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(76, 80, 92));
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(50, 54, 63);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, accent());

    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(10.0, 9.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.window_margin = egui::Margin::same(16.0);
    ctx.set_style(style);
}

fn app_bg() -> egui::Color32 {
    egui::Color32::from_rgb(14, 15, 18)
}

fn sidebar_bg() -> egui::Color32 {
    egui::Color32::from_rgb(18, 19, 23)
}

fn panel_bg() -> egui::Color32 {
    egui::Color32::from_rgb(22, 24, 29)
}

fn input_bg() -> egui::Color32 {
    egui::Color32::from_rgb(17, 19, 23)
}

fn border() -> egui::Color32 {
    egui::Color32::from_rgb(47, 50, 59)
}

fn accent() -> egui::Color32 {
    egui::Color32::from_rgb(76, 141, 105)
}

fn text_primary() -> egui::Color32 {
    egui::Color32::from_rgb(235, 236, 240)
}

fn text_secondary() -> egui::Color32 {
    egui::Color32::from_rgb(160, 166, 178)
}

fn warning_fg() -> egui::Color32 {
    egui::Color32::from_rgb(230, 179, 93)
}

fn error_fg() -> egui::Color32 {
    egui::Color32::from_rgb(229, 111, 111)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GuiSource {
    Repo,
    Bucket,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MountState {
    Idle,
    Running,
    Stopping,
    Stopped,
    Error,
}

#[derive(Clone, Debug)]
struct SharedStatus {
    state: MountState,
    message: String,
}

impl Default for SharedStatus {
    fn default() -> Self {
        Self {
            state: MountState::Idle,
            message: "Ready".to_string(),
        }
    }
}

type SharedMountStatus = Arc<Mutex<SharedStatus>>;

struct MountGuiApp {
    source: GuiSource,
    source_id: String,
    revision: String,
    mount_point: String,
    hf_token: String,
    hub_endpoint: String,
    cache_dir: String,
    read_only: bool,
    status: SharedMountStatus,
    mount_thread: Option<JoinHandle<()>>,
    active_mount_point: Option<PathBuf>,
}

impl Default for MountGuiApp {
    fn default() -> Self {
        Self {
            source: GuiSource::Repo,
            source_id: "openai/gpt-oss-20b".to_string(),
            revision: "main".to_string(),
            mount_point: default_mount_point(),
            hf_token: std::env::var("HF_TOKEN").unwrap_or_default(),
            hub_endpoint: "https://huggingface.co".to_string(),
            cache_dir: std::env::temp_dir()
                .join("hf-mount-cache")
                .to_string_lossy()
                .into_owned(),
            read_only: true,
            status: Arc::new(Mutex::new(SharedStatus::default())),
            mount_thread: None,
            active_mount_point: None,
        }
    }
}

impl eframe::App for MountGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.collect_finished_mount();
        ctx.request_repaint_after(std::time::Duration::from_millis(500));

        egui::SidePanel::left("hf-mount-sidebar")
            .resizable(false)
            .exact_width(230.0)
            .frame(
                egui::Frame::none()
                    .fill(sidebar_bg())
                    .inner_margin(egui::Margin::same(20.0))
                    .stroke(egui::Stroke::new(1.0, border())),
            )
            .show(ctx, |ui| self.draw_sidebar(ui));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(app_bg())
                    .inner_margin(egui::Margin::same(22.0)),
            )
            .show(ctx, |ui| self.draw_main_panel(ui));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(mount_point) = &self.active_mount_point {
            let _ = unmount_path(mount_point);
        }
    }
}

impl MountGuiApp {
    fn draw_sidebar(&self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.label(RichText::new("hf-mount").size(25.0).strong().color(text_primary()));
            ui.label(RichText::new("NFS backend").color(text_secondary()));
            ui.add_space(20.0);

            self.draw_status_card(ui);

            ui.add_space(18.0);
            ui.label(RichText::new("Backend").small().strong().color(text_secondary()));
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                pill(
                    ui,
                    "NFS",
                    egui::Color32::from_rgb(190, 235, 208),
                    egui::Color32::from_rgb(29, 58, 43),
                );
                pill(
                    ui,
                    platform_label(),
                    text_secondary(),
                    egui::Color32::from_rgb(31, 34, 40),
                );
            });

            ui.add_space(18.0);
            ui.label(RichText::new("Source").small().strong().color(text_secondary()));
            ui.add_space(6.0);
            ui.label(RichText::new(self.source_label()).monospace().color(text_primary()));

            ui.add_space(18.0);
            ui.label(RichText::new("Access").small().strong().color(text_secondary()));
            ui.add_space(6.0);
            ui.label(if self.read_only {
                RichText::new("Read-only").color(text_primary())
            } else {
                RichText::new("Read/write").color(text_primary())
            });
        });
    }

    fn draw_main_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Mount").size(24.0).strong().color(text_primary()));
                ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).color(text_secondary()));
            });
        });

        ui.add_space(14.0);
        self.draw_actions(ui);
        ui.add_space(16.0);

        section(ui, "Source", |ui| self.draw_source_section(ui));
        ui.add_space(12.0);
        section(ui, "Mount", |ui| self.draw_mount_section(ui));
        ui.add_space(12.0);
        section(ui, "Connection", |ui| self.draw_connection_section(ui));
    }

    fn draw_actions(&mut self, ui: &mut egui::Ui) {
        let running = self.is_mount_thread_running();
        ui.horizontal(|ui| {
            let start_button = egui::Button::new(RichText::new("Start mount").strong().color(egui::Color32::WHITE))
                .fill(if running { input_bg() } else { accent() })
                .min_size(egui::vec2(132.0, 36.0));
            if ui.add_enabled(!running, start_button).clicked() {
                self.start_mount();
            }

            let stop_button = egui::Button::new(RichText::new("Stop").strong())
                .fill(egui::Color32::from_rgb(67, 35, 38))
                .min_size(egui::vec2(92.0, 36.0));
            if ui.add_enabled(running, stop_button).clicked() {
                self.stop_mount();
            }
        });
    }

    fn draw_source_section(&mut self, ui: &mut egui::Ui) {
        field_row(ui, "Type", |ui| {
            egui::ComboBox::from_id_source("source-kind")
                .width(ui.available_width())
                .selected_text(match self.source {
                    GuiSource::Repo => "Repo",
                    GuiSource::Bucket => "Bucket",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.source, GuiSource::Repo, "Repo");
                    ui.selectable_value(&mut self.source, GuiSource::Bucket, "Bucket");
                });
        });

        field_row(
            ui,
            match self.source {
                GuiSource::Repo => "Repo ID",
                GuiSource::Bucket => "Bucket ID",
            },
            |ui| {
                ui.add_sized(
                    [ui.available_width(), 34.0],
                    TextEdit::singleline(&mut self.source_id).desired_width(f32::INFINITY),
                );
            },
        );

        if self.source == GuiSource::Repo {
            field_row(ui, "Revision", |ui| {
                ui.add_sized(
                    [ui.available_width(), 34.0],
                    TextEdit::singleline(&mut self.revision).desired_width(f32::INFINITY),
                );
            });
        }
    }

    fn draw_mount_section(&mut self, ui: &mut egui::Ui) {
        field_row(ui, "Mount point", |ui| {
            ui.add_sized(
                [ui.available_width(), 34.0],
                TextEdit::singleline(&mut self.mount_point).desired_width(f32::INFINITY),
            );
        });

        field_row(ui, "Access", |ui| {
            ui.checkbox(&mut self.read_only, "Read-only");
        });
    }

    fn draw_connection_section(&mut self, ui: &mut egui::Ui) {
        field_row(ui, "HF token", |ui| {
            ui.add_sized(
                [ui.available_width(), 34.0],
                TextEdit::singleline(&mut self.hf_token)
                    .password(true)
                    .desired_width(f32::INFINITY),
            );
        });
        field_row(ui, "Hub endpoint", |ui| {
            ui.add_sized(
                [ui.available_width(), 34.0],
                TextEdit::singleline(&mut self.hub_endpoint).desired_width(f32::INFINITY),
            );
        });
        field_row(ui, "Cache dir", |ui| {
            ui.add_sized(
                [ui.available_width(), 34.0],
                TextEdit::singleline(&mut self.cache_dir).desired_width(f32::INFINITY),
            );
        });
    }

    fn draw_status_card(&self, ui: &mut egui::Ui) {
        let status = self.status.lock().expect("status mutex poisoned").clone();
        let (label, fg, bg) = status_colors(&status.state);

        egui::Frame::none()
            .fill(panel_bg())
            .stroke(egui::Stroke::new(1.0, border()))
            .rounding(8.0)
            .inner_margin(egui::Margin::symmetric(14.0, 12.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Status").small().strong().color(text_secondary()));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    pill(ui, label, fg, bg);
                    if matches!(status.state, MountState::Running | MountState::Stopping) {
                        ui.add(egui::Spinner::new().size(15.0));
                    }
                });
                ui.add_space(8.0);
                ui.label(RichText::new(status.message).color(text_primary()));
            });
    }

    fn source_label(&self) -> &'static str {
        match self.source {
            GuiSource::Repo => "Repo",
            GuiSource::Bucket => "Bucket",
        }
    }

    fn start_mount(&mut self) {
        let source = match self.mount_source() {
            Ok(source) => source,
            Err(e) => {
                set_status(&self.status, MountState::Error, e);
                return;
            }
        };
        let options = match self.mount_options() {
            Ok(options) => options,
            Err(e) => {
                set_status(&self.status, MountState::Error, e);
                return;
            }
        };
        let mount_point = source.mount_point().to_path_buf();
        let shared_status = self.status.clone();

        set_status(
            &shared_status,
            MountState::Running,
            format!("Mount process running at {}", mount_point.display()),
        );
        self.active_mount_point = Some(mount_point);
        self.mount_thread = Some(thread::spawn(move || run_mount(source, options, shared_status)));
    }

    fn stop_mount(&mut self) {
        let Some(mount_point) = self.active_mount_point.clone() else {
            set_status(&self.status, MountState::Error, "No active mount point is recorded");
            return;
        };

        set_status(
            &self.status,
            MountState::Stopping,
            format!("Unmount requested for {}", mount_point.display()),
        );

        if let Err(e) = unmount_path(&mount_point) {
            set_status(&self.status, MountState::Error, e);
        }
    }

    fn collect_finished_mount(&mut self) {
        let finished = self.mount_thread.as_ref().is_some_and(JoinHandle::is_finished);
        if !finished {
            return;
        }

        if let Some(handle) = self.mount_thread.take()
            && handle.join().is_err()
        {
            set_status(&self.status, MountState::Error, "Mount thread panicked");
        }
        self.active_mount_point = None;
    }

    fn is_mount_thread_running(&self) -> bool {
        self.mount_thread.as_ref().is_some_and(|handle| !handle.is_finished())
    }

    fn mount_source(&self) -> Result<Source, String> {
        let source_id = self.source_id.trim();
        if source_id.is_empty() {
            return Err("Source ID is required".to_string());
        }

        let mount_point = parse_path(&self.mount_point, "Mount point")?;
        Ok(match self.source {
            GuiSource::Repo => Source::Repo {
                repo_id: source_id.to_string(),
                mount_point,
                revision: non_empty_or_default(&self.revision, "main"),
            },
            GuiSource::Bucket => Source::Bucket {
                bucket_id: source_id.to_string(),
                mount_point,
            },
        })
    }

    fn mount_options(&self) -> Result<MountOptions, String> {
        Ok(MountOptions {
            hf_token: optional_text(&self.hf_token),
            token_file: None,
            hub_endpoint: non_empty_or_default(&self.hub_endpoint, "https://huggingface.co"),
            cache_dir: parse_path(&self.cache_dir, "Cache directory")?,
            uid: None,
            gid: None,
            read_only: self.read_only,
            advanced_writes: false,
            poll_interval_secs: 30,
            poll_listing_concurrency: 4,
            cache_size: 10_000_000_000,
            max_staging_size: 0,
            no_disk_cache: false,
            cache_mode: CacheMode::Chunk,
            direct_io: false,
            metadata_ttl_ms: 10_000,
            metadata_ttl_minimal: false,
            max_threads: 16,
            flush_debounce_ms: 2_000,
            flush_max_batch_window_ms: 30_000,
            no_filter_os_files: false,
            fuse_owner_only: false,
            inode_soft_limit: 0,
            lru_sweep_interval_ms: 5_000,
            overlay: false,
        })
    }
}

fn section(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(panel_bg())
        .stroke(egui::Stroke::new(1.0, border()))
        .rounding(8.0)
        .inner_margin(egui::Margin::symmetric(16.0, 14.0))
        .show(ui, |ui| {
            ui.label(RichText::new(title).strong().color(text_primary()));
            ui.add_space(10.0);
            add_contents(ui);
        });
}

fn field_row(ui: &mut egui::Ui, label: &str, add_field: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_height(38.0);
        ui.add_sized(
            [116.0, 26.0],
            egui::Label::new(RichText::new(label).color(text_secondary())),
        );
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            add_field(ui);
        });
    });
}

fn pill(ui: &mut egui::Ui, text: &str, fg: egui::Color32, bg: egui::Color32) {
    egui::Frame::none()
        .fill(bg)
        .rounding(egui::Rounding::same(999.0))
        .inner_margin(egui::Margin::symmetric(10.0, 4.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(fg));
        });
}

fn status_colors(state: &MountState) -> (&'static str, egui::Color32, egui::Color32) {
    match state {
        MountState::Idle => ("Idle", text_secondary(), egui::Color32::from_rgb(31, 34, 40)),
        MountState::Running => (
            "Running",
            egui::Color32::from_rgb(190, 235, 208),
            egui::Color32::from_rgb(29, 58, 43),
        ),
        MountState::Stopping => (
            "Stopping",
            egui::Color32::from_rgb(248, 222, 166),
            egui::Color32::from_rgb(75, 55, 25),
        ),
        MountState::Stopped => ("Stopped", text_secondary(), egui::Color32::from_rgb(31, 34, 40)),
        MountState::Error => (
            "Error",
            egui::Color32::from_rgb(255, 196, 196),
            egui::Color32::from_rgb(76, 35, 40),
        ),
    }
}

fn platform_label() -> &'static str {
    #[cfg(windows)]
    {
        "Windows"
    }
    #[cfg(target_os = "macos")]
    {
        "macOS"
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        "Unix"
    }
}

fn run_mount(source: Source, options: MountOptions, shared_status: SharedMountStatus) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let setup = hf_mount::setup::build(source, options, true);
        setup.runtime.block_on(hf_mount::nfs::mount_nfs(
            setup.virtual_fs,
            &setup.mount_point,
            setup.metadata_ttl_ms,
            setup.read_only,
            None,
        ))
    }));

    match result {
        Ok(Ok(())) => set_status(&shared_status, MountState::Stopped, "Unmounted cleanly"),
        Ok(Err(e)) => set_status(&shared_status, MountState::Error, format!("NFS mount failed: {e}")),
        Err(payload) => set_status(
            &shared_status,
            MountState::Error,
            format!("Mount setup failed: {}", panic_message(payload)),
        ),
    }
}

fn unmount_path(mount_point: &Path) -> Result<(), String> {
    let mount_point = mount_point
        .to_str()
        .ok_or_else(|| "Mount point is not valid UTF-8".to_string())?;

    let output = unmount_command(mount_point)
        .output()
        .map_err(|e| format!("Failed to run unmount command: {e}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(format!(
        "Unmount failed with {}: stdout={} stderr={}",
        output.status,
        stdout.trim(),
        stderr.trim()
    ))
}

#[cfg(windows)]
fn unmount_command(mount_point: &str) -> Command {
    let mut command = Command::new("umount");
    command.args(["-f", mount_point]);
    command
}

#[cfg(not(windows))]
fn unmount_command(mount_point: &str) -> Command {
    let mut command = Command::new("umount");
    command.arg(mount_point);
    command
}

fn set_status(status: &SharedMountStatus, state: MountState, message: impl Into<String>) {
    *status.lock().expect("status mutex poisoned") = SharedStatus {
        state,
        message: message.into(),
    };
}

fn parse_path(text: &str, label: &str) -> Result<PathBuf, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} is required"));
    }
    Ok(PathBuf::from(trimmed))
}

fn optional_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn non_empty_or_default(text: &str, default: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn default_mount_point() -> String {
    #[cfg(windows)]
    {
        "Z:".to_string()
    }
    #[cfg(not(windows))]
    {
        std::env::temp_dir().join("hf-mount").to_string_lossy().into_owned()
    }
}
