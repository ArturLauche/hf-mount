#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, Once};
use std::thread::{self, JoinHandle};

#[cfg(windows)]
use std::net::{TcpListener, UdpSocket};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use std::process::Stdio;

use eframe::egui::{self, RichText, TextEdit};
use hf_mount::nfs::NfsMountEvent;
use hf_mount::setup::{CacheMode, MountOptions, Source};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const MAX_LOG_LINES: usize = 80;

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
            .with_inner_size([1120.0, 720.0])
            .with_min_inner_size([820.0, 560.0]),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "hf-mount",
        native_options,
        Box::new(|cc| {
            apply_theme(&cc.egui_ctx);
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
        "--check-setup" => {
            let mount_point = args.next().unwrap_or_else(default_mount_point);
            let checks = run_preflight_checks(&mount_point);
            for check in &checks {
                println!("[{}] {}: {}", check_level_label(check.level), check.label, check.detail);
            }
            if checks.iter().any(|check| check.level == CheckLevel::Fail) {
                std::process::exit(1);
            }
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
           hf-mount-gui --version\n\
           hf-mount-gui --check-setup [MOUNT_POINT]\n\n\
         Windows requires Client for NFS and an Administrator session.",
        version = env!("CARGO_PKG_VERSION")
    );
}

fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let mut visuals = egui::Visuals::dark();
    let rounding = egui::Rounding::same(8.0);

    visuals.panel_fill = app_bg();
    visuals.window_fill = panel_bg();
    visuals.extreme_bg_color = input_bg();
    visuals.faint_bg_color = elevated_bg();
    visuals.code_bg_color = input_bg();
    visuals.selection.bg_fill = egui::Color32::from_rgb(74, 74, 70);
    visuals.selection.stroke = egui::Stroke::new(1.0, text_primary());
    visuals.hyperlink_color = action_orange();
    visuals.warn_fg_color = warning_fg();
    visuals.error_fg_color = error_fg();
    visuals.window_rounding = rounding;
    visuals.menu_rounding = rounding;

    for widgets in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widgets.rounding = rounding;
    }

    visuals.widgets.noninteractive.bg_fill = panel_bg();
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, border());
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, text_primary());
    visuals.widgets.inactive.bg_fill = input_bg();
    visuals.widgets.inactive.weak_bg_fill = elevated_bg();
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border());
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, text_primary());
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(70, 70, 66);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(86, 86, 82));
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(76, 76, 72);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, text_secondary());

    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(13.0, 7.0);
    style.spacing.window_margin = egui::Margin::same(0.0);
    ctx.set_style(style);
}

fn app_bg() -> egui::Color32 {
    egui::Color32::from_rgb(40, 40, 38)
}

fn sidebar_bg() -> egui::Color32 {
    egui::Color32::from_rgb(27, 27, 27)
}

fn panel_bg() -> egui::Color32 {
    egui::Color32::from_rgb(59, 59, 57)
}

fn elevated_bg() -> egui::Color32 {
    egui::Color32::from_rgb(48, 48, 47)
}

fn input_bg() -> egui::Color32 {
    egui::Color32::from_rgb(48, 48, 47)
}

fn border() -> egui::Color32 {
    egui::Color32::from_rgb(74, 74, 71)
}

fn primary_button_bg() -> egui::Color32 {
    egui::Color32::from_rgb(242, 242, 242)
}

fn primary_button_text() -> egui::Color32 {
    egui::Color32::from_rgb(36, 36, 34)
}

fn action_orange() -> egui::Color32 {
    egui::Color32::from_rgb(240, 122, 50)
}

fn success_fg() -> egui::Color32 {
    egui::Color32::from_rgb(102, 192, 133)
}

fn text_primary() -> egui::Color32 {
    egui::Color32::from_rgb(242, 242, 242)
}

fn text_secondary() -> egui::Color32 {
    egui::Color32::from_rgb(167, 167, 162)
}

fn muted_text() -> egui::Color32 {
    egui::Color32::from_rgb(126, 126, 120)
}

fn warning_fg() -> egui::Color32 {
    egui::Color32::from_rgb(240, 122, 50)
}

