//! Top bar, decks, mixer and waveforms.

use crate::app::TandemApp;
use crate::ui_meters;
use eframe::egui::{self, pos2, vec2, Align, Color32, Layout, Rect, RichText, Sense, Stroke};
use tandem_core::audio::PEAK_BLOCK;
use tandem_core::grid::{BeatGrid, Marker, MAX_MARKERS};
use tandem_core::mapping::{beats_label, MidiEngine, Target};
use tandem_core::model::{Action, DeckState, XfCurve, XfSide, BEAT_LOOPS, DECKS, HOT_CUES};
use tandem_core::net::{LINK_QUIET, LINK_TIMEOUT, REJOIN_FOR};
use tandem_core::session::{
    deck_letter, DeckView, RejoinView, RoleView, SessionView, TrackStatus, TransferState,
};

pub const DECK_COLORS: [Color32; DECKS] = [
    Color32::from_rgb(255, 146, 60),
    Color32::from_rgb(70, 190, 255),
    Color32::from_rgb(185, 130, 255),
    Color32::from_rgb(170, 225, 70),
];
/// Mixer channels left to right in the four-deck view, as on a
/// four-channel DJ mixer (and the DDJ-FLX6): C, A, B, D.
pub(crate) const MIXER_ORDER: [usize; DECKS] = [2, 0, 1, 3];
/// A compact deck panel's height (four decks, or two on a small window), and
/// the gap between two.
const COMPACT_H: f32 = 160.0;
const COMPACT_GAP: f32 = 4.0;
/// A compact panel's track overview, and the least it shrinks to when the
/// window is short.
const OVERVIEW_H: f32 = 26.0;
const OVERVIEW_MIN_H: f32 = 12.0;
/// The widest row of a compact deck panel (its loop row), at full size.
const COMPACT_ROW_W: f32 = 410.0;
/// A compact panel's width with that row at full size, inside its frame.
const COMPACT_DECK_W: f32 = COMPACT_ROW_W + 12.0;
/// The narrowest a full deck panel (two-deck view) goes: its loop row.
const FULL_DECK_W: f32 = 432.0;
/// The two-deck mixer's width, and the least the four-deck mixer and the
/// short two-deck mixer narrow to so the decks beside them keep their width.
const MIXER_W: f32 = 290.0;
const MIXER4_W: f32 = 344.0;
const MIXER_MIN_W: f32 = 260.0;
/// The controls row as drawn: full two-deck panels (the mixer is their
/// tallest part), four compact panels, two compact panels beside the short
/// two-deck mixer. Each includes the spacing after it.
const FULL_ROW_H: f32 = 388.0;
const FOUR_ROW_H: f32 = 2.0 * COMPACT_H + COMPACT_GAP + 3.0;
const COMPACT_ROW_H: f32 = 186.0;
/// How much shorter a compact panel gets with tighter margins and rows.
const TIGHT_SAVE: f32 = 9.0;
/// The four-deck row squeezed as far as it goes: thin overviews, tight
/// panels, the mixer's booth levels behind a button.
const FOUR_ROW_MIN_H: f32 = FOUR_ROW_H - 2.0 * (OVERVIEW_H - OVERVIEW_MIN_H) - 2.0 * TIGHT_SAVE;
/// Between the controls and the waveforms.
const WAVE_GAP: f32 = 6.0;
/// The top bar shows the room's address and clock spread from this width.
const TOP_BAR_FULL_W: f32 = 1300.0;
/// Scrolling waveform heights, two decks then four: the most they grow to,
/// what the library leaves room for, and the least they ever get (the
/// controls above them scroll instead).
const WAVE_MAX_H: [f32; 2] = [90.0, 80.0];
const WAVE_ROOMY_H: [f32; 2] = [60.0, 40.0];
const WAVE_MIN_H: [f32; 2] = [46.0, 28.0];
/// Waveforms shorter than this make two decks take compact panels.
const FULL_WAVE_MIN_H: f32 = 52.0;
/// The least height the controls row keeps (scrolled) when the library is
/// dragged up as far as it goes.
const CONTROLS_MIN_H: f32 = 110.0;
pub(crate) const SYNC_BLUE: Color32 = Color32::from_rgb(40, 120, 200);
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
pub(crate) const GREEN: Color32 = Color32::from_rgb(90, 220, 120);
const RED: Color32 = Color32::from_rgb(240, 90, 90);
pub(crate) const AMBER: Color32 = Color32::from_rgb(250, 190, 60);
/// Beatgrid markers (red, like Serato's).
const MARKER: Color32 = Color32::from_rgb(235, 70, 70);
/// Loops, on the buttons and the waveforms.
pub(crate) const LOOP: Color32 = Color32::from_rgb(60, 200, 150);

/// ADJUST steps (BPM): a little, a bit more, more than that.
const ADJUST_STEPS: [f64; 3] = [0.01, 0.1, 1.0];
/// SLIP steps (seconds).
const SLIP_STEPS: [f64; 3] = [0.001, 0.005, 0.025];

/// How the decks area lays out its controls, from its size. Big windows get
/// the full two-deck panels; a short or narrow one gets compact panels and
/// the short mixer, as the four-deck view always has.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DeckLayout {
    /// Two decks with full panels and the two-channel mixer.
    Full,
    /// Two compact deck panels beside the short two-channel mixer.
    Compact,
    /// Four compact deck panels and the four-channel mixer.
    Four,
}

impl DeckLayout {
    /// The layout for a decks area of `width` × `height` points.
    pub(crate) fn pick(four: bool, width: f32, height: f32) -> Self {
        if four {
            DeckLayout::Four
        } else if Self::full_fits_width(width) && height >= DeckLayout::Full.least_full_h() {
            DeckLayout::Full
        } else {
            DeckLayout::Compact
        }
    }

    /// Whether full deck panels fit beside the mixer.
    pub(crate) fn full_fits_width(width: f32) -> bool {
        (width - MIXER_W - 2.0 * ITEM_GAP) / 2.0 >= FULL_DECK_W
    }

    /// How many decks (and waveforms) it shows.
    pub(crate) fn decks(self) -> usize {
        match self {
            DeckLayout::Four => DECKS,
            _ => 2,
        }
    }

    /// Which entry of the `WAVE_*_H` tables it uses: two decks, or four.
    fn wave_idx(self) -> usize {
        match self {
            DeckLayout::Four => 1,
            _ => 0,
        }
    }

    /// The controls row, at its natural height and squeezed.
    fn row_h(self) -> (f32, f32) {
        match self {
            DeckLayout::Full => (FULL_ROW_H, FULL_ROW_H),
            DeckLayout::Compact => (COMPACT_ROW_H, COMPACT_ROW_H),
            DeckLayout::Four => (FOUR_ROW_H, FOUR_ROW_MIN_H),
        }
    }

    /// The waveforms and the gaps around them, `wave` points each.
    fn waves_h(self, wave: f32) -> f32 {
        WAVE_GAP + self.decks() as f32 * wave + self.wave_spacing()
    }

    /// Room for the spacing between and after the waveforms.
    fn wave_spacing(self) -> f32 {
        8.0 + (self.decks() - 2) as f32 * 1.5 * ITEM_SPACING_Y
    }

    /// The decks area's height with the controls at their natural height
    /// and roomy waveforms: what the library leaves when there's room.
    pub(crate) fn roomy_h(self) -> f32 {
        self.row_h().0 + self.waves_h(WAVE_ROOMY_H[self.wave_idx()])
    }

    /// The least height the full panels are used at.
    fn least_full_h(self) -> f32 {
        FULL_ROW_H + self.waves_h(FULL_WAVE_MIN_H)
    }

    /// The least height of the decks area: waveforms at their least and
    /// a scrolled strip of controls.
    pub(crate) fn least_h(self) -> f32 {
        CONTROLS_MIN_H + self.waves_h(WAVE_MIN_H[self.wave_idx()])
    }
}

/// egui's default spacing between widgets (`Spacing::item_spacing`).
const ITEM_GAP: f32 = 8.0;
const ITEM_SPACING_Y: f32 = 3.0;

/// How a compact deck panel fits its place.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PanelFit {
    /// The panel's height, frame included.
    h: f32,
    /// Its track overview's height.
    overview_h: f32,
    /// Its buttons' widths, from 1 (as wide as usual) down, for a narrow panel.
    k: f32,
    /// Tighter margins and rows, for a short window.
    tight: bool,
}

/// How much narrower a compact panel `deck_w` wide draws its buttons, so its
/// rows fit inside it.
fn button_scale(deck_w: f32) -> f32 {
    ((deck_w - 12.0) / COMPACT_ROW_W).clamp(0.7, 1.0)
}

/// Camelot-compatible: same key, relative major/minor, or one step round the wheel.
/// Who's broadcasting this room, if anyone.
fn broadcasters(ui: &mut egui::Ui, view: &SessionView) {
    let names: Vec<&str> = view
        .link
        .roster
        .broadcasters
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    if names.is_empty() {
        return;
    }
    ui.label(RichText::new(format!("📡 {}", names.join(", "))).weak())
        .on_hover_text("Broadcasting this room: they see and hear the set as the stream does, and can't change anything.");
}

pub(crate) fn keys_fit(a: i8, b: i8) -> bool {
    let (ca, cb) = (tandem_core::keys::camelot(a), tandem_core::keys::camelot(b));
    let split = |c: &str| {
        (
            c[..c.len() - 1].parse::<i32>().unwrap_or(0),
            c.ends_with('A'),
        )
    };
    let ((na, ma), (nb, mb)) = (split(&ca), split(&cb));
    let dist = (na - nb).rem_euclid(12).min((nb - na).rem_euclid(12));
    (ma == mb && dist <= 1) || (na == nb)
}

pub fn fmt_time(s: f64) -> String {
    let s = s.max(0.0);
    let m = (s / 60.0).floor();
    format!("{}:{:04.1}", m as i64, s - m * 60.0)
}

