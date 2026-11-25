//! The DECK button check: Settings → Controller → **Check the DECK
//! buttons…** walks a DJ through pressing PLAY, LOAD and DECK on each side
//! of their controller, and moving the mixer's channel 3 and 4 faders. It
//! records everything the controller sends and says what DECK does there:
//!
//! - **The controller switches decks itself.** After DECK, the side sends
//!   its messages on another MIDI channel (the DDJ-FLX6 does: deck 3 sends
//!   on channel 3). A mapping needs deck C's controls on that channel, and
//!   the check can add them by copying deck A's ([`DeckCheck::fix`]).
//! - **It leaves decks to the software.** After DECK, the side sends what
//!   it sent before, so the software has to keep track of DECK. Mixxx's
//!   and Traktor's Denon SC Live 4 mappings, both written on a unit, do
//!   that (Mixxx's: "Both layers share the same MIDI channel, so switching
//!   is handled entirely in the mapping"), but we hadn't seen it ourselves.
//! - DECK sends nothing, or the side goes quiet after it.
//!
//! It was made for a Denon SC Live 4 whose DECK button did nothing in
//! Tandem, and it works for any controller with DECK buttons. While it runs,
//! the controller plays nothing in the room: the app hands every message to
//! the check instead of the mapping. At the end it writes a plain-text
//! report ([`DeckCheck::report`]) to send to us.

use crate::mapping::{parse, Binding, Mapping, MidiKind, RawMidi, Target, Trigger};

/// After the message a step waits for, how long (s) it keeps listening
/// before moving on, for the button's release and anything sent with it.
const SETTLE: f64 = 0.8;
/// The same for a DECK press: a controller may send a burst with it.
const SETTLE_DECK: f64 = 1.5;
/// A fader step moves on once the fader has been still this long.
const SETTLE_FADER: f64 = 1.2;

/// A button the check asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Play,
    Load,
    Deck,
}

/// One step of the check. Sides: 0 is the left, 1 the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Press a button on one side. `round` counts DECK presses before it:
    /// PLAY is asked for in rounds 0, 1 and 2, LOAD in 0 and 1, and DECK
    /// itself in 0 (to deck 3 or 4) and 1 (back).
    Press { side: u8, button: Button, round: u8 },
    /// Did the side's jog wheel screen change when DECK was pressed?
    ScreenChanged { side: u8 },
    /// Move a mixer channel's fader (2: channel 3, 3: channel 4).
    Fader { mixer: u8 },
    /// Tandem asks the left jog wheel's screen to show deck 3 (the Denon SC
    /// Live 4's note 0x38, velocity = deck number): does it?
    ScreenShows,
}

/// What a step waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// A button press (a note on).
    Button,
    /// A fader or knob (a control change).
    Knob,
    /// The DJ's answer to a question.
    Answer,
}

/// An answer to a question about the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    Yes,
    No,
    NotSure,
}

impl Seen {
    pub fn label(self) -> &'static str {
        match self {
            Seen::Yes => "yes",
            Seen::No => "no",
            Seen::NotSure => "not sure",
        }
    }
}

fn side_name(side: u8) -> &'static str {
    if side == 0 {
        "left"
    } else {
        "right"
    }
}

/// The side's first deck and the one DECK switches it to (1 and 3, or 2
/// and 4), as printed on controllers.
fn side_decks(side: u8) -> (u8, u8) {
    (side + 1, side + 3)
}

impl Step {
    pub fn wants(self) -> Want {
        match self {
            Step::Press { .. } => Want::Button,
            Step::Fader { .. } => Want::Knob,
            Step::ScreenChanged { .. } | Step::ScreenShows => Want::Answer,
        }
    }

    /// What the DJ is asked to do.
    pub fn instruction(self) -> String {
        match self {
            Step::Press { side, button, round } => {
                let s = side_name(side);
                let (first, other) = side_decks(side);
                match (button, round) {
                    (Button::Play, 0) => format!("Press PLAY on the {s} deck."),
                    (Button::Play, 1) => format!("Press PLAY on the {s} deck again."),
                    (Button::Play, _) => format!("Press PLAY on the {s} deck one last time."),
                    (Button::Load, 0) => format!(
                        "Press the LOAD button for the {s} deck (the one that loads a track onto it)."
                    ),
                    (Button::Load, _) => format!("Press the {s} deck's LOAD button again."),
                    (Button::Deck, 0) => format!(
                        "Watch the {s} jog wheel's screen, and press the {s} DECK button once \
                         (to switch the {s} side from deck {first} to deck {other})."
                    ),
                    (Button::Deck, _) => format!(
                        "Press the {s} DECK button again (back to deck {first})."
                    ),
                }
            }
            Step::ScreenChanged { side } => format!(
                "When you pressed DECK, did the {} jog wheel's screen change, for example to show {}?",
                side_name(side),
                side_decks(side).1
            ),
            Step::Fader { mixer } => format!(
                "Move the mixer's channel {} fader up and down a little.",
                mixer + 1
            ),
            Step::ScreenShows => {
                "Tandem is now asking the left jog wheel's screen to show deck 3. Does it?".into()
            }
        }
    }

    fn settle(self) -> f64 {
        match self {
            Step::Press {
                button: Button::Deck,
                ..
            } => SETTLE_DECK,
            Step::Fader { .. } => SETTLE_FADER,
            _ => SETTLE,
        }
    }
}

/// A message from the controller, `t` seconds into the check.
#[derive(Clone, Debug, PartialEq)]
pub struct Heard {
    pub t: f64,
    pub bytes: Vec<u8>,
}

impl Heard {
    fn raw(&self) -> Option<RawMidi> {
        parse(&self.bytes)
    }

    /// "94 0A 7F  Note 0x0a on (127) · ch 5".
    pub fn label(&self) -> String {
        let hex: Vec<String> = self.bytes.iter().map(|b| format!("{b:02X}")).collect();
        let what = self
            .raw()
            .map(|r| r.label())
            .unwrap_or_else(|| "sysex/other".into());
        format!("{:<9} {what}", hex.join(" "))
    }

    /// The label, and what `mapping` does with the message: "94 0A 7F
    /// Note 0x0a on (127) · ch 5  -> Deck A · Play/Pause". A release says
    /// nothing more.
    pub fn line(&self, mapping: &Mapping) -> String {
        let does = match self.raw() {
            Some(RawMidi::NoteOff { .. }) | None => String::new(),
            Some(_) => match meaning(mapping, &self.bytes) {
                Some(m) => format!("  -> {m}"),
                None => "  -> (not mapped)".into(),
            },
        };
        format!("{}{does}", self.label())
    }
}

/// The control a message comes from (notes on and off alike).
fn trigger_of(raw: RawMidi) -> Trigger {
    match raw {
        RawMidi::NoteOn { ch, note, .. } | RawMidi::NoteOff { ch, note } => Trigger::note(ch, note),
        RawMidi::Cc { ch, cc, .. } => Trigger::cc(ch, cc),
        RawMidi::PitchBend { ch, .. } => Trigger {
            kind: MidiKind::PitchBend,
            channel: ch,
            number: 0,
        },
    }
}

/// What asks a Denon SC Live 4's jog wheel screen on MIDI channel `ch` (4:
/// left, 5: right) to show `deck` (1–4) in its "DECK" box: the box's top,
/// bottom and word (CCs 0x35–0x37 at 127), then the digit (note 0x38, the
/// value is the digit). From Mixxx's SC Live 4 mapping, which found the
/// CCs by probing a unit and sends the digit again 150 ms later, in case
/// the unit dropped it.
pub fn jog_screen_deck(ch: u8, deck: u8) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = [0x35, 0x36, 0x37]
        .iter()
        .map(|&cc| vec![0xB0 | ch, cc, 0x7F])
        .collect();
    out.push(vec![0x90 | ch, 0x38, deck]);
    out
}

/// What a mapping does with a message's control: "Deck A · Play/Pause", or
/// None if nothing.
pub fn meaning(mapping: &Mapping, bytes: &[u8]) -> Option<String> {
    let trig = trigger_of(parse(bytes)?);
    let what: Vec<String> = mapping
        .bindings
        .iter()
        .filter(|b| b.trigger == trig)
        .map(|b| {
            let mut s = String::new();
            if b.shift {
                s.push_str("SHIFT + ");
            }
            s.push_str(&b.target.label());
            if let Some(m) = b.mode {
                s.push_str(&format!(" (pad mode: {})", m.label()));
            }
            s
        })
        .collect();
    (!what.is_empty()).then(|| what.join(" / "))
}

/// The check, step by step.
#[derive(Clone, Debug)]
pub struct DeckCheck {
    pub steps: Vec<Step>,
    /// The step being done; `steps.len()` once it's over.
    pub at: usize,
    heard: Vec<Vec<Heard>>,
    answers: Vec<Option<Seen>>,
    /// When the step being done got what it waits for.
    got: Option<f64>,
    /// What the controller's mapping does with each control, to tell the one
    /// a step asks for from another pressed or touched by mistake.
    known: Vec<(Trigger, Target)>,
    /// The last control pressed in the step being done that isn't the one
    /// asked for, as far as the mapping knows.
    refused: Option<Trigger>,
    /// Controls the DJ said were the one asked for all the same.
    taken: Vec<Option<Trigger>>,
}

impl DeckCheck {
    /// The steps for both sides and the mixer's channels 3 and 4, and with
    /// `screen_test` the jog wheel screen at the end (a Denon SC Live 4
    /// with an output port).
    pub fn new(screen_test: bool) -> Self {
        let mut steps = Vec::new();
        for side in 0..2u8 {
            let press = |button, round| Step::Press {
                side,
                button,
                round,
            };
            steps.push(press(Button::Play, 0));
            steps.push(press(Button::Load, 0));
            steps.push(press(Button::Deck, 0));
            if side == 0 {
                // Once is enough: the right side's screen works the same.
                steps.push(Step::ScreenChanged { side });
            }
            steps.push(press(Button::Play, 1));
            steps.push(press(Button::Load, 1));
            steps.push(press(Button::Deck, 1));
            steps.push(press(Button::Play, 2));
        }
        steps.push(Step::Fader { mixer: 2 });
        steps.push(Step::Fader { mixer: 3 });
        if screen_test {
            steps.push(Step::ScreenShows);
        }
        let n = steps.len();
        Self {
            steps,
            at: 0,
            heard: vec![Vec::new(); n],
            answers: vec![None; n],
            got: None,
            known: Vec::new(),
            refused: None,
            taken: vec![None; n],
        }
    }

