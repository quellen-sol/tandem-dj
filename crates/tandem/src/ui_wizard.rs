//! The step-by-step controller setup's window. What it asks, and what it
//! makes of the answers, is `tandem_core::wizard`; this draws it: a first
//! page of questions about the controller, then one control at a time with
//! a drawing of a controller that shows where it is, and a last page to
//! look over the result and try it.

use crate::app::{SettingsTab, TandemApp};
use crate::ui_decks::{AMBER, DECK_COLORS, GREEN};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Key, Layout, Pos2, Rect, RichText, Stroke, StrokeKind,
    Vec2,
};
use tandem_core::mapping::{side_of, Mapping, PadMode, Part, Target};
use tandem_core::model::deck_letter;
use tandem_core::wizard::{
    pad_sizes, Ask, Decks, End, Section, Setup, State, Step, Tone, Wizard, MIN_TURN_TICKS,
};

const RED: Color32 = Color32::from_rgb(240, 90, 90);
/// The window's width, and the height of the part between its heading and
/// its buttons: it fits the smallest main window (1024 × 520).
const WIDTH: f32 = 920.0;
const BODY: f32 = 392.0;
/// The list of groups down the left.
const LIST: f32 = 232.0;

/// The setup window's own state; the run itself is the MIDI engine's.
pub struct WizardUi {
    /// The DJ's answers on the first page.
    pub setup: Setup,
    /// Asking whether to keep what's mapped so far.
    leaving: bool,
    /// The group the list last scrolled to.
    shown: Option<usize>,
}

/// What the DJ did in the window this frame.
#[derive(Clone, Copy, PartialEq)]
enum Do {
    Start,
    Skip,
    SkipGroup,
    Back,
    Clear,
    Accept,
    Confirm,
    Jump(usize),
    End,
    Resume,
    Keep,
    Discard,
    Leave,
    Stay,
}

/// A first guess at the controller, from what's mapped already.
fn guess(mapping: &Mapping) -> Setup {
    let upper = mapping
        .bindings
        .iter()
        .any(|b| b.target.deck().is_some_and(|d| d >= 2));
    let switched = mapping
        .bindings
        .iter()
        .any(|b| matches!(b.target, Target::DeckSelect { .. }));
    let slots: Vec<u8> = mapping
        .bindings
        .iter()
        .filter_map(|b| match b.target {
            Target::HotCue { slot, .. } => Some(slot),
            _ => None,
        })
        .collect();
    Setup {
        decks: match (upper, switched) {
            (false, _) => Decks::Two,
            (true, true) => Decks::TwoSwitched,
            (true, false) => Decks::Four,
        },
        pads: if !slots.is_empty() && slots.iter().all(|s| *s < 4) {
            4
        } else {
            8
        },
        ..Setup::default()
    }
}

impl TandemApp {
    /// Open the setup on its first page (Settings closes meanwhile).
    pub(crate) fn open_wizard(&mut self) {
        // The DECK button check takes the controller's messages before the
        // setup would hear them, and its results can put an older mapping
        // back.
        if let Some(mut check) = self.ui.deck_check.take() {
            self.close_deck_check(&mut check);
        }
        let setup = guess(&self.midi.engine.lock().mapping);
        self.ui.wizard = Some(WizardUi {
            setup,
            leaving: false,
            shown: None,
        });
        self.ui.show_settings = false;
    }

