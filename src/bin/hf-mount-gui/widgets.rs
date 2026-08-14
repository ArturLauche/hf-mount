//! Reusable UI primitives: sidebar navigation, cards, chips, labeled field
//! rows, buttons, segmented controls.

use eframe::egui::{self, RichText};

use crate::app::MountState;
use crate::theme::*;

/// Sidebar navigation entry with an active accent bar. Returns `true` on click.
pub fn nav_item(ui: &mut egui::Ui, label: &str, active: bool) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

    if active {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(6), elevated_bg());
        let bar = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
        ui.painter().rect_filled(bar, egui::CornerRadius::same(2), accent());
    } else if response.hovered() {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(6), panel_bg());
    }

    let color = if active { text_primary() } else { text_secondary() };
    ui.painter().text(
        egui::pos2(rect.min.x + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.5),
        color,
    );
    response.clicked()
}

/// Card container: a bordered panel with padding, used to group form sections
/// and content blocks.
pub fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(panel_bg())
        .stroke(egui::Stroke::new(1.0_f32, border()))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

/// Small uppercase section title inside a card.
pub fn card_title(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title.to_uppercase())
            .size(10.5)
            .strong()
            .color(muted_text()),
    );
    ui.add_space(6.0);
}

pub fn chip(ui: &mut egui::Ui, text: &str, fg: egui::Color32, bg: egui::Color32) {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(fg));
        });
}

pub fn state_colors(state: &MountState) -> (&'static str, egui::Color32, egui::Color32) {
    match state {
        MountState::Ready => ("Ready", text_secondary(), elevated_bg()),
        MountState::Mounting => ("Mounting", warning_fg(), warning_chip_bg()),
        MountState::Mounted => ("Mounted", success_fg(), success_chip_bg()),
        MountState::Stopping => ("Stopping", warning_fg(), warning_chip_bg()),
        MountState::Stopped => ("Stopped", text_secondary(), elevated_bg()),
        MountState::Failed => ("Error", error_fg(), error_chip_bg()),
    }
}

pub fn status_chip(ui: &mut egui::Ui, state: &MountState) {
    let (label, fg, bg) = state_colors(state);
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

/// Numeric field bound to a `u64`, with a unit suffix. Edits go through a
/// `DragValue` so typing and scrubbing both work; invalid input can't occur.
pub fn number_field(ui: &mut egui::Ui, value: &mut u64, suffix: &str, max: u64) -> egui::Response {
    ui.add(
        egui::DragValue::new(value)
            .range(0..=max)
            .speed(1.0)
            .suffix(format!(" {suffix}")),
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
    let fg = if enabled { error_fg() } else { muted_text() };
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
    egui::Frame::new()
        .fill(input_bg())
        .stroke(egui::Stroke::new(1.0_f32, border()))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let spacing = ui.spacing().item_spacing.x;
                let width = ((ui.available_width() - spacing) / 2.0).max(90.0);
                for (option, label) in options {
                    let selected = *value == option;
                    let fg = if selected { text_primary() } else { text_secondary() };
                    let fill = if selected {
                        elevated_bg()
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    let stroke = if selected {
                        egui::Stroke::new(1.0_f32, border_strong())
                    } else {
                        egui::Stroke::NONE
                    };
                    let button = egui::Button::new(RichText::new(label).strong().color(fg))
                        .fill(fill)
                        .stroke(stroke)
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