fn error_fg() -> egui::Color32 {
    egui::Color32::from_rgb(238, 107, 107)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GuiSource {
    Repo,
    Bucket,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MountState {
    Ready,
    Mounting,
    Mounted,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Debug)]
struct SharedStatus {
    state: MountState,
    headline: String,
    detail: String,
    log: VecDeque<String>,
}

impl Default for SharedStatus {
    fn default() -> Self {
        let mut log = VecDeque::new();
        log.push_back("Ready".to_string());
        Self {
            state: MountState::Ready,
            headline: "Ready".to_string(),
            detail: "Configure a source and start the mount.".to_string(),
            log,
        }
    }
}

type SharedMountStatus = Arc<Mutex<SharedStatus>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckLevel {
    Pass,
    Warn,
    Fail,
}

#[derive(Clone, Debug)]
struct CheckItem {
    level: CheckLevel,
    label: String,
    detail: String,
}

struct MountGuiApp {
    source: GuiSource,
    source_id: String,
    revision: String,
    mount_point: String,
    hf_token: String,
    hub_endpoint: String,
    cache_dir: String,
    read_only: bool,
    show_advanced: bool,
    checks: Vec<CheckItem>,
    status: SharedMountStatus,
    mount_thread: Option<JoinHandle<()>>,
    active_mount_point: Option<PathBuf>,
}

impl Default for MountGuiApp {
    fn default() -> Self {
        let mount_point = default_mount_point();
        let checks = run_preflight_checks(&mount_point);
        Self {
            source: GuiSource::Repo,
            source_id: "openai-community/gpt2".to_string(),
            revision: "main".to_string(),
            mount_point,
            hf_token: std::env::var("HF_TOKEN").unwrap_or_default(),
            hub_endpoint: "https://huggingface.co".to_string(),
            cache_dir: std::env::temp_dir()
                .join("hf-mount-cache")
                .to_string_lossy()
                .into_owned(),
            read_only: true,
            show_advanced: false,
            checks,
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

        egui::SidePanel::left("left-rail")
            .resizable(false)
            .exact_width(268.0)
            .frame(
                egui::Frame::none()
                    .fill(sidebar_bg())
                    .inner_margin(egui::Margin::symmetric(18.0, 18.0)),
            )
            .show(ctx, |ui| {
                self.draw_sidebar(ui);
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(app_bg())
                    .inner_margin(egui::Margin::symmetric(30.0, 24.0)),
            )
            .show(ctx, |ui| {
                self.draw_main_workspace(ui);
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(mount_point) = &self.active_mount_point {
            let _ = unmount_path(mount_point);
        }
    }
}

impl MountGuiApp {
    fn draw_sidebar(&mut self, ui: &mut egui::Ui) {
        let status = self.status.lock().expect("status mutex poisoned").clone();
        ui.set_height(ui.available_height());

        ui.label(RichText::new("hf-mount").size(21.0).strong().color(text_primary()));
        ui.label(RichText::new("Native NFS mounter").size(13.0).color(text_secondary()));
        ui.add_space(22.0);

        nav_row(ui, "Mount", true);
        nav_row(ui, "Setup", false);
        nav_row(ui, "Activity", false);

        ui.add_space(20.0);
        ui.separator();
        ui.add_space(14.0);

        section_title(ui, "Session");
        ui.add_space(8.0);
        status_chip(ui, &status.state);
        ui.add_space(8.0);
        ui.label(RichText::new(status.headline).strong().color(text_primary()));
        ui.label(RichText::new(status.detail).size(12.0).color(text_secondary()));

        ui.add_space(18.0);
        section_title(ui, "Readiness");
        ui.add_space(8.0);
        checks_summary(ui, &self.checks);
        ui.add_space(10.0);
        for check in &self.checks {
            compact_check_row(ui, check);
            ui.add_space(5.0);
        }

        ui.add_space(18.0);
        section_title(ui, "Activity");
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .id_source("activity-log")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for line in status.log {
                    ui.label(RichText::new(line).monospace().size(11.0).color(text_secondary()));
                }
            });
    }

    fn draw_main_workspace(&mut self, ui: &mut egui::Ui) {
        self.draw_header(ui);
        ui.add_space(44.0);

        let composer_width = ui.available_width().min(860.0);
        let leading_space = ((ui.available_width() - composer_width) / 2.0).max(0.0);
        ui.horizontal_top(|ui| {
            ui.add_space(leading_space);
            ui.vertical(|ui| {
                ui.set_width(composer_width);
                self.draw_composer_panel(ui);
            });
        });
    }

    fn draw_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Mount Hugging Face storage").size(25.0).strong().color(text_primary()));
                ui.label(RichText::new("Choose a repo or bucket, then start the local NFS mount.").color(text_secondary()));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let status = self.status.lock().expect("status mutex poisoned").clone();
                meta_label(ui, platform_label());
                meta_label(ui, "NFS backend");
                status_chip(ui, &status.state);
            });
        });
    }

    fn draw_composer_panel(&mut self, ui: &mut egui::Ui) {
        composer(ui, |ui| {
            section_title(ui, "Mount");
            ui.add_space(14.0);
            self.draw_config_fields(ui);
            ui.add_space(14.0);
            if let Some(blocker) = self.first_blocking_check().cloned() {
                self.draw_blocker_row(ui, &blocker);
                ui.add_space(12.0);
            }
            self.draw_action_buttons(ui);
        });
    }

    fn draw_action_buttons(&mut self, ui: &mut egui::Ui) {
        let running = self.is_mount_thread_running();
        let mounted = {
            let status = self.status.lock().expect("status mutex poisoned");
            status.state == MountState::Mounted
        };
        let blocked = self.first_blocking_check().is_some();
        let full_width = ui.available_width();
        let small_gap = ui.spacing().item_spacing.x;
        let half_width = ((full_width - small_gap) / 2.0).max(96.0);
        let button_height = 38.0;

        let start_label = if running {
            "Mount running"
        } else {
            "Start mount"
        };
        let start = egui::Button::new(RichText::new(start_label).strong().color(egui::Color32::WHITE))
            .fill(if running || blocked {
                input_bg()
            } else {
                primary_button_bg()
            })
            .min_size(egui::vec2(full_width, button_height));
        let start = if running || blocked {
            start
        } else {
            egui::Button::new(RichText::new(start_label).strong().color(primary_button_text()))
                .fill(primary_button_bg())
                .min_size(egui::vec2(full_width, button_height))
        };
        if ui.add_enabled(!running && !blocked, start).clicked() {
            self.start_mount();
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_sized(
                    [half_width, button_height],
                    egui::Button::new(RichText::new("Check setup").color(text_primary())),
                )
                .clicked()
            {
                self.checks = run_preflight_checks(&self.mount_point);
                push_log(&self.status, summarize_checks(&self.checks));
            }

            let stop = egui::Button::new(RichText::new("Stop").strong().color(text_primary()))
                .fill(egui::Color32::from_rgb(77, 42, 42));
            ui.add_enabled_ui(running, |ui| {
                if ui.add_sized([half_width, button_height], stop).clicked() {
                    self.stop_mount();
                }
            });
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let open = egui::Button::new(RichText::new("Open mount").color(text_primary()));
            ui.add_enabled_ui(mounted && self.active_mount_point.is_some(), |ui| {
                if ui.add_sized([ui.available_width(), button_height], open).clicked() {
                    match open_mount_point(self.active_mount_point.as_deref()) {
                        Ok(()) => push_log(&self.status, "Opened mount point"),
                        Err(e) => set_status(&self.status, MountState::Failed, "Could not open mount point", e),
                    }
                }
            });
        });

    }

    fn draw_config_fields(&mut self, ui: &mut egui::Ui) {
        field_row(ui, "Type", |ui| {
            let before = self.source;
            source_selector(ui, &mut self.source);
            if before != self.source && self.source == GuiSource::Repo {
                self.read_only = true;
            }
        });
        field_row(
            ui,
            match self.source {
                GuiSource::Repo => "Repo ID",
                GuiSource::Bucket => "Bucket ID",
            },
            |ui| {
                let hint = match self.source {
                    GuiSource::Repo => "openai-community/gpt2",
                    GuiSource::Bucket => "namespace/bucket",
                };
                text_field(ui, &mut self.source_id, hint, false);
            },
        );
        if self.source == GuiSource::Repo {
            field_row(ui, "Revision", |ui| text_field(ui, &mut self.revision, "main", false));
        }
        field_row(ui, "Mount point", |ui| {
            text_field(ui, &mut self.mount_point, default_mount_hint(), false);
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(mount_point_hint()).small().color(muted_text()));
                #[cfg(windows)]
                if ui.small_button("Use Z:").clicked() {
                    self.mount_point = "Z:".to_string();
                    self.checks = run_preflight_checks(&self.mount_point);
                }
            });
        });
        field_row(ui, "Access", |ui| {
            if self.source == GuiSource::Repo {
                self.read_only = true;
                let mut locked = true;
                ui.add_enabled(false, egui::Checkbox::new(&mut locked, "Read-only"));
                ui.label(RichText::new("Repos are always read-only").small().color(muted_text()));
            } else {
                ui.checkbox(&mut self.read_only, "Read-only");
            }
        });
        field_row(ui, "HF token", |ui| {
            text_field(ui, &mut self.hf_token, "Optional access token", true);
            ui.add_space(3.0);
            ui.label(
                RichText::new("Uses HF_TOKEN automatically when set.")
                    .small()
                    .color(muted_text()),
            );
        });
        ui.add_space(2.0);
        if ui
            .button(if self.show_advanced {
                "Hide advanced"
            } else {
                "Show advanced"
            })
            .clicked()
        {
            self.show_advanced = !self.show_advanced;
        }
        if self.show_advanced {
            ui.add_space(8.0);
            field_row(ui, "Hub endpoint", |ui| {
                text_field(ui, &mut self.hub_endpoint, "https://huggingface.co", false);
            });
            field_row(ui, "Cache dir", |ui| {
                text_field(ui, &mut self.cache_dir, "Cache directory", false);
            });
        }
    }

    fn draw_blocker_row(&mut self, ui: &mut egui::Ui, blocker: &CheckItem) {
        let detail = format!("{}: {}", blocker.label, blocker.detail);
        egui::Frame::none()
            .fill(egui::Color32::from_rgb(58, 46, 39))
            .stroke(egui::Stroke::new(1.0, action_orange()))
            .rounding(8.0)
            .inner_margin(egui::Margin::symmetric(12.0, 10.0))
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.label(RichText::new("Blocked").strong().color(action_orange()));
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&detail).color(text_primary()));
                        if let Some(command) = blocker_command(blocker) {
                            ui.add_space(4.0);
                            ui.label(RichText::new(command).monospace().size(11.0).color(text_secondary()));
                        }
                    });
                });
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    self.draw_primary_blocker_action(ui, blocker);

                    if let Some(command) = blocker_command(blocker) {
                        if ui.button("Copy command").clicked() {
                            ui.output_mut(|output| output.copied_text = command.to_string());
                            push_log(&self.status, "Copied setup command");
                        }
                    }

                    if blocker.label == "Mount point" && ui.button("Use Z:").clicked() {
                        self.mount_point = "Z:".to_string();
                        self.checks = run_preflight_checks(&self.mount_point);
                        push_log(&self.status, summarize_checks(&self.checks));
                    }

                    if ui.button("Recheck").clicked() {
                        self.checks = run_preflight_checks(&self.mount_point);
                        push_log(&self.status, summarize_checks(&self.checks));
                    }
                });
            });
    }

    fn draw_primary_blocker_action(&mut self, ui: &mut egui::Ui, blocker: &CheckItem) {
        #[cfg(windows)]
        {
            if blocker.label == "Client for NFS" {
                let enable = egui::Button::new(RichText::new("Enable NFS").strong().color(egui::Color32::WHITE))
                    .fill(action_orange());
                if ui.add(enable).clicked() {
                    match enable_windows_nfs_client() {
                        Ok(()) => set_status(
                            &self.status,
                            MountState::Stopped,
                            "NFS enable requested",
                            "Approve the UAC prompt. Reboot if Windows asks, then press Recheck.",
                        ),
                        Err(e) => set_status(&self.status, MountState::Failed, "Could not enable NFS", e),
                    }
                }
                return;
            }

            if blocker.label == "Administrator" {
                let restart =
                    egui::Button::new(RichText::new("Restart as admin").strong().color(text_primary()));
                if ui.add(restart).clicked() {
                    match restart_as_administrator() {
                        Ok(()) => set_status(
                            &self.status,
                            MountState::Stopped,
                            "Elevation requested",
                            "Approve the Windows UAC prompt, then use the elevated window.",
                        ),
                        Err(e) => set_status(&self.status, MountState::Failed, "Could not relaunch as admin", e),
                    }
                }
                return;
            }
        }

        if blocker.label == "Mount point" {
            return;
        }

        ui.label(RichText::new("Fix this blocker, then recheck.").small().color(text_secondary()));
    }

    fn start_mount(&mut self) {
        let checks = run_preflight_checks(&self.mount_point);
        let failure = checks.iter().find(|check| check.level == CheckLevel::Fail).cloned();
        self.checks = checks;
        if let Some(failure) = failure {
            set_status(
                &self.status,
                MountState::Failed,
                format!("Setup check failed: {}", failure.label),
                failure.detail,
            );
            return;
        }

        push_log(&self.status, summarize_checks(&self.checks));
        let source = match self.mount_source() {
            Ok(source) => source,
            Err(e) => {
                set_status(&self.status, MountState::Failed, "Invalid source", e);
                return;
            }
        };
        let options = match self.mount_options() {
            Ok(options) => options,
            Err(e) => {
                set_status(&self.status, MountState::Failed, "Invalid mount options", e);
                return;
            }
        };
        let mount_point = source.mount_point().to_path_buf();
        let mount_label = mount_point.display().to_string();
        let shared_status = self.status.clone();

        set_status(
            &shared_status,
            MountState::Mounting,
            "Preparing mount",
            format!("Target: {mount_label}"),
        );
        self.active_mount_point = Some(mount_point);
        self.mount_thread = Some(thread::spawn(move || run_mount(source, options, shared_status)));
    }

    fn stop_mount(&mut self) {
        let Some(mount_point) = self.active_mount_point.clone() else {
            set_status(
                &self.status,
                MountState::Failed,
                "No active mount",
                "There is no recorded mount point to unmount.",
            );
            return;
        };

        set_status(
            &self.status,
            MountState::Stopping,
            "Unmount requested",
            format!("Target: {}", mount_point.display()),
        );

        if let Err(e) = unmount_path(&mount_point) {
            set_status(&self.status, MountState::Failed, "Unmount failed", e);
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
            set_status(
                &self.status,
                MountState::Failed,
                "Mount thread panicked",
                "The backend thread exited unexpectedly.",
            );
        }
        self.active_mount_point = None;

        let current = self.status.lock().expect("status mutex poisoned").state.clone();
        if !matches!(current, MountState::Failed | MountState::Stopped) {
            set_status(
                &self.status,
                MountState::Stopped,
                "Unmounted",
                "The mount process has stopped.",
            );
        }
    }

    fn is_mount_thread_running(&self) -> bool {
        self.mount_thread.as_ref().is_some_and(|handle| !handle.is_finished())
    }

    fn first_blocking_check(&self) -> Option<&CheckItem> {
        self.checks
            .iter()
            .find(|check| check.label == "Client for NFS" && check.level == CheckLevel::Fail)
            .or_else(|| self.checks.iter().find(|check| check.level == CheckLevel::Fail))
    }

    fn mount_source(&self) -> Result<Source, String> {
        let source_id = self.source_id.trim();
        if source_id.is_empty() {
            return Err("Source ID is required.".to_string());
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
            read_only: self.source == GuiSource::Repo || self.read_only,
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

fn composer(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(panel_bg())
        .stroke(egui::Stroke::new(1.0, border()))
        .rounding(10.0)
        .inner_margin(egui::Margin::symmetric(18.0, 16.0))
        .show(ui, add_contents);
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(14.0).strong().color(text_primary()));
}

fn nav_row(ui: &mut egui::Ui, label: &str, selected: bool) {
    let fill = if selected { elevated_bg() } else { egui::Color32::TRANSPARENT };
    let text = if selected { text_primary() } else { text_secondary() };
    egui::Frame::none()
        .fill(fill)
        .rounding(8.0)
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.label(RichText::new(label).strong().color(text));
        });
}