    pub(crate) fn wizard_window(&mut self, ctx: &egui::Context) {
        let Some(state) = self.ui.wizard.as_mut() else {
            return;
        };
        let device = self
            .midi
            .port
            .clone()
            .unwrap_or_else(|| "your controller".into());
        let now = self.session.lock().clock().stamp();
        let engine_arc = self.midi.engine.clone();
        let mut engine = engine_arc.lock();
        if let Some(w) = engine.wizard.as_mut() {
            w.tick(now);
        }
        let mut act = None;
        let modal = egui::Modal::new(egui::Id::new("controller-setup")).show(ctx, |ui| {
            ui.set_width(WIDTH);
            match engine.wizard.as_ref() {
                None => start_page(ui, &device, &engine.mapping, &mut state.setup, &mut act),
                Some(w) if state.leaving => leaving_page(ui, w, &mut act),
                Some(w) => match w.at() {
                    Some(i) => step_page(ui, &device, w, i, &mut state.shown, &mut act),
                    None => done_page(ui, &device, w, &mut act),
                },
            }
        });
        // The keyboard does what the buttons do, so a hand can stay on the
        // controller. Nothing keeps the focus, or Space would press the
        // button clicked last.
        if let Some(id) = ctx.memory(|m| m.focused()) {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        let stepping = !state.leaving && engine.wizard.as_ref().is_some_and(|w| w.at().is_some());
        if act.is_none() && stepping {
            act = ctx.input(|i| {
                if i.key_pressed(Key::Space) && i.modifiers.shift {
                    Some(Do::SkipGroup)
                } else if i.key_pressed(Key::Space) {
                    Some(Do::Skip)
                } else if i.key_pressed(Key::Backspace) {
                    Some(Do::Back)
                } else if i.key_pressed(Key::Delete) {
                    Some(Do::Clear)
                } else if i.key_pressed(Key::Enter) {
                    Some(Do::Accept)
                } else {
                    None
                }
            });
        }
        if act.is_none() && modal.should_close() {
            act = Some(if state.leaving { Do::Stay } else { Do::Leave });
        }
        let Some(act) = act else {
            return;
        };
        let mut close = false;
        match (act, engine.wizard.as_mut()) {
            (Do::Start, None) => {
                // A Learn left waiting in the list is off.
                engine.learn = None;
                let mapping = engine.mapping.clone();
                engine.wizard = Some(Wizard::new(mapping, state.setup.clone()));
            }
            (Do::Leave, Some(w)) if w.changed() => state.leaving = true,
            (Do::Leave | Do::Discard, _) => {
                engine.wizard = None;
                close = true;
            }
            (Do::Stay, _) => state.leaving = false,
            (Do::Keep, Some(w)) => {
                let made = w.finish();
                engine.load(made);
                close = true;
            }
            (Do::Skip, Some(w)) => w.skip(),
            (Do::SkipGroup, Some(w)) => w.skip_group(),
            (Do::Back, Some(w)) => w.back(),
            (Do::Clear, Some(w)) => w.clear(),
            // Enter says yes to what's offered, or takes the full turn.
            (Do::Accept, Some(w)) => {
                if w.said().is_some_and(|s| s.action.is_some()) {
                    w.accept();
                } else {
                    w.confirm();
                }
            }
            (Do::Confirm, Some(w)) => w.confirm(),
            (Do::Jump(g), Some(w)) => w.jump(g),
            (Do::End, Some(w)) => w.end(),
            (Do::Resume, Some(w)) => w.resume(),
            _ => {}
        }
        drop(engine);
        if close {
            self.ui.wizard = None;
            if act == Do::Keep {
                self.midi.backup_mapping(&self.paths.mappings_dir);
                self.midi.save_mapping(&self.paths.mappings_dir);
                self.toast("Controller mapping saved");
            }
            // Back where the DJ came from.
            self.ui.show_settings = true;
            self.ui.settings_tab = SettingsTab::Controller;
        }
    }
}

fn heading(ui: &mut egui::Ui, device: &str, right: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("Set up {device} step by step"))
                .size(18.0)
                .strong(),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), right);
    });
    ui.separator();
}

fn start_page(
    ui: &mut egui::Ui,
    device: &str,
    mapping: &Mapping,
    setup: &mut Setup,
    act: &mut Option<Do>,
) {
    heading(ui, device, |_| {});
    ui.label(
        "Tandem asks for one control at a time, and you press or move it on the controller. \
         What your controller doesn't have, you skip with Space. Nothing is saved until the end.",
    );
    ui.label(
        RichText::new("While this is open, the controller plays nothing in the room.").color(AMBER),
    );
    ui.add_space(10.0);
    ui.label(RichText::new("How many decks does it have?").strong());
    for (decks, text) in [
        (Decks::Two, "Two decks"),
        (
            Decks::TwoSwitched,
            "Two decks, each with a DECK button that switches it to deck 3 or 4",
        ),
        (Decks::Four, "Four decks, each with controls of its own"),
    ] {
        ui.radio_value(&mut setup.decks, decks, text);
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Pads on each deck").strong());
        ui.radio_value(&mut setup.pads, 8, "8");
        ui.radio_value(&mut setup.pads, 4, "4");
    });
    ui.add_space(8.0);
    ui.label(RichText::new("What does it have?").strong());
    egui::Grid::new("wizard-sections")
        .num_columns(4)
        .spacing([24.0, 4.0])
        .show(ui, |ui| {
            for (i, section) in Section::ALL.into_iter().enumerate() {
                let mut on = setup.sections.contains(&section);
                if ui.checkbox(&mut on, section.label()).changed() {
                    setup.sections.retain(|s| *s != section);
                    if on {
                        setup.sections.push(section);
                    }
                }
                if i % 4 == 3 {
                    ui.end_row();
                }
            }
        });
    let mapped = mapping.bindings.len();
    if mapped > 0 {
        ui.add_space(8.0);
        ui.label(
            RichText::new(match &mapping.preset {
                Some(p) => {
                    format!("{mapped} controls are mapped already (the built-in {p} mapping).")
                }
                None => format!("{mapped} controls are mapped already."),
            })
            .strong(),
        );
        ui.radio_value(
            &mut setup.skip_mapped,
            true,
            "Skip the controls that are mapped, and ask for the rest",
        );
        ui.radio_value(
            &mut setup.skip_mapped,
            false,
            "Go through every control again (one you skip keeps what it has)",
        );
    }
    ui.add_space(10.0);
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button(RichText::new("Start").strong()).clicked() {
            *act = Some(Do::Start);
        }
        if ui.button("Cancel").clicked() {
            *act = Some(Do::Discard);
        }
    });
}

