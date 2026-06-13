//! Activity tab: the session log, plus background-worker log location.

use eframe::egui::{self, RichText};

use crate::app::{MountGuiApp, push_log};
use crate::theme::*;
use crate::widgets::secondary_button;
use crate::worker::worker_log_path;

impl MountGuiApp {
    pub fn draw_activity_tab(&mut self, ui: &mut egui::Ui) {
        let status = self.current_status();

        ui.horizontal(|ui| {
            ui.label(RichText::new("Session log").size(14.0).strong().color(text_primary()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if secondary_button(ui, "Copy log", !status.log.is_empty(), 90.0).clicked() {
                    let text = status.log.iter().cloned().collect::<Vec<_>>().join("\n");
                    ui.ctx().copy_text(text);
                    push_log(&self.status, "Copied session log");
                }
            });
        });
        ui.add_space(6.0);

        let footer_height = 30.0;
        let log_height = (ui.available_height() - footer_height).max(80.0);
        egui::Frame::none()
            .fill(panel_bg())
            .stroke(egui::Stroke::new(1.0, border()))
            .rounding(8.0)
            .inner_margin(egui::Margin::symmetric(10.0, 8.0))
            .show(ui, |ui| {
                ui.set_height(log_height);
                egui::ScrollArea::vertical()
                    .id_salt("activity-log")
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for line in &status.log {
                            ui.label(RichText::new(line).monospace().size(11.5).color(text_secondary()));
                        }
                    });
            });

        ui.add_space(6.0);
        if let Ok(path) = worker_log_path() {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Background worker log: {}", path.display()))
                        .size(11.0)
                        .color(muted_text()),
                );
                if ui
                    .add(egui::Button::new(RichText::new("Copy path").size(11.0).color(text_secondary())).frame(false))
                    .clicked()
                {
                    ui.ctx().copy_text(path.display().to_string());
                    push_log(&self.status, "Copied worker log path");
                }
            });
        }
    }
}