fn field_row(ui: &mut egui::Ui, label: &str, add_field: impl FnOnce(&mut egui::Ui)) {
    if ui.available_width() < 430.0 {
        ui.vertical(|ui| {
            ui.label(RichText::new(label).color(text_secondary()));
            add_field(ui);
        });
    } else {
        ui.horizontal(|ui| {
            ui.set_min_height(42.0);
            ui.add_sized(
                [120.0, 28.0],
                egui::Label::new(RichText::new(label).color(text_secondary())),
            );
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                add_field(ui);
            });
        });
    }
}

fn source_selector(ui: &mut egui::Ui, source: &mut GuiSource) {
    egui::Frame::none()
        .fill(input_bg())
        .stroke(egui::Stroke::new(1.0, border()))
        .rounding(8.0)
        .inner_margin(egui::Margin::same(4.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let spacing = ui.spacing().item_spacing.x;
                let width = ((ui.available_width() - spacing) / 2.0).max(100.0);
                if source_button(ui, "Repo", *source == GuiSource::Repo, width).clicked() {
                    *source = GuiSource::Repo;
                }
                if source_button(ui, "Bucket", *source == GuiSource::Bucket, width).clicked() {
                    *source = GuiSource::Bucket;
                }
            });
        });
}

fn source_button(ui: &mut egui::Ui, label: &str, selected: bool, width: f32) -> egui::Response {
    let text_color = if selected {
        text_primary()
    } else {
        text_secondary()
    };
    let fill = if selected { egui::Color32::from_rgb(70, 70, 66) } else { egui::Color32::TRANSPARENT };
    ui.add_sized(
        [width, 32.0],
        egui::Button::new(RichText::new(label).strong().color(text_color))
            .fill(fill)
            .stroke(egui::Stroke::NONE),
    )
}