fn leaving_page(ui: &mut egui::Ui, w: &Wizard, act: &mut Option<Do>) {
    let (done, total) = w.progress();
    ui.label(
        RichText::new("Keep what you've mapped so far?")
            .size(18.0)
            .strong(),
    );
    ui.label(format!(
        "{done} of {total} controls are mapped. Keeping saves them as this controller's mapping \
         (the old one is kept beside it as a .bak file); discarding leaves the mapping as it was."
    ));
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(RichText::new("Keep").strong()).clicked() {
            *act = Some(Do::Keep);
        }
        if ui.button("Discard").clicked() {
            *act = Some(Do::Discard);
        }
        if ui.button("Carry on").clicked() {
            *act = Some(Do::Stay);
        }
    });
}

/// The colour a step's control is shown in: its deck's, or amber.
fn accent(target: Target) -> Color32 {
    match target.deck() {
        Some(d) => DECK_COLORS[(d as usize).min(DECK_COLORS.len() - 1)],
        None => AMBER,
    }
}

fn progress(ui: &mut egui::Ui, w: &Wizard) {
    let (done, total) = w.progress();
    ui.add(
        egui::ProgressBar::new(done as f32 / total.max(1) as f32)
            .desired_width(160.0)
            .desired_height(8.0),
    );
    ui.label(RichText::new(format!("{done} of {total} mapped")).small());
}

fn step_page(
    ui: &mut egui::Ui,
    device: &str,
    w: &Wizard,
    i: usize,
    shown: &mut Option<usize>,
    act: &mut Option<Do>,
) {
    let step = &w.steps[i];
    heading(ui, device, |ui| progress(ui, w));
    body(ui, w, Some(step.group), shown, act, |ui, act| {
        let col = accent(step.target);
        // What to do: a fixed height, so the drawing stays put.
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 92.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_min_height(92.0);
                ui.label(RichText::new(w.place(step)).small().color(col));
                ui.label(RichText::new(&step.title).size(22.0).strong());
                if !step.hint.is_empty() {
                    ui.label(RichText::new(&step.hint).weak());
                }
                if step.ask == Ask::FullTurn {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("Counted: {} ticks", w.count())).color(AMBER),
                        );
                        let enough = w.count() >= MIN_TURN_TICKS;
                        if ui
                            .add_enabled(enough, egui::Button::new("Use this (Enter)"))
                            .clicked()
                        {
                            *act = Some(Do::Confirm);
                        }
                    });
                }
                if let Some(b) = w.bound(step) {
                    ui.label(
                        RichText::new(format!(
                            "Now: {b}. Do the step to change it, or skip it to keep it."
                        ))
                        .small()
                        .weak(),
                    );
                }
            },
        );
        diagram(ui, w, i);
        // What was heard: two lines' room, so the buttons stay put.
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 40.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_min_height(40.0);
                match w.said() {
                    Some(said) => {
                        let colour = match said.tone {
                            Tone::Good => GREEN,
                            Tone::Info => ui.visuals().text_color(),
                            Tone::Warn => RED,
                        };
                        ui.label(RichText::new(&said.text).color(colour));
                        if let Some(action) = &said.action {
                            if ui.button(format!("{action} (Enter)")).clicked() {
                                *act = Some(Do::Accept);
                            }
                        }
                    }
                    None if w.shift_held() => {
                        ui.label(RichText::new("SHIFT is held.").color(AMBER));
                    }
                    None => {
                        ui.label(RichText::new("Listening to the controller…").weak());
                    }
                }
            },
        );
    });
    ui.separator();
    ui.horizontal(|ui| {
        let mut button = |ui: &mut egui::Ui, text: &str, hover: &str, what: Do| {
            if ui.button(text).on_hover_text(hover).clicked() {
                *act = Some(what);
            }
        };
        button(
            ui,
            "◀ Back (Backspace)",
            "The step before (Backspace)",
            Do::Back,
        );
        button(
            ui,
            "Skip (Space)",
            "My controller doesn't have this, or it's fine as it is (Space)",
            Do::Skip,
        );
        button(
            ui,
            "Skip the rest of this group (Shift + Space)",
            "On to the next part of the controller (Shift + Space)",
            Do::SkipGroup,
        );
        if step.state.mapped() {
            button(
                ui,
                "Unmap (Del)",
                "Take this control out of the mapping (Delete)",
                Do::Clear,
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            button(
                ui,
                "Close (Esc)",
                "Keep or discard what's mapped so far",
                Do::Leave,
            );
            button(
                ui,
                "Finish ▶",
                "Look over what's mapped, try it, and save",
                Do::End,
            );
        });
    });
}

/// A page's body, at a fixed height: the groups down the left, and the
/// page's own part beside them.
fn body(
    ui: &mut egui::Ui,
    w: &Wizard,
    current: Option<usize>,
    shown: &mut Option<usize>,
    act: &mut Option<Do>,
    page: impl FnOnce(&mut egui::Ui, &mut Option<Do>),
) {
    ui.allocate_ui_with_layout(
        Vec2::new(WIDTH, BODY),
        Layout::left_to_right(Align::Min),
        |ui| {
            ui.allocate_ui_with_layout(Vec2::new(LIST, BODY), Layout::top_down(Align::Min), |ui| {
                group_list(ui, w, current, shown, act)
            });
            ui.separator();
            ui.allocate_ui_with_layout(
                Vec2::new(WIDTH - LIST - 24.0, BODY),
                Layout::top_down(Align::Min),
                |ui| page(ui, act),
            );
        },
    );
}