    /// The same, for a controller with this mapping: a press of a control
    /// that the mapping knows as another one isn't taken.
    pub fn for_mapping(screen_test: bool, mapping: &Mapping) -> Self {
        Self {
            known: known(mapping),
            ..Self::new(screen_test)
        }
    }

    /// The control pressed last in this step that isn't the one asked for.
    pub fn refused(&self) -> Option<Trigger> {
        self.refused
    }

    /// It was the one asked for ("That was it"): on to the next step.
    pub fn take_refused(&mut self) {
        if let Some(t) = self.refused {
            self.taken[self.at] = Some(t);
            self.next();
        }
    }

    pub fn step(&self) -> Option<Step> {
        self.steps.get(self.at).copied()
    }

    pub fn done(&self) -> bool {
        self.at >= self.steps.len()
    }

    /// Everything heard during step `i`.
    pub fn heard(&self, i: usize) -> &[Heard] {
        self.heard.get(i).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn answer_of(&self, i: usize) -> Option<Seen> {
        self.answers.get(i).copied().flatten()
    }

    /// When the step being done got what it waits for, if it has.
    pub fn got(&self) -> Option<f64> {
        self.got
    }

    /// A message from the controller, `t` seconds into the check. After
    /// the last step, messages are no longer kept.
    pub fn hear(&mut self, t: f64, bytes: &[u8]) {
        let Some(step) = self.step() else { return };
        let at = self.at;
        self.heard[at].push(Heard {
            t,
            bytes: bytes.to_vec(),
        });
        let raw = parse(bytes);
        // A button that sends control changes: pressed (above 0), then let
        // go (0).
        let cc_press = |heard: &[Heard], ch: u8, cc: u8| {
            heard.iter().any(|h| {
                matches!(h.raw(), Some(RawMidi::Cc { ch: c, cc: n, val }) if c == ch && n == cc && val > 0)
            })
        };
        match (step.wants(), raw) {
            // A fader is done once it has stopped moving for a moment, so
            // one still moving doesn't run into the next step.
            (Want::Knob, Some(RawMidi::Cc { .. } | RawMidi::PitchBend { .. })) => {
                self.got = Some(t)
            }
            _ if self.got.is_some() => {}
            (Want::Button, Some(RawMidi::NoteOn { ch, note, .. })) => {
                self.press(t, Trigger::note(ch, note))
            }
            (Want::Button, Some(RawMidi::Cc { ch, cc, val: 0 }))
                if cc_press(&self.heard[at], ch, cc) =>
            {
                self.press(t, Trigger::cc(ch, cc))
            }
            _ => {}
        }
    }

    /// A control pressed in a button step: the one asked for, or not.
    fn press(&mut self, t: f64, trigger: Trigger) {
        let Some(step) = self.step() else { return };
        if self.suits(step, trigger, &self.known) {
            self.got = Some(t);
            self.refused = None;
        } else {
            self.refused = Some(trigger);
        }
    }

    /// Whether `t` can be the control `step` asks for, going by what the
    /// mapping does with it (`known`) and by the steps before. A control the
    /// mapping doesn't know can: it's what the check is there to find.
    fn suits(&self, step: Step, t: Trigger, known: &[(Trigger, Target)]) -> bool {
        let Step::Press {
            side,
            button,
            round,
        } = step
        else {
            return true;
        };
        let targets = |t: Trigger| -> Vec<Target> {
            known.iter().filter(|k| k.0 == t).map(|k| k.1).collect()
        };
        let does = targets(t);
        // A hand on the platter isn't a button.
        if does.iter().any(|x| matches!(x, Target::JogTouch { .. })) {
            return false;
        }
        let alike =
            |a: Option<Trigger>| a.is_some_and(|a| (a.kind, a.number) == (t.kind, t.number));
        let heard = |button, round| {
            let step = Step::Press {
                side,
                button,
                round,
            };
            self.control(step, known)
        };
        let wanted = |x: &Target| {
            matches!(
                (button, x),
                (Button::Play, Target::Play { .. })
                    | (Button::Load, Target::Load { .. })
                    | (Button::Deck, Target::DeckSelect { .. })
            )
        };
        let asked: Vec<&Target> = does.iter().filter(|x| wanted(x)).collect();
        if !asked.is_empty() {
            // This side's, not the other's.
            return asked
                .iter()
                .any(|x| x.deck().is_some_and(|d| d % 2 == side));
        }
        match button {
            Button::Deck => {
                // Not this side's PLAY or LOAD, nor a control of deck A's or
                // B's. (One of C's or D's may well be DECK, learned as that
                // while looking for a way to reach them.)
                !alike(heard(Button::Play, 0))
                    && !alike(heard(Button::Load, 0))
                    && does.iter().all(|x| x.deck().is_some_and(|d| d >= 2))
            }
            Button::Play | Button::Load => {
                let first = (round > 0).then(|| heard(button, 0)).flatten();
                // The same control, on the channel DECK moved it to.
                if alike(first) {
                    return true;
                }
                let deck = (0..round).any(|r| alike(heard(Button::Deck, r)));
                let load = button == Button::Play && round > 0 && alike(heard(Button::Load, 0));
                let play = button == Button::Load && alike(heard(Button::Play, 0));
                // Another of the side's controls, moved by DECK.
                let moved = first.is_some_and(|f| {
                    !targets(Trigger {
                        channel: f.channel,
                        ..t
                    })
                    .is_empty()
                });
                does.is_empty() && !deck && !load && !play && !moved
            }
        }
    }

    /// Moves on once the step got what it waits for a moment ago. Returns
    /// whether it did.
    pub fn tick(&mut self, now: f64) -> bool {
        match (self.step(), self.got) {
            (Some(step), Some(t)) if now - t >= step.settle() => {
                self.next();
                true
            }
            _ => false,
        }
    }

    /// On to the next step, whatever this one got ("it sent nothing").
    pub fn next(&mut self) {
        if !self.done() {
            self.at += 1;
            self.got = None;
            self.refused = None;
        }
    }

    /// Whether Back may go a step back. Not onto a DECK step, nor from
    /// one that has been pressed: pressing DECK again would switch the
    /// side back, and the steps after it would be on the wrong deck. Start
    /// again instead.
    pub fn can_back(&self) -> bool {
        let deck = |i: usize| {
            matches!(
                self.steps.get(i),
                Some(Step::Press {
                    button: Button::Deck,
                    ..
                })
            )
        };
        self.at > 0 && !deck(self.at - 1) && !(deck(self.at) && !self.heard(self.at).is_empty())
    }

    /// Back a step, to do it again: what both steps heard is forgotten.
    pub fn back(&mut self) {
        if !self.can_back() {
            return;
        }
        if let Some(h) = self.heard.get_mut(self.at) {
            h.clear();
        }
        if let Some(a) = self.answers.get_mut(self.at) {
            *a = None;
        }
        if let Some(t) = self.taken.get_mut(self.at) {
            *t = None;
        }
        self.at -= 1;
        self.heard[self.at].clear();
        self.answers[self.at] = None;
        self.taken[self.at] = None;
        self.got = None;
        self.refused = None;
    }

    pub fn answer(&mut self, seen: Seen) {
        if let Some(a) = self.answers.get_mut(self.at) {
            *a = Some(seen);
            self.next();
        }
    }

    fn index(&self, step: Step) -> Option<usize> {
        self.steps.iter().position(|&s| s == step)
    }

    /// The controls a button step heard pressed, each once: a note on, or a
    /// control change that went above 0 and back to 0 (a button that sends
    /// control changes).
    fn presses(&self, step: Step) -> Vec<Trigger> {
        let Some(i) = self.index(step) else {
            return Vec::new();
        };
        let heard = self.heard(i);
        let mut presses: Vec<Trigger> = Vec::new();
        for (i, h) in heard.iter().enumerate() {
            let t = match h.raw() {
                Some(RawMidi::NoteOn { ch, note, .. }) => Trigger::note(ch, note),
                Some(RawMidi::Cc { ch, cc, val }) if val > 0 => {
                    let let_go = heard[i + 1..].iter().any(|x| {
                        matches!(x.raw(), Some(RawMidi::Cc { ch: c, cc: n, val: 0 }) if c == ch && n == cc)
                    });
                    if !let_go {
                        continue;
                    }
                    Trigger::cc(ch, cc)
                }
                _ => continue,
            };
            if !presses.contains(&t) {
                presses.push(t);
            }
        }
        presses
    }

    /// The control a button step asked for, if it was pressed: what the DJ
    /// said it was, else the first that can be it, one the mapping knows as
    /// that (or that moved with DECK) before one it doesn't know.
    fn control(&self, step: Step, known: &[(Trigger, Target)]) -> Option<Trigger> {
        if let Some(t) = self.taken.get(self.index(step)?).copied().flatten() {
            return Some(t);
        }
        let can: Vec<Trigger> = self
            .presses(step)
            .into_iter()
            .filter(|&t| self.suits(step, t, known))
            .collect();
        let Step::Press {
            side,
            button,
            round,
        } = step
        else {
            return None;
        };
        let first = (round > 0)
            .then(|| {
                self.control(
                    Step::Press {
                        side,
                        button,
                        round: 0,
                    },
                    known,
                )
            })
            .flatten();
        let sure = |t: Trigger| {
            known.iter().any(|k| k.0 == t)
                || first.is_some_and(|f| (f.kind, f.number) == (t.kind, t.number))
        };
        can.iter()
            .copied()
            .find(|&t| sure(t))
            .or(can.first().copied())
    }

    /// What the check found on one side, read with `mapping` (to tell PLAY
    /// from a stray press).
    pub fn side(&self, side: u8, mapping: &Mapping) -> SideFindings {
        let known = known(mapping);
        let press = |button, round| Step::Press {
            side,
            button,
            round,
        };
        let control = |button, round| self.control(press(button, round), &known);
        // Pressed, but not what was asked for.
        let other = |round| {
            control(Button::Play, round).is_none()
                && !self.presses(press(Button::Play, round)).is_empty()
        };
        // What DECK sent, without what another control sent meanwhile.
        let deck_sent = |round| {
            let step = press(Button::Deck, round);
            let Some(i) = self.index(step) else {
                return Vec::new();
            };
            let forced = self.taken.get(i).copied().flatten();
            let keep = |h: &&Heard| {
                let t = match h.raw() {
                    None => return true,
                    Some(RawMidi::NoteOn { ch, note, .. } | RawMidi::NoteOff { ch, note }) => {
                        Trigger::note(ch, note)
                    }
                    Some(raw) => trigger_of(raw),
                };
                forced == Some(t) || self.suits(step, t, &known)
            };
            self.heard(i).iter().filter(keep).cloned().collect()
        };
        SideFindings {
            side,
            play: [
                control(Button::Play, 0),
                control(Button::Play, 1),
                control(Button::Play, 2),
            ],
            load: [control(Button::Load, 0), control(Button::Load, 1)],
            deck: [deck_sent(0), deck_sent(1)],
            other_pressed: other(1),
            screen_changed: self
                .index(Step::ScreenChanged { side })
                .and_then(|i| self.answer_of(i)),
        }
    }

    /// The control a mixer channel's fader moved, if it sent anything: one
    /// like the mapping's channel faders (the same kind and number, on
    /// another channel), else the one it sent most. Not the one the step
    /// before took, still moving.
    pub fn fader(&self, mixer: u8, mapping: &Mapping) -> Option<Trigger> {
        let heard = self.heard(self.index(Step::Fader { mixer })?);
        let before = if mixer > 2 {
            self.fader(mixer - 1, mapping)
        } else {
            None
        };
        let mut counts: Vec<(Trigger, usize)> = Vec::new();
        for h in heard {
            if let Some(raw @ (RawMidi::Cc { .. } | RawMidi::PitchBend { .. })) = h.raw() {
                let t = trigger_of(raw);
                if Some(t) == before {
                    continue;
                }
                match counts.iter_mut().find(|(x, _)| *x == t) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((t, 1)),
                }
            }
        }
        let fader_like = |t: &Trigger| {
            mapping.bindings.iter().any(|b| {
                matches!(b.target, Target::Volume { .. })
                    && b.trigger.kind == t.kind
                    && b.trigger.number == t.number
            })
        };
        // The most sent; of equals, the first.
        let most = |only_like: bool| {
            let mut best: Option<(Trigger, usize)> = None;
            for &(t, n) in &counts {
                if (!only_like || fader_like(&t)) && best.is_none_or(|(_, b)| n > b) {
                    best = Some((t, n));
                }
            }
            best.map(|(t, _)| t)
        };
        most(true).or_else(|| most(false))
    }

