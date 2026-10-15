// No console window for the desktop app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    hf_mount_desktop::run();
}