/// The groups down the left: how far each has got, and a click goes there.
fn group_list(
    ui: &mut egui::Ui,
    w: &Wizard,
    current: Option<usize>,
    shown: &mut Option<usize>,
    act: &mut Option<Do>,
) {
    egui::ScrollArea::vertical()
        .id_salt("wizard-groups")
        .max_height(BODY)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (g, group) in w.groups.iter().enumerate() {
                let (done, total) = w.group_progress(g);
                let steps = &w.steps[group.steps.clone()];
                let touched = steps.iter().any(|s| s.state != State::Todo);
                let (mark, colour) = match (current == Some(g), done == total, touched) {
                    (true, _, _) => ("▶", accent(steps[0].target)),
                    (_, true, _) => ("✔", GREEN),
                    (_, _, true) => ("–", AMBER),
                    _ => ("·", ui.visuals().weak_text_color()),
                };
                let r = ui
                    .horizontal(|ui| {
                        // A column of its own: the marks aren't one width.
                        let (slot, _) =
                            ui.allocate_exact_size(Vec2::new(12.0, 14.0), egui::Sense::hover());
                        ui.painter().text(
                            slot.center(),
                            Align2::CENTER_CENTER,
                            mark,
                            FontId::proportional(12.0),
                            colour,
                        );
                        let name = RichText::new(&group.title).small();
                        let r = ui.selectable_label(current == Some(g), name);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(format!("{done}/{total}")).small().weak());
                        });
                        r
                    })
                    .inner;
                if r.clicked() {
                    *act = Some(Do::Jump(g));
                }
                if current == Some(g) && *shown != current {
                    r.scroll_to_me(Some(Align::Center));
                }
            }
            *shown = current;
        });
}

fn done_page(ui: &mut egui::Ui, device: &str, w: &Wizard, act: &mut Option<Do>) {
    heading(ui, device, |ui| progress(ui, w));
    let mut shown = None;
    body(ui, w, None, &mut shown, act, |ui, act| {
        let (done, total) = w.progress();
        ui.label(RichText::new("Ready to save").size(22.0).strong());
        ui.label(format!("{done} of {total} controls are mapped."));
        let left = w.unmapped();
        if !left.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Not mapped (click a group to go back to it):").weak());
            egui::ScrollArea::vertical()
                .id_salt("wizard-left")
                .max_height(120.0)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (g, n) in left {
                            let text = format!("{} ({n})", w.groups[g].title);
                            if ui.small_button(text).clicked() {
                                *act = Some(Do::Jump(g));
                            }
                        }
                    });
                });
        }
        ui.add_space(12.0);
        ui.label(RichText::new("Try it").strong());
        ui.label(
            "Press or move anything on the controller to see what it does now. \
                     It still plays nothing in the room until you save.",
        );
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(28.0);
            match w.tried() {
                Some(t) => ui.label(RichText::new(t).monospace().color(GREEN)),
                None => ui.label(RichText::new("Listening to the controller…").weak()),
            };
        });
        if !w.changed() {
            ui.add_space(6.0);
            ui.label(RichText::new("Nothing has changed in the mapping.").weak());
        }
    });
    ui.separator();
    ui.horizontal(|ui| {
        if ui
            .button(RichText::new("Save and close").strong())
            .on_hover_text("The old mapping is kept beside it as a .bak file")
            .clicked()
        {
            *act = Some(Do::Keep);
        }
        if ui.button("◀ Back to the steps").clicked() {
            *act = Some(Do::Resume);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Discard").clicked() {
                *act = Some(Do::Discard);
            }
        });
    });
}

/// A control on the drawing of a controller. Decks are sides (0 left, 1
/// right) and mixer channels count from the left.
#[derive(Clone, Copy, PartialEq)]
enum El {
    Shift(usize),
    JogTop(usize),
    JogRing(usize),
    Play(usize),
    Cue(usize),
    Sync(usize),
    Load(usize),
    Deck(usize),
    Tempo(usize),
    Bend(usize, bool),
    LoopIn(usize),
    LoopOut(usize),
    Reloop(usize),
    Halve(usize),
    Double(usize),
    Auto(usize),
    PadMode(usize, usize),
    Pad(usize, usize),
    Trim(usize),
    Eq(usize, u8),
    Filter(usize),
    Fader(usize),
    Pfl(usize),
    Cross,
    CueMix,
    Phones,
    Master,
    Browse,
    Back,
    FxOn(usize, usize),
    FxKnob(usize, usize),
    FxBeats(usize, bool),
}