fn text_field(ui: &mut egui::Ui, value: &mut String, hint: &str, password: bool) {
    ui.add_sized(
        [ui.available_width(), 36.0],
        TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .hint_text(hint)
            .password(password),
    );
}

fn meta_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(text_secondary()));
}

fn status_chip(ui: &mut egui::Ui, state: &MountState) {
    let (label, fg, bg) = match state {
        MountState::Ready => ("Ready", text_secondary(), elevated_bg()),
        MountState::Mounting => ("Mounting", action_orange(), egui::Color32::from_rgb(58, 46, 39)),
        MountState::Mounted => ("Mounted", success_fg(), egui::Color32::from_rgb(34, 55, 42)),
        MountState::Stopping => ("Stopping", action_orange(), egui::Color32::from_rgb(58, 46, 39)),
        MountState::Stopped => ("Stopped", text_secondary(), elevated_bg()),
        MountState::Failed => ("Error", error_fg(), egui::Color32::from_rgb(63, 39, 39)),
    };
    chip(ui, label, fg, bg);
}

fn checks_summary(ui: &mut egui::Ui, checks: &[CheckItem]) {
    let failures = checks.iter().filter(|check| check.level == CheckLevel::Fail).count();
    let warnings = checks.iter().filter(|check| check.level == CheckLevel::Warn).count();

    let (label, color) = if checks.is_empty() {
        ("Not checked", text_secondary())
    } else if failures > 0 {
        ("Action needed", error_fg())
    } else if warnings > 0 {
        ("Usable with warnings", warning_fg())
    } else {
        ("Ready to mount", success_fg())
    };
    ui.horizontal(|ui| {
        chip(ui, label, color, elevated_bg());
        if !checks.is_empty() {
            ui.label(
                RichText::new(format!("{failures} blocking / {warnings} warning"))
                    .small()
                    .color(muted_text()),
            );
        }
    });
}