    /// The answer to "does the left jog wheel's screen show deck 3?".
    pub fn screen_shows(&self) -> Option<Seen> {
        self.index(Step::ScreenShows)
            .and_then(|i| self.answer_of(i))
    }

    /// What it found, in a few plain sentences, for the DJ: what DECK
    /// does, why it seemed not to work, and what to do next.
    pub fn summary(&self, mapping: &Mapping) -> Vec<String> {
        let sides = [self.side(0, mapping), self.side(1, mapping)];
        let switch = [sides[0].switch(), sides[1].switch()];
        let mut out = Vec::new();
        let silent = sides.iter().all(|s| s.deck.iter().all(|d| d.is_empty()));
        let used = sides.iter().any(|s| deck_used(s, mapping));
        match switch {
            [Switch::Software, Switch::Software] => {
                let mut p = "Your controller doesn't switch decks itself: after DECK, both sides \
                             send exactly what they sent before. So Tandem has to keep track of DECK"
                    .to_string();
                p.push_str(if silent {
                    ", but DECK itself sends nothing Tandem can see."
                } else if used {
                    ", and its mapping does."
                } else {
                    ", and its mapping doesn't yet: that's why DECK does nothing."
                });
                out.push(p);
            }
            // Both sides moved to another channel.
            [Switch::Hardware { from: a, to: b, .. }, Switch::Hardware { from: c, to: d, .. }]
                if a.channel != b.channel && c.channel != d.channel =>
            {
                let unmapped = !mapping
                    .bindings
                    .iter()
                    .any(|x| x.trigger == b || x.trigger == d);
                let mut p = format!(
                    "Your controller switches decks itself: after DECK, the left side sends on \
                     MIDI channel {} instead of {}, and the right side on {} instead of {}.",
                    b.channel + 1,
                    a.channel + 1,
                    d.channel + 1,
                    c.channel + 1
                );
                if unmapped {
                    p.push_str(
                        " Tandem's mapping has nothing there, so a side stops working while it's \
                         on deck 3 or 4: that's why DECK seems not to work.",
                    );
                }
                out.push(p);
            }
            _ => {
                for s in &sides {
                    let name = if s.side == 0 {
                        "Left side"
                    } else {
                        "Right side"
                    };
                    out.push(match s.switch() {
                        Switch::Software => format!(
                            "{name}: the controller doesn't switch decks itself (it sends the \
                             same after DECK)."
                        ),
                        Switch::Hardware { from, to, .. } => format!(
                            "{name}: the controller switches decks itself (after DECK, PLAY \
                             sends {}).",
                            if from.channel == to.channel {
                                "something else on the same MIDI channel".to_string()
                            } else {
                                format!(
                                    "on {} instead of {}",
                                    channel_label(to),
                                    channel_label(from)
                                )
                            }
                        ),
                        Switch::Quiet => format!("{name}: after DECK, PLAY sent nothing at all."),
                        Switch::Unclear => format!(
                            "{name}: not clear. After DECK, something other than PLAY was \
                             pressed. Click \"Do it again\"."
                        ),
                        Switch::Unknown => format!(
                            "{name}: PLAY sent nothing at the first step. Is the controller in \
                             computer mode, and picked in Settings > Controller?"
                        ),
                    });
                }
            }
        }
        match self.screen_shows() {
            Some(Seen::Yes) => out.push(
                "Tandem can show the deck number on the left jog wheel's screen.".to_string(),
            ),
            Some(Seen::No) => out.push(
                "The left jog wheel's screen didn't show the deck number when Tandem asked it to."
                    .to_string(),
            ),
            _ => {}
        }
        if self.deck_pressed_twice() {
            out.push(
                "DECK seems to have been pressed more than once at a step, which can give the \
                 wrong answer. If it was, click \"Do it again\"."
                    .to_string(),
            );
        }
        out
    }