/// The controls a step is about (and, for a SHIFT + step, the SHIFT to
/// hold), with a word to put beside the first.
fn wanted(w: &Wizard, i: usize) -> (Vec<El>, Option<El>, String) {
    let step = &w.steps[i];
    let part = w.groups[step.group].part;
    let strips = if w.setup().decks == Decks::Two { 2 } else { 4 };
    let channel = |deck: u8| match strips {
        2 => side_of(deck),
        _ => crate::ui_decks::MIXER_ORDER
            .iter()
            .position(|d| *d == deck as usize)
            .unwrap_or(0),
    };
    let (loops, rolls) = pad_sizes(w.setup().pads);
    let pad = |sizes: &[u8], size: u8| sizes.iter().position(|s| *s == size).unwrap_or(0);
    let shift = matches!(step.ask, Ask::ShiftPress | Ask::ShiftMove);
    let s = step.target.deck().map(side_of).unwrap_or(0);
    let (els, word): (Vec<El>, String) = match step.target {
        // The setup's first two steps: the left SHIFT, then the right.
        Target::Shift => (vec![El::Shift(i.min(1))], "SHIFT".into()),
        Target::JogTouch { .. } => (vec![El::JogTop(s)], "TOUCH".into()),
        Target::Jog { .. } => match step.ask {
            Ask::TurnEdge => (vec![El::JogRing(s)], "EDGE".into()),
            Ask::FullTurn => (vec![El::JogTop(s), El::JogRing(s)], "ONE TURN".into()),
            _ => (vec![El::JogTop(s)], "TURN".into()),
        },
        Target::Play { .. } => (vec![El::Play(s)], "PLAY".into()),
        Target::Cue { .. } | Target::BackToStart { .. } => (vec![El::Cue(s)], "CUE".into()),
        Target::Sync { .. } | Target::SyncOff { .. } => (vec![El::Sync(s)], "SYNC".into()),
        Target::Load { .. } => (vec![El::Load(s)], "LOAD".into()),
        Target::DeckSelect { .. } => (vec![El::Deck(s)], "DECK".into()),
        Target::Tempo { .. } | Target::TempoShift { .. } => (vec![El::Tempo(s)], "TEMPO".into()),
        Target::BendDown { .. } => (vec![El::Bend(s, false)], "BEND −".into()),
        Target::BendUp { .. } => (vec![El::Bend(s, true)], "BEND +".into()),
        Target::LoopIn { .. } => (vec![El::LoopIn(s)], "IN".into()),
        Target::LoopOut { .. } => (vec![El::LoopOut(s)], "OUT".into()),
        Target::Reloop { .. } => (vec![El::Reloop(s)], "RELOOP".into()),
        Target::LoopHalve { .. } => (vec![El::Halve(s)], "½".into()),
        Target::LoopDouble { .. } => (vec![El::Double(s)], "×2".into()),
        Target::BeatLoop { .. } if part == Part::Loops => (vec![El::Auto(s)], "AUTO LOOP".into()),
        Target::BeatLoop { size, .. } => {
            let p = pad(&loops, size);
            (vec![El::Pad(s, p)], format!("PAD {}", p + 1))
        }
        Target::Roll { size, .. } => {
            let p = pad(&rolls, size);
            (vec![El::Pad(s, p)], format!("PAD {}", p + 1))
        }
        Target::HotCue { slot, .. } | Target::HotCueClear { slot, .. } => {
            (vec![El::Pad(s, slot as usize)], format!("PAD {}", slot + 1))
        }
        Target::PadMode { mode, .. } => {
            let m = PadMode::ALL.iter().position(|x| *x == mode).unwrap_or(0);
            (vec![El::PadMode(s, m)], "PAD MODE".into())
        }
        Target::Trim { deck } => (vec![El::Trim(channel(deck))], "TRIM".into()),
        Target::Eq { deck, band } => (
            vec![El::Eq(channel(deck), band)],
            ["LOW", "MID", "HIGH"][band.min(2) as usize].into(),
        ),
        Target::Filter { deck } => (vec![El::Filter(channel(deck))], "FILTER".into()),
        Target::Volume { deck } => (vec![El::Fader(channel(deck))], "FADER".into()),
        Target::Pfl { deck } => (vec![El::Pfl(channel(deck))], "CUE".into()),
        Target::Crossfader => (vec![El::Cross], "CROSSFADER".into()),
        Target::CueMix => (vec![El::CueMix], "MIX".into()),
        Target::HeadphoneLevel => (vec![El::Phones], "PHONES".into()),
        Target::MasterLevel => (vec![El::Master], "MASTER".into()),
        Target::Browse | Target::BrowsePress => (vec![El::Browse], "BROWSE".into()),
        Target::BrowseBack => (vec![El::Back], "BACK".into()),
        Target::FxOn { unit, slot } => (
            vec![El::FxOn(unit.min(1) as usize, slot as usize)],
            "ON".into(),
        ),
        Target::FxDepth { unit, slot } => (
            vec![El::FxKnob(unit.min(1) as usize, slot as usize)],
            "KNOB".into(),
        ),
        Target::FxBeatsDown { unit } => (
            vec![El::FxBeats(unit.min(1) as usize, false)],
            "BEATS ◀".into(),
        ),
        Target::FxBeatsUp { unit } => (
            vec![El::FxBeats(unit.min(1) as usize, true)],
            "BEATS ▶".into(),
        ),
        _ => (Vec::new(), String::new()),
    };
    (els, shift.then_some(El::Shift(s)), word)
}