impl TandemApp {
    pub(crate) fn top_bar(&mut self, ctx: &egui::Context, view: &SessionView) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            // On a narrow window the address and the clock spread move into
            // hover text, so the room's status doesn't run into the buttons.
            let narrow = ui.available_width() < TOP_BAR_FULL_W;
            let detail = |ui: &mut egui::Ui, room: egui::Response, text: String| {
                if narrow {
                    room.on_hover_text(text);
                } else {
                    ui.label(RichText::new(text).weak());
                }
            };
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
                        let r = ui.label(RichText::new(format!("Hosting room {room}")).strong());
                        detail(ui, r, format!("port {port}"));
                        self.peer_status(ui, view, narrow);
                        broadcasters(ui, view);
                        if ui.button("Leave").clicked() {
                            self.session.lock().leave();
                        }
                    }
                    RoleView::Joined { addr, room } => {
                        let r = ui.label(RichText::new(format!("In room {room}")).strong());
                        detail(ui, r, addr.clone());
                        if let Some(r) = &view.link.rejoin {
                            self.rejoin_status(ui, r, false);
                        } else {
                            self.peer_status(ui, view, narrow);
                            broadcasters(ui, view);
                            if ui.button("Leave").clicked() {
                                self.session.lock().leave();
                            }
                        }
                    }
                    RoleView::Broadcasting { addr, room } => {
                        let r = ui
                            .label(RichText::new(format!("📡 Broadcasting room {room}")).strong());
                        detail(ui, r, addr.clone());
                        if let Some(r) = &view.link.rejoin {
                            self.rejoin_status(ui, r, true);
                        } else {
                            self.peer_status(ui, view, narrow);
                            let delay = self.bcast_delay_ms();
                            if let Some(late) = view
                                .link
                                .late_us
                                .filter(|&l| crate::ui_broadcaster::late_warning(l, delay))
                            {
                                ui.label(
                                    RichText::new(format!("⚠ moves {:.0} ms late", late as f64 / 1000.0))
                                        .color(AMBER),
                                )
                                .on_hover_text(format!("The DJs' moves are getting here too close to the {delay} ms broadcast delay, so some land late on the stream. TIMING, in this window, can raise the delay."));
                            }
                            if ui.button("Leave").clicked() {
                                self.session.lock().leave();
                            }
                        }
                    }
                }
                let broadcasting = matches!(view.link.role, RoleView::Broadcasting { .. });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("⚙ Settings").clicked() {
                        self.ui.show_settings = !self.ui.show_settings;
                    }
                    if ui.button("Terms").clicked() {
                        self.ui.show_terms = true;
                    }
                    if !broadcasting {
                        ui.toggle_value(&mut self.local_edit.click, "Sync click")
                            .on_hover_text("A metronome on the shared room clock. Both booths click at the same instant: put two laptops side by side to hear how well they're synced.");
                    }
                    let delay = self.settings.audio.broadcast_delay_ms;
                    let tip = if broadcasting {
                        "The window to share or capture for your stream: the decks and mixer exactly as this booth plays them, so the picture matches the sound.".to_string()
                    } else {
                        format!("A second window for your stream: the decks and mixer as the broadcast feed plays them, {delay} ms behind the room, so the picture matches the sound. Share or capture that window, and take the sound from the broadcast feed (Settings → Audio).")
                    };
                    ui.toggle_value(&mut self.ui.broadcast_view, "Broadcast view")
                        .on_hover_text(tip);
                    // Decks C and D can't be hidden while one is playing.
                    let busy = view.room.decks[2..].iter().any(|d| d.playing);
                    let mut four = self.settings.four_decks;
                    if !broadcasting {
                        let r = ui
                            .add_enabled(!(busy && four), egui::SelectableLabel::new(four, "4 decks"))
                            .on_hover_text("Show decks C and D: A and C on the left, B and D on the right, and a four-channel mixer. It switches on by itself when deck C or D gets a track.")
                            .on_disabled_hover_text("Deck C or D is playing.");
                        if r.clicked() {
                            four = !four;
                            self.settings.four_decks = four;
                            self.mark_dirty();
                        }
                    }
                });
            });
            ui.add_space(2.0);
        });
    }

    /// The link dropped and this booth is getting back in.
    fn rejoin_status(&mut self, ui: &mut egui::Ui, r: &RejoinView, broadcasting: bool) {
        let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 4.0, AMBER);
        let text = match r.attempt {
            0 => format!("Reconnecting to {}…", r.host),
            n => format!("Reconnecting to {}… attempt {n}", r.host),
        };
        let mins = REJOIN_FOR.as_secs() / 60;
        let mut tip = if broadcasting {
            format!("The link to the room dropped, so Tandem is getting this booth back in. It keeps trying for {mins} minutes. The stream plays on as the room last was, and follows the room again once it's back.")
        } else {
            format!("The link to the room dropped, so Tandem is getting you back in. It keeps trying for {mins} minutes. Your decks play on meanwhile, but nothing you change reaches the room, and once you're back they follow the room again.")
        };
        if let Some(e) = &r.error {
            tip.push_str(&format!("\n\nLast try: {e}"));
        }
        ui.label(RichText::new(text).color(AMBER))
            .on_hover_text(tip);
        if ui
            .button("Give up")
            .on_hover_text("Stop trying and leave the room.")
            .clicked()
        {
            self.session.lock().leave();
        }
    }

    /// Your partner, the round trip and whether the room is in sync.
    /// `narrow`: the clock spread goes in the round trip's hover text.
    fn peer_status(&mut self, ui: &mut egui::Ui, view: &SessionView, narrow: bool) {
        let Some(peer) = &view.link.peer else {
            match &view.link.awaiting {
                Some(name) => {
                    ui.label(RichText::new(format!("Waiting for {name} to reconnect…")).color(AMBER))
                        .on_hover_text(format!("The link to {name} dropped. Their Tandem keeps trying to get back in for {} minutes, and the room carries on meanwhile.", REJOIN_FOR.as_secs() / 60));
                }
                None => {
                    ui.label(RichText::new("Waiting for your partner to join…").color(AMBER));
                }
            }
            return;
        };
        let broadcasting = matches!(view.link.role, RoleView::Broadcasting { .. });
        // A stalled link keeps its last round trip, which would look healthy.
        let quiet = view.link.quiet_for.filter(|q| *q >= LINK_QUIET);
        let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
        ui.painter().circle_filled(
            dot.center(),
            4.0,
            if quiet.is_some() { AMBER } else { GREEN },
        );
        let name = if broadcasting {
            let djs: Vec<&str> = view
                .link
                .roster
                .djs
                .iter()
                .map(|p| p.name.as_str())
                .collect();
            djs.join(" b2b ")
        } else {
            peer.name.clone()
        };
        ui.label(RichText::new(name).color(GREEN).strong());
        if let Some(q) = quiet {
            let (other, after) = match &view.link.role {
                RoleView::Broadcasting { .. } => ("the host", "this booth reconnects by itself"),
                RoleView::Joined { .. } => ("your partner", "Tandem reconnects you by itself"),
                _ => ("your partner", "their Tandem reconnects by itself"),
            };
            ui.label(RichText::new(format!("⚠ no reply for {} s", q.as_secs())).color(AMBER))
                .on_hover_text(format!(
                    "Nothing has arrived from {other} for a while: a connection may have dropped. If it comes back within {} s the room carries on as before; after that, {after}.",
                    LINK_TIMEOUT.as_secs()
                ));
            return;
        }
        let clock = view
            .link
            .clock_spread_us
            .map(|spread| format!("{:.1} ms", spread as f64 / 2000.0));
        if let Some(rtt) = view.link.rtt_us {
            let mut tip = if broadcasting {
                "Time for a message to reach the host and come back. This booth plays the room a broadcast delay behind, so both DJs' moves are on time in it."
            } else {
                "Time for a message to reach your partner and come back. Your own controls are always instant; your partner's arrive about half this late in your headphones, and exactly on time in the broadcast."
            }
            .to_string();
            if let (true, Some(c)) = (narrow, &clock) {
                tip = format!("{tip}\n\nShared clock accurate to about ±{c}.");
            }
            ui.label(format!("{:.0} ms round trip", rtt as f64 / 1000.0))
                .on_hover_text(tip);
        }
        if let (false, Some(c)) = (narrow, clock) {
            ui.label(RichText::new(format!("clock ±{c}")).weak());
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

    /// The decks, the mixer and the scrolling waveforms underneath. The
    /// waveforms always keep at least `WAVE_MIN_H`: on a window too short
    /// for the controls above them, the controls squeeze (compact panels,
    /// thinner overviews, the booth levels behind a button) and, past that,
    /// scroll.
    pub(crate) fn decks_area(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        let layout = DeckLayout::pick(
            self.settings.four_decks,
            ui.available_width(),
            ui.available_height(),
        );
        let (n, w) = (layout.decks(), layout.wave_idx());
        // Room for the controls with the waveforms at their least.
        let room = ui.available_height() - layout.waves_h(WAVE_MIN_H[w]);
        let (natural, least) = layout.row_h();
        let squeeze = (natural - room).clamp(0.0, natural - least);
        ui.scope(|ui| {
            if room < least {
                // A scroll bar that stays in sight, so it's clear there's more.
                ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
            }
            egui::ScrollArea::vertical()
                .id_salt("deck-controls")
                .max_height(room.max(40.0))
                .auto_shrink([false, true])
                .show(ui, |ui| match layout {
                    DeckLayout::Full => self.full_row(ui, view),
                    DeckLayout::Compact => self.compact_row(ui, view),
                    DeckLayout::Four => self.four_row(ui, view, squeeze),
                });
        });
        ui.add_space(WAVE_GAP);
        let h = ((ui.available_height() - layout.wave_spacing()) / n as f32)
            .clamp(WAVE_MIN_H[w], WAVE_MAX_H[w]);
        for d in 0..n {
            scrolling_waveform(ui, d, &view.decks[d], view.now, h, self.ui.grid_edit[d]);
        }
    }

    /// Two decks with full panels, the mixer between them.
    fn full_row(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        let gap = ui.spacing().item_spacing.x;
        let deck_w = ((ui.available_width() - MIXER_W - 2.0 * gap) / 2.0).max(260.0);
        let top_h = 352.0;
        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
            ui.allocate_ui_with_layout(vec2(deck_w, top_h), Layout::top_down(Align::Min), |ui| {
                self.deck_panel(ui, 0, view)
            });
            ui.allocate_ui_with_layout(vec2(MIXER_W, top_h), Layout::top_down(Align::Min), |ui| {
                self.mixer_panel(ui, view, 338.0, false)
            });
            ui.allocate_ui_with_layout(vec2(deck_w, top_h), Layout::top_down(Align::Min), |ui| {
                self.deck_panel(ui, 1, view)
            });
        });
    }

    /// Two compact deck panels beside the short two-channel mixer, for a
    /// window too short or narrow for the full panels. The panels stretch
    /// to the mixer's height and give the room to their overviews.
    fn compact_row(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        let gap = ui.spacing().item_spacing.x;
        let width = ui.available_width();
        let mixer_w = (width - 2.0 * gap - 2.0 * COMPACT_DECK_W).clamp(MIXER_MIN_W, MIXER_W);
        let deck_w = (width - mixer_w - 2.0 * gap) / 2.0;
        let origin = ui.available_rect_before_wrap().min;
        // A column at x, w wide, as tall as its contents.
        let column = |ui: &mut egui::Ui, salt: &str, x: f32, w: f32| {
            ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(salt)
                    .max_rect(Rect::from_min_size(pos2(x, origin.y), vec2(w, 2000.0)))
                    .layout(Layout::top_down(Align::Min)),
            )
        };
        // The mixer first, to size the deck panels to it.
        let mut mixer = column(ui, "mixer", origin.x + deck_w + gap, mixer_w);
        self.mixer_compact(&mut mixer, view);
        let row_h = mixer.min_rect().height();
        let fit = PanelFit {
            h: row_h,
            overview_h: (OVERVIEW_H + row_h - COMPACT_H).clamp(OVERVIEW_H, 42.0),
            k: button_scale(deck_w),
            tight: false,
        };
        for (d, x) in [(0, origin.x), (1, origin.x + deck_w + mixer_w + 2.0 * gap)] {
            let mut deck = column(ui, if d == 0 { "deck-a" } else { "deck-b" }, x, deck_w);
            self.deck_panel_compact(&mut deck, d, view, fit);
        }
        ui.allocate_rect(
            Rect::from_min_size(origin, vec2(width, row_h)),
            Sense::hover(),
        );
    }

    /// Four decks: A over C on the left, B over D on the right, each in a
    /// compact panel, and the four-channel mixer between them. `squeeze`:
    /// how much shorter than usual the row has to be (thinner overviews,
    /// then tighter panels, and the booth levels behind a button).
    fn four_row(&mut self, ui: &mut egui::Ui, view: &SessionView, squeeze: f32) {
        let gap = ui.spacing().item_spacing.x;
        let width = ui.available_width();
        let mixer_w = (width - 2.0 * gap - 2.0 * COMPACT_DECK_W).clamp(MIXER_MIN_W, MIXER4_W);
        let deck_w = ((width - mixer_w - 2.0 * gap) / 2.0).max(240.0);
        // Thinner overviews first, then tighter panels.
        let thinner = squeeze.min(2.0 * (OVERVIEW_H - OVERVIEW_MIN_H));
        let tight = squeeze > thinner;
        let fit = PanelFit {
            h: COMPACT_H - thinner / 2.0 - if tight { TIGHT_SAVE } else { 0.0 },
            overview_h: OVERVIEW_H - thinner / 2.0,
            k: button_scale(deck_w),
            tight,
        };
        let top_h = 2.0 * fit.h + COMPACT_GAP;
        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
            for column in [[0, 2], [1, 3]] {
                ui.allocate_ui_with_layout(
                    vec2(deck_w, top_h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = COMPACT_GAP;
                        for d in column {
                            ui.allocate_ui_with_layout(
                                vec2(deck_w, fit.h),
                                Layout::top_down(Align::Min),
                                |ui| self.deck_panel_compact(ui, d, view, fit),
                            );
                        }
                    },
                );
                if column[0] == 0 {
                    ui.allocate_ui_with_layout(
                        vec2(mixer_w, top_h),
                        Layout::top_down(Align::Min),
                        |ui| self.mixer_panel(ui, view, top_h - 14.0, squeeze > 0.0),
                    );
                }
            }
        });
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
                if let Some(r) = st.roll {
                    roll_label(ui, r.beats);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.grid_button(ui, d, &st, pos);
                });
            });
            let (title, artist) = track_names(dv);
            ui.add(egui::Label::new(RichText::new(title).size(17.0).strong()).truncate());
            ui.add(egui::Label::new(RichText::new(artist).weak()).truncate());
            let status = deck_status(dv, view, false);
            ui.label(match status {
                Some((s, c)) => RichText::new(s).color(c).small(),
                None => RichText::new(" ").small(),
            });

            ui.horizontal(|ui| deck_readout(ui, d, view, pos, 15.0));

            if let Some(seek) = overview(ui, dv, pos, col, self.ui.grid_edit[d], 42.0, true) {
                self.pending.push(Action::Seek { deck, pos: seek });
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                self.transport(ui, d, &st, vec2(62.0, 40.0));
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = vec2(3.0, 2.0);
                    self.hot_cue_pads(ui, d, &st, 2, vec2(34.0, 19.0));
                });
            });

            ui.add_space(2.0);
            self.loop_row(ui, deck, &st, 1.0);
            ui.add_space(2.0);
            if self.ui.grid_edit[d] && st.track.is_some() {
                self.grid_editor(ui, d, dv, pos, false);
                return;
            }
            ui.horizontal(|ui| self.key_controls(ui, d, view, false));
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                self.sync_button(ui, d, view, vec2(52.0, 20.0));
                self.tempo_controls(ui, d, &st, false);
            });
        });
    }

    /// A deck in the four-deck view: everything the full panel has, in
    /// fewer, tighter rows.
    fn deck_panel_compact(
        &mut self,
        ui: &mut egui::Ui,
        d: usize,
        view: &SessionView,
        fit: PanelFit,
    ) {
        let dv = &view.decks[d];
        let st = dv.state;
        let pos = st.position_at(view.now);
        let col = DECK_COLORS[d];
        let deck = d as u8;
        let (frame, row_gap) = if fit.tight {
            let margin = egui::Margin::symmetric(6, 4);
            (egui::Frame::group(ui.style()).inner_margin(margin), 2.0)
        } else {
            (egui::Frame::group(ui.style()), 3.0)
        };
        let chrome = frame.total_margin().sum().y;
        frame.show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(fit.h - chrome);
            ui.spacing_mut().item_spacing.y = row_gap;
            // Deck, track and artist, then the bar and GRID on the right.
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("DECK {}", deck_letter(d)))
                        .strong()
                        .color(col),
                );
                let names = ui.available_width() - 136.0;
                ui.allocate_ui_with_layout(
                    vec2(names.max(40.0), 18.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        let (title, artist) = track_names(dv);
                        let mut job = egui::text::LayoutJob::default();
                        let font = egui::FontId::proportional(14.5);
                        let strong = ui.visuals().strong_text_color();
                        let weak = ui.visuals().weak_text_color();
                        job.append(&title, 0.0, egui::TextFormat::simple(font.clone(), strong));
                        if !artist.is_empty() {
                            job.append(
                                &format!("  {artist}"),
                                0.0,
                                egui::TextFormat::simple(font, weak),
                            );
                        }
                        ui.add(egui::Label::new(job).truncate());
                    },
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.grid_button(ui, d, &st, pos);
                });
            });
            // Readout, then what the deck is doing (or who touched it last).
            ui.horizontal(|ui| {
                deck_readout(ui, d, view, pos, 13.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some((s, c)) = deck_status(dv, view, true) {
                        let full = deck_status(dv, view, false).map(|(f, _)| f);
                        let r =
                            ui.add(egui::Label::new(RichText::new(s).color(c).small()).truncate());
                        if let Some(f) = full {
                            r.on_hover_text(f);
                        }
                    } else if let Some(r) = st.roll {
                        roll_label(ui, r.beats);
                    } else if st.scratching {
                        ui.label(RichText::new("SCRATCH").small().strong().color(AMBER));
                    } else if let Some(by) = view.name_of(st.last_touch) {
                        let tip = format!("Last touched by {by}");
                        ui.add(egui::Label::new(RichText::new(by).small().weak()).truncate())
                            .on_hover_text(tip);
                    }
                });
            });
            let k = fit.k;
            let grid_edit = self.ui.grid_edit[d];
            if let Some(seek) = overview(ui, dv, pos, col, grid_edit, fit.overview_h, true) {
                self.pending.push(Action::Seek { deck, pos: seek });
            }
            ui.horizontal(|ui| {
                self.transport(ui, d, &st, vec2(44.0 * k, 24.0));
                ui.add_space(4.0 * k);
                ui.spacing_mut().item_spacing.x = if k < 1.0 { 2.0 } else { 3.0 };
                self.hot_cue_pads(ui, d, &st, 1, vec2(24.0 * k, 22.0));
                ui.add_space(4.0 * k);
                self.sync_button(ui, d, view, vec2(44.0 * k, 22.0));
            });
            if self.ui.grid_edit[d] && st.track.is_some() {
                self.grid_editor(ui, d, dv, pos, true);
                return;
            }
            self.loop_row(ui, deck, &st, k);
            ui.horizontal(|ui| {
                if k < 1.0 {
                    // Closer together, so the tempo slider keeps some length.
                    ui.spacing_mut().item_spacing.x = 3.0;
                }
                self.key_controls(ui, d, view, true);
                ui.add_space(4.0);
                self.tempo_controls(ui, d, &st, true);
            });
        });
    }

    /// GRID (opens the beatgrid editor) and the bar.beat readout, laid out
    /// right to left.
    fn grid_button(&mut self, ui: &mut egui::Ui, d: usize, st: &DeckState, pos: f64) {
        let editing = &mut self.ui.grid_edit[d];
        let fill = if *editing {
            MARKER.gamma_multiply(0.55)
        } else {
            Color32::from_gray(50)
        };
        if ui
            .add_enabled(
                st.track.is_some(),
                egui::Button::new(RichText::new("GRID").small().strong()).fill(fill),
            )
            .on_hover_text(
                "Edit the beatgrid: set markers, ADJUST the BPM, SLIP the beats into place.",
            )
            .clicked()
        {
            *editing = !*editing;
        }
        if let (Some(g), Some(_)) = (st.grid, st.track) {
            let (bar, beat) = g.bar_beat_at(pos);
            ui.monospace(RichText::new(format!("{bar}.{beat}")).strong())
                .on_hover_text("Bar.beat. Bar 1 is the first grid marker; the bars before it count down -1, -2…, so you can park the track a bar or two early, press play and wait for the 1.");
            ui.label(RichText::new("bar").small().weak());
        }
    }

    /// CUE (hold from a pause to preview; right-click back to the start)
    /// and PLAY/PAUSE.
    fn transport(&mut self, ui: &mut egui::Ui, d: usize, st: &DeckState, size: egui::Vec2) {
        let deck = d as u8;
        let primary = ui.input(|i| i.pointer.primary_down());
        let text = size.y * 0.38;
        let cue_fill = if st.cue_preview {
            AMBER.gamma_multiply(0.6)
        } else {
            Color32::from_gray(50)
        };
        let r = ui
            .add(
                egui::Button::new(RichText::new("CUE").strong().size(text))
                    .min_size(size)
                    .fill(cue_fill),
            )
            .on_hover_text("Right-click (or SHIFT + CUE): back to the start of the track.");
        let (down, up) = self.hold(format!("cue{d}"), r.is_pointer_button_down_on() && primary);
        if down {
            self.pending.push(Action::CueDown { deck });
        }
        if up {
            self.pending.push(Action::CueUp { deck });
        }
        if r.secondary_clicked() {
            self.pending.push(Action::Seek { deck, pos: 0.0 });
        }
        let playing = st.playing && !st.cue_preview;
        let play_fill = if playing {
            GREEN.gamma_multiply(0.45)
        } else {
            Color32::from_gray(50)
        };
        let r = ui.add(
            egui::Button::new(RichText::new(if playing { "⏸" } else { "▶" }).size(size.y * 0.45))
                .min_size(size)
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
    }

    /// The eight hot cue pads, in one row or two.
    fn hot_cue_pads(
        &mut self,
        ui: &mut egui::Ui,
        d: usize,
        st: &DeckState,
        rows: u8,
        size: egui::Vec2,
    ) {
        let deck = d as u8;
        let primary = ui.input(|i| i.pointer.primary_down());
        let per_row = HOT_CUES as u8 / rows;
        for row in 0..rows {
            ui.horizontal(|ui| {
                for slot in row * per_row..(row + 1) * per_row {
                    let set = st.hot_cues[slot as usize].is_some();
                    let fill = if set {
                        HOT_CUE_COLORS[slot as usize].gamma_multiply(0.7)
                    } else {
                        Color32::from_gray(35)
                    };
                    let r = ui
                        .add(
                            egui::Button::new(RichText::new(format!("{}", slot + 1)).strong())
                                .min_size(size)
                                .fill(fill),
                        )
                        .on_hover_text(
                            "Click: set / jump to hot cue. While paused, plays only as long as you hold it. Right-click: clear.",
                        );
                    let (down, up) =
                        self.hold(format!("hc{d}-{slot}"), r.is_pointer_button_down_on() && primary);
                    if down {
                        self.pending.push(Action::HotCue { deck, slot });
                    }
                    if up {
                        self.pending.push(Action::HotCueUp { deck, slot });
                    }
                    if r.secondary_clicked() {
                        self.pending.push(Action::ClearHotCue { deck, slot });
                    }
                }
            });
        }
    }

    /// SYNC, and MASTER on the deck the synced decks follow.
    fn sync_button(&mut self, ui: &mut egui::Ui, d: usize, view: &SessionView, size: egui::Vec2) {
        let deck = d as u8;
        let room = &view.room;
        let st = &room.decks[d];
        let target = room.sync_target(d);
        let sync_fill = if st.synced {
            SYNC_BLUE
        } else {
            Color32::from_gray(50)
        };
        let tip = match target {
            _ if st.synced && room.master == Some(deck) => "SYNC is on, and this deck is the master: the synced decks follow its tempo and beats. Right-click (or SHIFT + SYNC): sync off, back to the deck's own BPM.".to_string(),
            Some(t) if st.synced && room.master == Some(t as u8) => format!(
                "Synced to deck {}, the master: this deck follows its tempo and beats. Right-click (or SHIFT + SYNC): sync off, back to the deck's own BPM.",
                deck_letter(t)
            ),
            Some(t) => format!(
                "Match this deck's tempo and beats to deck {}, and keep them locked while SYNC is on. Right-click (or SHIFT + SYNC): sync off, back to the deck's own BPM.",
                deck_letter(t)
            ),
            None => "Right-click (or SHIFT + SYNC): sync off, back to the deck's own BPM.".into(),
        };
        let r = ui
            .add_enabled(
                room.can_sync(d) || st.synced,
                egui::Button::new(RichText::new("SYNC").strong())
                    .min_size(size)
                    .fill(sync_fill),
            )
            .on_hover_text(tip)
            .on_disabled_hover_text("This deck and another one need a track with a detected beat.");
        if r.clicked() {
            self.pending.push(Action::Sync {
                deck,
                on: !st.synced,
            });
        }
        if r.secondary_clicked() {
            // Like SHIFT + SYNC: sync off, back to the deck's own BPM
            // (plus wherever the controller's tempo fader sits).
            self.pending.push(Action::Sync { deck, on: false });
            let tempo = self.midi.engine.lock().hardware_tempo(deck).unwrap_or(1.0);
            self.pending.push(Action::Tempo { deck, tempo });
        }
        if room.master == Some(deck) {
            ui.label(RichText::new("MASTER").small().strong().color(SYNC_BLUE.gamma_multiply(1.6)))
                .on_hover_text("The synced decks follow this deck's tempo and beats. When it stops or loads a new track, a synced deck that's playing takes over.");
        }
    }

    /// The tempo slider (filling the row), reset, and pitch bend. Compact:
    /// without the "tempo" label.
    fn tempo_controls(&mut self, ui: &mut egui::Ui, d: usize, st: &DeckState, compact: bool) {
        let deck = d as u8;
        let range = (self.settings.tempo_range * 100.0) as f32;
        let mut pct = ((st.tempo - 1.0) * 100.0) as f32;
        // Room for the value, the label, reset and the two bend buttons.
        let gaps = 5.0 * ui.spacing().item_spacing.x;
        let (rest, least) = if compact {
            (128.0 + gaps, 30.0)
        } else {
            (150.0 + gaps, 60.0)
        };
        ui.spacing_mut().slider_width = (ui.available_width() - rest).max(least);
        let slider = egui::Slider::new(&mut pct, -range..=range)
            .clamping(egui::SliderClamping::Edits)
            .fixed_decimals(2)
            .suffix("%");
        let r = ui
            .add(if compact {
                slider
            } else {
                slider.text("tempo")
            })
            .on_hover_text("Tempo");
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
    }

    /// KEY LOCK, key shift and MATCH KEY (against the deck this one is
    /// mixed with: see `RoomState::mix_partner`).
    fn key_controls(&mut self, ui: &mut egui::Ui, d: usize, view: &SessionView, compact: bool) {
        let deck = d as u8;
        let st = view.room.decks[d];
        let partner = view.room.mix_partner(d);
        let lock_fill = if st.key_lock {
            Color32::from_rgb(110, 70, 170)
        } else {
            Color32::from_gray(50)
        };
        let lock = if compact {
            egui::Button::new(RichText::new("KEY LOCK").small().strong()).fill(lock_fill)
        } else {
            egui::Button::new(RichText::new("KEY LOCK").strong())
                .fill(lock_fill)
                .min_size(vec2(72.0, 20.0))
        };
        if ui
            .add(lock)
            .on_hover_text("On: changing tempo keeps the original pitch. Off: pitch follows tempo, like vinyl.")
            .clicked()
        {
            self.pending.push(Action::KeyLock { deck, on: !st.key_lock });
        }
        if !compact {
            ui.label(RichText::new("key").weak());
        }
        if ui
            .small_button("−")
            .on_hover_text("Down a semitone")
            .clicked()
        {
            self.pending.push(Action::KeyShift {
                deck,
                semitones: (st.key_shift - 1).max(-12),
            });
        }
        ui.monospace(format!("{:+}", st.key_shift));
        if ui
            .small_button("+")
            .on_hover_text("Up a semitone")
            .clicked()
        {
            self.pending.push(Action::KeyShift {
                deck,
                semitones: (st.key_shift + 1).min(12),
            });
        }
        if !compact && st.key_shift != 0 && ui.small_button("reset").clicked() {
            self.pending.push(Action::KeyShift { deck, semitones: 0 });
        }
        let target = partner.and_then(|p| view.room.decks[p].sounding_key());
        let shift = match (st.key >= 0, target) {
            (true, Some(t)) => Some(tandem_core::keys::match_shift(st.key, t)),
            _ => None,
        };
        let label = if compact {
            RichText::new("MATCH").small().strong()
        } else {
            RichText::new("MATCH KEY")
        };
        let r = ui
            .add_enabled(shift.is_some(), egui::Button::new(label))
            .on_hover_text(match (shift, target, partner) {
                (Some(s), Some(t), Some(p)) => format!(
                    "Shift this deck {s:+} semitones to sound in {} (or its relative key), matching deck {}",
                    tandem_core::keys::camelot(t),
                    deck_letter(p)
                ),
                _ => String::new(),
            })
            .on_disabled_hover_text("This deck and the one it's mixed with need a track with a detected key.");
        if r.clicked() {
            if let Some(s) = shift {
                if !st.key_lock {
                    self.pending.push(Action::KeyLock { deck, on: true });
                }
                self.pending.push(Action::KeyShift { deck, semitones: s });
            }
        }
    }

    /// LOOP IN, OUT and RELOOP/EXIT, halve and double, and beat loops.
    /// `k` < 1 narrows the buttons, for a narrow deck panel.
    fn loop_row(&mut self, ui: &mut egui::Ui, deck: u8, st: &DeckState, k: f32) {
        let lit = LOOP.gamma_multiply(0.55);
        let off = Color32::from_gray(50);
        let h = 20.0;
        let span = st.loop_span();
        ui.add_enabled_ui(st.track.is_some(), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = if k < 1.0 { 2.0 } else { 3.0 };
                let title = RichText::new("LOOP").small().strong();
                let r = ui.label(if st.looping { title.color(LOOP) } else { title.weak() });
                if let Some((a, b)) = span {
                    let beats = st.grid.map(|g| (b - a) * g.bpm_at(a) / 60.0);
                    r.on_hover_text(match beats {
                        Some(n) => format!("{:.3} s, {n:.2} beats", b - a),
                        None => format!("{:.3} s", b - a),
                    });
                }
                let waiting = st.loop_in.is_some() && st.loop_out.is_none();
                let button = |text: &str, on: bool, w: f32| {
                    egui::Button::new(RichText::new(text).small().strong())
                        .fill(if on { lit } else { off })
                        .min_size(vec2(w * k, h))
                };
                // Quantize only does anything on a track with a beatgrid.
                let quantize = self.settings.quantize_loops;
                let (in_tip, out_tip) = if quantize && st.grid.is_some() {
                    (
                        "Loop in: mark where a loop starts (on the nearest beat), then press OUT where it ends. While looping, moves the start of the loop to the nearest beat.",
                        "Loop out: end the loop and go round it. The loop is 1 bar, ½ bar, ¼ bar … or 2, 4, 8 bars …, whichever is nearest to when you press, so OUT about a bar after IN makes a 1-bar loop. While looping, makes it the length nearest to here. (Quantize is in Settings › General.)",
                    )
                } else {
                    (
                        "Loop in: mark where a loop starts, then press OUT where it ends. While looping, moves the start of the loop to here.",
                        "Loop out: end the loop here and go round it. While looping, moves the end of the loop to here.",
                    )
                };
                if ui
                    .add(button("IN", st.looping || waiting, 26.0))
                    .on_hover_text(in_tip)
                    .clicked()
                {
                    self.pending.push(Action::LoopIn { deck, quantize });
                }
                if ui
                    .add_enabled(st.loop_in.is_some(), button("OUT", st.looping, 30.0))
                    .on_hover_text(out_tip)
                    .on_disabled_hover_text("Press IN first, where the loop should start.")
                    .clicked()
                {
                    self.pending.push(Action::LoopOut { deck, quantize });
                }
                let label = if st.looping { "EXIT" } else { "RELOOP" };
                if ui
                    .add_enabled(span.is_some(), button(label, st.looping, 52.0))
                    .on_hover_text("While looping: leave the loop and play on. Otherwise: jump back to the loop and go round it again.")
                    .clicked()
                {
                    self.pending.push(Action::Reloop { deck });
                }
                ui.add_space(4.0 * k);
                if ui
                    .add_enabled(span.is_some(), button("×½", false, 24.0))
                    .on_hover_text("Halve the loop")
                    .clicked()
                {
                    self.pending.push(Action::LoopHalve { deck });
                }
                if ui
                    .add_enabled(span.is_some(), button("×2", false, 24.0))
                    .on_hover_text("Double the loop")
                    .clicked()
                {
                    self.pending.push(Action::LoopDouble { deck });
                }
                ui.add_space(4.0 * k);
                for beats in BEAT_LOOPS {
                    let on = st.looping && st.loop_beats.is_some_and(|b| (b - beats).abs() < 1e-9);
                    let text = beats_label(beats);
                    let tip = if beats <= 1.0 { "beat" } else { "beats" };
                    if ui
                        .add(button(&text, on, 22.0))
                        .on_hover_text(format!(
                            "{text} {tip} loop, from the beat the playhead is in. Press it again to leave the loop; another size while looping resizes it."
                        ))
                        .clicked()
                    {
                        self.pending.push(Action::BeatLoop { deck, beats });
                    }
                }
            });
        });
    }

    /// Beatgrid editing, in place of the key and tempo rows (in a compact
    /// panel, in place of the loop row too, in two rows). Every edit acts
    /// on the section under the playhead (from its marker to the next), so
    /// fixing one part of a track never moves another.
    fn grid_editor(&mut self, ui: &mut egui::Ui, d: usize, dv: &DeckView, pos: f64, compact: bool) {
        let st = dv.state;
        let Some(track) = st.track else { return };
        let grid = st.grid;
        let i = grid.map(|g| g.section(pos)).unwrap_or(0);
        let mut edit = GridEdit::default();
        ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
        if compact {
            ui.horizontal(|ui| {
                grid_set_buttons(ui, dv, pos, &mut edit);
                ui.separator();
                ui.add_enabled_ui(grid.is_some(), |ui| grid_adjust(ui, grid, i, &mut edit));
            });
            ui.horizontal(|ui| {
                ui.add_enabled_ui(grid.is_some(), |ui| grid_slip(ui, grid, i, &mut edit));
                ui.separator();
                grid_status(ui, grid, i);
            });
        } else {
            ui.horizontal(|ui| {
                grid_set_buttons(ui, dv, pos, &mut edit);
                ui.separator();
                grid_status(ui, grid, i);
            });
            ui.add_enabled_ui(grid.is_some(), |ui| {
                ui.horizontal(|ui| grid_adjust(ui, grid, i, &mut edit));
                ui.horizontal(|ui| grid_slip(ui, grid, i, &mut edit));
            });
        }
        if let Some(msg) = edit.toast {
            self.toast(msg);
        }
        if edit.reset {
            self.session.lock().reset_grid(d);
        }
        if let Some(g) = edit.new {
            self.pending.push(Action::SetGrid {
                deck: d as u8,
                track,
                grid: g.map(Box::new),
            });
        }
    }

    /// The mixer, at least `min_h` tall inside its frame. `short`: the
    /// window is short, so the booth levels go behind a button.
    fn mixer_panel(&mut self, ui: &mut egui::Ui, view: &SessionView, min_h: f32, short: bool) {
        let four = self.settings.four_decks;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(min_h);
            if four {
                ui.spacing_mut().item_spacing.y = 2.0;
                let columns = self.knob_grid(ui, view);
                let chans: Vec<(usize, f32)> = MIXER_ORDER.iter().copied().zip(columns).collect();
                self.fader_row(
                    ui,
                    view,
                    &chans,
                    62.0,
                    (columns[1] + columns[2]) / 2.0,
                    false,
                );
            } else {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.vertical_centered(|ui| ui.label(RichText::new("MIXER").strong()));
                ui.columns(2, |cols| {
                    for (d, ui) in cols.iter_mut().enumerate() {
                        self.channel_strip(ui, d, view);
                    }
                });
                let row = ui.available_rect_before_wrap();
                let x = |f: f32| row.left() + row.width() * f;
                self.fader_row(
                    ui,
                    view,
                    &[(0, x(0.25)), (1, x(0.75))],
                    84.0,
                    row.center().x + 8.0,
                    true,
                );
            }
            ui.add_space(4.0);
            self.crossfader_row(ui, view);
            self.crossfader_curve(ui, view, short);
            if !short {
                ui.separator();
                if four {
                    self.local_levels_compact(ui);
                } else {
                    self.local_levels(ui);
                }
            }
        });
    }

    /// The two-channel mixer in less height, for a short or narrow window:
    /// each channel's knobs beside its fader (A on the left, B on the
    /// right) and the master meter between them, then the headphone cues
    /// and crossfader switches, the crossfader, and its curve beside the
    /// button for the booth levels.
    fn mixer_compact(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0;
            let (head_h, row_h, fader_w, master_w, label_w) = (15.0, 17.0, 40.0, 20.0, 34.0);
            let block_h = head_h + KNOBS.len() as f32 * (row_h + 2.0);
            let width = ui.available_width();
            let block = ui
                .allocate_exact_size(vec2(width, block_h), Sense::hover())
                .0;
            let knob_w = (block.width() - 2.0 * fader_w - master_w) / 2.0;
            let text = ui.visuals().text_color();
            for (d, &color) in DECK_COLORS.iter().enumerate().take(2) {
                // A's knobs on the left with their names outside, B's mirrored.
                let (outer, inner, align) = if d == 0 {
                    (
                        block.left(),
                        block.left() + knob_w,
                        egui::Align2::LEFT_CENTER,
                    )
                } else {
                    (
                        block.right(),
                        block.right() - knob_w,
                        egui::Align2::RIGHT_CENTER,
                    )
                };
                ui.painter().text(
                    pos2((outer + inner) / 2.0, block.top() + head_h / 2.0),
                    egui::Align2::CENTER_CENTER,
                    deck_letter(d),
                    egui::FontId::proportional(14.0),
                    color,
                );
                let slider_x = if d == 0 {
                    (outer + label_w, inner)
                } else {
                    (inner, outer - label_w)
                };
                for (i, (knob, name)) in KNOBS.into_iter().enumerate() {
                    let y = block.top() + head_h + i as f32 * (row_h + 2.0) + row_h / 2.0;
                    ui.painter().text(
                        pos2(outer, y),
                        align,
                        name,
                        egui::FontId::proportional(10.5),
                        text,
                    );
                    let cell = Rect::from_min_max(
                        pos2(slider_x.0, y - row_h / 2.0),
                        pos2(slider_x.1, y + row_h / 2.0),
                    );
                    let mut v = knob.get(&view.room.decks[d]);
                    let resp = ui.put(cell, |ui: &mut egui::Ui| {
                        ui.spacing_mut().slider_width = cell.width() - 4.0;
                        ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false))
                    });
                    let resp =
                        resp.on_hover_text(format!("Deck {} {}", deck_letter(d), knob.tip()));
                    self.knob_moved(d, knob, v, &resp);
                }
            }
            // The faders inside the knobs, as tall as the knobs.
            let (xa, xb) = (block.left() + knob_w + 14.0, block.right() - knob_w - 14.0);
            self.faders(
                ui,
                view,
                block,
                &[(0, xa), (1, xb)],
                block.center().x,
                false,
            );
            // Each channel's headphone cue and crossfader switch under it.
            let row = ui.allocate_exact_size(vec2(width, 19.0), Sense::hover()).0;
            for (d, left) in [(0, row.left()), (1, row.right() - knob_w - fader_w)] {
                let cell = Rect::from_min_size(pos2(left, row.top()), vec2(knob_w + fader_w, 19.0));
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(cell)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        ui.add_space(((ui.available_width() - 104.0) / 2.0).max(0.0));
                        ui.toggle_value(&mut self.local_edit.pfl[d], "🎧 CUE")
                            .on_hover_text("Send this deck to your headphones (your booth only)");
                        ui.add_space(2.0);
                        self.xf_assign(ui, d, view);
                    },
                );
            }
            ui.add_space(2.0);
            self.crossfader_row(ui, view);
            self.crossfader_curve(ui, view, true);
        });
    }

    /// The crossfader, with the decks on each side named at its ends.
    fn crossfader_row(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        let shown = self.shown_decks();
        let side = |s: XfSide| -> Vec<usize> {
            (0..shown)
                .filter(|&d| view.room.xf_assign[d] == s)
                .collect()
        };
        let (left, right) = (side(XfSide::A), side(XfSide::B));
        let mut xf = view.room.crossfader;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            side_letters(ui, &left);
            let right_w = 10.0 * right.len().max(1) as f32 + 6.0;
            ui.spacing_mut().slider_width = (ui.available_width() - right_w).max(40.0);
            let r = ui
                .add(egui::Slider::new(&mut xf, 0.0..=1.0).show_value(false))
                .on_hover_text("Crossfader. Double-click to centre it. Each channel's ◀ • ▶ switch puts it on a side, or through.");
            if r.changed() {
                self.pending.push(Action::Crossfader { v: xf });
            }
            if r.double_clicked() {
                self.pending.push(Action::Crossfader { v: 0.5 });
            }
            side_letters(ui, &right);
        });
    }

    /// Headphone mix and level, master and broadcast level.
    fn local_levels(&mut self, ui: &mut egui::Ui) {
        let knobs = self.level_knobs;
        let mixer = self.audio.phones_mixer;
        ui.label(RichText::new("Your booth only").small().weak());
        ui.spacing_mut().slider_width = 130.0;
        ui.add_enabled(
            mixer.is_none(),
            egui::Slider::new(&mut self.local_edit.cue_mix, 0.0..=1.0)
                .show_value(false)
                .text("phones: cue ↔ master"),
        )
        .on_disabled_hover_text(phones_mixer_tip(mixer));
        ui.add(
            egui::Slider::new(&mut self.local_edit.hp_level, 0.0..=1.0)
                .show_value(false)
                .text("phones level"),
        )
        .on_hover_text(knobs.phones.tip(Target::HeadphoneLevel));
        ui.add(
            egui::Slider::new(&mut self.local_edit.master_level, 0.0..=1.0)
                .show_value(false)
                .text("master level"),
        )
        .on_hover_text(knobs.master.tip(Target::MasterLevel));
        if self.settings.audio.broadcast.is_some() {
            ui.add(
                egui::Slider::new(&mut self.local_edit.bcast_level, 0.0..=1.0)
                    .show_value(false)
                    .text("broadcast level"),
            );
        }
        knobs.notes(ui);
    }

    /// The same, two to a row, for the four-deck mixer.
    fn local_levels_compact(&mut self, ui: &mut egui::Ui) {
        let tip = "Your booth only";
        let knobs = self.level_knobs;
        let mixer = self.audio.phones_mixer;
        ui.spacing_mut().slider_width = 64.0;
        let bcast = self.settings.audio.broadcast.is_some();
        let l = &mut self.local_edit;
        egui::Grid::new("local-levels")
            .num_columns(4)
            .spacing(vec2(6.0, 2.0))
            .show(ui, |ui| {
                ui.label(RichText::new("phones mix").small())
                    .on_hover_text(tip);
                ui.add_enabled(
                    mixer.is_none(),
                    egui::Slider::new(&mut l.cue_mix, 0.0..=1.0).show_value(false),
                )
                .on_hover_text("Headphones: cue (left) ↔ master (right). Your booth only.")
                .on_disabled_hover_text(phones_mixer_tip(mixer));
                ui.label(RichText::new("level").small()).on_hover_text(tip);
                ui.add(egui::Slider::new(&mut l.hp_level, 0.0..=1.0).show_value(false))
                    .on_hover_text(knobs.phones.tip(Target::HeadphoneLevel));
                ui.end_row();
                ui.label(RichText::new("master").small()).on_hover_text(tip);
                ui.add(egui::Slider::new(&mut l.master_level, 0.0..=1.0).show_value(false))
                    .on_hover_text(knobs.master.tip(Target::MasterLevel));
                if bcast {
                    ui.label(RichText::new("broadcast").small())
                        .on_hover_text(tip);
                    ui.add(egui::Slider::new(&mut l.bcast_level, 0.0..=1.0).show_value(false))
                        .on_hover_text("Broadcast level. Your booth only.");
                }
                ui.end_row();
            });
        knobs.notes(ui);
    }

    /// The booth levels behind a button, for a short window.
    fn levels_button(&mut self, ui: &mut egui::Ui) {
        ui.menu_button(RichText::new("🎧 LEVELS").small().strong(), |ui| {
            self.local_levels(ui)
        })
        .response
        .on_hover_text("Headphones, master and broadcast levels: your booth only");
    }

    /// The crossfader's curve switch. It's part of the room (one mixer, one
    /// switch), and this booth remembers the last curve it picked. With
    /// `levels`, the booth levels' button at the end of the row.
    fn crossfader_curve(&mut self, ui: &mut egui::Ui, view: &SessionView, levels: bool) {
        let cur = view.room.xf_curve;
        let mut picked = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new("CURVE").small().strong());
            curve_icon(ui, cur);
            let mut tip = cur.about().to_string();
            if view.link.peer.is_some() {
                tip.push_str("\n\nThe curve is part of the room: changing it changes it for your partner too.");
            }
            egui::ComboBox::from_id_salt("xf-curve")
                .selected_text(cur.label())
                .width(if levels { 104.0 } else { 120.0 })
                .show_ui(ui, |ui| {
                    for c in XfCurve::ALL {
                        ui.horizontal(|ui| {
                            curve_icon(ui, c);
                            let r = ui.selectable_label(c == cur, c.label()).on_hover_text(c.about());
                            if r.clicked() && c != cur {
                                picked = Some(c);
                            }
                        });
                    }
                })
                .response
                .on_hover_text(tip);
            if levels {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.levels_button(ui)
                });
            }
        });
        if let Some(curve) = picked {
            self.pending.push(Action::CrossfaderCurve { curve });
            self.settings.crossfader_curve = curve;
            self.mark_dirty();
        }
    }

    /// One channel of the two-deck mixer.
    fn channel_strip(&mut self, ui: &mut egui::Ui, d: usize, view: &SessionView) {
        let st = view.room.decks[d];
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(deck_letter(d).to_string())
                    .strong()
                    .color(DECK_COLORS[d]),
            );
            ui.spacing_mut().slider_width = 70.0;
            for (knob, name) in KNOBS {
                let mut v = knob.get(&st);
                let r = ui
                    .add(
                        egui::Slider::new(&mut v, 0.0..=1.0)
                            .show_value(false)
                            .text(name),
                    )
                    .on_hover_text(knob.tip());
                self.knob_moved(d, knob, v, &r);
            }
            ui.horizontal(|ui| {
                ui.add_space(((ui.available_width() - 108.0) / 2.0).max(0.0));
                ui.toggle_value(&mut self.local_edit.pfl[d], "🎧 CUE")
                    .on_hover_text("Send this deck to your headphones (your booth only)");
                ui.add_space(4.0);
                self.xf_assign(ui, d, view);
            });
        });
    }

    /// The four-deck mixer's knobs: a row each for TRIM, HI, MID, LOW and
    /// FILTER, a column per channel (C A B D) under its letter, then the
    /// headphone cues and crossfader switches. Returns each column's
    /// centre, for the faders.
    fn knob_grid(&mut self, ui: &mut egui::Ui, view: &SessionView) -> [f32; DECKS] {
        let label_w = 32.0;
        let full = ui.available_rect_before_wrap();
        let col_w = (full.width() - label_w) / DECKS as f32;
        let x: [f32; DECKS] =
            std::array::from_fn(|i| full.left() + label_w + (i as f32 + 0.5) * col_w);
        let row = |ui: &mut egui::Ui, h: f32| {
            ui.allocate_exact_size(vec2(full.width(), h), Sense::hover())
                .0
        };
        let r = row(ui, 15.0);
        for (i, &d) in MIXER_ORDER.iter().enumerate() {
            ui.painter().text(
                pos2(x[i], r.center().y),
                egui::Align2::CENTER_CENTER,
                deck_letter(d),
                egui::FontId::proportional(14.0),
                DECK_COLORS[d],
            );
        }
        for (knob, name) in KNOBS {
            let r = row(ui, 17.0);
            ui.painter().text(
                pos2(r.left(), r.center().y),
                egui::Align2::LEFT_CENTER,
                name,
                egui::FontId::proportional(10.5),
                ui.visuals().text_color(),
            );
            for (i, &d) in MIXER_ORDER.iter().enumerate() {
                let mut v = knob.get(&view.room.decks[d]);
                let cell =
                    Rect::from_center_size(pos2(x[i], r.center().y), vec2(col_w - 8.0, r.height()));
                let resp = ui.put(cell, |ui: &mut egui::Ui| {
                    ui.spacing_mut().slider_width = col_w - 10.0;
                    ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false))
                });
                let resp = resp.on_hover_text(format!("Deck {} {}", deck_letter(d), knob.tip()));
                self.knob_moved(d, knob, v, &resp);
            }
        }
        let r = row(ui, 19.0);
        for (i, &d) in MIXER_ORDER.iter().enumerate() {
            let cell =
                Rect::from_center_size(pos2(x[i], r.center().y), vec2(col_w - 6.0, r.height()));
            ui.put(cell, |ui: &mut egui::Ui| {
                ui.toggle_value(&mut self.local_edit.pfl[d], "🎧 CUE")
                    .on_hover_text(format!(
                        "Send deck {} to your headphones (your booth only)",
                        deck_letter(d)
                    ))
            });
        }
        let r = row(ui, 17.0);
        for (i, &d) in MIXER_ORDER.iter().enumerate() {
            let cell = Rect::from_center_size(pos2(x[i], r.center().y), vec2(50.0, r.height()));
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(cell)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| self.xf_assign(ui, d, view),
            );
        }
        x
    }

    /// The channel's crossfader assign switch: the left side, through (off
    /// the crossfader), or the right side.
    fn xf_assign(&mut self, ui: &mut egui::Ui, d: usize, view: &SessionView) {
        let cur = view.room.xf_assign[d];
        let letter = deck_letter(d);
        ui.spacing_mut().item_spacing.x = 1.0;
        ui.spacing_mut().button_padding = vec2(3.0, 0.0);
        for side in XfSide::ALL {
            let (label, tip) = match side {
                XfSide::A => ("◀", format!("Deck {letter} on the crossfader's left side")),
                XfSide::Thru => (
                    "•",
                    format!("Deck {letter} through: the crossfader doesn't change its level"),
                ),
                XfSide::B => ("▶", format!("Deck {letter} on the crossfader's right side")),
            };
            let on = side == cur;
            let fill = if on {
                DECK_COLORS[d].gamma_multiply(0.45)
            } else {
                Color32::from_gray(38)
            };
            let mut tip = tip;
            if view.link.peer.is_some() {
                tip.push_str(". Part of the room: it changes for your partner too.");
            }
            let r = ui
                .add(
                    egui::Button::new(RichText::new(label).small().strong())
                        .fill(fill)
                        .min_size(vec2(15.0, 15.0)),
                )
                .on_hover_text(tip);
            if r.clicked() && !on {
                self.pending.push(Action::XfAssign {
                    deck: d as u8,
                    side,
                });
            }
        }
    }

    /// A knob (slider) that was moved, or double-clicked back to centre.
    fn knob_moved(&mut self, d: usize, knob: Knob, v: f32, r: &egui::Response) {
        let deck = d as u8;
        let set = |v: f32| match knob {
            Knob::Trim => Action::Trim { deck, v },
            Knob::Eq(band) => Action::Eq { deck, band, v },
            Knob::Filter => Action::Filter { deck, v },
        };
        if r.changed() {
            self.pending.push(set(v));
        }
        if r.double_clicked() {
            self.pending.push(set(0.5));
        }
    }

    /// A row `h` tall of channel faders at their x positions (`chans`: deck,
    /// x), each with its level meter on the inside, and the master meter at
    /// `master_x`.
    fn fader_row(
        &mut self,
        ui: &mut egui::Ui,
        view: &SessionView,
        chans: &[(usize, f32)],
        h: f32,
        master_x: f32,
        scale: bool,
    ) {
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
        self.faders(ui, view, row, chans, master_x, scale);
    }

    /// The faders and meters of `fader_row`, in `row`.
    fn faders(
        &mut self,
        ui: &mut egui::Ui,
        view: &SessionView,
        row: Rect,
        chans: &[(usize, f32)],
        master_x: f32,
        scale: bool,
    ) {
        let h = row.height();
        let meter_h = h - 8.0;
        for &(d, x) in chans {
            let deck = d as u8;
            let mut v = view.room.decks[d].volume;
            let fader = Rect::from_center_size(pos2(x, row.center().y), vec2(24.0, h));
            let r = ui.put(fader, |ui: &mut egui::Ui| {
                ui.spacing_mut().slider_width = h - 4.0;
                ui.add(
                    egui::Slider::new(&mut v, 0.0..=1.0)
                        .vertical()
                        .show_value(false),
                )
            });
            if r.changed() {
                self.pending.push(Action::Volume { deck, v });
            }
            let inward = if x < master_x { 1.0 } else { -1.0 };
            let meter =
                Rect::from_center_size(pos2(x + inward * 20.0, row.center().y), vec2(7.0, meter_h));
            self.meter_view.channel(ui.painter(), meter, d);
            ui.interact(meter, ui.id().with(("meter", d)), Sense::hover())
                .on_hover_text(format!(
                    "Deck {} level, before the fader (after trim, EQ and filter). Red: over full scale.",
                    deck_letter(d)
                ));
        }
        let master = Rect::from_center_size(pos2(master_x, row.center().y), vec2(16.0, meter_h));
        self.meter_view.master(ui.painter(), master);
        if scale {
            ui_meters::scale(ui.painter(), master, master.left() - 3.0, 9.0);
        }
        ui.interact(master, ui.id().with("master-meter"), Sense::hover())
            .on_hover_text(
                "Master level: the mix after the faders and crossfader, before your master level. Red: over full scale.",
            );
    }
}

