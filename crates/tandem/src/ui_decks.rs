//! Top bar, decks, mixer and waveforms.

use crate::app::TandemApp;
use eframe::egui::{self, pos2, vec2, Align, Color32, Layout, RichText, Sense, Stroke};
use tandem_core::audio::PEAK_BLOCK;
use tandem_core::model::{Action, DECKS};
use tandem_core::session::{deck_letter, DeckView, RoleView, SessionView, TrackStatus};

pub const DECK_COLORS: [Color32; DECKS] = [
    Color32::from_rgb(255, 146, 60),
    Color32::from_rgb(70, 190, 255),
];
const HOT_CUE_COLORS: [Color32; 8] = [
    Color32::from_rgb(230, 60, 60),
    Color32::from_rgb(255, 160, 40),
    Color32::from_rgb(240, 220, 60),
    Color32::from_rgb(80, 210, 90),
    Color32::from_rgb(60, 200, 220),
    Color32::from_rgb(70, 110, 240),
    Color32::from_rgb(170, 80, 230),
    Color32::from_rgb(240, 90, 180),
];
const GREEN: Color32 = Color32::from_rgb(90, 220, 120);
const RED: Color32 = Color32::from_rgb(240, 90, 90);
const AMBER: Color32 = Color32::from_rgb(250, 190, 60);

pub fn fmt_time(s: f64) -> String {
    let s = s.max(0.0);
    let m = (s / 60.0).floor();
    format!("{}:{:04.1}", m as i64, s - m * 60.0)
}

