//! The broadcast window: a second window, made for the audience.
//!
//! The broadcast feed plays the room `broadcast_delay_ms` behind, so both
//! DJs' moves land on time in it. This window draws the decks and mixer
//! exactly as far behind, from the same timeline, so a stream that shares it
//! shows every move at the moment its sound arrives. Nothing in it is
//! clickable: it's a picture of the set, not a second set of controls.

use crate::app::TandemApp;
use crate::ui_decks::{
    fmt_time, keys_fit, overview, scrolling_waveform, AMBER, DECK_COLORS, GREEN,
};
use eframe::egui::{
    self, pos2, vec2, Align, Color32, Layout, Margin, Rect, RichText, Sense, Stroke,
};
use tandem_core::model::{DeckState, DECKS};
use tandem_core::session::{deck_letter, BroadcastView};

const BG: Color32 = Color32::from_gray(10);
const CARD: Color32 = Color32::from_gray(22);
const TRACK: Color32 = Color32::from_gray(40);
const SYNC_BLUE: Color32 = Color32::from_rgb(40, 120, 200);
const KEY_PURPLE: Color32 = Color32::from_rgb(110, 70, 170);
const GAP: f32 = 12.0;

impl TandemApp {
    /// The broadcast window, while it's switched on.
    ///
    /// It's an immediate viewport: drawn inside the main window's frame, at
    /// the main window's pace. (A deferred viewport repaints on its own, but
    /// in eframe 0.31 on Windows it starves the main window of repaints.)
    pub(crate) fn broadcast_window(&mut self, ctx: &egui::Context) {
        if !self.ui.broadcast_view {
            return;
        }
        let builder = egui::ViewportBuilder::default()
            .with_title("Tandem broadcast")
            .with_inner_size([1280.0, 720.0])
            .with_min_inner_size([640.0, 360.0]);
        let id = egui::ViewportId::from_hash_of("tandem-broadcast");
        ctx.show_viewport_immediate(id, builder, |ctx, class| {
            let b = self.session.lock().broadcast_view();
            if class == egui::ViewportClass::Embedded {
                // No second window here: show it inside the main one instead.
                let mut open = true;
                egui::Window::new("Broadcast view")
                    .open(&mut open)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.allocate_ui(vec2(960.0, 540.0), |ui| draw(ui, &b));
                    });
                self.ui.broadcast_view = open;
                return;
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(BG).inner_margin(16.0))
                .show(ctx, |ui| draw(ui, &b));
            if ctx.input(|i| i.viewport().close_requested()) {
                self.ui.broadcast_view = false;
            }
        });
    }
}

/// Header, then deck A | mixer | deck B, then both scrolling waveforms.
fn draw(ui: &mut egui::Ui, b: &BroadcastView) {
    header(ui, b);
    ui.add_space(GAP);
    let w = ui.available_width();
    let mixer_w = (w * 0.17).clamp(150.0, 230.0);
    let deck_w = ((w - mixer_w - 2.0 * GAP) / 2.0).max(150.0);
    let row_h = (ui.available_height() * 0.38).clamp(170.0, 300.0);
    ui.allocate_ui_with_layout(vec2(w, row_h), Layout::left_to_right(Align::Min), |ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        let cell = |ui: &mut egui::Ui, w: f32, add: &dyn Fn(&mut egui::Ui)| {
            ui.allocate_ui_with_layout(vec2(w, row_h), Layout::top_down(Align::Min), |ui| {
                card(ui, add)
            });
        };
        cell(ui, deck_w, &|ui| deck_card(ui, 0, b));
        cell(ui, mixer_w, &|ui| mixer(ui, b));
        cell(ui, deck_w, &|ui| deck_card(ui, 1, b));
    });
    ui.add_space(GAP);
    let h = ((ui.available_height() - GAP) / 2.0).max(40.0);
    for d in 0..DECKS {
        if d > 0 {
            ui.add_space(GAP);
        }
        scrolling_waveform(ui, d, &b.decks[d], b.t, h, false);
    }
}

/// "TANDEM  Alice b2b Bob".
fn header(ui: &mut egui::Ui, b: &BroadcastView) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("TANDEM")
                .strong()
                .size(22.0)
                .color(DECK_COLORS[0]),
        );
        ui.add_space(10.0);
        for (i, dj) in b.djs.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new("b2b").size(16.0).weak());
            }
            ui.label(RichText::new(&dj.name).size(20.0).strong());
        }
    });
}

