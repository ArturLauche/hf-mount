//! Visual design system: a layered dark palette with a single Hugging Face
//! orange accent. Flat surfaces, 1px borders, 8px radius — no gradients, no
//! glows. All colors live here so the tabs stay palette-free.

use eframe::egui;

// ── Surfaces ──────────────────────────────────────────────────────────

pub fn app_bg() -> egui::Color32 {
    egui::Color32::from_rgb(19, 19, 22)
}

pub fn sidebar_bg() -> egui::Color32 {
    egui::Color32::from_rgb(14, 14, 17)
}

pub fn panel_bg() -> egui::Color32 {
    egui::Color32::from_rgb(27, 27, 31)
}

pub fn elevated_bg() -> egui::Color32 {
    egui::Color32::from_rgb(37, 37, 43)
}

pub fn input_bg() -> egui::Color32 {
    egui::Color32::from_rgb(34, 34, 40)
}

// ── Lines ─────────────────────────────────────────────────────────────

pub fn border() -> egui::Color32 {
    egui::Color32::from_rgb(48, 48, 56)
}

pub fn border_strong() -> egui::Color32 {
    egui::Color32::from_rgb(66, 66, 76)
}

// ── Text ──────────────────────────────────────────────────────────────

pub fn text_primary() -> egui::Color32 {
    egui::Color32::from_rgb(240, 240, 243)
}

pub fn text_secondary() -> egui::Color32 {
    egui::Color32::from_rgb(166, 166, 176)
}

pub fn muted_text() -> egui::Color32 {
    // ≥ 4.5:1 contrast against every surface it renders on (app, panel,
    // input, elevated) — this token is used for ~11px helper text.
    egui::Color32::from_rgb(140, 140, 150)
}

// ── Accent & semantic colors ──────────────────────────────────────────

pub fn accent() -> egui::Color32 {
    egui::Color32::from_rgb(255, 140, 60)
}

pub fn success_fg() -> egui::Color32 {
    egui::Color32::from_rgb(104, 200, 138)
}

pub fn warning_fg() -> egui::Color32 {
    egui::Color32::from_rgb(240, 180, 84)
}

pub fn error_fg() -> egui::Color32 {
    egui::Color32::from_rgb(240, 110, 110)
}

pub fn success_chip_bg() -> egui::Color32 {
    egui::Color32::from_rgb(26, 46, 34)
}

pub fn warning_chip_bg() -> egui::Color32 {
    egui::Color32::from_rgb(52, 42, 26)
}

pub fn error_chip_bg() -> egui::Color32 {
    egui::Color32::from_rgb(54, 32, 32)
}

// ── Buttons ───────────────────────────────────────────────────────────

pub fn primary_button_bg() -> egui::Color32 {
    accent()
}

pub fn primary_button_text() -> egui::Color32 {
    egui::Color32::from_rgb(24, 16, 8)
}

pub fn danger_button_bg() -> egui::Color32 {
    egui::Color32::from_rgb(66, 36, 36)
}

// ── Style application ─────────────────────────────────────────────────

pub fn apply_theme(ctx: &egui::Context) {
    // The palette is dark-only; pin the theme so an OS light-mode preference
    // doesn't swap in unstyled light visuals.
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    let mut visuals = egui::Visuals::dark();
    let radius = egui::CornerRadius::same(8);

    visuals.panel_fill = app_bg();
    visuals.window_fill = panel_bg();
    visuals.extreme_bg_color = input_bg();
    visuals.faint_bg_color = elevated_bg();
    visuals.code_bg_color = input_bg();
    visuals.selection.bg_fill = egui::Color32::from_rgb(80, 56, 34);
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, accent());
    visuals.hyperlink_color = accent();
    visuals.warn_fg_color = warning_fg();
    visuals.error_fg_color = error_fg();
    visuals.window_corner_radius = radius;
    visuals.menu_corner_radius = radius;

    for widgets in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widgets.corner_radius = radius;
    }

    visuals.widgets.noninteractive.bg_fill = panel_bg();
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, border());
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, text_primary());
    visuals.widgets.inactive.bg_fill = input_bg();
    visuals.widgets.inactive.weak_bg_fill = elevated_bg();
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, border());
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, text_primary());
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(46, 46, 54);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(42, 42, 50);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, border_strong());
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(54, 54, 62);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, accent());

    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.window_margin = egui::Margin::same(0);
    ctx.set_style_of(egui::Theme::Dark, style);
}
