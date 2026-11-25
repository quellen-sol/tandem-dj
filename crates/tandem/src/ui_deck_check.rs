//! The DECK button check window (Settings → Controller → **Check the DECK
//! buttons…**). The steps, and what they find, are in
//! `tandem_core::deck_check`; this window shows them, hands the check what
//! the controller sends, and saves the report.

use crate::app::TandemApp;
use crate::ui_decks::{AMBER, GREEN};
use eframe::egui::{self, RichText};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tandem_core::deck_check::{
    self, jog_screen_deck, DeckCheck, Fix, Pretend, ReportInfo, Seen, Step, Want,
};
use tandem_core::mapping::{builtin_preset, Mapping, MidiOut, Trigger};

/// A step that has heard nothing this long says so.
const QUIET_HINT_SECS: f64 = 4.0;
/// The jog wheel screen's digit is sent again this long after the first
/// time (Mixxx does, in case the unit dropped it).
const SCREEN_AGAIN_SECS: f64 = 0.15;

pub struct DeckCheckUi {
    check: DeckCheck,
    /// Past the first page.
    started: bool,
    /// When the check started listening: message times count from here.
    start: Instant,
    /// When the step being done started (s, on the check's clock).
    step_since: f64,
    step_at: usize,
    /// The left side's MIDI channel (the jog wheel screen test talks to
    /// it), and when the screen was asked to show deck 3, once it has been.
    left_channel: u8,
    screen_asked: Option<f64>,
    screen_again: bool,
    /// Once it's over: what it found, the report, where the report went,
    /// and decks C and D to map, if it found where they are.
    summary: Vec<String>,
    details: Vec<String>,
    report: Option<String>,
    saved: Option<Result<PathBuf, String>>,
    fix: Option<Fix>,
    /// Once "Map decks C and D" is clicked: the mapping before, for Undo.
    fixed: Option<Mapping>,
    copied: bool,
    /// `--dev-deck-check`: a pretend controller doing the steps up to the
    /// step given (all of them if None).
    pretend: Option<(Pretend, Option<usize>)>,
    /// `--dev-deck-check hardware+map`: click "Map decks C and D" at the end.
    pretend_map: bool,
    /// At the last step it presses CUE, whatever is asked for (`N!`).
    pretend_wrong: bool,
}

/// A pretend controller has done the steps it was asked to: the check
/// stays where it is.
fn pretend_holds(st: &DeckCheckUi) -> bool {
    matches!(st.pretend, Some((_, Some(s))) if st.check.at >= s)
}

/// Open Explorer on the folder with `path` selected. Explorer reads its
/// own command line: the path goes in quotes after `/select,`, which Rust's
/// quoting of a whole argument with spaces in it would break.
fn show_file(path: &std::path::Path) {
    let mut cmd = std::process::Command::new("explorer");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(format!("/select,\"{}\"", path.display()));
    }
    #[cfg(not(windows))]
    cmd.arg(path.parent().unwrap_or(path));
    let _ = cmd.spawn();
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether this is a Denon SC Live 4, whose jog wheel screens Tandem can
/// ask to show a deck number.
fn is_sc_live_4(port: &str, mapping: &Mapping) -> bool {
    mapping.preset.as_deref() == Some("Denon SC Live 4")
        || builtin_preset(port).and_then(|p| p.preset).as_deref() == Some("Denon SC Live 4")
}

impl TandemApp {
    /// Whether Settings → Controller offers the check: a controller with
    /// decks C and D, or one Tandem has no mapping of its own for.
    pub(crate) fn deck_check_fits(mapping: &Mapping, port: &str) -> bool {
        mapping.preset.is_none()
            || is_sc_live_4(port, mapping)
            || mapping
                .bindings
                .iter()
                .any(|b| b.target.deck().is_some_and(|d| d >= 2))
    }