/// Why the headphone mix slider is greyed out: `mixer`, the controller the
/// headphones play through, blends cue and master itself.
fn phones_mixer_tip(mixer: Option<&str>) -> String {
    format!(
        "Your headphones play through the {}, which blends cue and master itself: \
         use its HEADPHONES MIX knob and MASTER CUE button.",
        mixer.unwrap_or("controller")
    )
}

/// What the controller's knob does to one of this booth's levels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum LevelKnob {
    /// No knob is mapped to it.
    #[default]
    None,
    /// The knob sets it. `waiting`: the slider was moved on screen, and the
    /// knob takes over once it reaches it (soft takeover).
    Sets { waiting: bool },
    /// The controller's own sound card plays this output, and the knob
    /// turns it up and down there: it leaves Tandem's level alone.
    OnController,
}

impl LevelKnob {
    fn of(engine: &MidiEngine, target: Target, waiting: &[Target]) -> Self {
        if engine.mapping.binding_for(target).is_none() {
            LevelKnob::None
        } else if engine.card.sets(target) {
            LevelKnob::OnController
        } else {
            LevelKnob::Sets {
                waiting: waiting.contains(&target),
            }
        }
    }

    /// The hover text for the slider of `level`'s target.
    fn tip(self, level: Target) -> String {
        let name = level.label();
        let knob = name.to_lowercase();
        match self {
            LevelKnob::None => format!("{name}. Your booth only."),
            LevelKnob::Sets { .. } => {
                format!("{name}. Your booth only. Your controller's {knob} knob sets it too.")
            }
            LevelKnob::OnController => format!(
                "{name}. Your booth only. Your controller's own sound card plays this, and its \
                 {knob} knob turns it up and down there, so the knob doesn't move this slider."
            ),
        }
    }
}