fn chip(ui: &mut egui::Ui, text: &str, fg: egui::Color32, bg: egui::Color32) {
    egui::Frame::none()
        .fill(bg)
        .rounding(egui::Rounding::same(7.0))
        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(fg));
        });
}

fn compact_check_row(ui: &mut egui::Ui, check: &CheckItem) {
    let (mark, color) = match check.level {
        CheckLevel::Pass => ("OK", success_fg()),
        CheckLevel::Warn => ("--", warning_fg()),
        CheckLevel::Fail => ("FIX", error_fg()),
    };
    ui.horizontal_top(|ui| {
        ui.add_sized(
            [34.0, 18.0],
            egui::Label::new(RichText::new(mark).size(11.0).strong().color(color)),
        );
        ui.vertical(|ui| {
            ui.label(RichText::new(&check.label).size(13.0).strong().color(text_primary()));
            ui.label(RichText::new(&check.detail).size(11.0).color(text_secondary()));
        });
    });
}

fn check_level_label(level: CheckLevel) -> &'static str {
    match level {
        CheckLevel::Pass => "OK",
        CheckLevel::Warn => "WARN",
        CheckLevel::Fail => "FAIL",
    }
}

fn run_mount(source: Source, options: MountOptions, shared_status: SharedMountStatus) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let setup = hf_mount::setup::build(source, options, true);
        let virtual_fs = setup.virtual_fs.clone();
        let mount_point = setup.mount_point.clone();
        let metadata_ttl_ms = setup.metadata_ttl_ms;
        let read_only = setup.read_only;
        let status_for_events = shared_status.clone();
        setup.runtime.block_on(hf_mount::nfs::mount_nfs_with_callback(
            virtual_fs,
            &mount_point,
            metadata_ttl_ms,
            read_only,
            None,
            move |event| handle_mount_event(&status_for_events, event),
        ))
    }));

    match result {
        Ok(Ok(())) => set_status(
            &shared_status,
            MountState::Stopped,
            "Unmounted",
            "The mount stopped cleanly.",
        ),
        Ok(Err(e)) => set_status(&shared_status, MountState::Failed, "NFS mount failed", e.to_string()),
        Err(payload) => set_status(
            &shared_status,
            MountState::Failed,
            "Mount setup failed",
            panic_message(payload),
        ),
    }
}

