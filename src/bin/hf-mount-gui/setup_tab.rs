//! Setup tab: full readiness check list with per-check fix actions.

use eframe::egui::{self, RichText};

use crate::app::{MountGuiApp, push_log};
use crate::platform;
use crate::preflight::{CheckLevel, blocker_command};
use crate::theme::*;
use crate::widgets::{chip, secondary_button};

impl MountGuiApp {
    pub fn draw_setup_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Environment checks")
                    .size(14.0)
                    .strong()
                    .color(text_primary()),
            );
            self.draw_checks_summary(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if secondary_button(ui, "Run checks", true, 100.0).clicked() {
                    self.refresh_checks();
                }
            });
        });
        ui.add_space(8.0);

        egui::ScrollArea::vertical()
            .id_salt("setup-checks")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Blocker actions borrow `self` mutably while we iterate the
                // checks, so temporarily move the list out instead of cloning
                // it every frame. Actions may replace `self.checks` (via
                // refresh); keep whichever list is newer.
                let checks = std::mem::take(&mut self.checks);
                for check in &checks {
                    egui::Frame::new()
                        .fill(panel_bg())
                        .stroke(egui::Stroke::new(1.0_f32, border()))
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal_top(|ui| {
                                let (mark, color) = match check.level {
                                    CheckLevel::Pass => ("OK", success_fg()),
                                    CheckLevel::Warn => ("WARN", warning_fg()),
                                    CheckLevel::Fail => ("FIX", error_fg()),
                                };
                                ui.allocate_ui_with_layout(
                                    egui::vec2(44.0, 18.0),
                                    egui::Layout::left_to_right(egui::Align::Min),
                                    |ui| {
                                        ui.label(RichText::new(mark).size(11.0).strong().color(color));
                                    },
                                );
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&check.label).size(13.0).strong().color(text_primary()));
                                    ui.label(RichText::new(&check.detail).size(12.0).color(text_secondary()));
                                    if check.level != CheckLevel::Pass {
                                        if let Some(command) = blocker_command(check) {
                                            ui.add_space(4.0);
                                            ui.label(RichText::new(command).monospace().size(11.0).color(muted_text()));
                                        }
                                        ui.add_space(6.0);
                                        ui.horizontal_wrapped(|ui| {
                                            if check.level == CheckLevel::Fail {
                                                self.draw_blocker_action(ui, check);
                                            }
                                            if let Some(command) = blocker_command(check)
                                                && ui.button("Copy command").clicked()
                                            {
                                                ui.ctx().copy_text(command.to_string());
                                                push_log(&self.status, "Copied setup command");
                                            }
                                        });
                                    }
                                });
                            });
                        });
                    ui.add_space(8.0);
                }
                if self.checks.is_empty() {
                    self.checks = checks;
                }

                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "hf-mount-gui v{} · {} · NFS backend",
                        env!("CARGO_PKG_VERSION"),
                        platform::platform_label()
                    ))
                    .size(11.0)
                    .color(muted_text()),
                );
                ui.label(
                    RichText::new("CLI: hf-mount-gui --check-setup [MOUNT_POINT] runs these checks headlessly.")
                        .size(11.0)
                        .color(muted_text()),
                );
            });
    }

    fn draw_checks_summary(&self, ui: &mut egui::Ui) {
        let failures = self
            .checks
            .iter()
            .filter(|check| check.level == CheckLevel::Fail)
            .count();
        let warnings = self
            .checks
            .iter()
            .filter(|check| check.level == CheckLevel::Warn)
            .count();

        let (label, color, bg) = if self.checks.is_empty() {
            ("Not checked", text_secondary(), elevated_bg())
        } else if failures > 0 {
            ("Action needed", error_fg(), error_chip_bg())
        } else if warnings > 0 {
            ("Usable with warnings", warning_fg(), warning_chip_bg())
        } else {
            ("Ready to mount", success_fg(), success_chip_bg())
        };
        chip(ui, label, color, bg);
        if failures > 0 || warnings > 0 {
            ui.label(
                RichText::new(format!("{failures} blocking · {warnings} warning"))
                    .size(11.0)
                    .color(muted_text()),
            );
        }
    }
}
