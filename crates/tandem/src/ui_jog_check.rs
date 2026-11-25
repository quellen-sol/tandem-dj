//! The jog wheel check window (Settings → Controller → **Check the jog
//! wheels…**). The steps, and what they find, are in
//! `tandem_core::jog_check`; this window shows them, hands the check what the
//! controller sends, fixes the mapping and saves the report. It works like
//! the DECK button check's (`ui_deck_check`).

use crate::app::TandemApp;
use crate::ui_deck_check::{control_name, now_secs, show_file};
use crate::ui_decks::{AMBER, GREEN};
use eframe::egui::{self, RichText};
use std::path::PathBuf;
use std::time::Instant;
use tandem_core::deck_check::{utc_stamp, ReportInfo};
use tandem_core::jog_check::{Do, Fix, JogCheck, PretendDj};
use tandem_core::mapping::{builtin_preset, Mapping, MidiOut, RelMode};

/// A step that has heard nothing this long says so.
const QUIET_HINT_SECS: f64 = 5.0;

pub struct JogCheckUi {
    check: JogCheck,
    /// Past the first page.
    started: bool,
    /// When the check started listening: message times count from here.
    start: Instant,
    /// When the step being done started (s, on the check's clock).
    step_since: f64,
    step_at: usize,
    /// Once it's over: what it found, the report, where the report went, and
    /// the mapping put right, if it wasn't.
    summary: Vec<String>,
    details: Vec<String>,
    report: Option<String>,
    saved: Option<Result<PathBuf, String>>,
    fix: Option<Fix>,
    /// Once "Fix the mapping" is clicked: the mapping before, for Undo.
    fixed: Option<Mapping>,
    copied: bool,
    /// `--dev-jog-check`: a pretend controller doing the steps up to the step
    /// given (all of them if None).
    pretend: Option<(PretendDj, Option<usize>)>,
    /// `+fix`: click "Fix the mapping" at the end.
    pretend_fix: bool,
    /// At the last step it presses PLAY, whatever is asked for (`N!`).
    pretend_wrong: bool,
}

impl TandemApp {
    /// Open the check. From now until it's over, the controller's messages
    /// go to it instead of the mapping.
    pub(crate) fn open_jog_check(&mut self) {
        // The step-by-step setup takes the controller's messages too.
        if self.ui.wizard.is_some() {
            return;
        }
        if let Some(mut other) = self.ui.deck_check.take() {
            self.close_deck_check(&mut other);
        }
        if let Some(mut old) = self.ui.jog_check.take() {
            self.close_jog_check(&mut old);
        }
        let port = self.midi.port.clone().unwrap_or_default();
        let check = JogCheck::new(&self.midi.engine.lock().mapping);
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
        self.ui.jog_check = Some(JogCheckUi {
            check,
            started: false,
            start: Instant::now(),
            step_since: 0.0,
            step_at: 0,
            summary: Vec::new(),
            details: Vec::new(),
            report: None,
            saved: None,
            fix: None,
            fixed: None,
            copied: false,
            pretend: None,
            pretend_fix: false,
            pretend_wrong: false,
        });
        log::info!("Jog wheel check: started on {port}");
    }

    /// `--dev-jog-check <start|model>[:N]`: the check with a pretend
    /// controller (no MIDI device needed; the models are
    /// `PretendDj::named`'s), done up to the middle of step N (1-based; all
    /// of them if no N). `N!` presses PLAY at step N, to see it refused;
    /// `+relearned` starts with the jog wheels learned as counting around
    /// 64, and `+fix` clicks "Fix the mapping" at the end.
    pub(crate) fn dev_jog_check(&mut self, how: &str) {
        let mut words = how.split('+');
        let how = words.next().unwrap_or_default();
        let extra: Vec<&str> = words.collect();
        let (how, wrong) = match how.strip_suffix('!') {
            Some(h) => (h, true),
            None => (how, false),
        };
        let (model, stop) = match how.split_once(':') {
            Some((m, n)) => (m, n.parse::<usize>().ok()),
            None => (how, None),
        };
        if let Some(mut p) = builtin_preset("SC LIVE 4") {
            if extra.contains(&"relearned") {
                p.bindings.retain(|b| !b.rel.is_position());
                for b in p.bindings.iter_mut().filter(|b| b.target.is_jog_turn()) {
                    b.rel = RelMode::Offset64;
                }
            }
            self.midi.engine.lock().load(p);
        }
        self.open_jog_check();
        let Some(ui) = self.ui.jog_check.as_mut() else {
            return;
        };
        let Some(model) = PretendDj::named(model) else {
            return;
        };
        ui.pretend = Some((PretendDj::new(model), stop.map(|n| n.saturating_sub(1))));
        ui.pretend_fix = extra.contains(&"fix");
        ui.pretend_wrong = wrong;
        ui.started = true;
    }

