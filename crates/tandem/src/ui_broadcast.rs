//! The broadcast window: a second window, made for the audience.
//!
//! The broadcast feed plays the room `broadcast_delay_ms` behind, so both
//! DJs' moves land on time in it. This window draws the decks and mixer
//! exactly as far behind, from the same timeline, so a stream that shares it
//! shows every move at the moment its sound arrives. Nothing in it is
//! clickable: it's a picture of the set, not a second set of controls.

use crate::app::TandemApp;
use crate::ui_decks::{
    fmt_time, keys_fit, overview, scrolling_waveform, AMBER, DECK_COLORS, GREEN, LOOP, MIXER_ORDER,
    SYNC_BLUE,
};
use crate::ui_meters::MeterView;
use eframe::egui::{
    self, pos2, vec2, Align, Color32, Layout, Margin, Rect, RichText, Sense, Stroke,
};
use tandem_core::mapping::beats_label;
use tandem_core::model::{DeckState, XfSide, DECKS};
use tandem_core::session::{deck_letter, BroadcastView};

const BG: Color32 = Color32::from_gray(10);
const CARD: Color32 = Color32::from_gray(22);
const TRACK: Color32 = Color32::from_gray(40);
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
        let meters = self.bcast_meter_view;
        ctx.show_viewport_immediate(id, builder, |ctx, class| {
            let b = self.session.lock().broadcast_view();
            if class == egui::ViewportClass::Embedded {
                // No second window here: show it inside the main one instead.
                let mut open = true;
                egui::Window::new("Broadcast view")
                    .open(&mut open)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.allocate_ui(vec2(960.0, 540.0), |ui| draw(ui, &b, &meters));
                    });
                self.ui.broadcast_view = open;
                return;
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(BG).inner_margin(16.0))
                .show(ctx, |ui| draw(ui, &b, &meters));
            if ctx.input(|i| i.viewport().close_requested()) {
                self.ui.broadcast_view = false;
            }
        });
    }
}

/// Header, then deck A | mixer | deck B, then the scrolling waveforms.
/// While deck C or D has a track: A over C | mixer | B over D, and four
/// waveforms.
fn draw(ui: &mut egui::Ui, b: &BroadcastView, meters: &MeterView) {
    let four = b.room.decks[2..].iter().any(|d| d.track.is_some());
    let decks = if four { DECKS } else { 2 };
    header(ui, b);
    ui.add_space(GAP);
    let w = ui.available_width();
    let mixer_w = if four {
        (w * 0.22).clamp(190.0, 300.0)
    } else {
        (w * 0.17).clamp(150.0, 230.0)
    };
    let deck_w = ((w - mixer_w - 2.0 * GAP) / 2.0).max(150.0);
    let card_h = if four {
        (ui.available_height() * 0.27).clamp(110.0, 230.0)
    } else {
        (ui.available_height() * 0.38).clamp(170.0, 300.0)
    };
    let row_h = if four { 2.0 * card_h + GAP } else { card_h };
    ui.allocate_ui_with_layout(vec2(w, row_h), Layout::left_to_right(Align::Min), |ui| {
        ui.spacing_mut().item_spacing = vec2(GAP, GAP);
        let column = |ui: &mut egui::Ui, side: [usize; 2]| {
            ui.allocate_ui_with_layout(vec2(deck_w, row_h), Layout::top_down(Align::Min), |ui| {
                for &d in &side[..decks / 2] {
                    ui.allocate_ui_with_layout(
                        vec2(deck_w, card_h),
                        Layout::top_down(Align::Min),
                        |ui| card(ui, &|ui| deck_card(ui, d, b)),
                    );
                }
            });
        };
        column(ui, [0, 2]);
        ui.allocate_ui_with_layout(vec2(mixer_w, row_h), Layout::top_down(Align::Min), |ui| {
            card(ui, &|ui| mixer(ui, b, meters, four))
        });
        column(ui, [1, 3]);
    });
    ui.add_space(GAP);
    let n = decks as f32;
    let h = ((ui.available_height() - (n - 1.0) * GAP) / n).max(if four { 24.0 } else { 40.0 });
    for d in 0..decks {
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
    // big one doesn't look empty. A short card (four decks) scales down to fit.
    let s = (ui.available_width() / 480.0)
        .min((ui.available_height() - 30.0) / 120.0)
        .clamp(0.6, 1.4);

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
        if let Some(r) = st.roll {
            let text = format!("ROLL {}", beats_label(r.beats));
            chip(ui, &text, LOOP.gamma_multiply(0.7), 13.0 * s);
        } else if st.looping {
            let text = match st.loop_beats {
                Some(n) => format!("LOOP {}", beats_label(n)),
                None => "LOOP".to_string(),
            };
            chip(ui, &text, LOOP.gamma_multiply(0.7), 13.0 * s);
        }
        if b.room.master == Some(d as u8) {
            chip(ui, "MASTER", SYNC_BLUE, 13.0 * s);
        } else if st.synced {
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
            let fits = b
                .room
                .mix_partner(d)
                .and_then(|p| b.room.decks[p].sounding_key())
                .is_some_and(|o| keys_fit(k, o));
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

/// Read-only mixer: EQ and filter as bars from centre, channel faders with
/// their level meters, the master meter between them, and the crossfader
/// with the name of whoever last moved it. Four channels (C A B D) while
/// decks C and D are on screen, with the knob names once, on the left.
fn mixer(ui: &mut egui::Ui, b: &BroadcastView, meters: &MeterView, four: bool) {
    let order: &[usize] = if four { &MIXER_ORDER } else { &[0, 1] };
    ui.vertical_centered(|ui| ui.label(RichText::new("MIXER").strong().weak()));
    ui.add_space(4.0);
    let xf_h = 44.0;
    let strip_h = (ui.available_height() - xf_h).max(60.0);
    let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), strip_h), Sense::hover());
    let label_w = if four { 26.0 } else { 0.0 };
    let col_w = (area.width() - label_w) / order.len() as f32;
    let mut faders = Vec::with_capacity(order.len());
    for (i, &d) in order.iter().enumerate() {
        let left = area.left() + label_w + i as f32 * col_w;
        let rect = Rect::from_min_size(pos2(left, area.top()), vec2(col_w, area.height()))
            .shrink2(vec2(3.0, 0.0));
        let fader = ui
            .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                channel(ui, d, &b.room.decks[d], !four, b.room.xf_assign[d])
            })
            .inner;
        faders.push(fader);
    }
    let p = ui.painter();
    if four {
        for (k, label) in KNOB_LABELS.iter().enumerate() {
            let y = area.top() + 7.0 + k as f32 * 18.0;
            p.text(
                pos2(area.left(), y),
                egui::Align2::LEFT_CENTER,
                *label,
                egui::FontId::proportional(10.0),
                Color32::from_gray(140),
            );
        }
    }
    let half = order.len() / 2;
    let mid = (faders[half - 1].center().x + faders[half].center().x) / 2.0;
    for (i, (&d, f)) in order.iter().zip(&faders).enumerate() {
        let inward = if i < half { 1.0 } else { -1.0 };
        let x = f.center().x + inward * if four { 13.0 } else { 17.0 };
        let r = Rect::from_center_size(pos2(x, f.center().y), vec2(6.0, f.height()));
        meters.channel(p, r, d);
    }
    meters.master(
        p,
        Rect::from_center_size(
            pos2(mid, faders[0].center().y),
            vec2(if four { 10.0 } else { 14.0 }, faders[0].height()),
        ),
    );
    crossfader(ui, b, order);
}

