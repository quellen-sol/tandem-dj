//! The main window on a broadcaster booth.
//!
//! A booth that joined "Broadcaster only" exists to stream the set: it plays
//! the broadcast feed and shows the broadcast window. Its main window has no
//! decks, mixer or library, just what the person running the stream needs:
//! whether both decks' tracks have arrived, whether the DJs' moves get here
//! in time for the broadcast delay, where the sound goes, and how to share
//! it.

use crate::app::{SettingsTab, TandemApp};
use crate::ui_decks::{AMBER, DECK_COLORS, GREEN};
use eframe::egui::{self, pos2, vec2, Color32, Rect, RichText, Sense};
use tandem_core::config::{broadcast_delay_for, BROADCAST_DELAY_MS, LATE_MARGIN_US};
use tandem_core::model::DECKS;
use tandem_core::session::{deck_letter, SessionView, TrackStatus, TransferState};

const WIDTH: f32 = 680.0;

/// Moves reaching this booth `late_us` after they happen are too close to a
/// `delay_ms` broadcast delay: some will land late on the stream.
pub(crate) fn late_warning(late_us: i64, delay_ms: u32) -> bool {
    late_us + LATE_MARGIN_US > delay_ms as i64 * 1000
}

impl TandemApp {
    pub(crate) fn broadcaster_panel(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        let avail = ui.available_rect_before_wrap();
        let w = avail.width().min(WIDTH);
        let rect = Rect::from_min_max(
            pos2(avail.center().x - w / 2.0, avail.top() + 18.0),
            pos2(avail.center().x + w / 2.0, avail.bottom()),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.set_width(w);
                self.broadcaster_contents(ui, view)
            });
        });
    }

    fn broadcaster_contents(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        let djs: Vec<&str> = view
            .link
            .roster
            .djs
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        let delay = self.bcast_delay_ms();
        ui.label(
            RichText::new("📡 BROADCASTING")
                .strong()
                .color(DECK_COLORS[0]),
        );
        ui.label(RichText::new(djs.join("  b2b  ")).size(28.0).strong());
        ui.label(format!(
            "This PC plays the set exactly as the stream should hear it, {delay} ms behind the DJs, \
             and nothing here changes the room."
        ));
        ui.add_space(10.0);
        let label = if self.ui.broadcast_view {
            "Close the broadcast window"
        } else {
            "Open the broadcast window"
        };
        if ui
            .add(
                egui::Button::new(RichText::new(label).size(16.0).strong())
                    .min_size(vec2(260.0, 34.0)),
            )
            .clicked()
        {
            self.ui.broadcast_view = !self.ui.broadcast_view;
        }
        ui.add_space(12.0);

        section(ui, "DECKS", |ui| {
            for d in 0..DECKS {
                deck_row(ui, d, view);
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Tracks come from the host. One a DJ has just loaded takes a few seconds to arrive; \
                     until it has, its deck is silent here.",
                )
                .small()
                .weak(),
            );
        });

        section(ui, "TIMING", |ui| self.timing(ui, view));

        section(ui, "SOUND", |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 54.0), Sense::hover());
                self.meter_view.master(ui.painter(), r);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    for s in &self.audio.status {
                        ui.label(RichText::new(s).small());
                    }
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Slider::new(&mut self.local_edit.bcast_level, 0.0..=1.0)
                                .show_value(false)
                                .text("level"),
                        );
                        if ui.small_button("Audio settings").clicked() {
                            self.ui.show_settings = true;
                            self.ui.settings_tab = SettingsTab::Audio;
                        }
                    });
                });
            });
        });

        section(ui, "STREAM IT", |ui| {
            bullet(
                ui,
                "Discord: share the \"Tandem broadcast\" window with its sound on. Tandem on this PC \
                 plays only the broadcast feed, so that's all Discord picks up: no virtual cable.",
            );
            bullet(
                ui,
                "OBS: a Window Capture of \"Tandem broadcast\", and an Application Audio Capture of \
                 Tandem (or a capture of the speakers above). Neither needs a delay.",
            );
            bullet(
                ui,
                "Keep the broadcast window visible, on a second screen if you have one: a minimised \
                 window can't be captured.",
            );
        });
    }
}