    pub(crate) fn jog_check_window(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.ui.jog_check.take() else {
            return;
        };
        if self.jog_check_frame(ctx, &mut st) {
            self.ui.jog_check = Some(st);
        } else {
            self.close_jog_check(&mut st);
        }
    }

    pub(crate) fn close_jog_check(&mut self, st: &mut JogCheckUi) {
        *self.midi.tap.lock() = None;
        if !st.check.done() {
            log::info!(
                "Jog wheel check: closed at step {} of {}",
                st.check.at + 1,
                st.check.steps.len()
            );
        }
    }

    /// One frame of the check: listen, move on, draw. Returns whether the
    /// window stays open.
    fn jog_check_frame(&mut self, ctx: &egui::Context, st: &mut JogCheckUi) -> bool {
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
            let holds = matches!(st.pretend, Some((_, Some(s))) if st.check.at >= s);
            pretend_step(st, now);
            if !holds {
                st.check.tick(now);
            }
        }
        if st.check.at != st.step_at {
            st.step_at = st.check.at;
            st.step_since = now;
        }
        if st.check.done() && st.report.is_none() {
            self.finish_jog_check(st);
        }

        let mut open = true;
        let mut again = false;
        egui::Window::new("Jog wheel check")
            .open(&mut open)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .collapsible(false)
            .default_width(600.0)
            .show(ctx, |ui| {
                ui.set_max_width(640.0);
                if !st.started {
                    self.jog_check_intro(ui, st);
                } else if st.check.done() {
                    again = self.jog_check_results(ui, st);
                } else {
                    again = self.jog_check_step(ui, st, now);
                }
            });
        if again {
            self.close_jog_check(st);
            self.open_jog_check();
            if let Some(new) = self.ui.jog_check.take() {
                *st = new;
            }
        }
        open
    }

    /// The check is over: the controller works as usual again, and the
    /// report is saved beside the log and written into it.
    fn finish_jog_check(&mut self, st: &mut JogCheckUi) {
        *self.midi.tap.lock() = None;
        let mapping = self.midi.engine.lock().mapping.clone();
        let port = match (&self.midi.port, &st.pretend) {
            (Some(p), _) => p.clone(),
            (None, Some(_)) => "SC LIVE 4 (pretend, --dev-jog-check)".into(),
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
        if let (true, Some(fix)) = (st.pretend_fix, &st.fix) {
            st.fixed = Some(std::mem::replace(
                &mut self.midi.engine.lock().mapping,
                fix.mapping.clone(),
            ));
        }
        let path = self
            .paths
            .data_dir
            .join(format!("jog-check-{}.txt", utc_stamp(secs)));
        st.saved = Some(match std::fs::write(&path, report.replace('\n', "\r\n")) {
            Ok(()) => Ok(path),
            Err(e) => Err(format!("{e}")),
        });
        log::info!("Jog wheel check report:\n{report}");
        match &st.saved {
            Some(Ok(p)) => log::info!("Jog wheel check: done, report saved as {}", p.display()),
            _ => log::info!("Jog wheel check: done"),
        }
        st.report = Some(report);
    }

    fn jog_check_intro(&mut self, ui: &mut egui::Ui, st: &mut JogCheckUi) {
        ui.label(
            RichText::new("What do your jog wheels send?")
                .size(18.0)
                .strong(),
        );
        ui.add_space(4.0);
        ui.label(
            "This finds out what your controller sends when you touch a jog wheel, turn it with a \
             hand on top and turn it by its edge, so Tandem can scratch and nudge the way your \
             wheels work. It takes about two minutes: do what it asks, one thing at a time, and it \
             moves on by itself.",
        );
        ui.add_space(4.0);
        for line in [
            "While this window is open, your controller plays nothing in the room.",
            "If your controller shows which deck a side is on, start on decks 1 and 2.",
            "Use one hand, and leave every other control alone.",
            "Have something to mark the wheel with, like a bit of tape, for one full turn.",
            "At the end it shows what it found, can fix your mapping, and saves a report to send to us.",
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
    fn jog_check_step(&mut self, ui: &mut egui::Ui, st: &mut JogCheckUi, now: f64) -> bool {
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
        ui.label(RichText::new(step.hint()).weak());
        ui.add_space(6.0);

        let progress = st.check.progress(at);
        let (got, hand_on) = (st.check.got(), st.check.hand_on());
        let turn = step.what == Do::FullTurn;
        let edge = matches!(step.what, Do::EdgeCw | Do::EdgeCcw);
        // How long since anything happened.
        let last = [progress.touched, progress.moved]
            .into_iter()
            .flatten()
            .fold(st.step_since, f64::max);
        let quiet = now - last;
        if let Some(t) = st.check.refused().filter(|_| !got) {
            let does = control_name(&self.midi.engine.lock().mapping, t);
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
                        "Tandem's mapping has this control as something else, but it's this jog \
                         wheel's.",
                    )
                    .clicked()
                {
                    st.check.take_refused();
                }
            });
        } else if turn && got {
            let counted = match st.check.count() {
                Some(n) => format!("Counted {n} ticks so far. Stop after exactly one turn."),
                None => "Stop after exactly one turn.".into(),
            };
            ui.label(RichText::new(counted).color(GREEN));
        } else if got && hand_on && edge {
            ui.label(
                RichText::new(
                    "The wheel says a hand is on top. If you only held its edge, that's worth \
                     knowing: let go, and it goes on (or click \"Go on\"). If you touched the top, do \
                     this step again.",
                )
                .color(AMBER),
            );
        } else if got && hand_on {
            ui.label(RichText::new("Got it. Now lift your hand off.").color(GREEN));
        } else if got {
            ui.label(RichText::new("Got it.").color(GREEN));
        } else if quiet >= QUIET_HINT_SECS {
            ui.label(
                RichText::new(
                    "Nothing more from the jog wheel. If you've done it, it sent nothing Tandem \
                     can see: click \"It sent nothing\" to go on.",
                )
                .color(AMBER),
            );
        } else {
            ui.label(RichText::new("Waiting for you…").weak());
        }
        if got && !turn && !(edge && hand_on) && quiet >= QUIET_HINT_SECS {
            ui.label(
                RichText::new(
                    "Hand off already? Then the wheel hasn't said so, and that's worth knowing: \
                     click \"Go on\".",
                )
                .color(AMBER),
            );
        }
        ui.add_space(4.0);
        let mut live = Vec::new();
        if progress.touched.is_some() {
            live.push(format!(
                "touched: {}",
                if progress.held.is_empty() {
                    "let go".to_string()
                } else {
                    progress
                        .held
                        .iter()
                        .map(|t| t.label())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ));
        }
        if progress.moves > 0 {
            let on: Vec<String> = progress.controls.iter().map(|t| t.label()).collect();
            live.push(format!(
                "turning: {} messages on {}",
                progress.moves,
                on.join(", ")
            ));
        }
        if !live.is_empty() {
            ui.label(RichText::new(live.join("   ·   ")).small());
        }
        let heard = st.check.heard(at);
        if !heard.is_empty() {
            let mapping = self.midi.engine.lock().mapping.clone();
            ui.label(RichText::new("Heard last:").small().weak());
            for h in &heard[heard.len().saturating_sub(4)..] {
                ui.label(RichText::new(h.line(&mapping)).monospace().small());
            }
        }
        ui.add_space(8.0);
        let mut again = false;
        ui.horizontal(|ui| {
            if turn
                && ui
                    .add_enabled(
                        got,
                        egui::Button::new(RichText::new("That was one turn").strong()),
                    )
                    .clicked()
            {
                st.check.next();
            }
            if ui
                .add_enabled(st.check.can_back(), egui::Button::new("◀ Back"))
                .clicked()
            {
                st.check.back();
            }
            if ui
                .add_enabled(
                    !st.check.heard(at).is_empty(),
                    egui::Button::new("Do this step again"),
                )
                .clicked()
            {
                st.check.again();
                st.step_since = now;
            }
            let go_on = match (got, turn) {
                (true, true) => None,
                (true, false) => Some("Go on"),
                (false, _) => Some("It sent nothing"),
            };
            if go_on.is_some_and(|l| ui.button(l).clicked()) {
                st.check.next();
            }
            if ui.button("Start again").clicked() {
                again = true;
            }
        });
        again
    }

    /// What it found, the report, and the mapping to fix. Returns whether to
    /// start again.
    fn jog_check_results(&mut self, ui: &mut egui::Ui, st: &mut JogCheckUi) -> bool {
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
                .id_salt("jog-check-details")
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
                        RichText::new("The mapping is fixed. Try the jog wheels.").color(GREEN),
                    );
                    if ui
                        .button("Undo")
                        .on_hover_text("Puts your mapping back as it was before.")
                        .clicked()
                    {
                        self.midi.engine.lock().mapping = before.clone();
                        self.midi.save_mapping(&self.paths.mappings_dir);
                        log::info!("Jog wheel check: mapping put back as it was");
                        undo = true;
                    }
                });
                ui.collapsing("What changed", |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("jog-check-changed")
                        .max_height(160.0)
                        .show(ui, |ui| {
                            for line in &fix.changed {
                                ui.label(RichText::new(line).monospace().small());
                            }
                        });
                });
            }
            (None, Some(fix)) => {
                let fix_it = ui
                    .button(RichText::new("Fix the mapping").strong())
                    .on_hover_text(format!(
                        "Maps the jog wheels as the check heard them, and leaves the rest of your \
                         mapping alone:\n{}",
                        fix.changed.join("\n")
                    ))
                    .clicked();
                if fix_it {
                    let before = std::mem::replace(
                        &mut self.midi.engine.lock().mapping,
                        fix.mapping.clone(),
                    );
                    self.midi.save_mapping(&self.paths.mappings_dir);
                    log::info!(
                        "Jog wheel check: fixed the mapping:\n{}",
                        fix.changed.join("\n")
                    );
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
                egui::ScrollArea::both()
                    .id_salt("jog-check-report")
                    .max_height(260.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new(r).monospace().small());
                    });
            });
        }
        again
    }
}

/// A pretend controller does the step it's on (`--dev-jog-check`), or half of
/// the one it's to stop at.
fn pretend_step(st: &mut JogCheckUi, now: f64) {
    let Some((dj, stop)) = st.pretend.as_mut() else {
        return;
    };
    let at = st.check.at;
    let Some(step) = st.check.step() else {
        return;
    };
    let last = *stop == Some(at);
    if stop.is_some_and(|s| at > s) || !st.check.heard(at).is_empty() {
        return;
    }
    let mut t = now;
    if last && st.pretend_wrong {
        st.check.hear(t, &[0x94 + step.side, 0x0A, 0x7F]);
        st.check.hear(t + 0.1, &[0x94 + step.side, 0x0A, 0x00]);
        return;
    }
    let mut msgs = dj.messages(step);
    if last {
        msgs.truncate(msgs.len() * 6 / 10);
    }
    let quick = if last { 0.0 } else { 1.0 };
    for (dt, m) in msgs {
        t += dt * quick;
        st.check.hear(t, &m);
    }
    if last {
        return;
    }
    // Straight on, without waiting.
    if !st.check.tick(t + 10.0) {
        st.check.next();
    }
}
