//! Native GUI for mounting Hugging Face repos and buckets through the NFS
//! backend. The same executable doubles as the detached background worker
//! (`--background-worker`) and the headless setup checker (`--check-setup`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod activity_tab;
mod app;
mod autostart;
mod mount_tab;
mod platform;
mod preflight;
mod profile;
mod setup_tab;
mod theme;
mod util;
mod widgets;
mod worker;

use eframe::egui;

use crate::app::MountGuiApp;
use crate::preflight::{CheckLevel, check_level_label, run_preflight_checks};
use crate::worker::BACKGROUND_WORKER_ARG;

fn load_icon() -> egui::IconData {
    let icon_bytes = include_bytes!("../../../assets/icon.rgba");
    egui::IconData {
        rgba: icon_bytes.to_vec(),
        width: 64,
        height: 64,
    }
}

fn main() {
    if handle_cli_command() {
        return;
    }

    util::init_backend_once();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([780.0, 640.0])
            .with_min_inner_size([620.0, 480.0])
            .with_icon(load_icon()),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "hf-mount",
        native_options,
        Box::new(|cc| {
            theme::apply_theme(&cc.egui_ctx);
            Ok(Box::new(MountGuiApp::new(cc)))
        }),
    ) {
        eprintln!("failed to start hf-mount GUI: {e}");
        std::process::exit(1);
    }
}

/// Handle CLI-style invocations. Returns `true` when the invocation was a CLI
/// command and the GUI should not start.
fn handle_cli_command() -> bool {
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
            let mount_point = args.next().unwrap_or_else(platform::default_mount_point);
            let checks = run_preflight_checks(&mount_point);
            for check in &checks {
                println!("[{}] {}: {}", check_level_label(check.level), check.label, check.detail);
            }
            if checks.iter().any(|check| check.level == CheckLevel::Fail) {
                std::process::exit(1);
            }
            true
        }
        BACKGROUND_WORKER_ARG => {
            if let Err(e) = worker::run_background_worker() {
                eprintln!("background worker failed: {e}");
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
           hf-mount-gui --check-setup [MOUNT_POINT]\n\
           hf-mount-gui --background-worker\n\n\
         Windows requires Client for NFS and an Administrator session.",
        version = env!("CARGO_PKG_VERSION")
    );
}