/// Where a fader or knob is in the drawing's little film of a step, 0 to 1:
/// through its travel to the end the step wants it left at, and a rest
/// there. `None` for a step that doesn't say which way.
fn film(time: f64, ask: Ask) -> Option<f32> {
    let end = match ask {
        Ask::Move(end) => end,
        _ => return None,
    };
    let u = (time % 2.6 / 2.6) as f32;
    let ease = |x: f32| x * x * (3.0 - 2.0 * x);
    // To the far end first, then all the way to the one asked for.
    let v = if u < 0.25 {
        0.5 - 0.5 * ease(u / 0.25)
    } else if u < 0.75 {
        ease((u - 0.25) / 0.5)
    } else {
        1.0
    };
    Some(match end {
        End::Top => v,
        End::Bottom => {
            // Halfway up, then all the way down.
            if u < 0.25 {
                0.5 * ease(u / 0.25)
            } else if u < 0.75 {
                0.5 - 0.5 * ease((u - 0.25) / 0.5)
            } else {
                0.0
            }
        }
    })
}

/// A drawing of a controller with the step's control lit: two decks either
/// side of a mixer, as most are laid out.
fn diagram(ui: &mut egui::Ui, w: &Wizard, i: usize) {
    const W: f32 = 640.0;
    const H: f32 = 236.0;
    // Room above and below the controller for the lit control's name.
    const STRIP: f32 = 19.0;
    let step: &Step = &w.steps[i];
    let width = ui.available_width().min(600.0);
    let k = width / W;
    let (area, _) = ui.allocate_exact_size(
        Vec2::new(width, (H + 2.0 * STRIP) * k),
        egui::Sense::hover(),
    );
    let p = ui.painter_at(area);
    let time = ui.input(|i| i.time);
    let pulse = 0.5 + 0.5 * (time * 5.0).sin() as f32;
    let (lit, shift, word) = wanted(w, i);
    let col = accent(step.target);
    let moved = film(time, step.ask);
    let pt = |x: f32, y: f32| area.min + Vec2::new(x, y + STRIP) * k;
    let rc = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(pt(x, y), Vec2::new(w, h) * k);
    let plain = Color32::from_gray(52);
    let edge = Stroke::new(1.0_f32, Color32::from_gray(84));
    let fill = |el: El| {
        if lit.contains(&el) {
            col
        } else if shift == Some(el) {
            AMBER
        } else {
            plain
        }
    };
    // Where to hang the word: on the first control that's lit.
    let mut tag: Option<Rect> = None;
    let mut glow = |p: &egui::Painter, el: El, r: Rect, round: bool| {
        if !lit.contains(&el) && shift != Some(el) {
            return;
        }
        let c = if lit.contains(&el) { col } else { AMBER };
        let grow = (2.0 + 3.0 * pulse) * k.max(0.8);
        let stroke = Stroke::new(1.5_f32, c.gamma_multiply(0.35 + 0.45 * pulse));
        if round {
            p.circle_stroke(r.center(), r.width() / 2.0 + grow, stroke);
        } else {
            p.rect_stroke(r.expand(grow), 4.0, stroke, StrokeKind::Outside);
        }
        if lit.first() == Some(&el) {
            tag = Some(r);
        }
    };
    let button = |p: &egui::Painter, el: El, r: Rect| {
        p.rect(r, 2.0, fill(el), edge, StrokeKind::Inside);
    };
    // The body.
    let body = Rect::from_min_size(pt(0.0, 0.0), Vec2::new(W, H) * k);
    p.rect(
        body,
        10.0,
        Color32::from_gray(24),
        Stroke::new(1.0_f32, Color32::from_gray(64)),
        StrokeKind::Inside,
    );
    let fx = w.setup().sections.contains(&Section::Effects);
    let pads = w.setup().pads.clamp(1, 8) as usize;
    for side in 0..2usize {
        let x0 = if side == 0 { 8.0 } else { 400.0 };
        let y0 = 8.0;
        let deck = step
            .target
            .deck()
            .filter(|d| side_of(*d) == side)
            .unwrap_or(side as u8);
        let here = step.target.deck().is_some_and(|d| side_of(d) == side);
        p.rect(
            rc(x0 - 3.0, y0 - 3.0, 238.0, 226.0),
            6.0,
            Color32::from_gray(if here { 34 } else { 29 }),
            Stroke::NONE,
            StrokeKind::Inside,
        );
        // Effects: three ON buttons, three knobs, BEATS ◀ ▶.
        if fx {
            for slot in 0..3 {
                let r = rc(x0 + 58.0 + 22.0 * slot as f32, y0, 18.0, 11.0);
                button(&p, El::FxOn(side, slot), r);
                glow(&p, El::FxOn(side, slot), r, false);
                let c = pt(x0 + 134.0 + 19.0 * slot as f32, y0 + 5.5);
                let el = El::FxKnob(side, slot);
                knob(
                    &p,
                    c,
                    5.5 * k,
                    fill(el),
                    edge,
                    if lit.contains(&el) { moved } else { None },
                );
                glow(
                    &p,
                    el,
                    Rect::from_center_size(c, Vec2::splat(11.0 * k)),
                    true,
                );
            }
            for (n, up) in [false, true].into_iter().enumerate() {
                let r = rc(x0 + 192.0 + 20.0 * n as f32, y0, 16.0, 11.0);
                button(&p, El::FxBeats(side, up), r);
                glow(&p, El::FxBeats(side, up), r, false);
            }
        }
        // The loop section and the transport, down the left.
        let column: [(El, f32, f32, f32, f32); 10] = [
            (El::LoopIn(side), 0.0, 22.0, 22.0, 14.0),
            (El::LoopOut(side), 26.0, 22.0, 22.0, 14.0),
            (El::Reloop(side), 0.0, 41.0, 48.0, 14.0),
            (El::Halve(side), 0.0, 60.0, 22.0, 14.0),
            (El::Double(side), 26.0, 60.0, 22.0, 14.0),
            (El::Auto(side), 0.0, 79.0, 48.0, 14.0),
            (El::Shift(side), 0.0, 110.0, 48.0, 16.0),
            (El::Sync(side), 0.0, 134.0, 48.0, 18.0),
            (El::Cue(side), 0.0, 158.0, 48.0, 24.0),
            (El::Play(side), 0.0, 188.0, 48.0, 24.0),
        ];
        for (el, x, y, bw, bh) in column {
            let r = rc(x0 + x, y0 + y, bw, bh);
            button(&p, el, r);
            glow(&p, el, r, false);
        }
        // The jog wheel: its edge, and the top a hand rests on.
        let centre = pt(x0 + 116.0, y0 + 74.0);
        let (ring, top) = (El::JogRing(side), El::JogTop(side));
        p.circle(centre, 50.0 * k, fill(ring), edge);
        p.circle(centre, 38.0 * k, fill(top), edge);
        glow(
            &p,
            ring,
            Rect::from_center_size(centre, Vec2::splat(100.0 * k)),
            true,
        );
        glow(
            &p,
            top,
            Rect::from_center_size(centre, Vec2::splat(76.0 * k)),
            true,
        );
        let turning = matches!(step.ask, Ask::Turn | Ask::TurnEdge | Ask::FullTurn)
            && (lit.contains(&ring) || lit.contains(&top));
        if turning {
            // A mark going round clockwise.
            let a = (time * 1.6) as f32;
            let r = if lit.contains(&top) { 30.0 } else { 44.0 } * k;
            let mark = centre + Vec2::new(a.cos(), a.sin()) * r;
            p.circle_filled(mark, 3.5 * k, Color32::WHITE);
        }
        p.text(
            centre,
            Align2::CENTER_CENTER,
            deck_letter(deck as usize),
            FontId::proportional(18.0 * k),
            if here {
                Color32::from_gray(20)
            } else {
                Color32::from_gray(96)
            },
        );
        // DECK, the tempo fader and pitch bend, down the right.
        let r = rc(x0 + 184.0, y0 + 22.0, 48.0, 14.0);
        if w.setup().decks == Decks::TwoSwitched {
            button(&p, El::Deck(side), r);
            glow(&p, El::Deck(side), r, false);
        }
        let el = El::Tempo(side);
        let slot = rc(x0 + 204.0, y0 + 44.0, 8.0, 74.0);
        p.rect(slot, 2.0, Color32::from_gray(14), edge, StrokeKind::Inside);
        let handle = Rect::from_center_size(slot.center(), Vec2::new(24.0, 9.0) * k);
        p.rect(handle, 2.0, fill(el), edge, StrokeKind::Inside);
        glow(&p, el, slot.expand2(Vec2::new(8.0, 2.0) * k), false);
        for (n, up) in [false, true].into_iter().enumerate() {
            let r = rc(x0 + 184.0 + 26.0 * n as f32, y0 + 126.0, 22.0, 14.0);
            button(&p, El::Bend(side, up), r);
            glow(&p, El::Bend(side, up), r, false);
        }
        // The pad mode buttons and the pads.
        for m in 0..3 {
            let r = rc(x0 + 64.0 + 42.0 * m as f32, y0 + 148.0, 38.0, 10.0);
            button(&p, El::PadMode(side, m), r);
            glow(&p, El::PadMode(side, m), r, false);
        }
        for n in 0..pads {
            let (row, colm) = (n / 4, n % 4);
            let tall = if pads <= 4 { 48.0 } else { 22.0 };
            let r = rc(
                x0 + 64.0 + 42.0 * colm as f32,
                y0 + 164.0 + 26.0 * row as f32,
                38.0,
                tall,
            );
            button(&p, El::Pad(side, n), r);
            glow(&p, El::Pad(side, n), r, false);
        }
    }
    // The mixer: browse on top, a strip for each channel, the crossfader.
    let (mx, my) = (248.0, 8.0);
    for (side, x) in [(0usize, 14.0), (1, 100.0)] {
        let r = rc(mx + x, my + 4.0, 30.0, 12.0);
        button(&p, El::Load(side), r);
        glow(&p, El::Load(side), r, false);
    }
    let r = rc(mx + 48.0, my + 20.0, 16.0, 9.0);
    button(&p, El::Back, r);
    glow(&p, El::Back, r, false);
    let c = pt(mx + 72.0, my + 11.0);
    knob(&p, c, 9.0 * k, fill(El::Browse), edge, None);
    glow(
        &p,
        El::Browse,
        Rect::from_center_size(c, Vec2::splat(18.0 * k)),
        true,
    );
    if lit.contains(&El::Browse) && step.ask == Ask::Turn {
        let a = (time * 2.4) as f32;
        p.circle_filled(
            c + Vec2::new(a.cos(), a.sin()) * 6.0 * k,
            2.0 * k,
            Color32::WHITE,
        );
    }
    let strips: &[f32] = if w.setup().decks == Decks::Two {
        &[30.0, 114.0]
    } else {
        &[13.0, 41.0, 103.0, 131.0]
    };
    for (ch, &x) in strips.iter().enumerate() {
        let knobs = [
            (El::Trim(ch), 46.0, 6.0),
            (El::Eq(ch, 2), 63.0, 5.5),
            (El::Eq(ch, 1), 79.0, 5.5),
            (El::Eq(ch, 0), 95.0, 5.5),
            (El::Filter(ch), 114.0, 7.0),
        ];
        for (el, y, r) in knobs {
            let c = pt(mx + x, my + y);
            knob(
                &p,
                c,
                r * k,
                fill(el),
                edge,
                if lit.contains(&el) { moved } else { None },
            );
            glow(
                &p,
                el,
                Rect::from_center_size(c, Vec2::splat(2.0 * r * k)),
                true,
            );
        }
        let r = Rect::from_center_size(pt(mx + x, my + 133.0), Vec2::new(16.0, 10.0) * k);
        button(&p, El::Pfl(ch), r);
        glow(&p, El::Pfl(ch), r, false);
        let el = El::Fader(ch);
        let slot = Rect::from_center_size(pt(mx + x, my + 172.0), Vec2::new(6.0, 52.0) * k);
        p.rect(slot, 2.0, Color32::from_gray(14), edge, StrokeKind::Inside);
        let at = if lit.contains(&el) {
            moved.unwrap_or(0.5)
        } else {
            0.5
        };
        let handle = Rect::from_center_size(
            Pos2::new(slot.center().x, slot.bottom() - at * slot.height()),
            Vec2::new(16.0, 7.0) * k,
        );
        p.rect(handle, 2.0, fill(el), edge, StrokeKind::Inside);
        glow(&p, el, slot.expand2(Vec2::new(6.0, 3.0) * k), false);
    }
    for (el, y, r) in [
        (El::Master, 46.0, 7.0),
        (El::CueMix, 72.0, 6.0),
        (El::Phones, 96.0, 6.0),
    ] {
        let c = pt(mx + 72.0, my + y);
        knob(
            &p,
            c,
            r * k,
            fill(el),
            edge,
            if lit.contains(&el) { moved } else { None },
        );
        glow(
            &p,
            el,
            Rect::from_center_size(c, Vec2::splat(2.0 * r * k)),
            true,
        );
    }
    let slot = rc(mx + 36.0, my + 211.0, 72.0, 6.0);
    p.rect(slot, 2.0, Color32::from_gray(14), edge, StrokeKind::Inside);
    let at = if lit.contains(&El::Cross) {
        moved.unwrap_or(0.5)
    } else {
        0.5
    };
    let handle = Rect::from_center_size(
        Pos2::new(slot.left() + at * slot.width(), slot.center().y),
        Vec2::new(9.0, 14.0) * k,
    );
    p.rect(handle, 2.0, fill(El::Cross), edge, StrokeKind::Inside);
    glow(&p, El::Cross, slot.expand2(Vec2::new(3.0, 6.0) * k), false);
    // The control's name, above or below the controller with a line to it,
    // so it covers nothing.
    if let Some(r) = tag {
        let font = FontId::proportional(11.0);
        let size = p.layout_no_wrap(word.clone(), font.clone(), col).size() + Vec2::new(10.0, 2.0);
        let above = r.center().y < body.center().y;
        let y = if above {
            area.top() + size.y / 2.0
        } else {
            area.bottom() - size.y / 2.0
        };
        let x = r
            .center()
            .x
            .clamp(area.left() + size.x / 2.0, area.right() - size.x / 2.0);
        let label = Rect::from_center_size(Pos2::new(x, y), size);
        let cx = r.center().x;
        let line = if above {
            [Pos2::new(cx, label.bottom()), Pos2::new(cx, r.top() - 3.0)]
        } else {
            [Pos2::new(cx, label.top()), Pos2::new(cx, r.bottom() + 3.0)]
        };
        p.line_segment(line, Stroke::new(1.0_f32, col.gamma_multiply(0.8)));
        p.rect(
            label,
            4.0,
            Color32::from_gray(12),
            Stroke::new(1.0_f32, col),
            StrokeKind::Inside,
        );
        p.text(label.center(), Align2::CENTER_CENTER, word, font, col);
    }
}

/// A knob: a disc with a line showing where it points (`at` 0 to 1, from
/// fully left to fully right; straight up when it isn't being moved).
fn knob(p: &egui::Painter, c: Pos2, r: f32, fill: Color32, edge: Stroke, at: Option<f32>) {
    p.circle(c, r, fill, edge);
    let turn = (at.unwrap_or(0.5) - 0.5) * 1.5 * std::f32::consts::PI;
    let dir = Vec2::new(turn.sin(), -turn.cos());
    let line = if at.is_some() {
        Color32::WHITE
    } else {
        Color32::from_gray(120)
    };
    p.line_segment(
        [c + dir * r * 0.25, c + dir * r * 0.95],
        Stroke::new(1.2_f32, line),
    );
}