/// What the controller's master and headphone level knobs do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LevelKnobs {
    pub master: LevelKnob,
    pub phones: LevelKnob,
}

impl LevelKnobs {
    pub(crate) fn of(engine: &MidiEngine, connected: bool) -> Self {
        if !connected {
            return Self::default();
        }
        let waiting = engine.waiting_for_pickup();
        Self {
            master: LevelKnob::of(engine, Target::MasterLevel, &waiting),
            phones: LevelKnob::of(engine, Target::HeadphoneLevel, &waiting),
        }
    }

    /// A line under the level sliders when a knob doesn't simply move its
    /// slider: it's waiting to pick it up, or it acts on the controller's
    /// own sound card instead.
    fn notes(self, ui: &mut egui::Ui) {
        let waiting = |k: LevelKnob| k == LevelKnob::Sets { waiting: true };
        let pickup = match (waiting(self.master), waiting(self.phones)) {
            (true, true) => Some("Turn the level knobs past the sliders to pick them up."),
            (true, false) => Some("Turn the master knob past the slider to pick it up."),
            (false, true) => Some("Turn the phones knob past the slider to pick it up."),
            (false, false) => None,
        };
        if let Some(text) = pickup {
            ui.label(RichText::new(text).small().color(AMBER));
        }
        let on = |k: LevelKnob| k == LevelKnob::OnController;
        let hardware = match (on(self.master), on(self.phones)) {
            (true, true) => {
                Some("Level knobs: set the levels on your controller's own sound card.")
            }
            (true, false) => {
                Some("Master knob: sets the level on your controller's own sound card.")
            }
            (false, true) => {
                Some("Phones knob: sets the level on your controller's own sound card.")
            }
            (false, false) => None,
        };
        if let Some(text) = hardware {
            ui.label(RichText::new(text).small().weak())
                .on_hover_text("Your controller's own sound card plays this, and turns it up and down by itself. So its knob leaves Tandem's level alone, or the sound would be turned down twice.");
        }
    }
}

