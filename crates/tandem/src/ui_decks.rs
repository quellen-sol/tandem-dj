//! Top bar, decks, mixer and waveforms.

use crate::app::TandemApp;
use eframe::egui::{self, pos2, vec2, Align, Color32, Layout, RichText, Sense, Stroke};
use tandem_core::audio::PEAK_BLOCK;
use tandem_core::grid::{BeatGrid, Marker, MAX_MARKERS};
use tandem_core::mapping::beats_label;
use tandem_core::model::{Action, DeckState, BEAT_LOOPS, DECKS};
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
pub(crate) const GREEN: Color32 = Color32::from_rgb(90, 220, 120);
const RED: Color32 = Color32::from_rgb(240, 90, 90);
pub(crate) const AMBER: Color32 = Color32::from_rgb(250, 190, 60);
/// Beatgrid markers (red, like Serato's).
const MARKER: Color32 = Color32::from_rgb(235, 70, 70);
/// Loops, on the buttons and the waveforms.
const LOOP: Color32 = Color32::from_rgb(60, 200, 150);

/// ADJUST steps (BPM): a little, a bit more, more than that.
const ADJUST_STEPS: [f64; 3] = [0.01, 0.1, 1.0];
/// SLIP steps (seconds).
const SLIP_STEPS: [f64; 3] = [0.001, 0.005, 0.025];