    /// What the DJ should do next. `mapped`: "Map decks C and D" has been
    /// clicked.
    pub fn next_step(&self, mapping: &Mapping, mapped: bool) -> &'static str {
        let sides = [self.side(0, mapping), self.side(1, mapping)];
        let hardware = sides
            .iter()
            .any(|s| matches!(s.switch(), Switch::Hardware { .. }));
        let software = sides.iter().any(|s| s.switch() == Switch::Software);
        let used = sides.iter().any(|s| deck_used(s, mapping));
        let silent = sides.iter().all(|s| s.deck.iter().all(|d| d.is_empty()));
        let unclear = sides.iter().any(|s| s.switch() == Switch::Unclear);
        if unclear {
            "Click \"Do it again\": one side isn't clear yet."
        } else if mapped {
            "Copy the report and send it to us too, so the built-in mapping can do the same."
        } else if hardware && self.fix(mapping).is_some() {
            "Click \"Map decks C and D\" below, then try DECK. Copy the report and send it to \
             us too, so the built-in mapping can do the same."
        } else if software && !used && !silent {
            "To make DECK work now: Settings > Controller > Set up step by step…, choose \"Two \
             decks, each with a DECK button\" and skip the controls that are mapped. Copy the \
             report and send it to us too, so the built-in mapping can do it by itself."
        } else {
            "Copy the report and send it to us."
        }
    }

    /// Whether DECK was pressed more than once at a step: the note it sent
    /// first came again, on any channel (a side may move to another one),
    /// later than one press's own messages do.
    pub fn deck_pressed_twice(&self) -> bool {
        const APART: f64 = 0.05;
        self.steps.iter().enumerate().any(|(i, step)| {
            let Step::Press {
                button: Button::Deck,
                ..
            } = step
            else {
                return false;
            };
            let mut presses = self.heard(i).iter().filter_map(|h| match h.raw() {
                Some(RawMidi::NoteOn { ch, note, .. })
                    if self.suits(*step, Trigger::note(ch, note), &self.known) =>
                {
                    Some((h.t, note))
                }
                _ => None,
            });
            let Some((first, deck)) = presses.next() else {
                return false;
            };
            presses.any(|(t, note)| note == deck && t - first >= APART)
        })
    }

    /// What it heard, side by side and control by control, for us.
    pub fn details(&self, mapping: &Mapping) -> Vec<String> {
        let mut out = Vec::new();
        let sides = [self.side(0, mapping), self.side(1, mapping)];
        for s in &sides {
            out.push(s.describe(mapping));
        }
        if let Some(seen) = sides[0].screen_changed {
            out.push(match seen {
                Seen::Yes => "The DJ saw the left jog wheel's screen change when DECK was \
                    pressed: the controller keeps track of which deck the side is on."
                    .to_string(),
                Seen::No => {
                    "The left jog wheel's screen didn't change when DECK was pressed.".to_string()
                }
                Seen::NotSure => {
                    "The DJ wasn't sure whether the left jog wheel's screen changed.".to_string()
                }
            });
        }
        for mixer in [2u8, 3] {
            let what = match self.fader(mixer, mapping) {
                Some(t) => {
                    let does = match meaning_of(mapping, t) {
                        Some(m) => format!("Tandem's mapping uses it for {m}."),
                        None => "Tandem's mapping has nothing on it.".to_string(),
                    };
                    format!(
                        "The mixer's channel {} fader sends {}. {does}",
                        mixer + 1,
                        t.label()
                    )
                }
                None => format!("The mixer's channel {} fader sent nothing.", mixer + 1),
            };
            out.push(what);
        }
        if let Some(seen) = self.screen_shows() {
            out.push(format!(
                "Asked to show deck 3 (CCs 0x35-0x37 = 127, then note 0x38 = 3, as Mixxx does), \
                 the left jog wheel's screen: {}.",
                match seen {
                    Seen::Yes => "showed it",
                    Seen::No => "didn't",
                    Seen::NotSure => "not sure",
                }
            ));
        }
        out
    }

    /// A mapping with decks C and D added where the check heard them: for
    /// each side that sends PLAY on another MIDI channel after DECK, that
    /// side's controls copied onto that channel for deck C or D, and the
    /// same for a LOAD button that changed and for the mixer's channels 3
    /// and 4. None if there's nothing to add (DECK changed nothing it
    /// sends), or the side's channel already has controls.
    pub fn fix(&self, mapping: &Mapping) -> Option<Fix> {
        let mut m = mapping.clone();
        let mut added = Vec::new();
        let mut moved = false;
        // The decks whose controls were copied to deck C or D.
        let mut first_decks = Vec::new();
        for side in 0..2u8 {
            let s = self.side(side, mapping);
            let Switch::Hardware { from, to, .. } = s.switch() else {
                continue;
            };
            if from.number != to.number || from.channel == to.channel {
                continue;
            }
            // The side may have been on its other deck when the check
            // started: the channel with PLAY mapped is the first deck's.
            let play_deck = |t: Trigger| {
                mapping.bindings.iter().find_map(|b| match b.target {
                    Target::Play { deck } if b.trigger == t => Some(deck),
                    _ => None,
                })
            };
            let (src, dst, deck) = match (play_deck(from), play_deck(to)) {
                (Some(d), None) => (from.channel, to.channel, d),
                (None, Some(d)) => (to.channel, from.channel, d),
                _ => continue,
            };
            if deck >= 2 {
                continue;
            }
            let n = copy_channel(&mut m, mapping, src, dst, deck, deck + 2, &mut added);
            moved |= n > 0;
            first_decks.push(deck);
            // LOAD isn't on the side's channel on every controller.
            if let [Some(a), Some(b)] = s.load {
                if a != b {
                    let load_deck = |t: Trigger| {
                        mapping.bindings.iter().find_map(|x| match x.target {
                            Target::Load { deck } if x.trigger == t => Some(deck),
                            _ => None,
                        })
                    };
                    let (bound, other) = match (load_deck(a), load_deck(b)) {
                        (Some(_), None) => (a, b),
                        (None, Some(_)) => (b, a),
                        _ => (a, a),
                    };
                    if bound != other {
                        if let Some(d) = load_deck(bound).filter(|&d| d < 2) {
                            // LOAD, and SHIFT + LOAD if it's the same button.
                            for b in mapping.bindings.iter().filter(|b| {
                                b.trigger == bound
                                    && [Target::Load { deck: d }, Target::Eject { deck: d }]
                                        .contains(&b.target)
                            }) {
                                add(
                                    &mut m,
                                    mapping,
                                    Binding {
                                        trigger: other,
                                        target: b.target.with_deck(d + 2),
                                        layered: false,
                                        light: None,
                                        ..b.clone()
                                    },
                                    &mut added,
                                );
                            }
                        }
                    }
                }
            }
        }
        if !moved {
            return None;
        }
        // Mixer channels 3 and 4: where deck A's or B's are, on the channel
        // their fader was heard on.
        for mixer in [2u8, 3] {
            let Some(heard) = self.fader(mixer, mapping) else {
                continue;
            };
            let deck = mixer - 2;
            let Some(vol) = mapping.binding_for(Target::Volume { deck }) else {
                continue;
            };
            let same_kind = vol.trigger.kind == heard.kind && vol.trigger.number == heard.number;
            if same_kind && vol.trigger.channel != heard.channel {
                copy_channel(
                    &mut m,
                    mapping,
                    vol.trigger.channel,
                    heard.channel,
                    deck,
                    mixer,
                    &mut added,
                );
            }
        }
        // What deck A or B can do that deck C or D now can't: controls on
        // channels the check didn't hear.
        let mut left_out: Vec<String> = Vec::new();
        for b in &mapping.bindings {
            let Some(d) = b.target.deck() else { continue };
            if !first_decks.contains(&d) {
                continue;
            }
            let target = b.target.with_deck(d + 2);
            let label = target.label();
            if !m.bindings.iter().any(|x| x.target == target) && !left_out.contains(&label) {
                left_out.push(label);
            }
        }
        Some(Fix {
            mapping: m,
            added,
            left_out,
        })
    }

    /// The whole check as text: what it found, then every step with what
    /// it heard and what Tandem's mapping makes of each message.
    pub fn report(&self, info: &ReportInfo, mapping: &Mapping) -> String {
        let mut r = String::new();
        r.push_str("Tandem DECK button check\n");
        r.push_str(&format!(
            "Tandem {} · {}\n",
            info.version,
            utc_label(info.unix_secs)
        ));
        r.push_str(&format!(
            "Controller: {} (MIDI out: {})\n",
            info.port,
            if info.output { "yes" } else { "no" }
        ));
        r.push_str(&format!(
            "Mapping: {} (preset version {}, {} bindings)\n",
            mapping
                .preset
                .as_deref()
                .map(|p| format!("built-in {p}"))
                .unwrap_or_else(|| "custom".into()),
            mapping.preset_version,
            mapping.bindings.len()
        ));
        if !self.done() {
            r.push_str(&format!(
                "Stopped at step {} of {}.\n",
                self.at + 1,
                self.steps.len()
            ));
        }
        r.push_str("\nWhat it found\n");
        for p in self.summary(mapping) {
            r.push_str(&format!("- {p}\n"));
        }
        r.push_str("\nDetails\n");
        for p in self.details(mapping) {
            r.push_str(&format!("- {p}\n"));
        }
        for side in 0..2u8 {
            let s = self.side(side, mapping);
            r.push_str(&format!("  ({} side: {:?})\n", side_name(side), s.switch()));
        }
        r.push_str(
            "\nStep by step (seconds, bytes, message, what Tandem's mapping does with it)\n",
        );
        for (i, step) in self.steps.iter().enumerate().take(self.at + 1) {
            r.push_str(&format!("{:2}. {}\n", i + 1, step.instruction()));
            if let Some(a) = self.answer_of(i) {
                r.push_str(&format!("      answer: {}\n", a.label()));
            }
            let heard = self.heard(i);
            if heard.is_empty() && step.wants() != Want::Answer {
                r.push_str("      (nothing)\n");
            }
            for h in heard {
                r.push_str(&format!("    {:8.3}  {}\n", h.t, h.line(mapping)));
            }
        }
        r
    }
}

fn meaning_of(mapping: &Mapping, t: Trigger) -> Option<String> {
    let bytes = match t.kind {
        MidiKind::Note => [0x90 | t.channel, t.number, 0x7F],
        MidiKind::Cc => [0xB0 | t.channel, t.number, 0],
        MidiKind::PitchBend => [0xE0 | t.channel, 0, 0],
    };
    meaning(mapping, &bytes)
}

/// Add a binding to `m` unless the control already did something in
/// `before` (with SHIFT or without, and in a pad mode both share), or `m`
/// has it already. A control with two bindings keeps both in the copy.
fn add(m: &mut Mapping, before: &Mapping, b: Binding, added: &mut Vec<String>) -> bool {
    let taken = before.bindings.iter().any(|x| {
        x.trigger == b.trigger
            && x.shift == b.shift
            && (x.mode.is_none() || b.mode.is_none() || x.mode == b.mode)
    });
    if taken || m.bindings.contains(&b) {
        return false;
    }
    let mut line = format!("{} -> {}", b.label(), b.target.label());
    if b.shift {
        line = format!("{line} (with SHIFT)");
    }
    added.push(line);
    m.bindings.push(b);
    true
}

/// Copy the bindings on MIDI channel `src` that work deck `from` (or no
/// deck, like SHIFT) onto channel `dst`, working deck `to`. Returns how
/// many were added.
fn copy_channel(
    m: &mut Mapping,
    before: &Mapping,
    src: u8,
    dst: u8,
    from: u8,
    to: u8,
    added: &mut Vec<String>,
) -> usize {
    let copies: Vec<Binding> = m
        .bindings
        .iter()
        .filter(|b| b.trigger.channel == src)
        .filter_map(|b| {
            let target = match b.target.deck() {
                Some(d) if d == from => b.target.with_deck(to),
                Some(_) => return None,
                None => b.target,
            };
            Some(Binding {
                trigger: Trigger {
                    channel: dst,
                    ..b.trigger
                },
                target,
                // It has a channel of its own: it doesn't take turns.
                layered: false,
                ..b.clone()
            })
        })
        .collect();
    copies
        .into_iter()
        .filter(|b| add(m, before, b.clone(), added))
        .count()
}

/// What [`DeckCheck::fix`] made: the mapping, a line for each control it
/// added, and what deck A or B does that deck C or D still can't (controls
/// on channels the check didn't hear, or LOAD when it doesn't change).
#[derive(Clone, Debug)]
pub struct Fix {
    pub mapping: Mapping,
    pub added: Vec<String>,
    pub left_out: Vec<String>,
}

/// For the report's heading.
#[derive(Clone, Debug, Default)]
pub struct ReportInfo {
    pub version: String,
    pub port: String,
    pub output: bool,
    pub unix_secs: u64,
}

/// What DECK does to one side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    /// Nothing to compare: PLAY sent nothing before DECK, or nothing was
    /// asked after it.
    Unknown,
    /// After DECK, PLAY sends what it sent before: the controller leaves
    /// decks to the software.
    Software,
    /// After DECK, PLAY sends something else (`to`, usually another MIDI
    /// channel): the controller switches decks itself. `back`: after the
    /// second DECK press, PLAY sends what it sent at first.
    Hardware {
        from: Trigger,
        to: Trigger,
        back: bool,
    },
    /// After DECK, PLAY sends nothing.
    Quiet,
    /// After DECK, another control was pressed instead of PLAY.
    Unclear,
}

/// What the check heard on one side.
#[derive(Clone, Debug)]
pub struct SideFindings {
    pub side: u8,
    /// The note PLAY sent: before DECK, after one press, after two.
    pub play: [Option<Trigger>; 3],
    /// The note LOAD sent: before DECK and after one press.
    pub load: [Option<Trigger>; 2],
    /// What each DECK press sent.
    pub deck: [Vec<Heard>; 2],
    /// After DECK, another control was pressed instead of PLAY.
    pub other_pressed: bool,
    pub screen_changed: Option<Seen>,
}

impl SideFindings {
    pub fn switch(&self) -> Switch {
        match self.play {
            [None, ..] => Switch::Unknown,
            [Some(_), None, _] if self.other_pressed => Switch::Unclear,
            [Some(_), None, _] => Switch::Quiet,
            [Some(a), Some(b), _] if a == b => Switch::Software,
            [Some(a), Some(b), c] => Switch::Hardware {
                from: a,
                to: b,
                back: c == Some(a),
            },
        }
    }

    /// The channel messages a DECK press sent, as labels.
    fn deck_labels(&self, round: usize) -> Vec<String> {
        self.deck[round]
            .iter()
            .filter_map(|h| h.raw().map(|r| r.label()))
            .collect()
    }