/// The knobs on a mixer channel, top to bottom.
#[derive(Clone, Copy)]
enum Knob {
    Trim,
    /// EQ band: 0 low, 1 mid, 2 high.
    Eq(u8),
    Filter,
}

const KNOBS: [(Knob, &str); 5] = [
    (Knob::Trim, "TRIM"),
    (Knob::Eq(2), "HI"),
    (Knob::Eq(1), "MID"),
    (Knob::Eq(0), "LOW"),
    (Knob::Filter, "FILTER"),
];

impl Knob {
    fn get(self, st: &DeckState) -> f32 {
        match self {
            Knob::Trim => st.trim,
            Knob::Eq(band) => st.eq[band as usize],
            Knob::Filter => st.filter,
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Knob::Filter => "Left: low-pass. Right: high-pass. Double-click to reset.",
            _ => "Double-click to reset",
        }
    }
}

/// Deck letters in their colours (the decks on one side of the crossfader).
fn side_letters(ui: &mut egui::Ui, decks: &[usize]) {
    if decks.is_empty() {
        ui.label(RichText::new("–").weak());
    }
    for &d in decks {
        ui.label(
            RichText::new(deck_letter(d).to_string())
                .color(DECK_COLORS[d])
                .strong(),
        );
    }
}

/// A small picture of a crossfader curve: each deck's volume (up) across
/// the fader's travel (left to right), in its deck colour.
fn curve_icon(ui: &mut egui::Ui, curve: XfCurve) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(26.0, 14.0), Sense::hover());
    let p = ui.painter();
    p.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0_f32, ui.visuals().weak_text_color()),
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink(2.5);
    for (side, color) in [(XfSide::A, DECK_COLORS[0]), (XfSide::B, DECK_COLORS[1])] {
        let pts: Vec<egui::Pos2> = (0..=48)
            .map(|i| {
                let x = i as f32 / 48.0;
                let g = tandem_core::render::xfader_gain(curve, side, x);
                pos2(
                    inner.left() + x * inner.width(),
                    inner.bottom() - g * inner.height(),
                )
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.5_f32, color)));
    }
    r
}