fn handle_mount_event(status: &SharedMountStatus, event: NfsMountEvent) {
    match event {
        NfsMountEvent::ServerListening { port } => set_status(
            status,
            MountState::Mounting,
            "Local NFS server is listening",
            format!("127.0.0.1:{port}"),
        ),
        NfsMountEvent::MountCommand { command } => push_log(status, format!("Running {command}")),
        NfsMountEvent::Mounted { mount_point } => set_status(
            status,
            MountState::Mounted,
            "Mounted",
            format!("Mounted at {mount_point}"),
        ),
        NfsMountEvent::ShuttingDown { reason } => set_status(status, MountState::Stopping, "Shutting down", reason),
    }
}

fn set_status(status: &SharedMountStatus, state: MountState, headline: impl Into<String>, detail: impl Into<String>) {
    let headline = headline.into();
    let detail = detail.into();
    let mut status = status.lock().expect("status mutex poisoned");
    status.state = state;
    status.headline = headline.clone();
    status.detail = detail.clone();
    push_log_locked(&mut status, format!("{headline}: {detail}"));
}

fn push_log(status: &SharedMountStatus, message: impl Into<String>) {
    let mut status = status.lock().expect("status mutex poisoned");
    push_log_locked(&mut status, message.into());
}

fn push_log_locked(status: &mut SharedStatus, message: String) {
    status.log.push_back(message);
    while status.log.len() > MAX_LOG_LINES {
        status.log.pop_front();
    }
}

fn run_preflight_checks(mount_point: &str) -> Vec<CheckItem> {
    #[cfg(windows)]
    {
        return windows_preflight_checks(mount_point);
    }
    #[cfg(target_os = "macos")]
    {
        return macos_preflight_checks(mount_point);
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        vec![CheckItem {
            level: if mount_point.trim().is_empty() {
                CheckLevel::Fail
            } else {
                CheckLevel::Pass
            },
            label: "Mount point".to_string(),
            detail: "Path is set.".to_string(),
        }]
    }
}

#[cfg(windows)]
fn windows_preflight_checks(mount_point: &str) -> Vec<CheckItem> {
    let mut checks = Vec::new();
    let elevated = windows_is_elevated();
    checks.push(if elevated {
        CheckItem {
            level: CheckLevel::Pass,
            label: "Administrator".to_string(),
            detail: "The GUI is elevated.".to_string(),
        }
    } else {
        CheckItem {
            level: CheckLevel::Fail,
            label: "Administrator".to_string(),
            detail: "Restart as Administrator so hf-mount can bind the local NFS portmapper.".to_string(),
        }
    });

    let mount_exe = windows_system32_exe("mount.exe");
    let umount_exe = windows_system32_exe("umount.exe");
    checks.push(if mount_exe.exists() && umount_exe.exists() {
        CheckItem {
            level: CheckLevel::Pass,
            label: "Client for NFS".to_string(),
            detail: "mount.exe and umount.exe are available.".to_string(),
        }
    } else {
        CheckItem {
            level: CheckLevel::Fail,
            label: "Client for NFS".to_string(),
            detail: "Enable Microsoft's Client for NFS optional feature and reboot if Windows asks.".to_string(),
        }
    });

    checks.push(if elevated {
        windows_portmapper_check()
    } else {
        CheckItem {
            level: CheckLevel::Warn,
            label: "Portmapper".to_string(),
            detail: "Port 111 is checked after elevation.".to_string(),
        }
    });

    checks.push(validate_windows_mount_point(mount_point));
    checks
}