    /// Open the check. From now until it's over, the controller's messages
    /// go to it instead of the mapping.
    pub(crate) fn open_deck_check(&mut self) {
        // The step-by-step setup takes the controller's messages too.
        if self.ui.wizard.is_some() {
            return;
        }
        if let Some(mut old) = self.ui.deck_check.take() {
            self.close_deck_check(&mut old);
        }
        let port = self.midi.port.clone().unwrap_or_default();
        let check = {
            let engine = self.midi.engine.lock();
            let screen = self.midi.has_output && is_sc_live_4(&port, &engine.mapping);
            DeckCheck::for_mapping(screen, &engine.mapping)
        };
        *self.midi.tap.lock() = Some(Vec::new());
        // Whatever was held on the controller (a jog wheel touched, SHIFT)
        // is let go now: its release would go to the check.
        let room = self.session.lock().latest();
        let released = self.midi.engine.lock().let_go(&room);
        for o in released {
            if let MidiOut::Action(a) = o {
                self.session.lock().act(a);
            }
        }
        self.ui.deck_check = Some(DeckCheckUi {
            check,
            started: false,
            start: Instant::now(),
            step_since: 0.0,
            step_at: 0,
            left_channel: 4,
            screen_asked: None,
            screen_again: false,
            summary: Vec::new(),
            details: Vec::new(),
            report: None,
            saved: None,
            fix: None,
            fixed: None,
            copied: false,
            pretend: None,
            pretend_map: false,
            pretend_wrong: false,
        });
        log::info!("DECK button check: started on {port}");
    }

    /// `--dev-deck-check <start|software|hardware|quiet>[:N]`: the check
    /// with a pretend Denon SC Live 4 (no MIDI device needed) whose DECK
    /// button works that way, done up to step N (1-based; all if no N).
    /// `hardware+map` also clicks "Map decks C and D" at the end, and `N!`
    /// presses CUE at step N, to see it refused.
    pub(crate) fn dev_deck_check(&mut self, how: &str) {
        let (how, map) = match how.strip_suffix("+map") {
            Some(h) => (h, true),
            None => (how, false),
        };
        let (how, wrong) = match how.strip_suffix('!') {
            Some(h) => (h, true),
            None => (how, false),
        };
        let (model, stop) = match how.split_once(':') {
            Some((m, n)) => (m, n.parse::<usize>().ok()),
            None => (how, None),
        };
        if let Some(p) = builtin_preset("SC LIVE 4") {
            self.midi.engine.lock().load(p);
        }
        self.open_deck_check();
        let Some(ui) = self.ui.deck_check.as_mut() else {
            return;
        };
        // A pretend controller has an output port.
        ui.check = DeckCheck::for_mapping(true, &self.midi.engine.lock().mapping);
        let pretend = match model {
            "software" => Pretend::Software,
            "hardware" => Pretend::Hardware,
            "quiet" => Pretend::Quiet,
            _ => return,
        };
        ui.pretend = Some((pretend, stop.map(|n| n.saturating_sub(1))));
        ui.pretend_map = map;
        ui.pretend_wrong = wrong;
        ui.started = true;
    }

    pub(crate) fn deck_check_window(&mut self, ctx: &egui::Context) {
        let Some(mut ui_state) = self.ui.deck_check.take() else {
            return;
        };
        let keep = self.deck_check_frame(ctx, &mut ui_state);
        if keep {
            self.ui.deck_check = Some(ui_state);
        } else {
            self.close_deck_check(&mut ui_state);
        }
    }

    pub(crate) fn close_deck_check(&mut self, st: &mut DeckCheckUi) {
        *self.midi.tap.lock() = None;
        if st.screen_asked.is_some() {
            self.restore_screen(st);
            st.screen_asked = None;
        }
        if !st.check.done() {
            log::info!(
                "DECK button check: closed at step {} of {}",
                st.check.at + 1,
                st.check.steps.len()
            );
        }
    }