/// "1", ".1", ".01" for an ADJUST step.
fn step_label(step: f64) -> String {
    let s = format!("{step}");
    s.strip_prefix('0').unwrap_or(&s).to_string()
}

/// What the grid editor's buttons asked for this frame.
#[derive(Default)]
struct GridEdit {
    /// A new grid for the deck (`Some(None)`: no beat).
    new: Option<Option<BeatGrid>>,
    reset: bool,
    toast: Option<String>,
}

/// SET, DELETE and RESET.
fn grid_set_buttons(ui: &mut egui::Ui, dv: &DeckView, pos: f64, edit: &mut GridEdit) {
    let grid = dv.state.grid;
    let i = grid.map(|g| g.section(pos)).unwrap_or(0);
    let r = ui
        .add(
            egui::Button::new(RichText::new("SET").strong())
                .fill(MARKER.gamma_multiply(0.6))
                .min_size(vec2(46.0, 20.0)),
        )
        .on_hover_text(
            "Put a marker right where the playhead is, on the first beat of a bar (the 1). \
             The first marker is bar 1.\n\nOn a detected grid, the first SET moves the whole grid \
             to line up here. After that, SET adds a marker (for a tempo change), or moves the \
             marker within half a beat of the playhead.",
        );
    if r.clicked() {
        let bpm = dv
            .meta
            .as_ref()
            .and_then(|m| m.bpm)
            .map(|b| b as f64)
            .unwrap_or(120.0);
        let g = match grid {
            Some(g) => g.set_marker(pos),
            None => BeatGrid::from_markers(&[Marker { pos, bpm }], true),
        };
        match g {
            Some(g) => edit.new = Some(Some(g)),
            None => {
                edit.toast = Some(format!(
                    "A beatgrid can have at most {MAX_MARKERS} markers."
                ))
            }
        }
    }
    let markers = grid.map(|g| g.markers().len()).unwrap_or(0);
    if ui
        .add_enabled(markers > 1, egui::Button::new("DELETE").min_size(vec2(0.0, 20.0)))
        .on_hover_text("Remove this section's marker (the bright one on the waveform). The section before takes over.")
        .on_disabled_hover_text("A grid keeps at least one marker. RESET goes back to the detected grid.")
        .clicked()
    {
        edit.new = grid.and_then(|g| g.remove_marker(i)).map(Some);
    }
    let edited = grid.map(|g| g.edited).unwrap_or(false);
    if ui
        .add_enabled(edited, egui::Button::new("RESET").min_size(vec2(0.0, 20.0)))
        .on_hover_text("Throw away your edits and go back to the grid Tandem detected.")
        .clicked()
    {
        edit.reset = true;
    }
}