#[cfg(windows)]
fn windows_is_elevated() -> bool {
    let mut command = Command::new(windows_system32_exe("fltmc.exe"));
    command.creation_flags(CREATE_NO_WINDOW);
    command.stdout(Stdio::null()).stderr(Stdio::null());
    command.status().map(|status| status.success()).unwrap_or(false)
}

#[cfg(windows)]
fn validate_windows_mount_point(mount_point: &str) -> CheckItem {
    let trimmed = mount_point.trim();
    if trimmed.is_empty() {
        return CheckItem {
            level: CheckLevel::Fail,
            label: "Mount point".to_string(),
            detail: "Choose a drive letter like Z: or an empty NTFS directory.".to_string(),
        };
    }

    if let Some(drive) = windows_drive_letter(trimmed) {
        let probe = format!("{drive}:\\");
        return if Path::new(&probe).exists() {
            CheckItem {
                level: CheckLevel::Fail,
                label: "Mount point".to_string(),
                detail: format!("{drive}: already exists. Pick an unused drive letter such as Y: or X:."),
            }
        } else {
            CheckItem {
                level: CheckLevel::Pass,
                label: "Mount point".to_string(),
                detail: format!("{drive}: is a drive-letter target."),
            }
        };
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        if path.exists() && !path.is_dir() {
            return CheckItem {
                level: CheckLevel::Fail,
                label: "Mount point".to_string(),
                detail: "The target exists but is not a directory.".to_string(),
            };
        }
        let detail = if path.exists() {
            "Directory target is absolute. A drive letter such as Z: is still the most reliable Windows target."
        } else {
            "Directory target is absolute and will be created if the Windows NFS client accepts it. A drive letter is more reliable."
        };
        CheckItem {
            level: CheckLevel::Warn,
            label: "Mount point".to_string(),
            detail: detail.to_string(),
        }
    } else {
        CheckItem {
            level: CheckLevel::Fail,
            label: "Mount point".to_string(),
            detail: "Use a drive letter or an absolute directory path.".to_string(),
        }
    }
}

#[cfg(windows)]
fn windows_portmapper_check() -> CheckItem {
    match (
        UdpSocket::bind(("127.0.0.1", 111)),
        TcpListener::bind(("127.0.0.1", 111)),
    ) {
        (Ok(udp), Ok(tcp)) => {
            drop(udp);
            drop(tcp);
            CheckItem {
                level: CheckLevel::Pass,
                label: "Portmapper".to_string(),
                detail: "TCP and UDP port 111 are available for the local NFS portmapper.".to_string(),
            }
        }
        (udp_result, tcp_result) => CheckItem {
            level: CheckLevel::Fail,
            label: "Portmapper".to_string(),
            detail: format!(
                "Port 111 is not available: UDP={} TCP={}. Close other NFS/portmap services or another hf-mount instance.",
                bind_result_label(&udp_result),
                bind_result_label(&tcp_result),
            ),
        },
    }
}

#[cfg(windows)]
fn bind_result_label<T>(result: &std::io::Result<T>) -> String {
    match result {
        Ok(_) => "ok".to_string(),
        Err(e) => e.to_string(),
    }
}

#[cfg(target_os = "macos")]
fn macos_preflight_checks(mount_point: &str) -> Vec<CheckItem> {
    let mount_cmd = Path::new("/sbin/mount_nfs");
    let mount_cmd_exists = mount_cmd.exists();
    let mount_path_absolute = Path::new(mount_point.trim()).is_absolute();
    vec![
        CheckItem {
            level: if mount_cmd_exists {
                CheckLevel::Pass
            } else {
                CheckLevel::Fail
            },
            label: "mount_nfs".to_string(),
            detail: if mount_cmd_exists {
                "/sbin/mount_nfs is available.".to_string()
            } else {
                "/sbin/mount_nfs was not found.".to_string()
            },
        },
        CheckItem {
            level: if mount_path_absolute {
                CheckLevel::Pass
            } else {
                CheckLevel::Fail
            },
            label: "Mount point".to_string(),
            detail: if mount_path_absolute {
                "Mount point is an absolute path.".to_string()
            } else {
                "Use an absolute local directory path.".to_string()
            },
        },
    ]
}

fn summarize_checks(checks: &[CheckItem]) -> String {
    if checks.iter().any(|check| check.level == CheckLevel::Fail) {
        "Setup checks found a blocking issue".to_string()
    } else if checks.iter().any(|check| check.level == CheckLevel::Warn) {
        "Setup checks passed with warnings".to_string()
    } else {
        "Setup checks passed".to_string()
    }
}