    /// One frame of the check: listen, move on, draw. Returns whether the
    /// window stays open.
    fn deck_check_frame(&mut self, ctx: &egui::Context, st: &mut DeckCheckUi) -> bool {
        let now = st.start.elapsed().as_secs_f64();
        let heard = self
            .midi
            .tap
            .lock()
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        if st.started {
            for (when, bytes) in heard {
                let t = when.saturating_duration_since(st.start).as_secs_f64();
                st.check.hear(t, &bytes);
            }
            self.pretend_step(st, now);
            if !pretend_holds(st) {
                st.check.tick(now);
            }
        }
        if st.check.at != st.step_at {
            st.step_at = st.check.at;
            st.step_since = now;
        }
        self.follow_screen_step(st, now);
        if st.check.done() && st.report.is_none() {
            self.finish_deck_check(st);
        }

        let mut open = true;
        let mut again = false;
        egui::Window::new("DECK button check")
            .open(&mut open)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .collapsible(false)
            .default_width(600.0)
            .show(ctx, |ui| {
                ui.set_max_width(640.0);
                if !st.started {
                    self.deck_check_intro(ui, st);
                } else if st.check.done() {
                    again = self.deck_check_results(ui, st);
                } else {
                    again = self.deck_check_step(ui, st, now);
                }
            });
        if again {
            self.close_deck_check(st);
            self.open_deck_check();
            // Back to the first page: a side may be on its other deck now.
            if let Some(new) = self.ui.deck_check.take() {
                *st = new;
            }
        }
        open
    }

    /// A pretend controller does the step it's on (`--dev-deck-check`).
    fn pretend_step(&mut self, st: &mut DeckCheckUi, now: f64) {
        let Some((model, stop)) = st.pretend else {
            return;
        };
        if stop.is_some_and(|s| st.check.at > s) {
            return;
        }
        let Some(step) = st.check.step() else {
            return;
        };
        let at = st.check.at;
        if !st.check.heard(at).is_empty() || st.check.got().is_some() {
            return;
        }
        if step.wants() == Want::Answer {
            if stop.is_some_and(|s| at >= s) {
                return;
            }
            let seen = match (step, model) {
                (Step::ScreenChanged { .. }, Pretend::Hardware) => Seen::Yes,
                (Step::ScreenChanged { .. }, Pretend::Software) => Seen::No,
                (Step::ScreenChanged { .. }, _) => Seen::NotSure,
                _ => Seen::Yes,
            };
            st.check.answer(seen);
            return;
        }
        let msgs = match step {
            Step::Press { side, .. } if st.pretend_wrong && stop == Some(at) => {
                vec![vec![0x94 + side, 0x09, 0x7F], vec![0x94 + side, 0x09, 0]]
            }
            _ => model.messages(step),
        };
        for (i, m) in msgs.iter().enumerate() {
            st.check.hear(now + i as f64 * 0.04, m);
        }
        if stop.is_some_and(|s| at >= s) {
            return;
        }
        // Straight on, without waiting.
        if !st.check.tick(now + 10.0) {
            st.check.next();
        }
    }

    /// The jog wheel screen test: ask the left screen to show deck 3 on
    /// the way into the step, the digit again a moment later, and deck 1
    /// on the way out.
    fn follow_screen_step(&mut self, st: &mut DeckCheckUi, now: f64) {
        let on_it = st.check.step() == Some(Step::ScreenShows);
        match (on_it, st.screen_asked) {
            (true, None) => {
                // The left side's channel, as PLAY showed it.
                if let Some(t) = st.check.side(0, &self.midi.engine.lock().mapping).play[0] {
                    st.left_channel = t.channel;
                }
                self.midi.send(&jog_screen_deck(st.left_channel, 3));
                st.screen_asked = Some(now);
                st.screen_again = false;
            }
            (true, Some(t)) if !st.screen_again && now - t >= SCREEN_AGAIN_SECS => {
                self.midi.send(&[vec![0x90 | st.left_channel, 0x38, 3]]);
                st.screen_again = true;
            }
            (false, Some(_)) => {
                self.restore_screen(st);
                st.screen_asked = None;
            }
            _ => {}
        }
    }