fn card(ui: &mut egui::Ui, add: &dyn Fn(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(8.0)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            add(ui)
        });
}

fn chip(ui: &mut egui::Ui, text: &str, fill: Color32, size: f32) {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(4.0)
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .strong()
                    .size(size)
                    .color(Color32::WHITE),
            )
        });
}

/// What the deck is doing, for the chip next to its letter.
fn deck_state(st: &DeckState, pos: f64) -> Option<(&'static str, Color32)> {
    st.track?;
    Some(if st.scratching {
        ("SCRATCH", AMBER.gamma_multiply(0.7))
    } else if st.cue_preview {
        ("CUE", AMBER.gamma_multiply(0.7))
    } else if st.playing && pos < st.duration {
        ("▶ PLAYING", GREEN.gamma_multiply(0.45))
    } else {
        ("⏸ PAUSED", Color32::from_gray(60))
    })
}

fn deck_card(ui: &mut egui::Ui, d: usize, b: &BroadcastView) {
    let dv = &b.decks[d];
    let st = &dv.state;
    let pos = st.position_at(b.t);
    let col = DECK_COLORS[d];
    // Text scales with the window, so a small capture stays readable and a
    // big one doesn't look empty.
    let s = (ui.available_width() / 480.0).clamp(0.7, 1.4);

    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0) * s, Sense::hover());
        ui.painter().circle_filled(r.center(), r.width() / 2.0, col);
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            deck_letter(d),
            egui::FontId::proportional(16.0 * s),
            Color32::BLACK,
        );
        if let Some((label, fill)) = deck_state(st, pos) {
            chip(ui, label, fill, 13.0 * s);
        }
        if st.synced {
            chip(ui, "SYNC", SYNC_BLUE, 13.0 * s);
        }
        if st.key_shift != 0 {
            chip(ui, &format!("KEY {:+}", st.key_shift), KEY_PURPLE, 13.0 * s);
        }
        // Whose hands are on this deck: the fun part of watching a b2b.
        if let Some(name) = b.dj_name(st.last_touch) {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                egui::Frame::new()
                    .stroke(Stroke::new(1.5_f32, col))
                    .corner_radius(12.0)
                    .inner_margin(Margin::symmetric(10, 2))
                    .show(ui, |ui| {
                        ui.label(RichText::new(name).strong().size(15.0 * s).color(col))
                    });
            });
        }
    });
    ui.add_space(4.0 * s);

    let (title, artist) = match &dv.meta {
        Some(m) => (m.title.clone(), m.artist.clone()),
        None if st.track.is_some() => ("…".into(), String::new()),
        None => ("No track".into(), String::new()),
    };
    let title = RichText::new(title).size(24.0 * s).strong();
    ui.add(
        egui::Label::new(if st.track.is_some() {
            title
        } else {
            title.weak()
        })
        .truncate(),
    );
    ui.add(egui::Label::new(RichText::new(artist).size(16.0 * s).weak()).truncate());
    if st.track.is_none() {
        return;
    }
    ui.add_space(6.0 * s);

    let big = 20.0 * s;
    let small = 12.0 * s;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0 * s;
        ui.label(RichText::new(fmt_time(pos)).monospace().size(big));
        ui.label(
            RichText::new(format!("-{}", fmt_time(st.duration - pos)))
                .monospace()
                .size(big * 0.75)
                .weak(),
        );
        ui.add_space(8.0 * s);
        let bpm = st.effective_bpm_at(b.t).or_else(|| {
            dv.meta
                .as_ref()
                .and_then(|m| m.bpm)
                .map(|v| v as f64 * st.tempo)
        });
        if let Some(v) = bpm {
            ui.label(RichText::new(format!("{v:.1}")).monospace().size(big));
            ui.label(RichText::new("BPM").size(small).weak());
            ui.add_space(8.0 * s);
        }
        if let Some(k) = st.sounding_key() {
            let other = &b.room.decks[(d + 1) % 2];
            let fits = other.track.is_some()
                && other
                    .sounding_key()
                    .map(|o| keys_fit(k, o))
                    .unwrap_or(false);
            let text = RichText::new(tandem_core::keys::camelot(k))
                .size(big)
                .strong();
            ui.label(if fits { text.color(GREEN) } else { text });
            ui.add_space(8.0 * s);
        }
        if let Some(g) = st.grid {
            let (bar, beat) = g.bar_beat_at(pos);
            ui.label(RichText::new(format!("{bar}.{beat}")).monospace().size(big));
            ui.label(RichText::new("bar").size(small).weak());
        }
    });
    ui.add_space(8.0 * s);
    let h = (ui.available_height() - 2.0).clamp(18.0, 80.0);
    overview(ui, dv, pos, col, false, h, false);
}