    /// What DECK sends, in a sentence.
    fn deck_sentence(&self, mapping: &Mapping) -> String {
        let first = self.deck_labels(0);
        let second = self.deck_labels(1);
        // The same presses, maybe on the channel the side was on then.
        let shape = |round: usize| -> Vec<(u8, u8, u8)> {
            self.deck[round]
                .iter()
                .filter(|h| h.raw().is_some())
                .map(|h| {
                    let b = |i: usize| h.bytes.get(i).copied().unwrap_or(0);
                    (b(0) & 0xF0, b(1), b(2))
                })
                .collect()
        };
        let sysex = self.deck.iter().flatten().any(|h| h.raw().is_none());
        let mut s = match (first.is_empty(), second.is_empty()) {
            (true, true) => "DECK itself sends nothing Tandem can see".to_string(),
            _ => {
                let pressed = if first.is_empty() { &second } else { &first };
                let mut s = format!("DECK sends {}", pressed.join(", then "));
                if !first.is_empty() && !second.is_empty() {
                    s.push_str(if first == second {
                        " (the same both times, so the software has to count the presses)"
                    } else if shape(0) == shape(1) {
                        " the first time, and the same on the side's other channel the second \
                         time"
                    } else {
                        " the first time, and something else the second time (the controller \
                         says which deck the side is on)"
                    });
                }
                s
            }
        };
        if sysex {
            s.push_str(", along with system exclusive messages (see the steps below)");
        }
        let deck_note = self.deck[0]
            .iter()
            .chain(&self.deck[1])
            .find(|h| matches!(h.raw(), Some(RawMidi::NoteOn { .. })));
        if let Some(h) = deck_note {
            match meaning(mapping, &h.bytes) {
                Some(m) => s.push_str(&format!("; Tandem's mapping uses it for {m}")),
                None => s.push_str("; Tandem's mapping doesn't use it"),
            }
        }
        s.push('.');
        s
    }

    /// What happened on this side, for the DJ.
    pub fn describe(&self, mapping: &Mapping) -> String {
        let name = if self.side == 0 { "Left" } else { "Right" };
        let (first, other) = side_decks(self.side);
        let deck = self.deck_sentence(mapping);
        let load = match self.load {
            [Some(a), Some(b)] if a != b => format!(
                " LOAD changes too: {} on deck {other}, {} before.",
                a_label(b),
                a_label(a)
            ),
            [Some(_), Some(_)] => " LOAD sends the same on both decks.".to_string(),
            _ => String::new(),
        };
        match self.switch() {
            Switch::Unknown => format!(
                "{name} side: PLAY sent nothing before DECK was pressed, so there's nothing to \
                 compare. Is the controller in computer mode, and is it the controller picked \
                 in Settings > Controller? {deck}"
            ),
            Switch::Quiet => format!("{name} side: after DECK, PLAY sent nothing at all. {deck}"),
            Switch::Unclear => format!(
                "{name} side: after DECK, something other than PLAY was pressed, so it isn't \
                 clear what DECK does. {deck}"
            ),
            Switch::Software => {
                let play = self.play[0].map(a_label).unwrap_or_default();
                let uses = deck_used(self, mapping);
                let why = if uses {
                    ""
                } else {
                    " Tandem's mapping doesn't follow DECK, so pressing it changes nothing in \
                     Tandem: that's why it seems not to work."
                };
                format!(
                    "{name} side: the controller doesn't switch decks itself. After DECK, PLAY \
                     still sends {play}, so the software has to keep track of which deck the \
                     side is on. {deck}{load}{why}"
                )
            }
            Switch::Hardware { from, to, back } => {
                let mapped = mapping.bindings.iter().any(|b| b.trigger == to);
                let back = if back {
                    format!(", and after the second DECK press, {} again", a_label(from))
                } else {
                    String::new()
                };
                let why = if mapped {
                    String::new()
                } else {
                    format!(
                        " Tandem's mapping has nothing on {}, so the {} side stops working while \
                         it's on deck {other}: that's why DECK seems not to work.",
                        if from.channel == to.channel {
                            a_label(to)
                        } else {
                            channel_label(to)
                        },
                        side_name(self.side)
                    )
                };
                format!(
                    "{name} side: the controller switches decks itself. After DECK, PLAY sends \
                     {} instead of {} (deck {other} instead of deck {first}){back}. \
                     {deck}{load}{why}",
                    a_label(to),
                    a_label(from)
                )
            }
        }
    }
}

/// What a mapping does with each control, without SHIFT.
fn known(mapping: &Mapping) -> Vec<(Trigger, Target)> {
    mapping
        .bindings
        .iter()
        .filter(|b| !b.shift)
        .map(|b| (b.trigger, b.target))
        .collect()
}

/// "Note 0x0a on MIDI channel 5".
fn a_label(t: Trigger) -> String {
    match t.kind {
        MidiKind::Note => format!("Note {:#04x} on MIDI channel {}", t.number, t.channel + 1),
        MidiKind::Cc => format!("CC {:#04x} on MIDI channel {}", t.number, t.channel + 1),
        MidiKind::PitchBend => format!("pitch bend on MIDI channel {}", t.channel + 1),
    }
}

fn channel_label(t: Trigger) -> String {
    format!("MIDI channel {}", t.channel + 1)
}

/// Whether the mapping does anything with what DECK sent.
fn deck_used(s: &SideFindings, mapping: &Mapping) -> bool {
    s.deck.iter().flatten().any(|h| {
        matches!(h.raw(), Some(RawMidi::NoteOn { .. })) && meaning(mapping, &h.bytes).is_some()
    })
}

/// "2026-11-24 05:30 UTC".
pub fn utc_label(unix_secs: u64) -> String {
    let (y, mo, d, h, mi, _) = utc(unix_secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02} UTC")
}

/// "20261124-053012", for file names.
pub fn utc_stamp(unix_secs: u64) -> String {
    let (y, mo, d, h, mi, s) = utc(unix_secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// Year, month, day, hour, minute, second (UTC) of a Unix time.
fn utc(unix_secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    )
}

/// How a pretend controller answers each step, for the tests and the
/// app's `--dev-deck-check`: a Denon SC Live 4 whose DECK button works the
/// way `model` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pretend {
    /// DECK sends note 0x1F and changes nothing else (what Mixxx's and
    /// Traktor's mappings say).
    Software,
    /// DECK sends note 0x1F and moves the side to MIDI channel 7 (left) or
    /// 8 (right) until it's pressed again.
    Hardware,
    /// DECK sends nothing, and after it the side sends nothing either.
    Quiet,
}