/// Which marker the playhead is under, or what kind of grid this is.
fn grid_status(ui: &mut egui::Ui, grid: Option<BeatGrid>, i: usize) {
    let markers = grid.map(|g| g.markers().len()).unwrap_or(0);
    match grid {
        Some(_) if markers > 1 => {
            ui.label(
                RichText::new(format!("marker {} of {markers}", i + 1))
                    .small()
                    .weak(),
            );
        }
        Some(g) if !g.edited => {
            ui.label(RichText::new("detected grid").small().weak());
        }
        Some(_) => {}
        None => {
            ui.label(
                RichText::new("no beat found: SET starts a grid here")
                    .small()
                    .weak(),
            );
        }
    }
}

/// ADJUST: stretch section `i` by small steps, or halve or double it.
fn grid_adjust(ui: &mut egui::Ui, grid: Option<BeatGrid>, i: usize, edit: &mut GridEdit) {
    let small = vec2(30.0, 18.0);
    ui.add_sized(vec2(44.0, 18.0), egui::Label::new(RichText::new("ADJUST").small().strong()))
        .on_hover_text("Stretch this section's grid to the track's tempo. Its marker stays put; beats further from it move more.");
    let mut adjust = |ui: &mut egui::Ui, label: &str, delta: f64| {
        let r = ui
            .add(egui::Button::new(label).min_size(small))
            .on_hover_text(format!("{delta:+} BPM"));
        if r.clicked() {
            edit.new = grid.map(|g| Some(g.adjust(i, delta)));
        }
    };
    for step in ADJUST_STEPS.iter().rev() {
        adjust(ui, &format!("−{}", step_label(*step)), -step);
    }
    let bpm = grid.map(|g| g.markers()[i].bpm).unwrap_or(0.0);
    ui.add_sized(
        vec2(54.0, 18.0),
        egui::Label::new(RichText::new(format!("{bpm:.2}")).monospace().strong()),
    )
    .on_hover_text("This section's BPM (before the tempo fader)");
    for step in ADJUST_STEPS {
        adjust(ui, &format!("+{}", step_label(step)), step);
    }
    ui.add_space(4.0);
    for (label, factor, tip) in [("½", 0.5, "Half the BPM"), ("×2", 2.0, "Double the BPM")] {
        if ui
            .add(egui::Button::new(label).min_size(vec2(24.0, 18.0)))
            .on_hover_text(tip)
            .clicked()
        {
            edit.new = grid.map(|g| Some(g.scale(i, factor)));
        }
    }
}

/// SLIP: slide section `i`'s beats earlier or later.
fn grid_slip(ui: &mut egui::Ui, grid: Option<BeatGrid>, i: usize, edit: &mut GridEdit) {
    let small = vec2(30.0, 18.0);
    ui.add_sized(
        vec2(44.0, 18.0),
        egui::Label::new(RichText::new("SLIP").small().strong()),
    )
    .on_hover_text("Slide this section's beats earlier or later, without changing the BPM.");
    for (k, step) in SLIP_STEPS.iter().enumerate().rev() {
        let r = ui
            .add(egui::Button::new("◀".repeat(k + 1)).min_size(small))
            .on_hover_text(format!("{:.0} ms earlier", step * 1000.0));
        if r.clicked() {
            edit.new = grid.map(|g| Some(g.slip(i, -step)));
        }
    }
    ui.add_space(6.0);
    for (k, step) in SLIP_STEPS.iter().enumerate() {
        let r = ui
            .add(egui::Button::new("▶".repeat(k + 1)).min_size(small))
            .on_hover_text(format!("{:.0} ms later", step * 1000.0));
        if r.clicked() {
            edit.new = grid.map(|g| Some(g.slip(i, *step)));
        }
    }
}

/// A deck's title and artist (or what to show while there's none).
fn track_names(dv: &DeckView) -> (String, String) {
    match &dv.meta {
        Some(m) => (m.title.clone(), m.artist.clone()),
        None if dv.state.track.is_some() => ("…".to_string(), String::new()),
        None => (
            "Empty".to_string(),
            "Double-click a track in the library to load it".to_string(),
        ),
    }
}

/// Time, time left, BPM, tempo and key (green when it fits the key of the
/// deck it's mixed with).
fn deck_readout(ui: &mut egui::Ui, d: usize, view: &SessionView, pos: f64, size: f32) {
    let st = &view.room.decks[d];
    ui.monospace(RichText::new(fmt_time(pos)).size(size));
    ui.monospace(RichText::new(format!("-{}", fmt_time(st.duration - pos))).weak());
    ui.separator();
    match st.effective_bpm_at(view.now) {
        Some(b) => ui.monospace(RichText::new(format!("{b:.2} BPM")).size(size)),
        None if st.track.is_some() => ui.monospace(RichText::new("no beat").weak()),
        None => ui.monospace(RichText::new("--- BPM").weak()),
    };
    ui.monospace(format!("{:+.2}%", (st.tempo - 1.0) * 100.0));
    ui.separator();
    let partner = view.room.mix_partner(d);
    match st.sounding_key() {
        Some(k) => {
            let fits = partner
                .and_then(|p| view.room.decks[p].sounding_key())
                .is_some_and(|o| keys_fit(k, o));
            let text = RichText::new(format!(
                "{} {}",
                tandem_core::keys::camelot(k),
                tandem_core::keys::name(k)
            ))
            .size(size)
            .strong();
            let tip = match partner {
                Some(p) if fits => format!("Harmonically compatible with deck {}", deck_letter(p)),
                _ => "Key (Camelot)".to_string(),
            };
            ui.label(if fits { text.color(GREEN) } else { text })
                .on_hover_text(tip);
        }
        None if st.track.is_some() => {
            ui.monospace(RichText::new("key ?").weak());
        }
        None => {}
    }
}

/// What a deck's track is waiting for, if anything. `short` for a compact
/// panel, where it shares a row with the readout: a download's progress then
/// goes first, so it isn't cut off.
fn deck_status(dv: &DeckView, view: &SessionView, short: bool) -> Option<(String, Color32)> {
    if let Some(l) = &dv.loading {
        return Some((format!("Loading {l}…"), AMBER));
    }
    let id = dv.state.track?;
    match dv.status.as_ref()? {
        TrackStatus::Searching => Some(("Looking for this track in your library…".into(), AMBER)),
        TrackStatus::Decoding => Some(("Decoding…".into(), AMBER)),
        TrackStatus::Fetching => {
            let t = view
                .transfers
                .iter()
                .find(|t| t.id == id && !t.upload && t.total > 0);
            let pct = |b: u64| t.map_or(0.0, |t| b as f64 * 100.0 / t.total as f64);
            if short {
                let text = match t {
                    Some(t) if t.state == TransferState::Paused && t.done == 0 => {
                        "Waiting for the link".into()
                    }
                    Some(t) if t.state == TransferState::Paused => {
                        format!("Paused at {:.0}% (link down)", pct(t.done))
                    }
                    Some(t) if t.state == TransferState::Queued && t.done > 0 => {
                        format!("Resuming at {:.0}%…", pct(t.done))
                    }
                    Some(t) if t.from > 0 => format!(
                        "Receiving {:.0}% (resumed at {:.0}%)",
                        pct(t.done),
                        pct(t.from)
                    ),
                    Some(t) => format!("Receiving {:.0}%", pct(t.done)),
                    None => "Receiving…".into(),
                };
                return Some((text, AMBER));
            }
            let text = match t {
                Some(t) if t.state == TransferState::Paused && t.done == 0 => {
                    "Receiving this track from your partner: waiting until the link is back".into()
                }
                Some(t) if t.state == TransferState::Paused => format!(
                    "Receiving this track from your partner: paused at {:.0}% until the link is back",
                    pct(t.done)
                ),
                Some(t) if t.state == TransferState::Queued && t.done > 0 => format!(
                    "Receiving this track from your partner… resuming at {:.0}%",
                    pct(t.done)
                ),
                Some(t) if t.from > 0 => format!(
                    "Receiving this track from your partner… {:.0}% (resumed at {:.0}%)",
                    pct(t.done),
                    pct(t.from)
                ),
                Some(t) => format!(
                    "Receiving this track from your partner… {:.0}%",
                    pct(t.done)
                ),
                None => "Receiving this track from your partner…".to_string(),
            };
            Some((text, AMBER))
        }
        TrackStatus::Failed(e) => Some((format!("Can't play: {e}"), RED)),
        TrackStatus::Ready => None,
    }
}