/// Read-only mixer: EQ and filter as bars from centre, channel faders and
/// the crossfader, with the name of whoever last moved it.
fn mixer(ui: &mut egui::Ui, b: &BroadcastView) {
    ui.vertical_centered(|ui| ui.label(RichText::new("MIXER").strong().weak()));
    ui.add_space(4.0);
    let xf_h = 44.0;
    let strip_h = (ui.available_height() - xf_h).max(60.0);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), strip_h),
        Layout::left_to_right(Align::Min),
        |ui| {
            ui.columns(DECKS, |cols| {
                for (d, ui) in cols.iter_mut().enumerate() {
                    channel(ui, d, &b.room.decks[d]);
                }
            });
        },
    );
    crossfader(ui, b);
}

fn channel(ui: &mut egui::Ui, d: usize, st: &DeckState) {
    let col = DECK_COLORS[d];
    ui.spacing_mut().item_spacing.y = 4.0;
    for (label, v) in [
        ("HI", st.eq[2]),
        ("MID", st.eq[1]),
        ("LOW", st.eq[0]),
        ("FLT", st.filter),
    ] {
        knob_bar(ui, label, v, col);
    }
    ui.add_space(4.0);
    let h = (ui.available_height() - 4.0).max(30.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let track = Rect::from_center_size(r.center(), vec2(16.0, r.height()));
    let p = ui.painter();
    p.rect_filled(track, 3.0, TRACK);
    let level = track.bottom() - st.volume.clamp(0.0, 1.0) * track.height();
    p.rect_filled(
        Rect::from_min_max(pos2(track.left(), level), track.max),
        3.0,
        col.gamma_multiply(0.85),
    );
    p.line_segment(
        [
            pos2(track.left() - 4.0, level),
            pos2(track.right() + 4.0, level),
        ],
        Stroke::new(2.0_f32, Color32::WHITE),
    );
}

/// A knob (0.5 = centre) as a bar growing left or right from the middle.
fn knob_bar(ui: &mut egui::Ui, label: &str, v: f32, col: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::hover());
    let p = ui.painter();
    let label_w = 28.0;
    p.text(
        pos2(r.left(), r.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(10.0),
        Color32::from_gray(140),
    );
    let bar = Rect::from_min_max(
        pos2(r.left() + label_w, r.top() + 3.0),
        pos2(r.right(), r.bottom() - 3.0),
    );
    p.rect_filled(bar, 2.0, TRACK);
    let x = |v: f32| bar.left() + v.clamp(0.0, 1.0) * bar.width();
    let (a, z) = (x(0.5).min(x(v)), x(0.5).max(x(v)));
    if z - a > 0.5 {
        p.rect_filled(
            Rect::from_x_y_ranges(a..=z, bar.y_range()),
            2.0,
            col.gamma_multiply(0.8),
        );
    }
    p.line_segment(
        [
            pos2(x(0.5), bar.top() - 2.0),
            pos2(x(0.5), bar.bottom() + 2.0),
        ],
        Stroke::new(1.0_f32, Color32::from_gray(110)),
    );
}

fn crossfader(ui: &mut egui::Ui, b: &BroadcastView) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let p = ui.painter();
    let font = egui::FontId::proportional(13.0);
    p.text(
        pos2(r.left(), r.center().y),
        egui::Align2::LEFT_CENTER,
        "A",
        font.clone(),
        DECK_COLORS[0],
    );
    p.text(
        pos2(r.right(), r.center().y),
        egui::Align2::RIGHT_CENTER,
        "B",
        font,
        DECK_COLORS[1],
    );
    let track = Rect::from_min_max(
        pos2(r.left() + 16.0, r.center().y - 2.0),
        pos2(r.right() - 16.0, r.center().y + 2.0),
    );
    p.rect_filled(track, 2.0, TRACK);
    let x = track.left() + b.room.crossfader.clamp(0.0, 1.0) * track.width();
    p.rect_filled(
        Rect::from_center_size(pos2(x, r.center().y), vec2(10.0, 18.0)),
        2.0,
        Color32::WHITE,
    );
    let name = b.dj_name(b.room.xf_touch).unwrap_or("");
    ui.vertical_centered(|ui| ui.label(RichText::new(name).small().weak()));
}