impl TandemApp {
    pub(crate) fn top_bar(&mut self, ctx: &egui::Context, view: &SessionView) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("TANDEM").strong().size(20.0).color(DECK_COLORS[0]));
                ui.label(RichText::new(&view.me.name).weak());
                ui.separator();
                match &view.link.role {
                    RoleView::Solo => {
                        ui.label("Not in a room (practising solo).");
                        if ui.button("Host a room").clicked() {
                            self.ui.host_open = true;
                        }
                        if ui.button("Join a room").clicked() {
                            self.ui.join_open = true;
                        }
                    }
                    RoleView::Hosting { port, room } => {
                        ui.label(RichText::new(format!("Hosting room {room}")).strong());
                        ui.label(RichText::new(format!("port {port}")).weak());
                        self.peer_status(ui, view);
                        if ui.button("Leave").clicked() {
                            self.session.lock().leave();
                        }
                    }
                    RoleView::Joined { addr, room } => {
                        ui.label(RichText::new(format!("In room {room}")).strong());
                        ui.label(RichText::new(addr.as_str()).weak());
                        self.peer_status(ui, view);
                        if ui.button("Leave").clicked() {
                            self.session.lock().leave();
                        }
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("⚙ Settings").clicked() {
                        self.ui.show_settings = !self.ui.show_settings;
                    }
                    if ui.button("Terms").clicked() {
                        self.ui.show_terms = true;
                    }
                    ui.toggle_value(&mut self.local_edit.click, "Sync click")
                        .on_hover_text("A metronome on the shared room clock. Both booths click at the same instant: put two laptops side by side to hear how well they're synced.");
                });
            });
            ui.add_space(2.0);
        });
    }

    fn peer_status(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        let Some(peer) = &view.link.peer else {
            ui.label(RichText::new("Waiting for your partner to join…").color(AMBER));
            return;
        };
        let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 4.0, GREEN);
        ui.label(RichText::new(&peer.name).color(GREEN).strong());
        if let Some(rtt) = view.link.rtt_us {
            ui.label(format!("{:.0} ms round trip", rtt as f64 / 1000.0))
                .on_hover_text("Time for a message to reach your partner and come back. Your own controls are always instant; your partner's arrive about half this late in your headphones, and exactly on time in the broadcast.");
        }
        if let Some(spread) = view.link.clock_spread_us {
            ui.label(RichText::new(format!("clock ±{:.1} ms", spread as f64 / 2000.0)).weak());
        }
        match view.link.in_sync {
            Some(true) => {
                ui.label(RichText::new("✔ in sync").color(GREEN));
            }
            Some(false) => {
                ui.label(RichText::new("⚠ resyncing").color(AMBER));
            }
            None => {}
        }
    }

    pub(crate) fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let errors = self.audio.errors.lock().last().cloned();
                if let Some(e) = errors {
                    ui.label(RichText::new(format!("Audio error: {e}")).color(RED));
                    if ui.small_button("Restart audio").clicked() {
                        self.audio.errors.lock().clear();
                        self.restart_audio();
                    }
                    ui.separator();
                }
                if let Some((msg, at)) = &self.ui.toast {
                    if at.elapsed().as_secs() < 6 {
                        ui.label(RichText::new(msg.as_str()).color(AMBER));
                        ui.separator();
                    }
                }
                if let Some(line) = tandem_core::logging::recent(1).pop() {
                    ui.add(egui::Label::new(RichText::new(line).weak()).truncate());
                }
            });
        });
    }

    pub(crate) fn decks_area(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        let mixer_w = 290.0;
        let gap = ui.spacing().item_spacing.x;
        let deck_w = ((ui.available_width() - mixer_w - 2.0 * gap) / 2.0).max(260.0);
        let top_h = 352.0;
        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
            ui.allocate_ui_with_layout(vec2(deck_w, top_h), Layout::top_down(Align::Min), |ui| {
                self.deck_panel(ui, 0, view)
            });
            ui.allocate_ui_with_layout(vec2(mixer_w, top_h), Layout::top_down(Align::Min), |ui| {
                self.mixer_panel(ui, view)
            });
            ui.allocate_ui_with_layout(vec2(deck_w, top_h), Layout::top_down(Align::Min), |ui| {
                self.deck_panel(ui, 1, view)
            });
        });
        ui.add_space(6.0);
        let h = ((ui.available_height() - 8.0) / 2.0).clamp(40.0, 90.0);
        for d in 0..DECKS {
            scrolling_waveform(ui, d, &view.decks[d], view.now, h);
        }
    }

    fn deck_panel(&mut self, ui: &mut egui::Ui, d: usize, view: &SessionView) {
        let dv = &view.decks[d];
        let st = dv.state;
        let pos = st.position_at(view.now);
        let col = DECK_COLORS[d];
        let deck = d as u8;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(338.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("DECK {}", deck_letter(d)))
                        .strong()
                        .color(col),
                );
                if let Some(by) = view.name_of(st.last_touch) {
                    ui.label(
                        RichText::new(format!("last touched by {by}"))
                            .small()
                            .weak(),
                    );
                }
                if st.scratching {
                    ui.label(RichText::new("SCRATCH").small().strong().color(AMBER));
                }
            });
            let (title, artist) = match &dv.meta {
                Some(m) => (m.title.clone(), m.artist.clone()),
                None if st.track.is_some() => ("…".to_string(), String::new()),
                None => (
                    "Empty".to_string(),
                    "Double-click a track in the library to load it".to_string(),
                ),
            };
            ui.add(egui::Label::new(RichText::new(title).size(17.0).strong()).truncate());
            ui.add(egui::Label::new(RichText::new(artist).weak()).truncate());
            let status = deck_status(dv, view);
            ui.label(match status {
                Some((s, c)) => RichText::new(s).color(c).small(),
                None => RichText::new(" ").small(),
            });

            ui.horizontal(|ui| {
                ui.monospace(RichText::new(fmt_time(pos)).size(15.0));
                ui.monospace(RichText::new(format!("-{}", fmt_time(st.duration - pos))).weak());
                ui.separator();
                match dv.meta.as_ref().and_then(|m| m.bpm) {
                    Some(b) => ui.monospace(
                        RichText::new(format!("{:.2} BPM", b as f64 * st.tempo)).size(15.0),
                    ),
                    None => ui.monospace(RichText::new("--- BPM").weak()),
                };
                ui.monospace(format!("{:+.2}%", (st.tempo - 1.0) * 100.0));
            });

            if let Some(seek) = overview(ui, dv, pos, col) {
                self.pending.push(Action::Seek { deck, pos: seek });
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let big = vec2(62.0, 40.0);
                let primary = ui.input(|i| i.pointer.primary_down());
                let cue_fill = if st.cue_preview {
                    AMBER.gamma_multiply(0.6)
                } else {
                    Color32::from_gray(50)
                };
                let r = ui.add(
                    egui::Button::new(RichText::new("CUE").strong().size(15.0))
                        .min_size(big)
                        .fill(cue_fill),
                );
                let (down, up) =
                    self.hold(format!("cue{d}"), r.is_pointer_button_down_on() && primary);
                if down {
                    self.pending.push(Action::CueDown { deck });
                }
                if up {
                    self.pending.push(Action::CueUp { deck });
                }
                let playing = st.playing && !st.cue_preview;
                let play_fill = if playing {
                    GREEN.gamma_multiply(0.45)
                } else {
                    Color32::from_gray(50)
                };
                let r = ui.add(
                    egui::Button::new(RichText::new(if playing { "⏸" } else { "▶" }).size(18.0))
                        .min_size(big)
                        .fill(play_fill),
                );
                if self
                    .hold(format!("play{d}"), r.is_pointer_button_down_on() && primary)
                    .0
                {
                    self.pending.push(if playing {
                        Action::Pause { deck }
                    } else {
                        Action::Play { deck }
                    });
                }
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = vec2(3.0, 2.0);
                    for row in 0..2u8 {
                        ui.horizontal(|ui| {
                            for slot in row * 4..row * 4 + 4 {
                                let set = st.hot_cues[slot as usize].is_some();
                                let fill = if set {
                                    HOT_CUE_COLORS[slot as usize].gamma_multiply(0.7)
                                } else {
                                    Color32::from_gray(35)
                                };
                                let r = ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new(format!("{}", slot + 1)).strong(),
                                        )
                                        .min_size(vec2(34.0, 19.0))
                                        .fill(fill),
                                    )
                                    .on_hover_text(
                                        "Click: set / jump to hot cue. Right-click: clear.",
                                    );
                                if self
                                    .hold(
                                        format!("hc{d}-{slot}"),
                                        r.is_pointer_button_down_on() && primary,
                                    )
                                    .0
                                {
                                    self.pending.push(Action::HotCue { deck, slot });
                                }
                                if r.secondary_clicked() {
                                    self.pending.push(Action::ClearHotCue { deck, slot });
                                }
                            }
                        });
                    }
                });
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let range = (self.settings.tempo_range * 100.0) as f32;
                let mut pct = ((st.tempo - 1.0) * 100.0) as f32;
                ui.spacing_mut().slider_width = (ui.available_width() - 190.0).max(80.0);
                let r = ui.add(
                    egui::Slider::new(&mut pct, -range..=range)
                        .clamping(egui::SliderClamping::Edits)
                        .fixed_decimals(2)
                        .suffix("%")
                        .text("tempo"),
                );
                if r.changed() {
                    self.pending.push(Action::Tempo {
                        deck,
                        tempo: 1.0 + pct as f64 / 100.0,
                    });
                }
                if ui.small_button("0").on_hover_text("Reset tempo").clicked() {
                    self.pending.push(Action::Tempo { deck, tempo: 1.0 });
                }
                let primary = ui.input(|i| i.pointer.primary_down());
                for (label, bend, key) in [("◀", -0.04, "bd"), ("▶", 0.04, "bu")] {
                    let r = ui
                        .add(egui::Button::new(label).min_size(vec2(26.0, 20.0)))
                        .on_hover_text("Hold to pitch-bend");
                    let (down, up) = self.hold(
                        format!("{key}{d}"),
                        r.is_pointer_button_down_on() && primary,
                    );
                    if down {
                        self.pending.push(Action::Bend { deck, bend });
                    }
                    if up {
                        self.pending.push(Action::Bend { deck, bend: 0.0 });
                    }
                }
            });
        });
    }

    fn mixer_panel(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(338.0);
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.vertical_centered(|ui| ui.label(RichText::new("MIXER").strong()));
            ui.columns(DECKS, |cols| {
                for (d, ui) in cols.iter_mut().enumerate() {
                    self.channel_strip(ui, d, view);
                }
            });
            ui.add_space(4.0);
            let mut xf = view.room.crossfader;
            ui.horizontal(|ui| {
                ui.label(RichText::new("A").color(DECK_COLORS[0]).strong());
                ui.spacing_mut().slider_width = ui.available_width() - 24.0;
                let r = ui.add(egui::Slider::new(&mut xf, 0.0..=1.0).show_value(false));
                if r.changed() {
                    self.pending.push(Action::Crossfader { v: xf });
                }
                if r.double_clicked() {
                    self.pending.push(Action::Crossfader { v: 0.5 });
                }
                ui.label(RichText::new("B").color(DECK_COLORS[1]).strong());
            });
            ui.separator();
            ui.label(RichText::new("Your booth only").small().weak());
            ui.spacing_mut().slider_width = 130.0;
            ui.add(
                egui::Slider::new(&mut self.local_edit.cue_mix, 0.0..=1.0)
                    .show_value(false)
                    .text("phones: cue ↔ master"),
            );
            ui.add(
                egui::Slider::new(&mut self.local_edit.hp_level, 0.0..=1.0)
                    .show_value(false)
                    .text("phones level"),
            );
            ui.add(
                egui::Slider::new(&mut self.local_edit.master_level, 0.0..=1.0)
                    .show_value(false)
                    .text("master level"),
            );
            if self.settings.audio.broadcast.is_some() {
                ui.add(
                    egui::Slider::new(&mut self.local_edit.bcast_level, 0.0..=1.0)
                        .show_value(false)
                        .text("broadcast level"),
                );
            }
        });
    }

    fn channel_strip(&mut self, ui: &mut egui::Ui, d: usize, view: &SessionView) {
        let st = view.room.decks[d];
        let deck = d as u8;
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(deck_letter(d).to_string())
                    .strong()
                    .color(DECK_COLORS[d]),
            );
            ui.spacing_mut().slider_width = 70.0;
            let mut trim = st.trim;
            let r = ui
                .add(
                    egui::Slider::new(&mut trim, 0.0..=1.0)
                        .show_value(false)
                        .text("TRIM"),
                )
                .on_hover_text("Double-click to reset");
            if r.changed() {
                self.pending.push(Action::Trim { deck, v: trim });
            }
            if r.double_clicked() {
                self.pending.push(Action::Trim { deck, v: 0.5 });
            }
            for (band, name) in [(2u8, "HI"), (1, "MID"), (0, "LOW")] {
                let mut v = st.eq[band as usize];
                let r = ui
                    .add(
                        egui::Slider::new(&mut v, 0.0..=1.0)
                            .show_value(false)
                            .text(name),
                    )
                    .on_hover_text("Double-click to reset");
                if r.changed() {
                    self.pending.push(Action::Eq { deck, band, v });
                }
                if r.double_clicked() {
                    self.pending.push(Action::Eq { deck, band, v: 0.5 });
                }
            }
            let mut filt = st.filter;
            let r = ui
                .add(
                    egui::Slider::new(&mut filt, 0.0..=1.0)
                        .show_value(false)
                        .text("FILTER"),
                )
                .on_hover_text("Left: low-pass. Right: high-pass. Double-click to reset.");
            if r.changed() {
                self.pending.push(Action::Filter { deck, v: filt });
            }
            if r.double_clicked() {
                self.pending.push(Action::Filter { deck, v: 0.5 });
            }
            ui.toggle_value(&mut self.local_edit.pfl[d], "🎧 CUE")
                .on_hover_text("Send this deck to your headphones (your booth only)");
            ui.spacing_mut().slider_width = 74.0;
            let mut v = st.volume;
            if ui
                .add(
                    egui::Slider::new(&mut v, 0.0..=1.0)
                        .vertical()
                        .show_value(false),
                )
                .changed()
            {
                self.pending.push(Action::Volume { deck, v });
            }
        });
    }
}

