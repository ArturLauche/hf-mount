//! Mount tab: the source/mount form grouped into cards, a mounted "hero"
//! panel while a mount is active, the blocker banner, and Start/Stop actions.

use eframe::egui::{self, RichText};

use crate::app::{MountGuiApp, MountState, Tab, push_log};
use crate::platform;
use crate::preflight::{CheckItem, blocker_command};
use crate::profile::GuiSource;
use crate::theme::*;
use crate::util::format_elapsed;
use crate::widgets::{
    card, card_title, danger_button, field_error, field_hint, field_row, number_field, primary_button,
    secondary_button, segmented_pair, text_field,
};

impl MountGuiApp {
    pub fn draw_mount_tab(&mut self, ui: &mut egui::Ui) {
        // The action row is a nested bottom panel: it sizes to whatever it
        // renders (the buttons may grow an "Active target" line while
        // mounting), and the scrolling form gets exactly the space left over,
        // so Start/Stop stay reachable at any window height without overlap.
        egui::Panel::bottom("mount-actions")
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                top: 10,
                ..Default::default()
            }))
            .resizable(false)
            .show_separator_line(false)
            .show(ui, |ui| {
                let form_width = ui.available_width().min(660.0);
                ui.vertical(|ui| {
                    ui.set_width(form_width);
                    self.draw_actions(ui);
                });
            });

        egui::ScrollArea::vertical()
            .id_salt("mount-form")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let form_width = ui.available_width().min(660.0);
                ui.vertical(|ui| {
                    ui.set_width(form_width);

                    if self.current_status().state == MountState::Mounted {
                        self.draw_mounted_hero(ui);
                        ui.add_space(12.0);
                    }

                    card(ui, |ui| {
                        card_title(ui, "Source");
                        self.draw_source_section(ui);
                    });
                    ui.add_space(10.0);
                    card(ui, |ui| {
                        card_title(ui, "Destination");
                        self.draw_destination_section(ui);
                    });
                    ui.add_space(10.0);
                    card(ui, |ui| {
                        card_title(ui, "Options");
                        self.draw_options_section(ui);
                    });

                    if !self.is_mount_running()
                        && let Some(blocker) = self.first_blocking_check().cloned()
                    {
                        ui.add_space(12.0);
                        self.draw_blocker_banner(ui, &blocker);
                    }
                    ui.add_space(8.0);
                });
            });
    }

    /// Prominent panel while mounted: what is mounted where, uptime, and the
    /// open-folder action front and center.
    fn draw_mounted_hero(&mut self, ui: &mut egui::Ui) {
        let mount_label = self
            .active_mount_point
            .as_ref()
            .map(|mount_point| mount_point.display().to_string())
            .unwrap_or_else(|| "—".to_string());
        let uptime = self.mounted_uptime_secs().map(format_elapsed);

        egui::Frame::new()
            .fill(success_chip_bg())
            .stroke(egui::Stroke::new(1.0_f32, success_fg()))
            .corner_radius(egui::CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&mount_label).size(20.0).strong().color(text_primary()));
                        // Describe the mount from the identity captured at mount
                        // start (or reported by a reconnected worker) — never the
                        // editable form, which may have changed since.
                        let mut subtitle = match &self.active_source_label {
                            Some(label) => format!("{label} mounted"),
                            None => "Mounted".to_string(),
                        };
                        if let Some(uptime) = uptime {
                            subtitle.push_str(&format!(" · up {uptime}"));
                        }
                        ui.label(RichText::new(subtitle).size(12.0).color(text_secondary()));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary_button(ui, "Open folder", self.active_mount_point.is_some(), 110.0).clicked() {
                            self.open_active_mount();
                        }
                    });
                });
            });
    }

    fn draw_source_section(&mut self, ui: &mut egui::Ui) {
        field_row(ui, "Type", |ui| {
            let before = self.source;
            segmented_pair(
                ui,
                &mut self.source,
                [(GuiSource::Repo, "Repo"), (GuiSource::Bucket, "Bucket")],
            );
            if before != self.source && self.source == GuiSource::Repo {
                self.read_only = true;
            }
        });

        let id_label = match self.source {
            GuiSource::Repo => "Repo ID",
            GuiSource::Bucket => "Bucket ID",
        };
        let mut recent_pick = None;
        field_row(ui, id_label, |ui| {
            let hint = match self.source {
                GuiSource::Repo => "openai-community/gpt2",
                GuiSource::Bucket => "namespace/bucket",
            };
            text_field(ui, &mut self.source_id, hint, false);
            if let Some(problem) = self.source_problem() {
                ui.add_space(2.0);
                field_error(ui, problem);
            }
            if !self.recent_sources.is_empty() {
                ui.add_space(2.0);
                egui::ComboBox::from_id_salt("recent-sources")
                    .selected_text(RichText::new("Recent").size(12.0).color(text_secondary()))
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for recent in &self.recent_sources {
                            if ui.selectable_label(false, recent.label()).clicked() {
                                recent_pick = Some(recent.clone());
                            }
                        }
                    });
            }
        });
        if let Some(recent) = recent_pick {
            self.apply_recent_source(&recent);
        }

        if self.source == GuiSource::Repo {
            field_row(ui, "Revision", |ui| {
                text_field(ui, &mut self.revision, "main", false);
            });
        }

        field_row(ui, "HF token", |ui| {
            ui.horizontal(|ui| {
                let toggle_width = 52.0;
                let field_width = (ui.available_width() - toggle_width - ui.spacing().item_spacing.x).max(120.0);
                ui.add_sized(
                    [field_width, 30.0],
                    egui::TextEdit::singleline(&mut self.hf_token)
                        .desired_width(f32::INFINITY)
                        .hint_text("Optional access token")
                        .password(!self.show_token),
                );
                let label = if self.show_token { "Hide" } else { "Show" };
                if ui.add_sized([toggle_width, 30.0], egui::Button::new(label)).clicked() {
                    self.show_token = !self.show_token;
                }
            });
            ui.add_space(2.0);
            field_hint(ui, "Uses HF_TOKEN automatically when set. Inline tokens are not saved.");
        });
    }

    fn draw_destination_section(&mut self, ui: &mut egui::Ui) {
        field_row(ui, "Mount point", |ui| {
            #[cfg(windows)]
            {
                let mut picked = None;
                ui.horizontal(|ui| {
                    let picker_width = 64.0;
                    let field_width = (ui.available_width() - picker_width - ui.spacing().item_spacing.x).max(120.0);
                    ui.add_sized(
                        [field_width, 30.0],
                        egui::TextEdit::singleline(&mut self.mount_point)
                            .desired_width(f32::INFINITY)
                            .hint_text(platform::default_mount_hint()),
                    );
                    egui::ComboBox::from_id_salt("drive-letter")
                        .selected_text(RichText::new("Free").size(12.0).color(text_secondary()))
                        .width(picker_width)
                        .show_ui(ui, |ui| {
                            let letters = platform::free_drive_letters();
                            if letters.is_empty() {
                                ui.label(RichText::new("No free letters found").color(muted_text()));
                            }
                            for letter in letters {
                                if ui.selectable_label(false, format!("{letter}:")).clicked() {
                                    picked = Some(format!("{letter}:"));
                                }
                            }
                        });
                });
                if let Some(target) = picked {
                    self.mount_point = target;
                    self.refresh_checks();
                }
            }
            #[cfg(not(windows))]
            {
                text_field(ui, &mut self.mount_point, platform::default_mount_hint(), false);
            }
            ui.add_space(2.0);
            field_hint(ui, platform::mount_point_hint());
        });

        field_row(ui, "Access", |ui| {
            if self.source == GuiSource::Repo {
                self.read_only = true;
                let mut locked = true;
                ui.horizontal(|ui| {
                    ui.add_enabled(false, egui::Checkbox::new(&mut locked, "Read-only"));
                    field_hint(ui, "Repos are always read-only.");
                });
            } else {
                ui.checkbox(&mut self.read_only, "Read-only");
            }
        });

        field_row(ui, "Run", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.run_in_background, "Background")
                    .on_hover_text("Keep the mount running after this window is closed.");
                if ui
                    .checkbox(&mut self.autostart_enabled, "Start at login")
                    .on_hover_text("Register the saved mount to start when you log in.")
                    .changed()
                {
                    self.apply_autostart_setting();
                }
            });
        });
    }

    fn draw_options_section(&mut self, ui: &mut egui::Ui) {
        let advanced_label = if self.show_advanced {
            "Hide advanced options"
        } else {
            "Show advanced options"
        };
        if ui
            .add(egui::Button::new(RichText::new(advanced_label).size(12.0).color(text_secondary())).frame(false))
            .clicked()
        {
            self.show_advanced = !self.show_advanced;
        }
        if !self.show_advanced {
            return;
        }

        ui.add_space(6.0);
        field_row(ui, "Hub endpoint", |ui| {
            text_field(ui, &mut self.hub_endpoint, "https://huggingface.co", false);
        });
        field_row(ui, "Cache dir", |ui| {
            text_field(ui, &mut self.cache_dir, "Cache directory", false);
        });
        field_row(ui, "Cache size", |ui| {
            ui.horizontal(|ui| {
                number_field(ui, &mut self.cache_size_gb, "GB", 100_000);
                field_hint(ui, "On-disk chunk cache limit.");
            });
        });
        field_row(ui, "Poll interval", |ui| {
            ui.horizontal(|ui| {
                number_field(ui, &mut self.poll_interval_secs, "s", 86_400);
                field_hint(ui, "How often to poll for remote changes. 0 disables polling.");
            });
        });
        field_row(ui, "Metadata TTL", |ui| {
            ui.horizontal(|ui| {
                number_field(ui, &mut self.metadata_ttl_ms, "ms", 3_600_000);
                field_hint(ui, "How long file attributes are trusted before re-checking.");
            });
        });
        field_row(ui, "Read timeout", |ui| {
            ui.horizontal(|ui| {
                number_field(ui, &mut self.read_fetch_timeout_ms, "ms", 600_000);
                field_hint(ui, "Fail a stalled remote read after this long. 0 waits forever.");
            });
        });
        field_row(ui, "Token file", |ui| {
            text_field(ui, &mut self.token_file, "Path to token file", false);
            ui.add_space(2.0);
            field_hint(ui, "Re-read on each request; used by background and autostart mounts.");
        });
        field_row(ui, "NFS access", |ui| {
            ui.checkbox(&mut self.nfs_allow_unsafe_loopback, "Allow unsafe loopback fallback")
                .on_hover_text(
                    "Permit NFS without enforceable local caller authorization. Required for \
                     credential-backed mounts on Windows.",
                );
        });
    }

    fn draw_blocker_banner(&mut self, ui: &mut egui::Ui, blocker: &CheckItem) {
        egui::Frame::new()
            .fill(warning_chip_bg())
            .stroke(egui::Stroke::new(1.0_f32, accent()))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!("{}: {}", blocker.label, blocker.detail))
                        .size(13.0)
                        .color(text_primary()),
                );
                if let Some(command) = blocker_command(blocker) {
                    ui.add_space(2.0);
                    ui.label(RichText::new(command).monospace().size(11.0).color(text_secondary()));
                }
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    self.draw_blocker_action(ui, blocker);
                    if let Some(command) = blocker_command(blocker)
                        && ui.button("Copy command").clicked()
                    {
                        ui.ctx().copy_text(command.to_string());
                        push_log(&self.status, "Copied setup command");
                    }
                    if ui.button("Recheck").clicked() {
                        self.refresh_checks();
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("Details in Setup").color(text_secondary())).frame(false))
                        .clicked()
                    {
                        self.tab = Tab::Setup;
                    }
                });
            });
    }

    pub(crate) fn draw_blocker_action(&mut self, ui: &mut egui::Ui, blocker: &CheckItem) {
        #[cfg(windows)]
        {
            if blocker.label == "Client for NFS" {
                let enable =
                    egui::Button::new(RichText::new("Enable NFS").strong().color(egui::Color32::WHITE)).fill(accent());
                if ui.add(enable).clicked() {
                    match platform::enable_windows_nfs_client() {
                        Ok(()) => crate::app::set_status(
                            &self.status,
                            MountState::Stopped,
                            "NFS enable requested",
                            "Approve the UAC prompt. Reboot if Windows asks, then press Recheck.",
                        ),
                        Err(e) => crate::app::set_status(&self.status, MountState::Failed, "Could not enable NFS", e),
                    }
                }
                return;
            }

            if blocker.label == "Administrator" {
                if ui
                    .add(egui::Button::new(
                        RichText::new("Restart as admin").strong().color(text_primary()),
                    ))
                    .clicked()
                {
                    match platform::restart_as_administrator() {
                        Ok(()) => crate::app::set_status(
                            &self.status,
                            MountState::Stopped,
                            "Elevation requested",
                            "Approve the Windows UAC prompt, then use the elevated window.",
                        ),
                        Err(e) => {
                            crate::app::set_status(&self.status, MountState::Failed, "Could not relaunch as admin", e)
                        }
                    }
                }
                return;
            }

            if blocker.label == "Mount point" && ui.button("Use a free letter").clicked() {
                self.mount_point = platform::default_mount_point();
                self.refresh_checks();
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (ui, blocker);
        }
    }

    fn draw_actions(&mut self, ui: &mut egui::Ui) {
        let running = self.is_mount_running();
        let stopping = self.is_stopping();
        let mounted = self.current_status().state == MountState::Mounted;
        let blocked = self.first_blocking_check().is_some() || self.source_problem().is_some();

        let start_label = if running {
            "Mount running"
        } else if self.run_in_background {
            "Start in background"
        } else {
            "Start mount"
        };

        ui.horizontal(|ui| {
            if primary_button(ui, start_label, !running && !blocked, 170.0).clicked() {
                self.start_mount();
            }
            if danger_button(ui, "Stop", running && !stopping, 90.0).clicked() {
                self.stop_mount();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if secondary_button(ui, "Check setup", true, 110.0).clicked() {
                    self.refresh_checks();
                }
            });
        });

        if let Some(mount_point) = &self.active_mount_point
            && running
            && !mounted
        {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("Active target: {}", mount_point.display()))
                    .size(11.0)
                    .color(muted_text()),
            );
        }
    }
}