    /// After the jog wheel screen test: the left screen, the only one it
    /// touched, shows the deck the left side is on again.
    fn restore_screen(&self, st: &DeckCheckUi) {
        let deck = self.midi.engine.lock().side_deck(0) + 1;
        self.midi.send(&jog_screen_deck(st.left_channel, deck));
    }

    /// The check is over: the controller works as usual again, and the
    /// report is saved beside the log and written into it.
    fn finish_deck_check(&mut self, st: &mut DeckCheckUi) {
        *self.midi.tap.lock() = None;
        let mapping = self.midi.engine.lock().mapping.clone();
        let port = match (&self.midi.port, st.pretend) {
            (Some(p), _) => p.clone(),
            (None, Some(_)) => "SC LIVE 4 (pretend, --dev-deck-check)".into(),
            (None, None) => "none".into(),
        };
        let secs = now_secs();
        let info = ReportInfo {
            version: env!("CARGO_PKG_VERSION").into(),
            port,
            output: self.midi.has_output || st.pretend.is_some(),
            unix_secs: secs,
        };
        let report = st.check.report(&info, &mapping);
        st.summary = st.check.summary(&mapping);
        st.details = st.check.details(&mapping);
        st.fix = st.check.fix(&mapping);
        if let (true, Some(fix)) = (st.pretend_map, &st.fix) {
            st.fixed = Some(std::mem::replace(
                &mut self.midi.engine.lock().mapping,
                fix.mapping.clone(),
            ));
        }
        let path = self
            .paths
            .data_dir
            .join(format!("deck-check-{}.txt", deck_check::utc_stamp(secs)));
        st.saved = Some(match std::fs::write(&path, report.replace('\n', "\r\n")) {
            Ok(()) => Ok(path),
            Err(e) => Err(format!("{e}")),
        });
        log::info!("DECK button check report:\n{report}");
        match &st.saved {
            Some(Ok(p)) => log::info!("DECK button check: done, report saved as {}", p.display()),
            _ => log::info!("DECK button check: done"),
        }
        st.report = Some(report);
    }

    fn deck_check_intro(&mut self, ui: &mut egui::Ui, st: &mut DeckCheckUi) {
        ui.label(
            RichText::new("What do your DECK buttons do?")
                .size(18.0)
                .strong(),
        );
        ui.add_space(4.0);
        ui.label(
            "This finds out what your controller sends when you press DECK, so Tandem can \
             switch decks the way your controller does. It takes about a minute: press the \
             buttons it asks for, one at a time, and it moves on by itself.",
        );
        ui.add_space(4.0);
        for line in [
            "While this window is open, your controller plays nothing in the room.",
            "If your controller shows which deck a side is on, start on decks 1 and 2.",
            "Keep your hands off the jog wheels, and press each button once.",
            "At the end it shows what it found and saves a report to send to us.",
        ] {
            ui.label(format!("•  {line}"));
        }
        if self.midi.port.is_none() && st.pretend.is_none() {
            ui.label(
                RichText::new("No controller is picked in Settings > Controller.").color(AMBER),
            );
        }
        ui.add_space(8.0);
        if ui.button(RichText::new("Start").strong()).clicked() {
            // Anything sent before now isn't part of a step.
            if let Some(h) = self.midi.tap.lock().as_mut() {
                h.clear();
            }
            st.started = true;
            st.step_since = st.start.elapsed().as_secs_f64();
        }
    }