/// Whole-track overview. When `seekable`, a click returns the position to seek to.
pub(crate) fn overview(
    ui: &mut egui::Ui,
    dv: &DeckView,
    pos: f64,
    col: Color32,
    grid_edit: bool,
    height: f32,
    seekable: bool,
) -> Option<f64> {
    let sense = if seekable {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), sense);
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
        loop_band(&p, rect, &dv.state, x_of);
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
        // Grid markers (where the tempo can change) along the bottom.
        if let Some(g) = dv.state.grid.filter(|g| grid_edit || g.markers().len() > 1) {
            for m in g.markers() {
                let x = x_of(m.pos.max(0.0));
                p.rect_filled(
                    egui::Rect::from_min_size(pos2(x - 1.0, rect.bottom() - 6.0), vec2(2.0, 6.0)),
                    0.0,
                    MARKER,
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

/// "ROLL ½" while a loop roll is held (the BEAT JUMP pads on a DDJ-FLX6).
fn roll_label(ui: &mut egui::Ui, beats: f64) {
    ui.label(
        RichText::new(format!("ROLL {}", beats_label(beats)))
            .small()
            .strong()
            .color(LOOP),
    )
    .on_hover_text("A loop roll is held: it loops while the pad is held, and the track runs on underneath (the dashed line on the waveform). Let go and it carries on from there.");
}

/// The loop on a waveform: a band from loop in to loop out (bright while
/// looping), or just the in point while LOOP IN waits for LOOP OUT. A loop
/// roll is a bright band of its own, over a dimmed loop.
fn loop_band(p: &egui::Painter, rect: egui::Rect, st: &DeckState, x_of: impl Fn(f64) -> f32) {
    let edge = |x: f32, c: Color32| {
        if rect.x_range().contains(x) {
            p.line_segment(
                [pos2(x, rect.top()), pos2(x, rect.bottom())],
                Stroke::new(1.5_f32, c),
            );
        }
    };
    let band = |a: f64, b: f64, lit: bool| {
        let c = if lit { LOOP } else { LOOP.gamma_multiply(0.45) };
        let r = egui::Rect::from_min_max(pos2(x_of(a), rect.top()), pos2(x_of(b), rect.bottom()))
            .intersect(rect);
        if r.width() > 0.0 {
            p.rect_filled(r, 0.0, c.gamma_multiply(if lit { 0.22 } else { 0.1 }));
        }
        edge(x_of(a), c);
        edge(x_of(b), c);
    };
    let rolling = st.roll_span();
    match (st.loop_span(), st.loop_in) {
        (Some((a, b)), _) => band(a, b, st.looping && rolling.is_none()),
        (None, Some(a)) => edge(x_of(a), LOOP),
        _ => {}
    }
    if let Some((a, b)) = rolling {
        band(a, b, true);
    }
}

/// Under a loop roll, a dashed line where the track has got to underneath:
/// where it carries on when the roll is let go.
fn slip_line(p: &egui::Painter, rect: egui::Rect, x: f32) {
    if rect.x_range().contains(x) {
        p.extend(egui::Shape::dashed_line(
            &[pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(2.0_f32, Color32::WHITE.gamma_multiply(0.85)),
            5.0,
            3.0,
        ));
    }
}

/// Scrolling waveform centred on the playhead. The window is 8 seconds of
/// real time, so two decks at the same effective BPM show the same beat
/// spacing. Beat lines come from the grid, with bar numbers on the
/// downbeats; grid markers show while editing (or when there are several).
pub(crate) fn scrolling_waveform(
    ui: &mut egui::Ui,
    d: usize,
    dv: &DeckView,
    now: i64,
    height: f32,
    grid_edit: bool,
) {
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
        loop_band(&p, rect, &st, x_of);
        if let Some(g) = st.grid {
            // Beat lines: downbeats full height with their bar number.
            let font = egui::FontId::monospace(10.0);
            let from = (pos - span / 2.0).max(0.0);
            let to = (pos + span / 2.0).min(st.duration);
            g.beats_in(from, to, |b| {
                let x = x_of(b.pos);
                let downbeat = b.beat == 1;
                let c = Color32::WHITE.gamma_multiply(if downbeat { 0.6 } else { 0.3 });
                let h = if downbeat {
                    rect.height()
                } else {
                    rect.height() * 0.25
                };
                p.line_segment(
                    [pos2(x, rect.top()), pos2(x, rect.top() + h)],
                    Stroke::new(1.0_f32, c),
                );
                if downbeat {
                    let galley = p.layout_no_wrap(
                        b.bar.to_string(),
                        font.clone(),
                        Color32::WHITE.gamma_multiply(0.85),
                    );
                    let at = pos2(x + 3.0, rect.bottom() - galley.size().y - 1.0);
                    p.rect_filled(
                        egui::Rect::from_min_size(
                            at - vec2(1.0, 0.0),
                            galley.size() + vec2(2.0, 0.0),
                        ),
                        2.0,
                        Color32::from_black_alpha(170),
                    );
                    p.galley(at, galley, Color32::WHITE);
                }
            });
            if grid_edit || g.markers().len() > 1 {
                let active = g.section(pos);
                for (k, m) in g.markers().iter().enumerate() {
                    let x = x_of(m.pos);
                    if !rect.x_range().contains(x) {
                        continue;
                    }
                    let bright = grid_edit && k == active;
                    let c = if bright {
                        MARKER
                    } else {
                        MARKER.gamma_multiply(0.6)
                    };
                    if grid_edit {
                        p.line_segment(
                            [pos2(x, rect.top()), pos2(x, rect.bottom())],
                            Stroke::new(if bright { 2.0_f32 } else { 1.0 }, c),
                        );
                    }
                    p.add(egui::Shape::convex_polygon(
                        vec![
                            pos2(x - 5.0, rect.top()),
                            pos2(x + 5.0, rect.top()),
                            pos2(x, rect.top() + 7.0),
                        ],
                        c,
                        Stroke::NONE,
                    ));
                }
            }
        }
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
        if st.roll.is_some() {
            slip_line(&p, rect, x_of(st.slip_position_at(now)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_library::{
        library_height, library_max_height, CENTRAL_MARGIN, LIBRARY_H, LIBRARY_MIN_H,
        LIBRARY_ROOMY_H,
    };

    /// A window's inner size in points → the decks area's width and the
    /// height the library and decks area share (inside the top bar, 36,
    /// the status bar, 22, and the central panel's margins).
    fn space(w: f32, h: f32) -> (f32, f32) {
        (w - CENTRAL_MARGIN, h - 36.0 - 22.0 - CENTRAL_MARGIN)
    }

    /// The library's height, the layout, and the room its controls get with
    /// the waveforms at their least.
    fn plan(w: f32, h: f32, four: bool) -> (f32, DeckLayout, f32) {
        let (decks_w, room) = space(w, h);
        let lib = library_height(room, decks_w, four);
        let layout = DeckLayout::pick(four, decks_w, room - lib);
        let controls = room - lib - layout.waves_h(WAVE_MIN_H[layout.wave_idx()]);
        (lib, layout, controls)
    }

    #[test]
    fn big_window_keeps_its_layout() {
        // The usual 1440 × 930 window, and bigger: the library at 330, the
        // full panels, nothing squeezed.
        for (w, h) in [(1440.0, 930.0), (2560.0, 1369.0)] {
            let (lib, layout, controls) = plan(w, h, false);
            assert_eq!((lib, layout), (LIBRARY_H, DeckLayout::Full));
            assert!(controls >= FULL_ROW_H);
            let (lib, layout, controls) = plan(w, h, true);
            assert_eq!((lib, layout), (LIBRARY_H, DeckLayout::Four));
            assert!(controls >= FOUR_ROW_H);
        }
    }

    #[test]
    fn laptop_screens_fit_without_scrolling() {
        // Maximised windows: 1080p at 125%, 1366 × 768, 1080p at 150%.
        for (w, h) in [(1536.0, 785.0), (1366.0, 689.0), (1280.0, 641.0)] {
            for four in [false, true] {
                let (lib, layout, controls) = plan(w, h, four);
                assert!(lib >= LIBRARY_MIN_H, "{w}×{h} four {four}: library {lib}");
                assert!(
                    controls >= layout.row_h().1,
                    "{w}×{h} {layout:?}: {controls} for the controls"
                );
            }
        }
        // 1080p at 125% keeps the full two-deck panels, with less library.
        let (lib, layout, _) = plan(1536.0, 785.0, false);
        assert_eq!(layout, DeckLayout::Full);
        assert!((LIBRARY_ROOMY_H..LIBRARY_H).contains(&lib), "{lib}");
        // Shorter ones swap to compact panels and keep a roomier library.
        let (lib, layout, _) = plan(1280.0, 641.0, false);
        assert_eq!(layout, DeckLayout::Compact);
        assert!(lib > LIBRARY_ROOMY_H, "{lib}");
    }

    #[test]
    fn smallest_window_keeps_the_waveforms() {
        // Down to the smallest window, the waveforms get their least and the
        // controls a (scrolled) strip; two decks still fit without scrolling.
        for (w, h) in [(1097.0, 538.0), (MIN_W, MIN_H)] {
            let (lib, layout, controls) = plan(w, h, false);
            assert_eq!((lib, layout), (LIBRARY_MIN_H, DeckLayout::Compact));
            assert!(controls >= COMPACT_ROW_H, "{w}×{h}: {controls}");
            let (lib, _, controls) = plan(w, h, true);
            assert_eq!(lib, LIBRARY_MIN_H);
            assert!(controls >= CONTROLS_MIN_H, "{w}×{h}: {controls}");
        }
    }

    const MIN_W: f32 = crate::app::MIN_WINDOW[0];
    const MIN_H: f32 = crate::app::MIN_WINDOW[1];

    #[test]
    fn dragged_library_leaves_the_waveforms() {
        for (w, h) in [(1440.0, 930.0), (1280.0, 641.0), (MIN_W, MIN_H)] {
            for four in [false, true] {
                let (decks_w, room) = space(w, h);
                let max = library_max_height(room, decks_w, four);
                assert!(max >= library_height(room, decks_w, four));
                let layout = DeckLayout::pick(four, decks_w, room - max);
                assert_ne!(layout, DeckLayout::Full);
                let controls = room - max - layout.waves_h(WAVE_MIN_H[layout.wave_idx()]);
                assert!(controls >= CONTROLS_MIN_H - 0.01, "{w}×{h}: {controls}");
            }
        }
    }

    #[test]
    fn full_panels_need_the_width() {
        assert!(DeckLayout::full_fits_width(space(1440.0, 930.0).0));
        assert!(DeckLayout::full_fits_width(space(1280.0, 641.0).0));
        assert!(!DeckLayout::full_fits_width(space(1097.0, 538.0).0));
        // A narrow window gets compact panels even when it's tall.
        assert_eq!(
            DeckLayout::pick(false, space(1100.0, 0.0).0, 900.0),
            DeckLayout::Compact
        );
        // Compact panels narrow their buttons rather than spill over.
        assert_eq!(button_scale(532.0), 1.0);
        assert!(button_scale(380.0) < 1.0);
        assert_eq!(button_scale(100.0), 0.7);
    }
}
