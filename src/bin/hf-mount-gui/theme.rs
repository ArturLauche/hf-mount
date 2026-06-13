//! Dark neutral theme: flat surfaces, 1px borders, 8px radius, one orange
//! accent. No gradients, no glows, no decorative chrome.

use eframe::egui;

pub fn app_bg() -> egui::Color32 {
    egui::Color32::from_rgb(24, 24, 24)
}

pub fn header_bg() -> egui::Color32 {
    egui::Color32::from_rgb(28, 28, 28)
}

pub fn panel_bg() -> egui::Color32 {
    egui::Color32::from_rgb(33, 33, 33)
}

pub fn elevated_bg() -> egui::Color32 {
    egui::Color32::from_rgb(42, 42, 42)
}

pub fn input_bg() -> egui::Color32 {
    egui::Color32::from_rgb(42, 42, 42)
}

pub fn border() -> egui::Color32 {
    egui::Color32::from_rgb(58, 58, 58)
}

pub fn primary_button_bg() -> egui::Color32 {
    egui::Color32::from_rgb(242, 242, 242)
}

pub fn primary_button_text() -> egui::Color32 {
    egui::Color32::from_rgb(28, 28, 28)
}

pub fn accent() -> egui::Color32 {
    egui::Color32::from_rgb(240, 122, 50)
}

pub fn success_fg() -> egui::Color32 {
    egui::Color32::from_rgb(102, 192, 133)
}

pub fn text_primary() -> egui::Color32 {
    egui::Color32::from_rgb(242, 242, 242)
}

pub fn text_secondary() -> egui::Color32 {
    egui::Color32::from_rgb(168, 168, 168)
}

pub fn muted_text() -> egui::Color32 {
    egui::Color32::from_rgb(126, 126, 126)
}

pub fn warning_fg() -> egui::Color32 {
    egui::Color32::from_rgb(240, 173, 78)
}

pub fn error_fg() -> egui::Color32 {
    egui::Color32::from_rgb(238, 107, 107)
}

pub fn success_chip_bg() -> egui::Color32 {
    egui::Color32::from_rgb(31, 48, 38)
}

pub fn warning_chip_bg() -> egui::Color32 {
    egui::Color32::from_rgb(56, 44, 30)
}

pub fn error_chip_bg() -> egui::Color32 {
    egui::Color32::from_rgb(58, 36, 36)
}

pub fn danger_button_bg() -> egui::Color32 {
    egui::Color32::from_rgb(70, 40, 40)
}

pub fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let mut visuals = egui::Visuals::dark();
    let rounding = egui::Rounding::same(8.0);

    visuals.panel_fill = app_bg();
    visuals.window_fill = panel_bg();
    visuals.extreme_bg_color = input_bg();
    visuals.faint_bg_color = elevated_bg();
    visuals.code_bg_color = input_bg();
    visuals.selection.bg_fill = egui::Color32::from_rgb(70, 70, 70);
    visuals.selection.stroke = egui::Stroke::new(1.0, text_primary());
    visuals.hyperlink_color = accent();
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
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(52, 52, 52);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(48, 48, 48);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(82, 82, 82));
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(58, 58, 58);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, text_secondary());

    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.window_margin = egui::Margin::same(0.0);
    ctx.set_style(style);
}
