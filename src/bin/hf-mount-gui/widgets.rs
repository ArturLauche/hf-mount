//! Reusable UI primitives: tab bar, chips, labeled field rows, buttons.

use eframe::egui::{self, RichText};

use crate::app::MountState;
use crate::theme::*;

/// Underline-style tab button. Returns `true` when clicked.
pub fn tab_button(ui: &mut egui::Ui, label: &str, active: bool) -> bool {
    let color = if active { text_primary() } else { text_secondary() };
    let text = RichText::new(label).size(14.0).strong().color(color);
    let response = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

    let underline = if active {
        Some(accent())
    } else if response.hovered() {
        Some(border())
    } else {
        None
    };
    if let Some(color) = underline {
        let rect = response.rect;
        ui.painter()
            .hline(rect.x_range(), rect.bottom() + 6.0, egui::Stroke::new(2.0, color));
    }
    response.clicked()
}

pub fn chip(ui: &mut egui::Ui, text: &str, fg: egui::Color32, bg: egui::Color32) {
    egui::Frame::none()
        .fill(bg)
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(fg));
        });
}

pub fn status_chip(ui: &mut egui::Ui, state: &MountState) {
    let (label, fg, bg) = match state {
        MountState::Ready => ("Ready", text_secondary(), elevated_bg()),
        MountState::Mounting => ("Mounting", warning_fg(), warning_chip_bg()),
        MountState::Mounted => ("Mounted", success_fg(), success_chip_bg()),
        MountState::Stopping => ("Stopping", warning_fg(), warning_chip_bg()),
        MountState::Stopped => ("Stopped", text_secondary(), elevated_bg()),
        MountState::Failed => ("Error", error_fg(), error_chip_bg()),
    };
    chip(ui, label, fg, bg);
}

/// A form row: fixed-width label column on the left, control on the right.
/// Collapses to stacked label/control when the panel is narrow.
pub fn field_row(ui: &mut egui::Ui, label: &str, add_field: impl FnOnce(&mut egui::Ui)) {
    if ui.available_width() < 420.0 {
        ui.vertical(|ui| {
            ui.label(RichText::new(label).size(13.0).color(text_secondary()));
            add_field(ui);
        });
        ui.add_space(2.0);
    } else {
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(120.0, 30.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(RichText::new(label).size(13.0).color(text_secondary()));
                },
            );
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                add_field(ui);
            });
        });
        ui.add_space(2.0);
    }
}

pub fn text_field(ui: &mut egui::Ui, value: &mut String, hint: &str, password: bool) -> egui::Response {
    ui.add_sized(
        [ui.available_width(), 30.0],
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .hint_text(hint)
            .password(password),
    )
}

pub fn field_hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(muted_text()));
}

pub fn field_error(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(error_fg()));
}

pub fn primary_button(ui: &mut egui::Ui, label: &str, enabled: bool, width: f32) -> egui::Response {
    let (fill, fg) = if enabled {
        (primary_button_bg(), primary_button_text())
    } else {
        (input_bg(), muted_text())
    };
    let button = egui::Button::new(RichText::new(label).strong().color(fg))
        .fill(fill)
        .min_size(egui::vec2(width, 32.0));
    ui.add_enabled(enabled, button)
}

pub fn danger_button(ui: &mut egui::Ui, label: &str, enabled: bool, width: f32) -> egui::Response {
    let fg = if enabled { text_primary() } else { muted_text() };
    let button = egui::Button::new(RichText::new(label).strong().color(fg))
        .fill(danger_button_bg())
        .min_size(egui::vec2(width, 32.0));
    ui.add_enabled(enabled, button)
}

pub fn secondary_button(ui: &mut egui::Ui, label: &str, enabled: bool, width: f32) -> egui::Response {
    let fg = if enabled { text_primary() } else { muted_text() };
    let button = egui::Button::new(RichText::new(label).color(fg)).min_size(egui::vec2(width, 32.0));
    ui.add_enabled(enabled, button)
}

/// Two-option segmented control. Returns `true` when the selection changed.
pub fn segmented_pair<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: [(T, &str); 2]) -> bool {
    let mut changed = false;
    egui::Frame::none()
        .fill(input_bg())
        .stroke(egui::Stroke::new(1.0, border()))
        .rounding(8.0)
        .inner_margin(egui::Margin::same(3.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let spacing = ui.spacing().item_spacing.x;
                let width = ((ui.available_width() - spacing) / 2.0).max(90.0);
                for (option, label) in options {
                    let selected = *value == option;
                    let fg = if selected { text_primary() } else { text_secondary() };
                    let fill = if selected {
                        egui::Color32::from_rgb(58, 58, 58)
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    let button = egui::Button::new(RichText::new(label).strong().color(fg))
                        .fill(fill)
                        .stroke(egui::Stroke::NONE)
                        .min_size(egui::vec2(width, 26.0));
                    if ui.add(button).clicked() && !selected {
                        *value = option;
                        changed = true;
                    }
                }
            });
        });
    changed
}