fn blocker_command(check: &CheckItem) -> Option<&'static str> {
    #[cfg(windows)]
    {
        match check.label.as_str() {
            "Client for NFS" => Some(windows_enable_nfs_command()),
            "Administrator" => Some("Start-Process hf-mount-gui.exe -Verb RunAs"),
            "Portmapper" => Some("Close other NFS/portmap services or another hf-mount instance, then recheck."),
            "Mount point" => Some("Use an unused drive letter such as Z:, Y:, or X:."),
            _ => None,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = check;
        None
    }
}

#[cfg(windows)]
fn windows_enable_nfs_command() -> &'static str {
    "Enable-WindowsOptionalFeature -Online -FeatureName ServicesForNFS-ClientOnly,ClientForNFS-Infrastructure -All"
}

#[cfg(windows)]
fn enable_windows_nfs_client() -> Result<(), String> {
    let powershell = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let elevated_args = format!(
        "-NoProfile -ExecutionPolicy Bypass -Command \"{}\"",
        windows_enable_nfs_command()
    );

    let status = Command::new(&powershell)
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "Start-Process -FilePath $args[0] -Verb RunAs -ArgumentList $args[1]",
        ])
        .arg(&powershell)
        .arg(elevated_args)
        .status()
        .map_err(|e| format!("Failed to launch the UAC prompt: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("PowerShell exited with {status}"))
    }
}

fn unmount_path(mount_point: &Path) -> Result<(), String> {
    let mount_point = mount_point
        .to_str()
        .ok_or_else(|| "Mount point is not valid UTF-8".to_string())?;
    let target = unmount_target(mount_point);

    let output = unmount_command(&target)
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
    let mut command = Command::new(windows_system32_exe("umount.exe"));
    command.creation_flags(CREATE_NO_WINDOW);
    command.args(["-f", mount_point]);
    command
}

#[cfg(target_os = "macos")]
fn unmount_command(mount_point: &str) -> Command {
    let mut command = Command::new("/sbin/umount");
    command.arg(mount_point);
    command
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn unmount_command(mount_point: &str) -> Command {
    let mut command = Command::new("umount");
    command.arg(mount_point);
    command
}

fn unmount_target(mount_point: &str) -> String {
    #[cfg(windows)]
    {
        if let Some(drive) = windows_drive_letter(mount_point) {
            return format!("{drive}:");
        }
    }
    mount_point.to_string()
}

fn open_mount_point(mount_point: Option<&Path>) -> Result<(), String> {
    let mount_point = mount_point.ok_or_else(|| "No active mount point is recorded.".to_string())?;
    let target = open_target(mount_point)?;

    let mut command = open_command(&target);
    command
        .spawn()
        .map_err(|e| format!("Failed to open mount point: {e}"))?;
    Ok(())
}

fn open_target(mount_point: &Path) -> Result<String, String> {
    let text = mount_point
        .to_str()
        .ok_or_else(|| "Mount point is not valid UTF-8".to_string())?;
    #[cfg(windows)]
    {
        if let Some(drive) = windows_drive_letter(text) {
            return Ok(format!("{drive}:\\"));
        }
    }
    Ok(text.to_string())
}

#[cfg(windows)]
fn open_command(target: &str) -> Command {
    let mut command = Command::new("explorer.exe");
    command.creation_flags(CREATE_NO_WINDOW);
    command.arg(target);
    command
}

#[cfg(target_os = "macos")]
fn open_command(target: &str) -> Command {
    let mut command = Command::new("/usr/bin/open");
    command.arg(target);
    command
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn open_command(target: &str) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(target);
    command
}

fn parse_path(text: &str, label: &str) -> Result<PathBuf, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} is required."));
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

fn default_mount_hint() -> &'static str {
    #[cfg(windows)]
    {
        "Z:"
    }
    #[cfg(not(windows))]
    {
        "/tmp/hf-mount"
    }
}

fn mount_point_hint() -> &'static str {
    #[cfg(windows)]
    {
        "Use an unused drive letter. Directory targets are less reliable."
    }
    #[cfg(target_os = "macos")]
    {
        "Use an absolute folder path."
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        "Use an absolute folder path."
    }
}

#[cfg(windows)]
fn restart_as_administrator() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("Could not locate current executable: {e}"))?;
    let powershell = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");

    let status = Command::new(powershell)
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "Start-Process -FilePath $args[0] -Verb RunAs",
        ])
        .arg(exe)
        .status()
        .map_err(|e| format!("Failed to launch the UAC prompt: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("PowerShell exited with {status}"))
    }
}

#[cfg(windows)]
fn windows_system32_exe(name: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join(name)
}

#[cfg(windows)]
fn windows_drive_letter(path: &str) -> Option<char> {
    let mut chars = path.chars();
    let drive = chars.next()?;
    if !drive.is_ascii_alphabetic() || chars.next() != Some(':') {
        return None;
    }
    match (chars.next(), chars.next()) {
        (None, None) => Some(drive),
        (Some('\\' | '/'), None) => Some(drive),
        _ => None,
    }
}