fn deck_status(dv: &DeckView, view: &SessionView) -> Option<(String, Color32)> {
    if let Some(l) = &dv.loading {
        return Some((format!("Loading {l}…"), AMBER));
    }
    let id = dv.state.track?;
    match dv.status.as_ref()? {
        TrackStatus::Searching => Some(("Looking for this track in your library…".into(), AMBER)),
        TrackStatus::Decoding => Some(("Decoding…".into(), AMBER)),
        TrackStatus::Fetching => {
            let pct = view
                .transfers
                .iter()
                .find(|t| t.id == id && !t.upload && t.total > 0)
                .map(|t| format!(" {:.0}%", t.done as f64 * 100.0 / t.total as f64))
                .unwrap_or_default();
            Some((
                format!("Receiving this track from your partner…{pct}"),
                AMBER,
            ))
        }
        TrackStatus::Failed(e) => Some((format!("Can't play: {e}"), RED)),
        TrackStatus::Ready => None,
    }
}

/// Whole-track overview; click to seek. Returns the seek position.
fn overview(ui: &mut egui::Ui, dv: &DeckView, pos: f64, col: Color32) -> Option<f64> {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, Color32::from_gray(16));
    let dur = dv.state.duration;
    if let (Some(a), true) = (&dv.audio, dur > 0.0) {
        let cols = rect.width().max(1.0) as usize;
        let n = a.overview.len();
        for x in 0..cols {
            let v = a.overview[(x * n / cols).min(n - 1)] as f32 / 255.0;
            let h = (v * rect.height() * 0.95).max(1.0);
            let cx = rect.left() + x as f32 + 0.5;
            let played = (x as f64 / cols as f64) < pos / dur;
            let c = if played {
                col.gamma_multiply(0.35)
            } else {
                col.gamma_multiply(0.85)
            };
            p.line_segment(
                [
                    pos2(cx, rect.center().y - h / 2.0),
                    pos2(cx, rect.center().y + h / 2.0),
                ],
                Stroke::new(1.0_f32, c),
            );
        }
    }
    if dur > 0.0 {
        let x_of = |t: f64| rect.left() + (t / dur) as f32 * rect.width();
        let cx = x_of(dv.state.cue_point);
        p.line_segment(
            [pos2(cx, rect.top()), pos2(cx, rect.bottom())],
            Stroke::new(1.0_f32, Color32::WHITE.gamma_multiply(0.6)),
        );
        for (i, hc) in dv.state.hot_cues.iter().enumerate().take(8) {
            if let Some(t) = hc {
                let x = x_of(*t);
                p.rect_filled(
                    egui::Rect::from_min_size(pos2(x - 1.0, rect.top()), vec2(3.0, 8.0)),
                    0.0,
                    HOT_CUE_COLORS[i],
                );
            }
        }
        let px = x_of(pos);
        p.line_segment(
            [pos2(px, rect.top()), pos2(px, rect.bottom())],
            Stroke::new(2.0_f32, Color32::WHITE),
        );
        if resp.clicked() || resp.drag_stopped() {
            if let Some(pp) = resp.interact_pointer_pos() {
                return Some(((pp.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * dur);
            }
        }
    }
    None
}

/// Scrolling waveform centred on the playhead. The window is 8 seconds of
/// real time, so two decks at the same effective BPM show the same beat spacing.
fn scrolling_waveform(ui: &mut egui::Ui, d: usize, dv: &DeckView, now: i64, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, Color32::from_gray(12));
    let col = DECK_COLORS[d];
    let st = dv.state;
    let pos = st.position_at(now);
    if let Some(a) = &dv.audio {
        let sr = a.sample_rate as f64;
        // Zoom follows the tempo fader only. A jog nudge (temporary bend)
        // slides the waveform instead of stretching it, like Serato.
        let span = 8.0 * st.tempo.max(0.05);
        let cols = rect.width().max(1.0) as usize;
        let per_col = span / cols as f64;
        for x in 0..cols {
            let t0 = pos + (x as f64 / cols as f64 - 0.5) * span;
            let t1 = t0 + per_col;
            if t1 <= 0.0 || t0 >= st.duration {
                continue;
            }
            let i0 = (t0.max(0.0) * sr / PEAK_BLOCK as f64) as usize;
            let i1 = ((t1 * sr / PEAK_BLOCK as f64) as usize)
                .max(i0 + 1)
                .min(a.peaks.len());
            if i0 >= a.peaks.len() {
                continue;
            }
            let v = a.peaks[i0..i1].iter().copied().max().unwrap_or(0) as f32 / 255.0;
            let h = (v * rect.height() * 0.95).max(1.0);
            let cx = rect.left() + x as f32 + 0.5;
            p.line_segment(
                [
                    pos2(cx, rect.center().y - h / 2.0),
                    pos2(cx, rect.center().y + h / 2.0),
                ],
                Stroke::new(1.0_f32, col.gamma_multiply(0.85)),
            );
        }
        let x_of = |t: f64| rect.center().x + ((t - pos) / span) as f32 * rect.width();
        for (i, hc) in st.hot_cues.iter().enumerate().take(8) {
            if let Some(t) = hc {
                let x = x_of(*t);
                if rect.x_range().contains(x) {
                    p.line_segment(
                        [pos2(x, rect.top()), pos2(x, rect.bottom())],
                        Stroke::new(1.5_f32, HOT_CUE_COLORS[i]),
                    );
                }
            }
        }
        let cx = x_of(st.cue_point);
        if rect.x_range().contains(cx) {
            p.line_segment(
                [pos2(cx, rect.top()), pos2(cx, rect.bottom())],
                Stroke::new(1.0_f32, Color32::WHITE.gamma_multiply(0.5)),
            );
        }
    }
    p.line_segment(
        [
            pos2(rect.center().x, rect.top()),
            pos2(rect.center().x, rect.bottom()),
        ],
        Stroke::new(2.0_f32, Color32::WHITE),
    );
    p.text(
        rect.left_top() + vec2(6.0, 4.0),
        egui::Align2::LEFT_TOP,
        deck_letter(d).to_string(),
        egui::FontId::proportional(14.0),
        col,
    );
}