impl Pretend {
    /// What the controller sends for `step`, given where the check is
    /// (`check` knows which DECK presses came before). Answers aren't
    /// messages: the caller answers question steps.
    pub fn messages(self, step: Step) -> Vec<Vec<u8>> {
        let press = |status: u8, note: u8| vec![vec![status, note, 0x7F], vec![status, note, 0x00]];
        match step {
            Step::Press {
                side,
                button,
                round,
            } => {
                let home = 0x94 + side;
                let away = 0x96 + side;
                // Round 1 comes after one DECK press: the other deck.
                let on_other = round == 1;
                let ch = match self {
                    Pretend::Hardware if on_other => away,
                    _ => home,
                };
                match button {
                    Button::Play if self == Pretend::Quiet && on_other => Vec::new(),
                    Button::Play => press(ch, 0x0A),
                    Button::Load if self == Pretend::Quiet && on_other => Vec::new(),
                    Button::Load => press(0x9F, 0x01 + side),
                    Button::Deck => match self {
                        Pretend::Quiet => Vec::new(),
                        // Pressed on the deck the side is leaving.
                        Pretend::Hardware if round == 1 => press(away, 0x1F),
                        _ => press(home, 0x1F),
                    },
                }
            }
            Step::Fader { mixer } => (60..66).map(|v| vec![0xB0 + mixer, 0x0E, v]).collect(),
            Step::ScreenChanged { .. } | Step::ScreenShows => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{builtin_preset, sc_live_4_preset_13, MidiEngine, MidiOut};
    use crate::model::{Action, RoomState};
    use crate::render::LocalParams;

    /// Two decks and no DECK buttons, as the SC Live 4's was when the check
    /// was made for it.
    fn sc_live_4() -> Mapping {
        sc_live_4_preset_13()
    }

    /// Run the whole check against a pretend controller, answering every
    /// question `seen`.
    fn run(model: Pretend, seen: Seen) -> DeckCheck {
        let mut c = DeckCheck::new(true);
        let mut t = 0.0;
        while let Some(step) = c.step() {
            t += 1.0;
            if step.wants() == Want::Answer {
                c.answer(seen);
                continue;
            }
            for m in model.messages(step) {
                c.hear(t, &m);
                t += 0.05;
            }
            t += 2.0;
            if !c.tick(t) {
                // Nothing came: "it sent nothing".
                c.next();
            }
        }
        c
    }

    #[test]
    fn steps_cover_both_sides_the_mixer_and_the_screen() {
        let c = DeckCheck::new(true);
        assert_eq!(c.steps.len(), 8 + 7 + 2 + 1);
        assert_eq!(
            c.steps.first(),
            Some(&Step::Press {
                side: 0,
                button: Button::Play,
                round: 0
            })
        );
        assert_eq!(c.steps.last(), Some(&Step::ScreenShows));
        assert_eq!(DeckCheck::new(false).steps.len(), 17);
        for s in &c.steps {
            assert!(!s.instruction().is_empty());
        }
    }

    #[test]
    fn a_controller_that_leaves_decks_to_the_software() {
        let m = sc_live_4();
        let c = run(Pretend::Software, Seen::No);
        assert!(c.done());
        for side in 0..2 {
            let s = c.side(side, &m);
            assert_eq!(s.switch(), Switch::Software, "side {side}");
            assert_eq!(s.play[0], Some(Trigger::note(4 + side, 0x0A)));
            assert_eq!(s.load, [Some(Trigger::note(15, 1 + side)); 2]);
        }
        let text = [c.summary(&m), c.details(&m)].concat().join("\n");
        assert!(
            text.contains("the controller doesn't switch decks itself"),
            "{text}"
        );
        assert!(
            text.contains("DECK sends Note 0x1f on (127) · ch 5"),
            "{text}"
        );
        assert!(text.contains("the same both times"), "{text}");
        assert!(
            text.contains("Tandem's mapping doesn't follow DECK"),
            "{text}"
        );
        assert!(
            text.contains("channel 3 fader sends CC 0x0e · ch 3"),
            "{text}"
        );
        assert!(text.contains("doesn't yet"), "{text}");
        // Nothing to map: the side's controls don't move.
        assert!(c.fix(&m).is_none());
    }

    #[test]
    fn a_controller_that_switches_decks_itself() {
        let m = sc_live_4();
        let c = run(Pretend::Hardware, Seen::Yes);
        let left = c.side(0, &m);
        assert_eq!(
            left.switch(),
            Switch::Hardware {
                from: Trigger::note(4, 0x0A),
                to: Trigger::note(6, 0x0A),
                back: true
            }
        );
        let text = [c.summary(&m), c.details(&m)].concat().join("\n");
        assert!(
            text.contains("the controller switches decks itself"),
            "{text}"
        );
        assert!(
            text.contains("Note 0x0a on MIDI channel 7 instead of Note 0x0a on MIDI channel 5"),
            "{text}"
        );
        assert!(
            text.contains("stops working while it's on deck 3"),
            "{text}"
        );
        assert!(text.contains("keeps track of which deck"), "{text}");
        assert!(c.next_step(&m, false).contains("Map decks C and D"));

        let fix = c.fix(&m).expect("decks C and D to map");
        let has = |t: Trigger, target: Target| {
            fix.mapping
                .bindings
                .iter()
                .any(|b| b.trigger == t && b.target == target)
        };
        assert!(has(Trigger::note(6, 0x0A), Target::Play { deck: 2 }));
        assert!(has(Trigger::note(7, 0x0A), Target::Play { deck: 3 }));
        assert!(has(Trigger::cc(6, 0x1F), Target::Tempo { deck: 2 }));
        assert!(has(Trigger::note(6, 0x1C), Target::Shift));
        // The mixer: channel 3's controls on MIDI channel 3, for deck C.
        assert!(has(Trigger::cc(2, 0x0E), Target::Volume { deck: 2 }));
        assert!(has(Trigger::cc(3, 0x08), Target::Eq { deck: 3, band: 0 }));
        assert!(has(Trigger::note(2, 0x0D), Target::Pfl { deck: 2 }));
        // Pads keep their pad modes and lights; nothing on A or B moved.
        let pad = fix
            .mapping
            .bindings
            .iter()
            .find(|b| {
                b.trigger == Trigger::note(6, 0x0F)
                    && b.mode == Some(crate::mapping::PadMode::Rolls)
            })
            .unwrap();
        assert_eq!(pad.target, Target::Roll { deck: 2, size: 0 });
        for b in &m.bindings {
            assert!(fix.mapping.bindings.contains(b));
        }
        assert!(
            fix.added.iter().any(|l| l.contains("Deck C · Play/Pause")),
            "{:?}",
            fix.added
        );

        // With it, the left side on deck 3 plays deck C.
        let mut e = MidiEngine::new(fix.mapping.clone(), 0.08);
        let (room, local) = (RoomState::default(), LocalParams::default());
        assert_eq!(
            e.handle(&[0x96, 0x0A, 0x7F], &room, &local),
            vec![MidiOut::Action(Action::Play { deck: 2 })]
        );
        assert_eq!(
            e.handle(&[0x94, 0x0A, 0x7F], &room, &local),
            vec![MidiOut::Action(Action::Play { deck: 0 })]
        );
        // Run again on the fixed mapping, the check has nothing to add.
        assert!(c.fix(&fix.mapping).is_none());
    }

    #[test]
    fn started_on_the_other_deck_still_maps_the_right_way_round() {
        // The left side was on deck 3 when the check started: PLAY came on
        // channel 7 first, and on channel 5 after DECK.
        let m = sc_live_4();
        let mut c = DeckCheck::new(false);
        let mut t = 0.0;
        while let Some(step) = c.step() {
            t += 3.0;
            let msgs = match step {
                Step::Press {
                    side,
                    button: Button::Play,
                    round,
                } => {
                    // Left: deck 3 (channel 7) first; right: as usual.
                    let ch = match (side, round) {
                        (0, 1) => 0x94,
                        (0, _) => 0x96,
                        (_, 1) => 0x97,
                        _ => 0x95,
                    };
                    vec![vec![ch, 0x0A, 0x7F]]
                }
                s => Pretend::Hardware.messages(s),
            };
            for msg in msgs {
                c.hear(t, &msg);
            }
            if step.wants() == Want::Answer {
                c.answer(Seen::NotSure);
            } else if !c.tick(t + 2.0) {
                c.next();
            }
        }
        assert_eq!(
            c.side(0, &m).switch(),
            Switch::Hardware {
                from: Trigger::note(6, 0x0A),
                to: Trigger::note(4, 0x0A),
                back: true
            }
        );
        let fix = c.fix(&m).unwrap();
        assert!(fix
            .mapping
            .bindings
            .iter()
            .any(|b| b.trigger == Trigger::note(6, 0x0A) && b.target == Target::Play { deck: 2 }));
    }

    /// DECK pressed twice at one step leaves the side where it was, so a
    /// controller that switches decks itself would look as if it didn't:
    /// the check says so. One press that sends its note on both channels at
    /// once, or DECK pressed once at each of its steps, isn't that.
    #[test]
    fn deck_pressed_twice_at_a_step_is_noticed() {
        let m = sc_live_4();
        for p in [Pretend::Software, Pretend::Hardware, Pretend::Quiet] {
            let c = run(p, Seen::No);
            assert!(!c.deck_pressed_twice(), "{p:?}");
            assert!(!c.summary(&m).join("\n").contains("more than once"));
        }
        let at_deck = |msgs: &[(f64, [u8; 3])]| {
            let mut c = DeckCheck::new(false);
            while !matches!(
                c.step(),
                Some(Step::Press {
                    button: Button::Deck,
                    ..
                })
            ) {
                c.next();
            }
            for (t, b) in msgs {
                c.hear(*t, b);
            }
            c
        };
        let twice = at_deck(&[
            (1.0, [0x94, 0x1F, 0x7F]),
            (1.1, [0x84, 0x1F, 0]),
            (1.4, [0x94, 0x1F, 0x7F]),
        ]);
        assert!(twice.deck_pressed_twice());
        assert!(twice.summary(&m).join("\n").contains("more than once"));
        // Its note on the other channel the second time (as the FLX6).
        assert!(
            at_deck(&[(1.0, [0x92, 0x3C, 0x7F]), (1.4, [0x90, 0x3C, 0x7F])]).deck_pressed_twice()
        );
        // One press, its note on both channels at once; and a note off sent
        // as a note on with velocity 0.
        assert!(!at_deck(&[
            (1.0, [0x92, 0x3C, 0x7F]),
            (1.002, [0x90, 0x3C, 0x7F]),
            (1.3, [0x92, 0x3C, 0]),
        ])
        .deck_pressed_twice());
    }

    /// A controller that answers DECK with another note on the same channel
    /// isn't said to have moved from a channel to itself.
    #[test]
    fn another_note_on_the_same_channel_isnt_called_another_channel() {
        let m = sc_live_4();
        let mut c = DeckCheck::new(false);
        let mut t = 0.0;
        while let Some(step) = c.step() {
            t += 3.0;
            if let Step::Press {
                side,
                button,
                round,
            } = step
            {
                let note = match button {
                    Button::Play if round == 1 => 0x4A,
                    Button::Play => 0x0A,
                    Button::Deck => 0x1F,
                    Button::Load => 0x01 + side,
                };
                c.hear(t, &[0x94 + side, note, 0x7F]);
            }
            if step.wants() == Want::Answer {
                c.answer(Seen::No);
            } else if !c.tick(t + 2.0) {
                c.next();
            }
        }
        assert!(matches!(c.side(0, &m).switch(), Switch::Hardware { .. }));
        let text = [c.summary(&m), c.details(&m)].concat().join("\n");
        assert!(!text.contains("channel 5 instead of 5"), "{text}");
        assert!(
            !text.contains("MIDI channel 5 instead of MIDI channel 5)"),
            "{text}"
        );
        assert!(
            text.contains("something else on the same MIDI channel"),
            "{text}"
        );
        assert!(
            text.contains("has nothing on Note 0x4a on MIDI channel 5"),
            "{text}"
        );
        assert!(c.fix(&m).is_none());
    }

    /// The first real check: a DJ's Denon SC Live 4 (2026-11-24, v0.15.1),
    /// with mixer channels 3 and 4 mapped, and the left DECK button learned
    /// as deck D's Play. Brushing the left jog wheel was taken for DECK and
    /// CUE for PLAY, so the check said the left side switches decks itself.
    fn sc_live_4_checked() -> Mapping {
        let mut m = sc_live_4_preset_13();
        m.bindings.extend(
            builtin_preset("SC LIVE 4")
                .unwrap()
                .bindings
                .into_iter()
                .filter(|b| !b.layered && b.target.deck().is_some_and(|d| d >= 2))
                .filter(|b| !matches!(b.target, Target::DeckSelect { .. })),
        );
        m.bind(Trigger::note(4, 0x1F), Target::Play { deck: 3 });
        assert_eq!(m.bindings.len(), 137, "as its report says");
        m
    }

    /// What the unit sent, in order (each button pressed and let go).
    const CHECKED: &[(f64, [u8; 3])] = &[
        (9.126, [0x94, 0x0A, 0x7F]),
        (9.284, [0x94, 0x0A, 0x00]),
        (33.557, [0x9F, 0x01, 0x7F]),
        (33.686, [0x9F, 0x01, 0x00]),
        (37.583, [0x94, 0x21, 0x7F]), // the jog wheel, brushed
        (38.137, [0x94, 0x21, 0x00]),
        (40.283, [0x94, 0x1F, 0x7F]), // DECK
        (40.386, [0x94, 0x1F, 0x00]),
        (50.749, [0x94, 0x09, 0x7F]), // CUE, for PLAY
        (50.894, [0x94, 0x09, 0x00]),
    ];
    const CHECKED_ON: &[(f64, [u8; 3])] = &[
        (53.849, [0x9F, 0x01, 0x7F]),
        (53.970, [0x9F, 0x01, 0x00]),
        (58.800, [0x94, 0x1F, 0x7F]),
        (58.954, [0x94, 0x1F, 0x00]),
        (65.009, [0x94, 0x0A, 0x7F]),
        (65.171, [0x94, 0x0A, 0x00]),
        (68.386, [0x95, 0x0A, 0x7F]),
        (68.516, [0x95, 0x0A, 0x00]),
        (71.334, [0x9F, 0x02, 0x7F]),
        (71.471, [0x9F, 0x02, 0x00]),
        (77.840, [0x95, 0x1F, 0x7F]),
        (77.978, [0x95, 0x1F, 0x00]),
        (82.977, [0x95, 0x0A, 0x7F]),
        (83.117, [0x95, 0x0A, 0x00]),
        (89.240, [0x9F, 0x02, 0x7F]),
        (89.375, [0x9F, 0x02, 0x00]),
        (92.844, [0x95, 0x1F, 0x7F]),
        (92.996, [0x95, 0x1F, 0x00]),
        (97.088, [0x95, 0x0A, 0x7F]),
        (97.208, [0x95, 0x0A, 0x00]),
        (100.184, [0xB2, 0x0E, 0x7E]),
        (100.298, [0xB2, 0x0E, 0x00]),
        (100.562, [0xB2, 0x0E, 0x7F]),
        (104.278, [0xB3, 0x0E, 0x02]),
        (104.417, [0xB3, 0x0E, 0x7F]),
        (104.684, [0xB3, 0x0E, 0x00]),
    ];

    /// Feed it messages at their times, moving on and answering as it goes.
    fn feed(c: &mut DeckCheck, msgs: &[(f64, [u8; 3])]) {
        for (t, m) in msgs {
            c.tick(*t);
            while c.step().is_some_and(|s| s.wants() == Want::Answer) {
                c.answer(Seen::No);
            }
            c.hear(*t, m);
        }
    }

    #[test]
    fn a_real_sc_live_4_leaves_decks_to_the_software() {
        let m = sc_live_4_checked();
        let mut c = DeckCheck::for_mapping(true, &m);
        feed(&mut c, CHECKED);
        // The jog wheel wasn't taken for DECK, and DECK was (though it's
        // learned as something else). CUE isn't taken for PLAY.
        assert_eq!(
            c.step(),
            Some(Step::Press {
                side: 0,
                button: Button::Play,
                round: 1
            })
        );
        assert_eq!(c.refused(), Some(Trigger::note(4, 0x09)));
        assert_eq!(c.got(), None);
        // Nor is LOAD, or DECK.
        c.hear(51.5, &[0x9F, 0x01, 0x7F]);
        c.hear(52.0, &[0x94, 0x1F, 0x7F]);
        assert_eq!(c.got(), None);
        c.hear(52.5, &[0x94, 0x0A, 0x7F]);
        assert_eq!((c.got(), c.refused()), (Some(52.5), None));
        feed(&mut c, CHECKED_ON);
        assert!(c.tick(110.0));
        assert_eq!(c.step(), Some(Step::ScreenShows));
        c.answer(Seen::Yes);
        assert!(c.done());
        let text = [c.summary(&m), c.details(&m)].concat().join("\n");
        for side in 0..2 {
            let s = c.side(side, &m);
            assert_eq!(s.switch(), Switch::Software, "{text}");
            assert!(s
                .deck
                .iter()
                .all(|d| d.iter().all(|h| h.bytes[1] == 0x1F) && d.len() == 2));
        }
        assert!(
            text.contains("both sides send exactly what they sent before"),
            "{text}"
        );
        assert!(!text.contains("0x21"), "{text}");
        assert!(
            text.contains("channel 4 fader sends CC 0x0e · ch 4"),
            "{text}"
        );
        assert!(!c.deck_pressed_twice());
    }

    /// The steps exactly as that check took them (it took any button): read
    /// again, the left side isn't clear rather than switching by itself.
    #[test]
    fn a_wrong_button_is_unclear_not_a_verdict() {
        let m = sc_live_4_checked();
        let mut c = DeckCheck::new(true);
        feed(&mut c, CHECKED);
        feed(&mut c, CHECKED_ON);
        assert!(c.tick(110.0));
        c.answer(Seen::Yes);
        assert!(c.done());
        assert_eq!(c.side(0, &m).switch(), Switch::Unclear);
        assert_eq!(c.side(1, &m).switch(), Switch::Software);
        let text = c.summary(&m).join("\n");
        assert!(text.contains("Left side: not clear"), "{text}");
        assert!(!text.contains("switches decks itself"), "{text}");
        assert!(c.next_step(&m, false).contains("Do it again"));
        assert!(c.fix(&m).is_none());
    }

    #[test]
    fn the_dj_can_say_that_was_it() {
        let mut m = sc_live_4();
        // PLAY learned as something else.
        m.bindings.retain(|b| b.trigger != Trigger::note(4, 0x0A));
        m.bind(Trigger::note(4, 0x0A), Target::Sync { deck: 0 });
        let mut c = DeckCheck::for_mapping(false, &m);
        c.hear(1.0, &[0x94, 0x0A, 0x7F]);
        assert_eq!((c.got(), c.refused()), (None, Some(Trigger::note(4, 0x0A))));
        c.take_refused();
        assert_eq!((c.at, c.refused()), (1, None));
        assert_eq!(c.side(0, &m).play[0], Some(Trigger::note(4, 0x0A)));
        // Back forgets it.
        c.back();
        assert_eq!(c.side(0, &m).play[0], None);
    }

    #[test]
    fn a_deck_button_that_sends_nothing() {
        let m = sc_live_4();
        let c = run(Pretend::Quiet, Seen::NotSure);
        assert_eq!(c.side(0, &m).switch(), Switch::Quiet);
        let text = [c.summary(&m), c.details(&m)].concat().join("\n");
        assert!(
            text.contains("after DECK, PLAY sent nothing at all"),
            "{text}"
        );
        assert!(
            text.contains("DECK itself sends nothing Tandem can see"),
            "{text}"
        );
        assert!(c.fix(&m).is_none());
    }

    #[test]
    fn the_summary_is_short_and_plain() {
        let m = sc_live_4();
        let soft = run(Pretend::Software, Seen::Yes).summary(&m);
        assert_eq!(soft.len(), 2, "{soft:?}");
        assert!(soft[0].starts_with("Your controller doesn't switch decks itself"));
        assert!(soft[0].ends_with("that's why DECK does nothing."));
        assert_eq!(
            soft[1],
            "Tandem can show the deck number on the left jog wheel's screen."
        );
        let hard = run(Pretend::Hardware, Seen::Yes).summary(&m);
        assert!(hard[0].contains(
            "the left side sends on MIDI channel 7 instead of 5, and the right side on 8 instead of 6"
        ));
        // What to do next is said apart from what it found, so it isn't in
        // the report, and changes once decks C and D are mapped.
        let c = run(Pretend::Hardware, Seen::Yes);
        assert!(c
            .next_step(&m, false)
            .starts_with("Click \"Map decks C and D\""));
        assert!(!c.next_step(&m, true).contains("Map decks C and D"));
        assert!(run(Pretend::Software, Seen::Yes)
            .next_step(&m, false)
            .contains("Set up step by step"));
        let info = ReportInfo {
            version: "1.2.3".into(),
            port: "SC LIVE 4".into(),
            output: true,
            unix_secs: 0,
        };
        let report = c.report(&info, &m);
        assert!(!report.contains("Click"), "{report}");
        assert!(!report.contains("Copy the report"), "{report}");
        let quiet = run(Pretend::Quiet, Seen::No).summary(&m);
        assert!(quiet[0].starts_with("Left side: after DECK, PLAY sent nothing"));
        // No MIDI bytes: those are in the details.
        for p in soft.iter().chain(&hard).chain(&quiet) {
            assert!(!p.contains("0x"), "{p}");
        }
    }

    /// The DDJ-FLX6 switches decks itself, and its built-in mapping already
    /// has decks C and D where it sends them.
    #[test]
    fn the_flx6_switches_decks_itself_and_is_mapped_already() {
        let m = builtin_preset("DDJ-FLX6").unwrap();
        let mut c = DeckCheck::new(false);
        let mut t = 0.0;
        while let Some(step) = c.step() {
            t += 3.0;
            let msgs: Vec<[u8; 3]> = match step {
                Step::Press {
                    side,
                    button,
                    round,
                } => {
                    // Deck 3 (or 4) after one DECK press.
                    let hw = side + if round == 1 { 2 } else { 0 };
                    match button {
                        Button::Play => vec![[0x90 + hw, 0x0B, 0x7F], [0x90 + hw, 0x0B, 0]],
                        Button::Load => vec![[0x96, 0x46 + hw, 0x7F], [0x96, 0x46 + hw, 0]],
                        // It says which deck the side has now.
                        Button::Deck => {
                            let (to, from) = if round == 0 {
                                (side + 2, side)
                            } else {
                                (side, side + 2)
                            };
                            vec![[0x90 + to, 0x3C, 0x7F], [0x90 + from, 0x3C, 0]]
                        }
                    }
                }
                Step::Fader { mixer } => vec![[0xB0 + mixer, 0x13, 60], [0xB0 + mixer, 0x13, 61]],
                _ => vec![],
            };
            for msg in msgs {
                c.hear(t, &msg);
            }
            if step.wants() == Want::Answer {
                c.answer(Seen::Yes);
            } else if !c.tick(t + 2.0) {
                c.next();
            }
        }
        assert_eq!(
            c.side(0, &m).switch(),
            Switch::Hardware {
                from: Trigger::note(0, 0x0B),
                to: Trigger::note(2, 0x0B),
                back: true
            }
        );
        let summary = c.summary(&m);
        assert!(summary[0].contains(
            "the left side sends on MIDI channel 3 instead of 1, and the right side on 4 instead of 2."
        ), "{summary:?}");
        assert!(!summary[0].contains("stops working"), "{summary:?}");
        let details = c.details(&m).join("\n");
        assert!(details.contains("LOAD changes too"), "{details}");
        assert!(
            details.contains("Tandem's mapping uses it for Deck C · Channel fader"),
            "{details}"
        );
        // Nothing to add.
        assert!(c.fix(&m).is_none());
    }

    #[test]
    fn back_never_goes_over_a_deck_press() {
        let mut c = DeckCheck::new(true);
        // PLAY, LOAD: Back is fine.
        c.hear(1.0, &[0x94, 0x0A, 0x7F]);
        c.tick(5.0);
        assert!(c.can_back());
        c.hear(6.0, &[0x9F, 0x01, 0x7F]);
        c.tick(10.0);
        // On DECK, not pressed yet: Back to LOAD is fine.
        assert_eq!(
            c.step(),
            Some(Step::Press {
                side: 0,
                button: Button::Deck,
                round: 0
            })
        );
        assert!(c.can_back());
        // Pressed: the side has switched, so no going back.
        c.hear(11.0, &[0x94, 0x1F, 0x7F]);
        assert!(!c.can_back());
        c.tick(20.0);
        // The screen question right after it can't go back onto it either.
        assert_eq!(c.step(), Some(Step::ScreenChanged { side: 0 }));
        assert!(!c.can_back());
        c.back();
        assert_eq!(c.at, 3);
    }

    #[test]
    fn a_fader_still_moving_doesnt_run_into_the_next_step() {
        let m = sc_live_4();
        let mut c = DeckCheck::new(false);
        let fader3 = c.index(Step::Fader { mixer: 2 }).unwrap();
        c.at = fader3;
        // Channel 3's fader moved for 2.6 s, a message every 0.1 s.
        let mut t = 0.0;
        while t < 2.6 {
            c.hear(t, &[0xB2, 0x0E, (t * 40.0) as u8]);
            assert!(!c.tick(t + 0.05), "moved on at {t}");
            t += 0.1;
        }
        assert!(c.tick(t + SETTLE_FADER));
        // Channel 4's step: a last message from channel 3, then channel 4.
        c.hear(t + 2.0, &[0xB2, 0x0E, 100]);
        for i in 0..5 {
            c.hear(t + 2.5 + i as f64 * 0.1, &[0xB3, 0x0E, 60 + i]);
        }
        assert_eq!(c.fader(2, &m), Some(Trigger::cc(2, 0x0E)));
        assert_eq!(c.fader(3, &m), Some(Trigger::cc(3, 0x0E)));
    }

    #[test]
    fn a_fader_like_the_mappings_wins_over_its_fine_half() {
        // A 14-bit fader sends its fine half as often as its coarse half.
        let m = builtin_preset("DDJ-FLX6").unwrap();
        let mut c = DeckCheck::new(false);
        c.at = c.index(Step::Fader { mixer: 2 }).unwrap();
        for v in 0..4u8 {
            c.hear(f64::from(v), &[0xB2, 0x33, v]);
            c.hear(f64::from(v), &[0xB2, 0x13, v]);
        }
        assert_eq!(c.fader(2, &m), Some(Trigger::cc(2, 0x13)));
    }

    #[test]
    fn buttons_that_send_control_changes_are_heard() {
        let mut m = Mapping::empty("CC buttons");
        m.bindings.push(Binding {
            trigger: Trigger::cc(0, 0x10),
            target: Target::Play { deck: 0 },
            invert: false,
            rel: Default::default(),
            shift: false,
            mode: None,
            layered: false,
            light: None,
        });
        let mut c = DeckCheck::new(false);
        // A jog wheel tick (1/127 style) isn't a press.
        c.hear(0.5, &[0xB0, 0x20, 0x7F]);
        assert_eq!(c.got(), None);
        c.hear(1.0, &[0xB0, 0x10, 0x7F]);
        assert_eq!(c.got(), None);
        c.hear(1.1, &[0xB0, 0x10, 0x00]);
        assert_eq!(c.got(), Some(1.1));
        assert_eq!(c.side(0, &m).play[0], Some(Trigger::cc(0, 0x10)));
    }

    #[test]
    fn the_other_sides_play_isnt_taken() {
        let m = sc_live_4();
        let mut c = DeckCheck::new(false);
        // Right PLAY by mistake, then left PLAY.
        c.hear(1.0, &[0x95, 0x0A, 0x7F]);
        c.hear(1.2, &[0x94, 0x0A, 0x7F]);
        assert_eq!(c.side(0, &m).play[0], Some(Trigger::note(4, 0x0A)));
        // Only the right one: nothing for the left.
        let mut c = DeckCheck::new(false);
        c.hear(1.0, &[0x95, 0x0A, 0x7F]);
        assert_eq!(c.side(0, &m).play[0], None);
    }

    #[test]
    fn map_decks_c_and_d_says_what_it_left_out() {
        let m = sc_live_4();
        let fix = run(Pretend::Hardware, Seen::Yes).fix(&m).unwrap();
        // LOAD sends the same on both decks: no LOAD for C or D.
        assert!(
            fix.left_out
                .iter()
                .any(|l| l == "Deck C · Load selected track"),
            "{:?}",
            fix.left_out
        );
        assert!(
            fix.left_out
                .iter()
                .any(|l| l == "Deck D · Load selected track"),
            "{:?}",
            fix.left_out
        );
        assert!(
            !fix.left_out.iter().any(|l| l.contains("Play")),
            "{:?}",
            fix.left_out
        );
    }

    /// LOAD with SHIFT + LOAD on it is LOAD all the same (the SC Live 4's),
    /// and a SHIFT + LOAD of its own isn't (the FLX6's).
    #[test]
    fn shift_load_isnt_taken_for_load() {
        let m = builtin_preset("SC LIVE 4").unwrap();
        let eject = |b: &Binding| matches!(b.target, Target::Eject { .. });
        assert!(m.bindings.iter().any(|b| b.shift && eject(b)));
        let mut c = DeckCheck::for_mapping(false, &m);
        c.hear(1.0, &[0x94, 0x0A, 0x7F]);
        c.tick(3.0);
        c.hear(4.0, &[0x9F, 0x01, 0x7F]);
        assert_eq!((c.got(), c.refused()), (Some(4.0), None));
        assert_eq!(c.side(0, &m).load[0], Some(Trigger::note(15, 0x01)));
        // A pass with SHIFT + LOAD mapped finds what it found without.
        let without = {
            let mut m = m.clone();
            m.bindings.retain(|b| !eject(b));
            m
        };
        let (with, without) = (
            run(Pretend::Software, Seen::No).summary(&m),
            run(Pretend::Software, Seen::No).summary(&without),
        );
        assert_eq!(with, without);

        let m = builtin_preset("DDJ-FLX6").unwrap();
        let mut c = DeckCheck::for_mapping(false, &m);
        c.hear(1.0, &[0x90, 0x0B, 0x7F]);
        c.tick(3.0);
        c.hear(4.0, &[0x96, 0x58, 0x7F]);
        assert_eq!((c.got(), c.refused()), (None, Some(Trigger::note(6, 0x58))));
        c.hear(5.0, &[0x96, 0x46, 0x7F]);
        assert_eq!((c.got(), c.refused()), (Some(5.0), None));
    }

    /// "Map decks C and D" for a LOAD that changes with DECK takes SHIFT +
    /// LOAD along.
    #[test]
    fn map_decks_c_and_d_takes_shift_load_along() {
        let mut m = sc_live_4_preset_13();
        for (side, deck) in [(0x01, 0), (0x02, 1)] {
            m.bind(Trigger::note(15, side), Target::Eject { deck })
                .shift = true;
        }
        let mut c = DeckCheck::new(false);
        let mut t = 0.0;
        while let Some(step) = c.step() {
            t += 3.0;
            if let Step::Press {
                side,
                button,
                round,
            } = step
            {
                let moved = if round == 1 { 2 } else { 0 };
                let msg = match button {
                    Button::Play => [0x94 + side + moved, 0x0A, 0x7F],
                    Button::Deck => [0x94 + side, 0x1F, 0x7F],
                    Button::Load => [0x9F, 0x01 + side + moved, 0x7F],
                };
                c.hear(t, &msg);
            }
            if step.wants() == Want::Answer {
                c.answer(Seen::No);
            } else if !c.tick(t + 2.0) {
                c.next();
            }
        }
        let fix = c.fix(&m).unwrap();
        for (note, deck) in [(0x03, 2), (0x04, 3)] {
            let on: Vec<(Target, bool)> = fix
                .mapping
                .bindings
                .iter()
                .filter(|b| b.trigger == Trigger::note(15, note))
                .map(|b| (b.target, b.shift))
                .collect();
            assert_eq!(
                on,
                [
                    (Target::Load { deck }, false),
                    (Target::Eject { deck }, true)
                ]
            );
        }
        assert!(
            !fix.left_out.iter().any(|l| l.contains("Eject")),
            "{:?}",
            fix.left_out
        );
    }

    #[test]
    fn letting_go_of_what_was_held_when_the_check_opened() {
        let mut e = MidiEngine::new(sc_live_4(), 0.08);
        let (mut room, local) = (RoomState::default(), LocalParams::default());
        e.handle(&[0x94, 0x1C, 0x7F], &room, &local); // SHIFT
        e.handle(&[0x94, 0x21, 0x7F], &room, &local); // jog wheel touched
                                                      // The room took the touch: deck A is held.
        room.decks[0].scratching = true;
        assert!(e.shift_held());
        let out = e.let_go(&room);
        assert!(!e.shift_held());
        assert!(
            out.iter().any(|o| matches!(
                o,
                MidiOut::Action(Action::JogTouch {
                    deck: 0,
                    down: false,
                    ..
                })
            )),
            "{out:?}"
        );
        assert!(e.let_go(&room).is_empty());
    }

    #[test]
    fn a_stray_touch_isnt_taken_for_play() {
        let m = sc_live_4();
        let mut c = DeckCheck::new(false);
        // The jog wheel's touch sensor, then PLAY.
        c.hear(1.0, &[0x94, 0x21, 0x7F]);
        c.hear(1.1, &[0x94, 0x0A, 0x7F]);
        assert!(c.tick(2.0));
        assert_eq!(c.side(0, &m).play[0], Some(Trigger::note(4, 0x0A)));
    }

    #[test]
    fn steps_wait_for_what_they_ask_for() {
        let mut c = DeckCheck::new(false);
        // A fader moved at a button step doesn't count.
        c.hear(0.5, &[0xB0, 0x0E, 40]);
        assert_eq!(c.got(), None);
        assert!(!c.tick(5.0));
        // A release doesn't either.
        c.hear(0.6, &[0x94, 0x0A, 0x00]);
        assert_eq!(c.got(), None);
        c.hear(1.0, &[0x94, 0x0A, 0x7F]);
        assert_eq!(c.got(), Some(1.0));
        assert!(!c.tick(1.5));
        assert!(c.tick(1.0 + SETTLE));
        assert_eq!(c.at, 1);
        // Back: both steps are done again from nothing.
        c.hear(2.0, &[0x9F, 0x01, 0x7F]);
        c.back();
        assert_eq!(c.at, 0);
        assert!(c.heard(0).is_empty() && c.heard(1).is_empty());
        assert_eq!(c.got(), None);
    }

    #[test]
    fn the_report_lists_every_step_with_what_the_mapping_makes_of_it() {
        let m = sc_live_4();
        let c = run(Pretend::Software, Seen::Yes);
        let info = ReportInfo {
            version: "0.14.1".into(),
            port: "SC LIVE 4".into(),
            output: true,
            unix_secs: 1_795_500_000,
        };
        let r = c.report(&info, &m);
        assert!(
            r.starts_with("Tandem DECK button check\nTandem 0.14.1 · 2026-11-24 06:00 UTC"),
            "{r}"
        );
        assert!(r.contains("Mapping: built-in Denon SC Live 4"), "{r}");
        assert!(
            r.contains("94 0A 7F  Note 0x0a on (127) · ch 5  -> Deck A · Play/Pause"),
            "{r}"
        );
        assert!(
            r.contains("94 1F 7F  Note 0x1f on (127) · ch 5  -> (not mapped)"),
            "{r}"
        );
        assert!(r.contains("answer: yes"), "{r}");
        assert!(r.contains(" 1. Press PLAY on the left deck."), "{r}");
        assert!(r.contains("18. Tandem is now asking"), "{r}");
        assert!(!r.contains("Stopped at step"), "{r}");
    }

    #[test]
    fn the_jog_screen_is_asked_as_mixxx_asks_it() {
        assert_eq!(
            jog_screen_deck(4, 3),
            vec![
                vec![0xB4, 0x35, 0x7F],
                vec![0xB4, 0x36, 0x7F],
                vec![0xB4, 0x37, 0x7F],
                vec![0x94, 0x38, 3]
            ]
        );
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc_label(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc_stamp(951_782_400), "20000229-000000");
        assert_eq!(utc_label(1_795_500_000), "2026-11-24 06:00 UTC");
    }
}