const KNOB_LABELS: [&str; 4] = ["HI", "MID", "LOW", "FLT"];

/// One channel's knobs and fader (with its crossfader side under it while
/// it's off its usual side). Returns the fader's track.
fn channel(ui: &mut egui::Ui, d: usize, st: &DeckState, labels: bool, side: XfSide) -> Rect {
    let col = DECK_COLORS[d];
    ui.spacing_mut().item_spacing.y = 4.0;
    let values = [st.eq[2], st.eq[1], st.eq[0], st.filter];
    for (label, v) in KNOB_LABELS.iter().zip(values) {
        knob_bar(ui, labels.then_some(*label), v, col);
    }
    ui.add_space(4.0);
    let moved = side != XfSide::default_for(d);
    let h = (ui.available_height() - if moved { 18.0 } else { 4.0 }).max(30.0);
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
    if moved {
        let text = match side {
            XfSide::A => "◀",
            XfSide::Thru => "THRU",
            XfSide::B => "▶",
        };
        ui.vertical_centered(|ui| ui.label(RichText::new(text).small().weak()));
    }
    track
}

/// A knob (0.5 = centre) as a bar growing left or right from the middle,
/// with its name on the left if given.
fn knob_bar(ui: &mut egui::Ui, label: Option<&str>, v: f32, col: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::hover());
    let p = ui.painter();
    let label_w = if label.is_some() { 28.0 } else { 0.0 };
    if let Some(label) = label {
        p.text(
            pos2(r.left(), r.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(10.0),
            Color32::from_gray(140),
        );
    }
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

/// The crossfader, with the decks on each side (of `shown`) at its ends.
fn crossfader(ui: &mut egui::Ui, b: &BroadcastView, shown: &[usize]) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let p = ui.painter();
    let font = egui::FontId::proportional(13.0);
    let mut sides = [Vec::new(), Vec::new()];
    for d in 0..DECKS {
        let side = b.room.xf_assign[d];
        if shown.contains(&d) && side != XfSide::Thru {
            sides[(side == XfSide::B) as usize].push(d);
        }
    }
    let letter_w = 9.0;
    for (k, decks) in sides.iter().enumerate() {
        for (i, &d) in decks.iter().enumerate() {
            let (x, align) = if k == 0 {
                (r.left() + i as f32 * letter_w, egui::Align2::LEFT_CENTER)
            } else {
                let i = decks.len() - 1 - i;
                (r.right() - i as f32 * letter_w, egui::Align2::RIGHT_CENTER)
            };
            p.text(
                pos2(x, r.center().y),
                align,
                deck_letter(d),
                font.clone(),
                DECK_COLORS[d],
            );
        }
    }
    let inset = |n: usize| 7.0 + letter_w * n.max(1) as f32;
    let track = Rect::from_min_max(
        pos2(r.left() + inset(sides[0].len()), r.center().y - 2.0),
        pos2(r.right() - inset(sides[1].len()), r.center().y + 2.0),
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