impl TandemApp {
    /// Do the DJs' moves get here in time for the broadcast delay? If not,
    /// offer a delay that leaves room.
    fn timing(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        let delay = self.bcast_delay_ms();
        let Some(late) = view.link.late_us else {
            ui.label(
                RichText::new(
                    "Once the DJs make a move, this shows how long their moves take to get here.",
                )
                .weak(),
            );
            return;
        };
        let late_ms = late as f64 / 1000.0;
        if !late_warning(late, delay) {
            ui.label(
                RichText::new(format!(
                    "✔ The DJs' moves get here at most {late_ms:.0} ms after they happen \
                     (in the last minute). The stream runs {delay} ms behind, so every one is on time."
                ))
                .color(GREEN),
            );
            return;
        }
        ui.label(
            RichText::new(format!(
                "⚠ The DJs' moves get here up to {late_ms:.0} ms after they happen (in the last \
                 minute). That's too close to the {delay} ms the stream runs behind, so some \
                 land late on it."
            ))
            .color(AMBER),
        );
        let to = broadcast_delay_for(late);
        let mut raise = false;
        if to > delay {
            ui.horizontal(|ui| {
                raise = ui
                    .button(RichText::new(format!("Raise the delay to {to} ms")).strong())
                    .clicked();
                ui.label(
                    RichText::new(format!(
                        "The stream repeats {} ms once as it changes: between tracks is best.",
                        to - delay
                    ))
                    .small()
                    .weak(),
                );
            });
        }
        if late + LATE_MARGIN_US > *BROADCAST_DELAY_MS.end() as i64 * 1000 {
            ui.label(
                RichText::new(
                    "This connection is too slow for every move to be on time, even at the \
                     longest delay. If it happens while tracks are being sent, lower the upload \
                     speed limit on the host (Settings → General).",
                )
                .small(),
            );
        }
        if raise {
            self.settings.audio.broadcaster_delay_ms = to;
            self.restart_audio();
            self.toast(format!("The broadcast now runs {to} ms behind the room."));
        }
    }
}

fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).small().strong().weak());
            ui.add_space(4.0);
            add(ui);
        });
    ui.add_space(8.0);
}

fn bullet(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_top(|ui| {
        ui.label("•");
        ui.add(egui::Label::new(text).wrap());
    });
}

/// "A  Title – Artist          ✔ ready".
fn deck_row(ui: &mut egui::Ui, d: usize, view: &SessionView) {
    let dv = &view.decks[d];
    let col = DECK_COLORS[d];
    let download = |id| view.transfers.iter().find(|t| t.id == id && !t.upload);
    let (name, status) = match dv.state.track {
        None => (RichText::new("No track").weak(), None),
        Some(id) => {
            let name = match &dv.meta {
                Some(m) if m.artist.is_empty() => m.title.clone(),
                Some(m) => format!("{} – {}", m.title, m.artist),
                None => "…".into(),
            };
            let status = match &dv.status {
                Some(TrackStatus::Ready) => ("✔ ready".to_string(), GREEN),
                Some(TrackStatus::Fetching) => match download(id) {
                    Some(t) if t.state == TransferState::Queued => {
                        ("waiting to download".to_string(), AMBER)
                    }
                    Some(t) if t.total > 0 => (
                        format!("receiving {:.0}%", t.done as f64 / t.total as f64 * 100.0),
                        AMBER,
                    ),
                    _ => ("receiving…".to_string(), AMBER),
                },
                Some(TrackStatus::Searching) => ("looking for it…".to_string(), AMBER),
                Some(TrackStatus::Decoding) => ("decoding…".to_string(), AMBER),
                Some(TrackStatus::Failed(e)) => (format!("⚠ {e} (trying again)"), AMBER),
                None => ("…".to_string(), Color32::GRAY),
            };
            (RichText::new(name).strong(), Some(status))
        }
    };
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(deck_letter(d).to_string())
                .strong()
                .size(16.0)
                .color(col),
        );
        ui.add_space(6.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some((text, color)) = status {
                ui.label(RichText::new(text).color(color));
                ui.add_space(12.0);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(name).truncate());
            });
        });
    });
}