    /// One step. Returns whether to start again.
    fn deck_check_step(&mut self, ui: &mut egui::Ui, st: &mut DeckCheckUi, now: f64) -> bool {
        let Some(step) = st.check.step() else {
            return false;
        };
        let at = st.check.at;
        let n = st.check.steps.len();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Step {} of {n}", at + 1)).weak());
            ui.add(egui::ProgressBar::new(at as f32 / n as f32).desired_width(200.0));
        });
        ui.add_space(6.0);
        ui.label(RichText::new(step.instruction()).size(18.0).strong());
        ui.add_space(6.0);
        let waited = now - st.step_since;
        match step.wants() {
            Want::Answer => {
                let answers: [(&str, Seen); 3] = match step {
                    Step::ScreenShows => [
                        ("Yes, it shows 3", Seen::Yes),
                        ("No", Seen::No),
                        ("Not sure", Seen::NotSure),
                    ],
                    _ => [
                        ("Yes, it changed", Seen::Yes),
                        ("No, it stayed the same", Seen::No),
                        ("I didn't see", Seen::NotSure),
                    ],
                };
                ui.horizontal(|ui| {
                    for (label, seen) in answers {
                        if ui.button(label).clicked() {
                            st.check.answer(seen);
                        }
                    }
                });
                if step == Step::ScreenShows && !self.midi.has_output && st.pretend.is_none() {
                    ui.label(
                        RichText::new("Tandem has no way to talk to this controller (no MIDI output port): answer No.")
                            .small()
                            .color(AMBER),
                    );
                }
            }
            want => {
                let what = if want == Want::Knob {
                    "a fader"
                } else {
                    "a button"
                };
                if st.check.got().is_some() {
                    ui.label(RichText::new("Got it.").color(GREEN));
                } else if let Some(t) = st.check.refused() {
                    let does = {
                        let engine = self.midi.engine.lock();
                        control_name(&engine.mapping, t)
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "That was {does}, not what's asked for here. Try again."
                            ))
                            .color(AMBER),
                        );
                        if ui
                            .button("That was it")
                            .on_hover_text(
                                "Tandem's mapping has this control as something else, but it's \
                                 the one asked for.",
                            )
                            .clicked()
                        {
                            st.check.take_refused();
                        }
                    });
                } else if waited >= QUIET_HINT_SECS {
                    ui.label(
                        RichText::new(format!(
                            "Nothing from {what} yet. If you've done it, it sent nothing Tandem \
                             can see: click \"It sent nothing\" to go on."
                        ))
                        .color(AMBER),
                    );
                } else {
                    ui.label(RichText::new("Waiting for you…").weak());
                }
            }
        }
        ui.add_space(4.0);
        let heard = st.check.heard(at);
        if !heard.is_empty() {
            let mapping = self.midi.engine.lock().mapping.clone();
            ui.label(RichText::new("Heard in this step:").small().weak());
            let skip = heard.len().saturating_sub(8);
            if skip > 0 {
                ui.label(
                    RichText::new(format!("… and {skip} more before these"))
                        .small()
                        .weak(),
                );
            }
            for h in &heard[skip..] {
                ui.label(RichText::new(h.line(&mapping)).monospace());
            }
        }
        ui.add_space(8.0);
        let mut again = false;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(st.check.can_back(), egui::Button::new("◀ Back"))
                .on_disabled_hover_text(
                    "DECK has switched the side already, so the steps can't go back past it: \
                     click Start again instead.",
                )
                .clicked()
            {
                st.check.back();
            }
            if step.wants() != Want::Answer
                && st.check.got().is_none()
                && ui.button("It sent nothing").clicked()
            {
                st.check.next();
            }
            if ui.button("Start again").clicked() {
                again = true;
            }
        });
        again
    }

    /// What it found, the report, and decks C and D to map. Returns
    /// whether to start again.
    fn deck_check_results(&mut self, ui: &mut egui::Ui, st: &mut DeckCheckUi) -> bool {
        ui.label(RichText::new("What the check found").size(18.0).strong());
        ui.label(
            RichText::new("Your controller works as usual again.")
                .small()
                .weak(),
        );
        ui.add_space(4.0);
        for p in &st.summary {
            ui.label(format!("•  {p}"));
            ui.add_space(2.0);
        }
        let mapping = self.midi.engine.lock().mapping.clone();
        let next = match &st.fixed {
            // Judged on the mapping as it was before.
            Some(before) => st.check.next_step(before, true),
            None => st.check.next_step(&mapping, false),
        };
        ui.label(format!("•  {next}"));
        ui.add_space(2.0);
        ui.collapsing("Details", |ui| {
            let list_h = (ui.ctx().screen_rect().height() - 420.0).clamp(120.0, 360.0);
            egui::ScrollArea::vertical()
                .id_salt("deck-check-details")
                .max_height(list_h)
                .show(ui, |ui| {
                    for p in &st.details {
                        ui.label(RichText::new(format!("•  {p}")).small());
                    }
                });
        });
        ui.add_space(6.0);
        let mut undo = false;
        match (&st.fixed, &st.fix) {
            (Some(before), Some(fix)) => {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "Mapped {} controls for decks C and D. Press DECK and try them.",
                            fix.added.len()
                        ))
                        .color(GREEN),
                    );
                    if ui
                        .button("Undo")
                        .on_hover_text("Puts your mapping back as it was before.")
                        .clicked()
                    {
                        self.midi.engine.lock().mapping = before.clone();
                        self.midi.save_mapping(&self.paths.mappings_dir);
                        log::info!("DECK button check: decks C and D unmapped again");
                        undo = true;
                    }
                });
                if !fix.left_out.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "Not mapped, since the check didn't hear where they are: {}.",
                            fix.left_out.join(", ")
                        ))
                        .small()
                        .weak(),
                    );
                }
                ui.collapsing("What was added", |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("deck-check-added")
                        .max_height(160.0)
                        .show(ui, |ui| {
                            for line in &fix.added {
                                ui.label(RichText::new(line).monospace().small());
                            }
                        });
                });
            }
            (None, Some(fix)) => {
                let n = fix.added.len();
                if ui
                    .button(RichText::new("Map decks C and D").strong())
                    .on_hover_text(format!(
                        "Adds {n} controls: deck A's and B's, copied to where the check heard decks 3 and 4."
                    ))
                    .clicked()
                {
                    let before =
                        std::mem::replace(&mut self.midi.engine.lock().mapping, fix.mapping.clone());
                    self.midi.save_mapping(&self.paths.mappings_dir);
                    log::info!("DECK button check: mapped {n} controls for decks C and D");
                    st.fixed = Some(before);
                }
            }
            _ => {}
        }
        if undo {
            st.fixed = None;
        }
        ui.separator();
        match &st.saved {
            Some(Ok(path)) => {
                ui.label(
                    RichText::new(format!("The report is saved as {}", path.display())).small(),
                );
            }
            Some(Err(e)) => {
                ui.label(
                    RichText::new(format!("Couldn't save the report: {e}"))
                        .small()
                        .color(AMBER),
                );
            }
            None => {}
        }
        let mut again = false;
        ui.horizontal(|ui| {
            if let Some(r) = &st.report {
                let label = if st.copied { "Copied" } else { "Copy report" };
                if ui.button(label).clicked() {
                    ui.ctx().copy_text(r.clone());
                    st.copied = true;
                }
            }
            if let Some(Ok(path)) = &st.saved {
                if ui.button("Show the file").clicked() {
                    show_file(path);
                }
            }
            if ui.button("Do it again").clicked() {
                again = true;
            }
        });
        if let Some(r) = &st.report {
            ui.collapsing("Show the report", |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("deck-check-report")
                    .max_height(260.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new(r).monospace().small());
                    });
            });
        }
        again
    }
}

/// What the mapping has a control as, or the control itself.
fn control_name(mapping: &Mapping, t: Trigger) -> String {
    mapping
        .bindings
        .iter()
        .find(|b| b.trigger == t && !b.shift)
        .map_or_else(|| t.label(), |b| b.target.label())
}