/// Camelot-compatible: same key, relative major/minor, or one step round the wheel.
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
                    let delay = self.settings.audio.broadcast_delay_ms;
                    ui.toggle_value(&mut self.ui.broadcast_view, "Broadcast view")
                        .on_hover_text(format!("A second window for your stream: the decks and mixer as the broadcast feed plays them, {delay} ms behind the room, so the picture matches the sound. Share or capture that window, and take the sound from the broadcast feed (Settings → Audio)."));
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
            scrolling_waveform(ui, d, &view.decks[d], view.now, h, self.ui.grid_edit[d]);
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
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let editing = &mut self.ui.grid_edit[d];
                    let fill = if *editing { MARKER.gamma_multiply(0.55) } else { Color32::from_gray(50) };
                    if ui
                        .add_enabled(
                            st.track.is_some(),
                            egui::Button::new(RichText::new("GRID").small().strong()).fill(fill),
                        )
                        .on_hover_text("Edit the beatgrid: set markers, ADJUST the BPM, SLIP the beats into place.")
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
                });
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
                let bpm = st.effective_bpm_at(view.now);
                match bpm {
                    Some(b) => ui.monospace(RichText::new(format!("{b:.2} BPM")).size(15.0)),
                    None if st.track.is_some() => ui.monospace(RichText::new("no beat").weak()),
                    None => ui.monospace(RichText::new("--- BPM").weak()),
                };
                ui.monospace(format!("{:+.2}%", (st.tempo - 1.0) * 100.0));
                ui.separator();
                let other = &view.room.decks[(d + 1) % 2];
                match st.sounding_key() {
                    Some(k) => {
                        let fits = other.sounding_key().map(|o| keys_fit(k, o)).unwrap_or(false);
                        let text = RichText::new(format!("{} {}", tandem_core::keys::camelot(k), tandem_core::keys::name(k))).size(15.0).strong();
                        ui.label(if fits { text.color(GREEN) } else { text })
                            .on_hover_text(if fits { "Harmonically compatible with the other deck" } else { "Key (Camelot)" });
                    }
                    None if st.track.is_some() => {
                        ui.monospace(RichText::new("key ?").weak());
                    }
                    None => {}
                }
            });

            if let Some(seek) = overview(ui, dv, pos, col, self.ui.grid_edit[d], 42.0, true) {
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
                let r = ui
                    .add(
                        egui::Button::new(RichText::new("CUE").strong().size(15.0))
                            .min_size(big)
                            .fill(cue_fill),
                    )
                    .on_hover_text("Right-click (or SHIFT + CUE): back to the start of the track.");
                let (down, up) =
                    self.hold(format!("cue{d}"), r.is_pointer_button_down_on() && primary);
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
                                        "Click: set / jump to hot cue. While paused, plays only as long as you hold it. Right-click: clear.",
                                    );
                                let (down, up) = self.hold(
                                    format!("hc{d}-{slot}"),
                                    r.is_pointer_button_down_on() && primary,
                                );
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
                });
            });

            ui.add_space(2.0);
            self.loop_row(ui, deck, &st);
            ui.add_space(2.0);
            if self.ui.grid_edit[d] && st.track.is_some() {
                self.grid_editor(ui, d, dv, pos);
                return;
            }
            ui.horizontal(|ui| {
                let other = view.room.decks[(d + 1) % 2];
                let lock_fill = if st.key_lock { Color32::from_rgb(110, 70, 170) } else { Color32::from_gray(50) };
                if ui
                    .add(egui::Button::new(RichText::new("KEY LOCK").strong()).fill(lock_fill).min_size(vec2(72.0, 20.0)))
                    .on_hover_text("On: changing tempo keeps the original pitch. Off: pitch follows tempo, like vinyl.")
                    .clicked()
                {
                    self.pending.push(Action::KeyLock { deck, on: !st.key_lock });
                }
                ui.label(RichText::new("key").weak());
                if ui.small_button("−").on_hover_text("Down a semitone").clicked() {
                    self.pending.push(Action::KeyShift { deck, semitones: (st.key_shift - 1).max(-12) });
                }
                ui.monospace(format!("{:+}", st.key_shift));
                if ui.small_button("+").on_hover_text("Up a semitone").clicked() {
                    self.pending.push(Action::KeyShift { deck, semitones: (st.key_shift + 1).min(12) });
                }
                if st.key_shift != 0 && ui.small_button("reset").clicked() {
                    self.pending.push(Action::KeyShift { deck, semitones: 0 });
                }
                let target = other.sounding_key();
                let shift = match (st.key >= 0, target) {
                    (true, Some(t)) => Some(tandem_core::keys::match_shift(st.key, t)),
                    _ => None,
                };
                let r = ui
                    .add_enabled(shift.is_some(), egui::Button::new("MATCH KEY"))
                    .on_hover_text(match (shift, target) {
                        (Some(s), Some(t)) => format!(
                            "Shift this deck {s:+} semitones to sound in {} (or its relative key), matching the other deck",
                            tandem_core::keys::camelot(t)
                        ),
                        _ => String::new(),
                    })
                    .on_disabled_hover_text("Both decks need a track with a detected key.");
                if r.clicked() {
                    if let Some(s) = shift {
                        if !st.key_lock {
                            self.pending.push(Action::KeyLock { deck, on: true });
                        }
                        self.pending.push(Action::KeyShift { deck, semitones: s });
                    }
                }
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let other = &view.room.decks[(d + 1) % 2];
                let can_sync = st.grid.is_some() && other.grid.is_some() && other.track.is_some();
                let sync_fill = if st.synced {
                    Color32::from_rgb(40, 120, 200)
                } else {
                    Color32::from_gray(50)
                };
                let r = ui
                    .add_enabled(
                        can_sync || st.synced,
                        egui::Button::new(RichText::new("SYNC").strong())
                            .min_size(vec2(52.0, 20.0))
                            .fill(sync_fill),
                    )
                    .on_hover_text("Match this deck's tempo and beats to the other deck, and keep their tempos locked together. Right-click (or SHIFT + SYNC): sync off, back to the deck's own BPM.")
                    .on_disabled_hover_text("Both decks need a track with a detected beat.");
                if r.clicked() {
                    self.pending.push(Action::Sync { deck, on: !st.synced });
                }
                if r.secondary_clicked() {
                    // Like SHIFT + SYNC: sync off, back to the deck's own BPM
                    // (plus wherever the controller's tempo fader sits).
                    self.pending.push(Action::Sync { deck, on: false });
                    let tempo = self.midi.engine.lock().hardware_tempo(deck).unwrap_or(1.0);
                    self.pending.push(Action::Tempo { deck, tempo });
                }
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

    /// LOOP IN, OUT and RELOOP/EXIT, halve and double, and beat loops.
    fn loop_row(&mut self, ui: &mut egui::Ui, deck: u8, st: &DeckState) {
        let lit = LOOP.gamma_multiply(0.55);
        let off = Color32::from_gray(50);
        let h = 20.0;
        let span = st.loop_span();
        ui.add_enabled_ui(st.track.is_some(), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
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
                        .min_size(vec2(w, h))
                };
                if ui
                    .add(button("IN", st.looping || waiting, 26.0))
                    .on_hover_text("Loop in: mark where a loop starts, then press OUT where it ends. While looping, moves the start of the loop to here.")
                    .clicked()
                {
                    self.pending.push(Action::LoopIn { deck });
                }
                if ui
                    .add_enabled(st.loop_in.is_some(), button("OUT", st.looping, 30.0))
                    .on_hover_text("Loop out: end the loop here and go round it. While looping, moves the end of the loop to here.")
                    .on_disabled_hover_text("Press IN first, where the loop should start.")
                    .clicked()
                {
                    self.pending.push(Action::LoopOut { deck });
                }
                let label = if st.looping { "EXIT" } else { "RELOOP" };
                if ui
                    .add_enabled(span.is_some(), button(label, st.looping, 52.0))
                    .on_hover_text("While looping: leave the loop and play on. Otherwise: jump back to the loop and go round it again.")
                    .clicked()
                {
                    self.pending.push(Action::Reloop { deck });
                }
                ui.add_space(4.0);
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
                ui.add_space(4.0);
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

    /// Beatgrid editing, in place of the key and tempo rows. Every edit acts
    /// on the section under the playhead (from its marker to the next), so
    /// fixing one part of a track never moves another.
    fn grid_editor(&mut self, ui: &mut egui::Ui, d: usize, dv: &DeckView, pos: f64) {
        let st = dv.state;
        let Some(track) = st.track else { return };
        let grid = st.grid;
        let i = grid.map(|g| g.section(pos)).unwrap_or(0);
        let mut new: Option<Option<BeatGrid>> = None;
        let mut reset = false;
        let small = vec2(30.0, 18.0);
        ui.spacing_mut().item_spacing = vec2(3.0, 3.0);

        ui.horizontal(|ui| {
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
                let bpm = dv.meta.as_ref().and_then(|m| m.bpm).map(|b| b as f64).unwrap_or(120.0);
                let g = match grid {
                    Some(g) => g.set_marker(pos),
                    None => BeatGrid::from_markers(&[Marker { pos, bpm }], true),
                };
                match g {
                    Some(g) => new = Some(Some(g)),
                    None => self.toast(format!("A beatgrid can have at most {MAX_MARKERS} markers.")),
                }
            }
            let markers = grid.map(|g| g.markers().len()).unwrap_or(0);
            if ui
                .add_enabled(markers > 1, egui::Button::new("DELETE").min_size(vec2(0.0, 20.0)))
                .on_hover_text("Remove this section's marker (the bright one on the waveform). The section before takes over.")
                .on_disabled_hover_text("A grid keeps at least one marker. RESET goes back to the detected grid.")
                .clicked()
            {
                new = grid.and_then(|g| g.remove_marker(i)).map(Some);
            }
            let edited = grid.map(|g| g.edited).unwrap_or(false);
            if ui
                .add_enabled(edited, egui::Button::new("RESET").min_size(vec2(0.0, 20.0)))
                .on_hover_text("Throw away your edits and go back to the grid Tandem detected.")
                .clicked()
            {
                reset = true;
            }
            ui.separator();
            match grid {
                Some(_) if markers > 1 => {
                    ui.label(RichText::new(format!("marker {} of {markers}", i + 1)).small().weak());
                }
                Some(g) if !g.edited => {
                    ui.label(RichText::new("detected grid").small().weak());
                }
                Some(_) => {}
                None => {
                    ui.label(RichText::new("no beat found: SET starts a grid here").small().weak());
                }
            }
        });

        ui.add_enabled_ui(grid.is_some(), |ui| {
            ui.horizontal(|ui| {
                ui.add_sized(vec2(44.0, 18.0), egui::Label::new(RichText::new("ADJUST").small().strong()))
                    .on_hover_text("Stretch this section's grid to the track's tempo. Its marker stays put; beats further from it move more.");
                let mut adjust = |ui: &mut egui::Ui, label: &str, delta: f64| {
                    let r = ui
                        .add(egui::Button::new(label).min_size(small))
                        .on_hover_text(format!("{delta:+} BPM"));
                    if r.clicked() {
                        new = grid.map(|g| Some(g.adjust(i, delta)));
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
                    if ui.add(egui::Button::new(label).min_size(vec2(24.0, 18.0))).on_hover_text(tip).clicked() {
                        new = grid.map(|g| Some(g.scale(i, factor)));
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.add_sized(vec2(44.0, 18.0), egui::Label::new(RichText::new("SLIP").small().strong()))
                    .on_hover_text("Slide this section's beats earlier or later, without changing the BPM.");
                for (k, step) in SLIP_STEPS.iter().enumerate().rev() {
                    let r = ui
                        .add(egui::Button::new("◀".repeat(k + 1)).min_size(small))
                        .on_hover_text(format!("{:.0} ms earlier", step * 1000.0));
                    if r.clicked() {
                        new = grid.map(|g| Some(g.slip(i, -step)));
                    }
                }
                ui.add_space(6.0);
                for (k, step) in SLIP_STEPS.iter().enumerate() {
                    let r = ui
                        .add(egui::Button::new("▶".repeat(k + 1)).min_size(small))
                        .on_hover_text(format!("{:.0} ms later", step * 1000.0));
                    if r.clicked() {
                        new = grid.map(|g| Some(g.slip(i, *step)));
                    }
                }
            });
        });

        if reset {
            self.session.lock().reset_grid(d);
        }
        if let Some(g) = new {
            self.pending.push(Action::SetGrid {
                deck: d as u8,
                track,
                grid: g.map(Box::new),
            });
        }
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

/// "1", ".1", ".01" for an ADJUST step.
fn step_label(step: f64) -> String {
    let s = format!("{step}");
    s.strip_prefix('0').unwrap_or(&s).to_string()
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

/// The loop on a waveform: a band from loop in to loop out (bright while
/// looping), or just the in point while LOOP IN waits for LOOP OUT.
fn loop_band(p: &egui::Painter, rect: egui::Rect, st: &DeckState, x_of: impl Fn(f64) -> f32) {
    let edge = |x: f32, c: Color32| {
        if rect.x_range().contains(x) {
            p.line_segment(
                [pos2(x, rect.top()), pos2(x, rect.bottom())],
                Stroke::new(1.5_f32, c),
            );
        }
    };
    match (st.loop_span(), st.loop_in) {
        (Some((a, b)), _) => {
            let c = if st.looping {
                LOOP
            } else {
                LOOP.gamma_multiply(0.45)
            };
            let band =
                egui::Rect::from_min_max(pos2(x_of(a), rect.top()), pos2(x_of(b), rect.bottom()))
                    .intersect(rect);
            if band.width() > 0.0 {
                p.rect_filled(
                    band,
                    0.0,
                    c.gamma_multiply(if st.looping { 0.22 } else { 0.1 }),
                );
            }
            edge(x_of(a), c);
            edge(x_of(b), c);
        }
        (None, Some(a)) => edge(x_of(a), LOOP),
        _ => {}
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
