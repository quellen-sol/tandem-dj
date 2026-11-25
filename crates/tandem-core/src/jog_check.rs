//! The jog wheel check: Settings → Controller → **Check the jog wheels…**
//! asks a DJ to rest a hand on a jog wheel and lift it, to turn it with a
//! hand on top and by its edge, both ways, to turn it exactly once round, and
//! to flick it and let go. It records everything the controller sends and
//! works out, for each side:
//!
//! - the touch sensor's message, and whether it's let go when the hand is;
//! - which messages the platter and the edge send, and whether they're the
//!   same one; how they count (or whether they say where the wheel is
//!   instead), which way round, and how many ticks are in a turn (see
//!   [`crate::wheel`]);
//! - whether holding the edge sets off the touch sensor, as it can on a metal
//!   wheel, and what the wheel sends once it's let go;
//! - what the mapping makes of all that, and how to put it right
//!   ([`JogCheck::fix`]).
//!
//! It was made for a Denon SC Live 4, whose jog wheel nobody has recorded,
//! and works for any controller. Like the DECK button check (see
//! [`crate::deck_check`]) it keeps the controller's messages out of the room
//! while it runs, doesn't take a control the mapping knows as something else
//! for the one asked for, says so when a result isn't clear, and writes a
//! plain-text report to send to us.

use crate::deck_check::{utc_label, Heard, ReportInfo};
use crate::mapping::{
    builtin_preset, parse, side_of, Binding, Mapping, MidiKind, RawMidi, RelMode, Target, Trigger,
};
use crate::model::deck_letter;
use crate::wheel::{self, Msg, Wheel, TURN_MESSAGES};
use crate::wizard::MIN_TURN_TICKS;

/// A hand lifted (s): what comes with it is still part of the step.
const SETTLE_TOUCH: f64 = 1.0;
/// A wheel that has been still this long (s) has been turned.
const SETTLE_TURN: f64 = 1.3;
/// A control change the mapping knows as another control's has moved once it
/// has sent this many messages in a step.
const OTHER_MOVED: usize = 4;
/// Messages the report lists for one step.
const REPORT_MESSAGES: usize = 6_000;

/// What a step asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Do {
    /// Rest a hand on top, and lift it.
    Touch,
    /// Turn it with a hand on top, clockwise, then back.
    PlatterCw,
    PlatterCcw,
    /// Turn it by its edge, clockwise, then back.
    EdgeCw,
    EdgeCcw,
    /// Turn it exactly once round.
    FullTurn,
    /// Flick it and let go.
    Flick,
}

impl Do {
    /// Everything, in the order it's asked on the left side.
    const ALL: [Do; 7] = [
        Do::Touch,
        Do::PlatterCw,
        Do::PlatterCcw,
        Do::EdgeCw,
        Do::EdgeCcw,
        Do::FullTurn,
        Do::Flick,
    ];
    /// What tells whether the right side sends what the left does.
    const SHORT: [Do; 3] = [Do::Touch, Do::PlatterCw, Do::EdgeCw];

    fn edge(self) -> bool {
        matches!(self, Do::EdgeCw | Do::EdgeCcw)
    }
}

/// One step of the check. Sides: 0 is the left, 1 the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub side: u8,
    pub what: Do,
}

fn side_name(side: u8) -> &'static str {
    if side == 0 {
        "left"
    } else {
        "right"
    }
}

impl Step {
    /// What the DJ is asked to do.
    pub fn instruction(self) -> String {
        let s = side_name(self.side);
        match self.what {
            Do::Touch => format!(
                "Rest your hand flat on top of the {s} jog wheel, count to two, and lift it off."
            ),
            Do::PlatterCw => format!(
                "Hand flat on top of the {s} jog wheel: turn it slowly clockwise, about a quarter \
                 turn. Then lift your hand off."
            ),
            Do::PlatterCcw => format!(
                "Hand flat on top of the {s} jog wheel again: turn it slowly back, about a quarter \
                 turn anticlockwise. Then lift your hand off."
            ),
            Do::EdgeCw => format!(
                "Now hold the {s} jog wheel only by its edge, without touching the top: turn it \
                 slowly clockwise, about a quarter turn, and let go."
            ),
            Do::EdgeCcw => format!(
                "By the edge of the {s} jog wheel again, without touching the top: turn it slowly \
                 back, about a quarter turn anticlockwise, and let go."
            ),
            Do::FullTurn => format!(
                "Put a mark on the {s} jog wheel. Hand on top: turn it clockwise exactly one full \
                 turn, and stop."
            ),
            Do::Flick => format!(
                "Hand on top of the {s} jog wheel: flick it backwards and let go while it's \
                 moving. Don't touch it again until it has stopped."
            ),
        }
    }

    /// What to mind.
    pub fn hint(self) -> &'static str {
        match self.what {
            Do::Touch => "Try not to turn it.",
            Do::PlatterCw | Do::PlatterCcw => "Slowly: it takes a second or two.",
            Do::EdgeCw | Do::EdgeCcw => {
                "A finger or two on the side or the rim. If your hand touches the top, do the \
                 step again."
            }
            Do::FullTurn => {
                "A bit of tape will do for a mark. Keep your hand on top the whole way, then \
                 click \"That was one turn\"."
            }
            Do::Flick => "As for a backspin, gently.",
        }
    }
}

/// Whose control a message's is, going by the mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Whose {
    /// This side's jog wheel's.
    Its,
    /// Nobody's: it may well be the jog wheel's.
    Nobodys,
    /// Something else's, or the other side's jog wheel's.
    Others,
}

/// How far the step being done has got.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    /// The touch sensors (or unknown buttons) pressed and not let go, and
    /// when the last one came or went.
    pub held: Vec<Trigger>,
    pub touched: Option<f64>,
    /// Wheel messages, when the last came, and the controls they're from.
    pub moves: usize,
    pub moved: Option<f64>,
    pub controls: Vec<Trigger>,
}

/// What a side's touch sensor does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touch {
    /// Nothing came from it.
    Missing,
    /// It comes with the hand and goes with it.
    Works(Trigger),
    /// It never says the hand has gone.
    Stuck(Trigger),
    /// It comes and goes while the hand stays on top.
    Flaky(Trigger),
    /// What came isn't clearly a touch sensor.
    Unclear,
}

impl Touch {
    pub fn trigger(self) -> Option<Trigger> {
        match self {
            Touch::Works(t) | Touch::Stuck(t) | Touch::Flaky(t) => Some(t),
            _ => None,
        }
    }
}

/// What the wheel did once it was flicked and let go.
#[derive(Clone, Debug, PartialEq)]
pub struct RanOn {
    /// How long it sent messages after the hand left (s), how far it went
    /// and what it sent.
    pub secs: f64,
    pub ticks: i64,
    pub controls: Vec<Trigger>,
}

/// What the check found on one side.
#[derive(Clone, Debug, PartialEq)]
pub struct SideFindings {
    pub side: u8,
    /// Whether any of its steps heard anything.
    pub heard: bool,
    pub touch: Touch,
    /// Everything that turned with the platter, and with the edge.
    pub platter_all: Vec<Wheel>,
    pub edge_all: Vec<Wheel>,
    /// What to go by: the same one, if they send the same.
    pub platter: Option<Wheel>,
    pub edge: Option<Wheel>,
    /// Holding the edge sets off the touch sensor.
    pub edge_touches: Option<bool>,
    /// Ticks in a turn, for each wheel that sent any.
    pub turn: Vec<(Wheel, i64)>,
    pub ran_on: Option<RanOn>,
    /// It sends what the left side does, so the left side's steps stand for
    /// the ones it wasn't asked for.
    pub like_left: bool,
    /// What isn't clear, and which steps to do again.
    pub unclear: Vec<String>,
}

impl SideFindings {
    fn name(&self) -> &'static str {
        if self.side == 0 {
            "Left jog wheel"
        } else {
            "Right jog wheel"
        }
    }

    /// The platter and the edge send the same message.
    pub fn one_message(&self) -> bool {
        match (self.platter, self.edge) {
            (Some(p), Some(e)) => p.trigger == e.trigger,
            _ => false,
        }
    }

    /// Whether the touch sensor can be used: it works, and it isn't set off
    /// by an edge that could be told from the platter without it.
    fn touch_used(&self) -> Option<Trigger> {
        match self.touch {
            Touch::Works(t) if self.one_message() || self.edge_touches != Some(true) => Some(t),
            _ => None,
        }
    }

    /// Ticks in a turn of the wheel the mapping goes by.
    fn ticks(&self, wheel: Option<Wheel>) -> Option<i64> {
        let w = wheel?;
        self.turn
            .iter()
            .find(|(x, _)| x.trigger == w.trigger)
            .map(|(_, n)| n.abs())
    }

    /// Everything the wheel sends on: its touch sensor and what it turns on,
    /// both halves of a position.
    fn triggers(&self) -> Vec<Trigger> {
        let mut out: Vec<Trigger> = self.touch.trigger().into_iter().collect();
        for w in self.platter_all.iter().chain(&self.edge_all) {
            out.push(w.trigger);
            if let RelMode::Absolute14 { fine } = w.rel {
                out.push(Trigger::cc(w.trigger.channel, fine));
            }
        }
        out.sort_by_key(|t| (t.kind as u8, t.channel, t.number));
        out.dedup();
        out
    }

    /// Clear enough to map.
    pub fn clear(&self) -> bool {
        self.unclear.is_empty() && (self.platter.is_some() || self.edge.is_some())
    }

    /// What the mapping should bind for deck `deck`.
    fn wanted(&self, deck: u8, layered: bool) -> Vec<Binding> {
        let mut out = Vec::new();
        let mut add = |trigger, target, wheel: Option<Wheel>| {
            out.push(Binding {
                trigger,
                target,
                invert: wheel.is_some_and(|w| w.ticks < 0),
                rel: wheel.map(|w| w.rel).unwrap_or_default(),
                shift: false,
                mode: None,
                light: None,
                layered,
            });
        };
        if let Some(t) = self.touch_used() {
            add(t, Target::JogTouch { deck }, None);
        }
        match (self.platter, self.edge) {
            (Some(p), _) if self.one_message() => add(p.trigger, Target::Jog { deck }, Some(p)),
            (p, e) => {
                if let Some(p) = p {
                    add(p.trigger, Target::JogPlatter { deck }, Some(p));
                }
                if let Some(e) = e {
                    add(e.trigger, Target::JogEdge { deck }, Some(e));
                }
            }
        }
        out
    }
}

/// What [`JogCheck::fix`] made: the mapping, and a line for each thing that
/// changed.
#[derive(Clone, Debug)]
pub struct Fix {
    pub mapping: Mapping,
    pub changed: Vec<String>,
}

/// The check, step by step.
#[derive(Clone, Debug)]
pub struct JogCheck {
    pub steps: Vec<Step>,
    /// The step being done; `steps.len()` once it's over.
    pub at: usize,
    heard: Vec<Vec<Heard>>,
    /// The controller's mapping, to tell a jog wheel's controls from others.
    mapping: Mapping,
    /// Controls the DJ said were the wheel's all the same.
    taken: Vec<Trigger>,
    /// The last control in the step being done that isn't the wheel's.
    refused: Option<Trigger>,
}

impl JogCheck {
    /// For a controller with this mapping.
    pub fn new(mapping: &Mapping) -> Self {
        let steps: Vec<Step> = Do::ALL
            .map(|what| Step { side: 0, what })
            .into_iter()
            .chain(Do::SHORT.map(|what| Step { side: 1, what }))
            .collect();
        Self {
            heard: vec![Vec::new(); steps.len()],
            steps,
            at: 0,
            mapping: mapping.clone(),
            taken: Vec::new(),
            refused: None,
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

    /// The control heard last in this step that isn't the wheel's, as far as
    /// the mapping knows.
    pub fn refused(&self) -> Option<Trigger> {
        self.refused
    }

    /// It was the wheel's ("That was it").
    pub fn take_refused(&mut self) {
        if let Some(t) = self.refused.take() {
            self.taken.push(t);
        }
    }

    /// Whose control `t` is, for a side.
    fn whose(&self, side: u8, t: Trigger) -> Whose {
        if self.taken.contains(&t) {
            return Whose::Its;
        }
        let ours = |b: &&Binding| b.target.deck().is_some_and(|d| side_of(d) == side as usize);
        let jog =
            |b: &&Binding| b.target.is_jog_turn() || matches!(b.target, Target::JogTouch { .. });
        // (The fine half of a position, too.)
        let uses: Vec<&Binding> = self
            .mapping
            .bindings
            .iter()
            .filter(|b| !b.shift)
            .filter(|b| {
                b.trigger == t
                    || (t.kind == MidiKind::Cc
                        && b.trigger.channel == t.channel
                        && b.rel == RelMode::Absolute14 { fine: t.number })
            })
            .collect();
        if uses.is_empty() {
            Whose::Nobodys
        } else if uses.iter().any(|b| jog(b) && ours(b)) {
            Whose::Its
        } else {
            Whose::Others
        }
    }

    /// A message from the controller, `t` seconds into the check. After the
    /// last step, messages are no longer kept.
    pub fn hear(&mut self, t: f64, bytes: &[u8]) {
        let Some(step) = self.step() else { return };
        let at = self.at;
        self.heard[at].push(Heard {
            t,
            bytes: bytes.to_vec(),
        });
        let Some(raw) = parse(bytes) else { return };
        let trig = trigger_of(raw);
        if self.whose(step.side, trig) != Whose::Others {
            return;
        }
        match raw {
            RawMidi::NoteOn { .. } => self.refused = Some(trig),
            RawMidi::Cc { .. } | RawMidi::PitchBend { .. } => {
                let n = self.heard[at]
                    .iter()
                    .filter(|h| parse(&h.bytes).map(trigger_of) == Some(trig))
                    .count();
                if n >= OTHER_MOVED {
                    self.refused = Some(trig);
                }
            }
            RawMidi::NoteOff { .. } => {}
        }
    }

    /// The messages of step `i` that can be the wheel's: not another
    /// control's, nor the fine half of one (sent straight after it).
    fn its(&self, i: usize) -> Vec<(f64, RawMidi)> {
        let Some(step) = self.steps.get(i) else {
            return Vec::new();
        };
        let mut other: Option<(f64, u8)> = None;
        let mut out = Vec::new();
        // (A wheel sends thousands of messages, from a few controls.)
        let mut known: Vec<(Trigger, Whose)> = Vec::new();
        for h in self.heard(i) {
            let Some(raw) = parse(&h.bytes) else { continue };
            let trig = trigger_of(raw);
            let whose = match known.iter().find(|k| k.0 == trig) {
                Some(k) => k.1,
                None => {
                    let w = self.whose(step.side, trig);
                    known.push((trig, w));
                    w
                }
            };
            match whose {
                Whose::Others => {
                    if trig.kind == MidiKind::Cc {
                        other = Some((h.t, trig.channel));
                    }
                }
                Whose::Nobodys
                    if trig.kind == MidiKind::Cc
                        && other.is_some_and(|(t, ch)| {
                            ch == trig.channel && (0.0..0.005).contains(&(h.t - t))
                        }) => {}
                _ => out.push((h.t, raw)),
            }
        }
        out
    }

    /// The control changes of step `i` that can be the wheel's.
    fn turned(&self, i: usize) -> Vec<Msg> {
        let cc = |(t, raw): (f64, RawMidi)| Msg::of(raw, (t * 1e6) as i64);
        self.its(i).into_iter().filter_map(cc).collect()
    }

    /// Where the steps of a side that ask for this are.
    fn find(&self, side: u8, what: Do) -> Option<usize> {
        let i = self.steps.iter().position(|s| *s == Step { side, what })?;
        (i <= self.at).then_some(i)
    }

    /// How far step `i` has got.
    pub fn progress(&self, i: usize) -> Progress {
        let mut p = Progress::default();
        let mut down: Vec<Trigger> = Vec::new();
        for (t, raw) in self.its(i) {
            let trig = trigger_of(raw);
            match raw {
                RawMidi::NoteOn { .. } => {
                    if !down.contains(&trig) {
                        down.push(trig);
                    }
                    p.touched = Some(t);
                }
                RawMidi::NoteOff { .. } => {
                    down.retain(|x| *x != trig);
                    if p.touched.is_some() {
                        p.touched = Some(t);
                    }
                }
                RawMidi::Cc { .. } => {
                    p.moves += 1;
                    p.moved = Some(t);
                    if !p.controls.contains(&trig) {
                        p.controls.push(trig);
                    }
                }
                RawMidi::PitchBend { .. } => {}
            }
        }
        p.held = down;
        p
    }

    /// Touch sensors a side's first step left pressed: waiting for those to
    /// be let go would be waiting for ever.
    fn stuck(&self, side: u8) -> Vec<Trigger> {
        match self.find(side, Do::Touch) {
            Some(i) if i < self.at => self.progress(i).held,
            _ => Vec::new(),
        }
    }

    /// Whether the step being done got what it asked for, as far as can be
    /// told (a full turn is for the DJ to say).
    pub fn got(&self) -> bool {
        let Some(step) = self.step() else {
            return false;
        };
        let p = self.progress(self.at);
        match step.what {
            Do::Touch => p.touched.is_some(),
            _ => p.moves >= TURN_MESSAGES,
        }
    }

    /// Whether it's waiting for a hand to be lifted.
    pub fn hand_on(&self) -> bool {
        let Some(step) = self.step() else {
            return false;
        };
        let stuck = self.stuck(step.side);
        let held = self.progress(self.at).held;
        held.iter().any(|t| !stuck.contains(t))
    }

    /// Moves on once the step got what it asks for and all has been still
    /// for a moment. Returns whether it did.
    pub fn tick(&mut self, now: f64) -> bool {
        let Some(step) = self.step() else {
            return false;
        };
        if step.what == Do::FullTurn || !self.got() || self.hand_on() {
            return false;
        }
        let p = self.progress(self.at);
        let last = [p.touched, p.moved]
            .into_iter()
            .flatten()
            .fold(0.0, f64::max);
        let settle = if step.what == Do::Touch {
            SETTLE_TOUCH
        } else {
            SETTLE_TURN
        };
        if now - last < settle {
            return false;
        }
        self.next();
        true
    }

    /// On to the next step, whatever this one got.
    pub fn next(&mut self) {
        if self.done() {
            return;
        }
        self.at += 1;
        self.refused = None;
        // The right side sends something else than the left: the rest of its
        // steps are asked for too.
        let right = |s: &&Step| s.side == 1;
        if self.done()
            && self.steps.iter().filter(right).count() == Do::SHORT.len()
            && self.like_left().is_none()
        {
            for what in Do::ALL.into_iter().filter(|d| !Do::SHORT.contains(d)) {
                self.steps.push(Step { side: 1, what });
                self.heard.push(Vec::new());
            }
        }
    }

    /// Do the step being done again: what it heard is forgotten.
    pub fn again(&mut self) {
        if let Some(h) = self.heard.get_mut(self.at) {
            h.clear();
        }
        self.refused = None;
    }

    pub fn can_back(&self) -> bool {
        self.at > 0
    }

    /// Back a step, to do it again: what both steps heard is forgotten.
    pub fn back(&mut self) {
        if !self.can_back() {
            return;
        }
        self.again();
        self.at -= 1;
        self.again();
    }

    /// Ticks counted so far in a full turn, of the wheel the steps before
    /// showed: None if they showed none.
    pub fn count(&self) -> Option<i64> {
        let step = self.step().filter(|s| s.what == Do::FullTurn)?;
        let touch = self.touch(step.side, &mut Vec::new());
        let w = self.wheels(step.side, touch, Do::PlatterCw, Do::PlatterCcw);
        let w = w.first()?;
        Some(
            wheel::decode(&self.turned(self.at), w.trigger, w.rel)
                .0
                .abs(),
        )
    }

    /// What turned at two steps, there and back. A wheel let go of runs on
    /// for a moment, by its edge's message if it has one of its own: so at
    /// the platter's steps, what came without a hand on top doesn't count
    /// (where the touch sensor can tell), and nor does a control that moved
    /// much less than another.
    fn wheels(&self, side: u8, touch: Touch, cw: Do, ccw: Do) -> Vec<Wheel> {
        let (Some(a), Some(b)) = (self.find(side, cw), self.find(side, ccw)) else {
            return Vec::new();
        };
        let (there, back) = (self.by_hand(a, touch), self.by_hand(b, touch));
        let mut out = wheel::read_both(&there, &back);
        // A position too coarse for a quarter turn to show both its halves
        // shows them in a full turn.
        if out.is_empty() {
            if let Some(i) = self.find(side, Do::FullTurn) {
                out = wheel::read(&self.by_hand(i, touch))
                    .into_iter()
                    .filter_map(|w| wheel::turned_back(&there, &back, w.trigger, w.rel))
                    .collect();
            }
        }
        let most = out.iter().map(|w| w.n).max().unwrap_or(0);
        out.retain(|w| w.n * 2 >= most);
        out
    }

    /// What step `i`'s wheel sent while it was turned: at a step with a hand
    /// on top, only under it, where the touch sensor can tell.
    fn by_hand(&self, i: usize, touch: Touch) -> Vec<Msg> {
        let edge = self.steps.get(i).is_some_and(|s| s.what.edge());
        (!edge)
            .then(|| self.under(i, touch))
            .flatten()
            .unwrap_or_else(|| self.turned(i))
    }

    /// How much of step `i`'s turning happened with `touch` held (0 to 1),
    /// whether `touch` came at all, and whether it was let go.
    fn cover(&self, i: usize, touch: Trigger) -> (f64, bool, bool) {
        let (mut on, mut came) = (false, false);
        let (mut under, mut all) = (0usize, 0usize);
        for (_, raw) in self.its(i) {
            match raw {
                RawMidi::NoteOn { .. } if trigger_of(raw) == touch => (on, came) = (true, true),
                RawMidi::NoteOff { .. } if trigger_of(raw) == touch => on = false,
                RawMidi::Cc { .. } => {
                    all += 1;
                    under += usize::from(on);
                }
                _ => {}
            }
        }
        (under as f64 / all.max(1) as f64, came, !on)
    }

    /// The buttons step `i` heard pressed that can be the wheel's.
    fn pressed(&self, i: usize) -> Vec<Trigger> {
        let mut out = Vec::new();
        for (_, raw) in self.its(i) {
            let trig = trigger_of(raw);
            if matches!(raw, RawMidi::NoteOn { .. }) && !out.contains(&trig) {
                out.push(trig);
            }
        }
        out
    }

    /// The touch sensor: what came as the hand rested on top, and every time
    /// the wheel was turned with a hand on top.
    fn touch(&self, side: u8, unclear: &mut Vec<String>) -> Touch {
        let turns: Vec<usize> = [Do::PlatterCw, Do::PlatterCcw, Do::FullTurn, Do::Flick]
            .into_iter()
            .filter_map(|w| self.find(side, w))
            .filter(|&i| self.turned(i).len() >= TURN_MESSAGES)
            .collect();
        let rested = self
            .find(side, Do::Touch)
            .map(|i| self.pressed(i))
            .unwrap_or_default();
        let mut all = rested.clone();
        for &i in &turns {
            for t in self.pressed(i) {
                if !all.contains(&t) {
                    all.push(t);
                }
            }
        }
        // The one that came most, with a hand on top.
        let times = |t: &Trigger| turns.iter().filter(|&&i| self.cover(i, *t).1).count();
        let Some(t) = all.iter().copied().max_by_key(|t| {
            (
                times(t),
                rested.contains(t),
                self.whose(side, *t) == Whose::Its,
            )
        }) else {
            return Touch::Missing;
        };
        let name = side_name(side);
        if turns.len() >= 2 && times(&t) * 2 < turns.len() {
            unclear.push(format!(
                "{} came when you were asked to rest your hand on the {name} jog wheel, and not \
                 when you turned it with your hand on top. If it's another button, do the first \
                 step again.",
                t.label()
            ));
            return Touch::Unclear;
        }
        let steps: Vec<usize> = self
            .find(side, Do::Touch)
            .into_iter()
            .chain(turns.iter().copied())
            .filter(|&i| self.cover(i, t).1)
            .collect();
        let let_go = steps.iter().filter(|&&i| self.cover(i, t).2).count();
        if let_go == 0 {
            return Touch::Stuck(t);
        }
        if let_go < steps.len() {
            unclear.push(format!(
                "The {name} touch sensor ({}) said your hand had gone only some of the times. Lift \
                 your hand off at the end of each step, and do it again.",
                t.label()
            ));
            return Touch::Unclear;
        }
        // It has to stay on while the wheel turns under the hand. (Once
        // flicked, it turns without one.)
        let covers: Vec<f64> = turns
            .iter()
            .filter(|&&i| self.steps[i].what != Do::Flick)
            .map(|&i| self.cover(i, t))
            .filter(|c| c.1)
            .map(|c| c.0)
            .collect();
        let on = covers.iter().filter(|&&c| c >= 0.5).count();
        if on == covers.len() {
            Touch::Works(t)
        } else if on == 0 {
            Touch::Flaky(t)
        } else {
            unclear.push(format!(
                "The {name} touch sensor ({}) was on for only part of some turns with your hand on \
                 top. Keep your hand flat on top while you turn, and do them again.",
                t.label()
            ));
            Touch::Unclear
        }
    }

    /// What it found on one side.
    pub fn side(&self, side: u8) -> SideFindings {
        if let (1, Some(f)) = (side, self.like_left()) {
            return f;
        }
        self.full(side)
    }

    /// The same, from the side's own steps alone.
    fn full(&self, side: u8) -> SideFindings {
        let name = side_name(side);
        let mut unclear = Vec::new();
        let steps = |s: &(usize, &Step)| s.1.side == side && s.0 < self.at;
        let heard = self
            .steps
            .iter()
            .enumerate()
            .filter(steps)
            .any(|(i, _)| !self.its(i).is_empty());
        let touch = self.touch(side, &mut unclear);
        let platter_all = self.wheels(side, touch, Do::PlatterCw, Do::PlatterCcw);
        let edge_all = self.wheels(side, touch, Do::EdgeCw, Do::EdgeCcw);
        // Turned, and not read.
        for (all, cw, ccw, how) in [
            (
                &platter_all,
                Do::PlatterCw,
                Do::PlatterCcw,
                "with your hand on top",
            ),
            (&edge_all, Do::EdgeCw, Do::EdgeCcw, "by its edge"),
        ] {
            let n = |d| self.find(side, d).map_or(0, |i| self.turned(i).len());
            let (a, b) = (n(cw), n(ccw));
            let asked = [cw, ccw]
                .iter()
                .all(|d| self.find(side, *d).is_some_and(|i| i < self.at));
            if all.is_empty() && asked && a.max(b) >= TURN_MESSAGES {
                unclear.push(format!(
                    "What the {name} jog wheel sent when it was turned {how} doesn't read as a \
                     wheel turned one way and then back. Turn it slowly, clockwise at the first \
                     step and back at the second."
                ));
            }
        }
        let only = |all: &[Wheel], other: &[Wheel]| -> Option<Wheel> {
            all.iter()
                .find(|w| !other.iter().any(|o| o.trigger == w.trigger))
                .copied()
        };
        let both = platter_all
            .iter()
            .copied()
            .find(|w| edge_all.iter().any(|o| o.trigger == w.trigger));
        let (platter, edge) = match (
            only(&platter_all, &edge_all),
            only(&edge_all, &platter_all),
            both,
        ) {
            (Some(p), Some(e), _) => (Some(p), Some(e)),
            (_, _, Some(w)) => (
                Some(w),
                edge_all.iter().copied().find(|e| e.trigger == w.trigger),
            ),
            (p, e, None) => (p, e),
        };
        // Whether the edge sets off the touch sensor.
        let mut edge_touches = None;
        if let Some(t) = touch.trigger() {
            let covers: Vec<(f64, bool, bool)> = [Do::EdgeCw, Do::EdgeCcw]
                .into_iter()
                .filter_map(|d| self.find(side, d))
                .filter(|&i| self.turned(i).len() >= TURN_MESSAGES)
                .map(|i| self.cover(i, t))
                .collect();
            let set_off = covers.iter().filter(|c| c.1 && c.0 >= 0.5).count();
            if covers.is_empty() {
            } else if set_off == covers.len() {
                edge_touches = Some(true);
            } else if covers.iter().all(|c| !c.1) {
                edge_touches = Some(false);
            } else {
                unclear.push(format!(
                    "The {name} touch sensor came on for part of the steps by the edge. If your \
                     hand touched the top, do those steps again."
                ));
            }
        }
        if let (true, Some(p), Some(e)) = (
            platter.map(|p| p.trigger) == edge.map(|e| e.trigger),
            platter,
            edge,
        ) {
            if p.ticks.signum() != e.ticks.signum() || p.rel != e.rel {
                unclear.push(format!(
                    "The {name} jog wheel turned one way with your hand on top and the other way \
                     by its edge. Turn it clockwise first each time, and then back."
                ));
            }
        }
        // One turn, with a hand on top for as long as it can tell.
        let mut turn = Vec::new();
        if let Some(i) = self.find(side, Do::FullTurn).filter(|&i| i < self.at) {
            let under = self.under(i, touch);
            for w in &platter_all {
                let all = wheel::decode(&self.turned(i), w.trigger, w.rel).0;
                let n = match &under {
                    Some(msgs) => wheel::decode(msgs, w.trigger, w.rel).0,
                    None => all,
                };
                if n != 0 {
                    turn.push((*w, n));
                }
            }
            if let Some((w, n)) =
                platter.and_then(|p| turn.iter().find(|t| t.0.trigger == p.trigger))
            {
                let quarter = w.ticks.abs();
                if n.abs() < MIN_TURN_TICKS || n.abs() * 2 < quarter * 3 {
                    unclear.push(format!(
                        "The {name} jog wheel's full turn was too short to be one ({} ticks; the \
                         quarter turn was {quarter}). Do it again.",
                        n.abs()
                    ));
                    turn.clear();
                } else if n.signum() != w.ticks.signum() {
                    unclear.push(format!(
                        "The {name} jog wheel's full turn went the other way from the quarter turn \
                         clockwise before it. Do both again, clockwise."
                    ));
                    turn.clear();
                }
            }
        }
        let ran_on = self.ran_on(side, touch);
        SideFindings {
            side,
            heard,
            touch,
            platter_all,
            edge_all,
            platter,
            edge,
            edge_touches,
            turn,
            ran_on,
            like_left: false,
            unclear,
        }
    }

    /// What step `i`'s wheel sent with a hand on top, where the touch sensor
    /// can tell: what it sends after being let go isn't part of a turn.
    fn under(&self, i: usize, touch: Touch) -> Option<Vec<Msg>> {
        let Touch::Works(t) = touch else { return None };
        if !self.cover(i, t).1 {
            return None;
        }
        let mut on = false;
        let mut out = Vec::new();
        for (at, raw) in self.its(i) {
            match raw {
                RawMidi::NoteOn { .. } if trigger_of(raw) == t => on = true,
                RawMidi::NoteOff { .. } if trigger_of(raw) == t => on = false,
                _ if on => out.extend(Msg::of(raw, (at * 1e6) as i64)),
                _ => {}
            }
        }
        Some(out)
    }

    /// What the wheel sent after the flick, once the hand had left it.
    fn ran_on(&self, side: u8, touch: Touch) -> Option<RanOn> {
        let Touch::Works(t) = touch else { return None };
        let i = self.find(side, Do::Flick).filter(|&i| i < self.at)?;
        let its = self.its(i);
        let left = its.iter().rposition(|(_, raw)| {
            matches!(raw, RawMidi::NoteOff { .. }) && trigger_of(*raw) == t
        })?;
        let at = its[left].0;
        let after: Vec<Msg> = its[left..]
            .iter()
            .filter_map(|&(t, raw)| Msg::of(raw, (t * 1e6) as i64))
            .collect();
        let mut controls: Vec<Trigger> = Vec::new();
        for m in &after {
            if !controls.contains(&m.trigger) {
                controls.push(m.trigger);
            }
        }
        let wheels: Vec<Wheel> = self
            .wheels(side, touch, Do::PlatterCw, Do::PlatterCcw)
            .into_iter()
            .chain(self.wheels(side, touch, Do::EdgeCw, Do::EdgeCcw))
            .collect();
        let ticks = wheels
            .iter()
            .map(|w| wheel::decode(&after, w.trigger, w.rel).0)
            .max_by_key(|n| n.abs())
            .unwrap_or(0);
        Some(RanOn {
            secs: after.last().map_or(0.0, |m| m.t as f64 * 1e-6 - at),
            ticks,
            controls,
        })
    }

    /// The right side's findings, if its few steps heard just what the left
    /// side's did: the same messages on a channel of its own, counting the
    /// same way. None while they haven't all been done, or once it has been
    /// asked for the rest.
    fn like_left(&self) -> Option<SideFindings> {
        if self.steps.iter().filter(|s| s.side == 1).count() != Do::SHORT.len() || !self.done() {
            return None;
        }
        let left = self.full(0);
        if !left.clear() {
            return None;
        }
        let find = |what| self.find(1, what);
        let (rest, cw, edge) = (find(Do::Touch)?, find(Do::PlatterCw)?, find(Do::EdgeCw)?);
        let touch = match left.touch {
            Touch::Missing if self.pressed(rest).is_empty() => Touch::Missing,
            Touch::Works(t) => {
                let t = self
                    .pressed(rest)
                    .into_iter()
                    .find(|x| (x.kind, x.number) == (t.kind, t.number))?;
                let (a, b) = (self.cover(rest, t), self.cover(cw, t));
                if !(a.1 && a.2 && b.1 && b.2 && b.0 >= 0.5) {
                    return None;
                }
                // The edge does what it does on the left.
                let c = self.cover(edge, t);
                if left.edge_touches != Some(c.1 && c.0 >= 0.5) {
                    return None;
                }
                Touch::Works(t)
            }
            _ => return None,
        };
        // The same controls on another channel, turned the way the left's
        // were, and nothing else that moved as much.
        let each = |all: &[Wheel], i: usize| -> Option<Vec<Wheel>> {
            let msgs = self.by_hand(i, touch);
            let mut out: Vec<Wheel> = Vec::new();
            for w in all {
                let channel = msgs
                    .iter()
                    .find(|m| m.trigger.number == w.trigger.number)?
                    .trigger
                    .channel;
                let trigger = Trigger {
                    channel,
                    ..w.trigger
                };
                let (net, a, b) = wheel::decode(&msgs, trigger, w.rel);
                let one_way = a + b >= TURN_MESSAGES && a.max(b) * 20 >= (a + b) * 17;
                if !one_way || net.signum() != w.ticks.signum() {
                    return None;
                }
                out.push(Wheel {
                    trigger,
                    ticks: net,
                    n: a + b,
                    ..*w
                });
            }
            let own = |m: &&Msg| {
                out.iter().any(|w| {
                    w.trigger == m.trigger
                        || (w.trigger.channel == m.trigger.channel
                            && w.rel
                                == RelMode::Absolute14 {
                                    fine: m.trigger.number,
                                })
                })
            };
            let n = msgs.iter().filter(own).count();
            ((msgs.len() - n) * 2 < n.max(1)).then_some(out)
        };
        let platter_all = each(&left.platter_all, cw)?;
        let edge_all = each(&left.edge_all, edge)?;
        let of = |w: Option<Wheel>, all: &[Wheel]| {
            w.and_then(|w| {
                all.iter()
                    .copied()
                    .find(|x| x.trigger.number == w.trigger.number)
            })
        };
        let turn = left
            .turn
            .iter()
            .filter_map(|(w, n)| of(Some(*w), &platter_all).map(|w| (w, *n)))
            .collect();
        Some(SideFindings {
            side: 1,
            heard: true,
            touch,
            platter: of(left.platter, &platter_all),
            edge: of(left.edge, &edge_all),
            platter_all,
            edge_all,
            turn,
            ran_on: None,
            like_left: true,
            ..left
        })
    }

    /// The decks a side's jog wheel works: both, where they take turns on
    /// the side's controls; otherwise the one whose controls are on the
    /// wheel's channel, or the side's first.
    fn decks(&self, f: &SideFindings, mapping: &Mapping) -> (Vec<u8>, bool) {
        let on_side = |b: &&Binding| {
            b.target
                .deck()
                .is_some_and(|d| side_of(d) == f.side as usize)
        };
        if mapping.bindings.iter().filter(on_side).any(|b| b.layered) {
            return (vec![f.side, f.side + 2], true);
        }
        let channel = f.triggers().first().map(|t| t.channel);
        let on = |deck: u8| {
            mapping.bindings.iter().any(|b| {
                Some(b.trigger.channel) == channel
                    && matches!(b.target, Target::Play { deck: d } if d == deck)
            })
        };
        let deck = if on(f.side + 2) && !on(f.side) {
            f.side + 2
        } else {
            f.side
        };
        (vec![deck], false)
    }

    /// What's wrong with `mapping` for a side, in plain words: nothing, if it
    /// does what the wheel needs.
    pub fn problems(&self, f: &SideFindings, mapping: &Mapping) -> Vec<String> {
        if !f.clear() {
            return Vec::new();
        }
        let (decks, layered) = self.decks(f, mapping);
        let preset = builtin_preset(&mapping.device);
        // Each, with the decks it's true of.
        let mut found: Vec<(String, Vec<u8>)> = Vec::new();
        for &deck in &decks {
            let mut say = |s: String| match found.iter_mut().find(|x| x.0 == s) {
                Some(x) if x.1.contains(&deck) => {}
                Some(x) => x.1.push(deck),
                None => found.push((s, vec![deck])),
            };
            let wanted = f.wanted(deck, layered);
            let touch = wanted
                .iter()
                .any(|b| matches!(b.target, Target::JogTouch { .. }));
            for w in &wanted {
                let what = match w.target {
                    Target::JogTouch { .. } => "the touch sensor",
                    Target::JogEdge { .. } => "turning it by the edge",
                    _ => "turning it",
                };
                let on: Vec<&Binding> = mapping
                    .bindings
                    .iter()
                    .filter(|b| b.trigger == w.trigger && !b.shift)
                    .filter(|b| b.target.deck() == Some(deck) || !b.layered)
                    .collect();
                // With a touch sensor to go by, one message for both does
                // for a platter or an edge as well.
                let does = |b: &&Binding| {
                    b.target == w.target
                        || (touch && w.target.is_jog_turn() && b.target == Target::Jog { deck })
                };
                let Some(b) = on.iter().find(|b| does(b)) else {
                    // (Mapped to another control: said below.)
                    let elsewhere = matches!(w.target, Target::JogTouch { .. })
                        && mapping.binding_for(w.target).is_some();
                    match on.first() {
                        Some(b) => say(format!("{what} is mapped to {}.", b.target.label())),
                        None if elsewhere => {}
                        None => say(format!("{what} isn't mapped.")),
                    }
                    continue;
                };
                if w.target.is_jog_turn() && b.rel != w.rel {
                    say(format!(
                        "{what} is read wrongly (as a wheel that {}, and it {}), so it goes the \
                         wrong way, or much too far.",
                        how(b.rel),
                        how(w.rel)
                    ));
                } else if w.target.is_jog_turn() && b.invert != w.invert {
                    say(format!("{what} runs backwards."));
                }
                let wheels = |x: &&&Binding| {
                    w.target.is_jog_turn()
                        && (x.target.is_jog_turn() || matches!(x.target, Target::JogTouch { .. }))
                };
                if let Some(x) = on.iter().find(|x| !does(x) && !wheels(x)) {
                    say(format!(
                        "{what} also does something else: {}.",
                        x.target.label()
                    ));
                }
            }
            // Bound where the wheel sends nothing, or a sensor that can't be
            // used.
            let triggers = f.triggers();
            for b in &mapping.bindings {
                if b.target.deck() != Some(deck)
                    || b.shift
                    || wanted
                        .iter()
                        .any(|w| w.trigger == b.trigger && w.target == b.target)
                {
                    continue;
                }
                let touch_bound = matches!(b.target, Target::JogTouch { .. });
                if touch_bound && Some(b.trigger) == f.touch.trigger() {
                    say("the touch sensor is mapped, and it can't be used here.".into());
                } else if touch_bound {
                    say(if triggers.contains(&b.trigger) {
                        "the touch sensor is mapped to what turning the wheel sends.".into()
                    } else {
                        "the touch sensor is mapped to something that isn't it.".into()
                    });
                } else if b.target.is_jog_turn()
                    && triggers.contains(&b.trigger)
                    && !wanted.iter().any(|w| w.trigger == b.trigger)
                    && !f
                        .platter_all
                        .iter()
                        .chain(&f.edge_all)
                        .any(|w| w.trigger == b.trigger && w.rel == b.rel)
                {
                    say("turning it is mapped a second time, and read wrongly there.".into());
                } else if b.target.is_jog_turn()
                    && !triggers.contains(&b.trigger)
                    && preset.as_ref().is_some_and(|p| !has(p, b))
                {
                    say("turning it is also mapped to something the wheel didn't send.".into());
                }
            }
        }
        let mut out: Vec<String> = found
            .into_iter()
            .map(|(s, on)| {
                if on.len() == decks.len() {
                    return s;
                }
                let on: Vec<String> = on
                    .iter()
                    .map(|d| deck_letter(*d as usize).to_string())
                    .collect();
                format!("On deck {}, {s}", on.join(" and "))
            })
            .map(|s| s[..1].to_uppercase() + &s[1..])
            .collect();
        if let Some(n) = self.ticks(f, mapping) {
            let have = mapping.ticks_per_rev();
            if (n as f64 - have).abs() > n as f64 * 0.05 {
                out.push(format!(
                    "A turn is {n} ticks, and the mapping counts {have:.0}, so the record moves \
                     {:.1}× as far as your hand.",
                    n as f64 / have
                ));
            }
        }
        out
    }

    /// Ticks in a turn of what `mapping` goes by on this side: of the wheel's
    /// position, where that's bound and heard.
    fn ticks(&self, f: &SideFindings, mapping: &Mapping) -> Option<i64> {
        let bound = |w: &&Wheel| {
            mapping
                .bindings
                .iter()
                .any(|b| b.trigger == w.trigger && b.rel == w.rel && b.target.is_jog_turn())
        };
        let position = f
            .platter_all
            .iter()
            .filter(|w| w.rel.is_position())
            .find(bound);
        f.ticks(position.copied().or(f.platter))
    }

    /// The mapping put right for the sides that are clear: touch, platter and
    /// edge (or the one message for both), how they count, which way round,
    /// and the ticks in a turn. None if there's nothing to change. The rest
    /// of the mapping stays, but what else was on the wheel's own messages
    /// goes.
    pub fn fix(&self, mapping: &Mapping) -> Option<Fix> {
        let mut m = mapping.clone();
        let mut changed: Vec<String> = Vec::new();
        for side in 0..2u8 {
            let f = self.side(side);
            if !f.clear() || self.problems(&f, &m).is_empty() {
                continue;
            }
            let (decks, layered) = self.decks(&f, &m);
            let ticks_only = {
                let mut t = m.clone();
                t.jog_ticks_per_rev = self.ticks(&f, &m).map(|n| n as f32);
                self.problems(&f, &t).is_empty()
            };
            if !ticks_only {
                let preset = builtin_preset(&m.device);
                let triggers = f.triggers();
                let wanted: Vec<Binding> =
                    decks.iter().flat_map(|&d| f.wanted(d, layered)).collect();
                let before = m.bindings.clone();
                m.bindings.retain(|b| {
                    let deck = b.target.deck().is_some_and(|d| decks.contains(&d));
                    let jog = b.target.is_jog_turn() || matches!(b.target, Target::JogTouch { .. });
                    // (A message the built-in mapping has for it that this
                    // check didn't hear stays: a controller may have a mode
                    // for it.)
                    let known =
                        b.target.is_jog_turn() && preset.as_ref().is_some_and(|p| has(p, b));
                    b.shift || !(triggers.contains(&b.trigger) || (deck && jog && !known))
                });
                for w in wanted {
                    // What it had and keeps isn't a change.
                    if !before.contains(&w) {
                        let how = match w.target {
                            Target::JogTouch { .. } => String::new(),
                            _ if w.invert => format!(" ({}, backwards)", w.rel.label()),
                            _ => format!(" ({})", w.rel.label()),
                        };
                        changed.push(format!("{} -> {}{how}", w.label(), w.target.label()));
                    }
                    m.bindings.push(w);
                }
                for b in before.iter().filter(|b| !m.bindings.contains(b)) {
                    let again = m
                        .bindings
                        .iter()
                        .any(|x| x.trigger == b.trigger && x.target == b.target);
                    if !again {
                        changed.push(format!("{} -> no longer {}", b.label(), b.target.label()));
                    }
                }
            }
            if let Some(n) = self.ticks(&f, &m) {
                if (n as f64 - m.ticks_per_rev()).abs() > n as f64 * 0.05 {
                    changed.push(format!(
                        "Jog: {n} ticks per turn, instead of {:.0}",
                        m.ticks_per_rev()
                    ));
                    m.jog_ticks_per_rev = Some(n as f32);
                }
            }
        }
        (!changed.is_empty()).then_some(Fix {
            mapping: m,
            changed,
        })
    }

    /// What it found on a side, in a sentence or two for the DJ.
    fn verdict(&self, f: &SideFindings, mapping: &Mapping) -> String {
        let name = f.name();
        if !f.heard {
            return format!(
                "{name}: nothing was heard. Is the controller in computer mode, and picked in \
                 Settings > Controller?"
            );
        }
        if !f.unclear.is_empty() {
            return format!("{name}: not clear yet. {}", f.unclear.join(" "));
        }
        let place = [f.platter, f.edge]
            .iter()
            .flatten()
            .any(|w| w.rel.is_position());
        let place = if place {
            " It says where the wheel is, instead of how far it turned."
        } else {
            ""
        };
        let mut s = match (f.platter, f.edge, f.one_message()) {
            (None, None, _) => {
                return match f.touch {
                    Touch::Works(_) => format!(
                        "{name}: the touch sensor works, but turning the wheel sends nothing Tandem \
                         can see."
                    ),
                    _ => format!("{name}: turning it sends nothing Tandem can see."),
                }
            }
            (Some(_), Some(_), true) => match (f.touch, f.edge_touches) {
                (Touch::Works(_), Some(true)) => format!(
                    "{name}: the top and the edge send the same message, and holding the edge sets \
                     off the touch sensor too. Tandem can't tell the two apart, so turning the edge \
                     scratches as well: to nudge with it, untick Vinyl mode."
                ),
                (Touch::Works(_), _) => format!(
                    "{name}: the top and the edge send the same message, and the touch sensor tells \
                     them apart."
                ),
                (Touch::Missing, _) => format!(
                    "{name}: the top and the edge send the same message, and there's no touch \
                     sensor to tell them apart. So it can nudge and search, but not scratch."
                ),
                _ => format!(
                    "{name}: the top and the edge send the same message, and the touch sensor can't \
                     be used to tell them apart ({}). So it can nudge and search, but not scratch.",
                    touch_fault(f.touch)
                ),
            },
            (Some(_), Some(_), false) => match (f.touch, f.edge_touches) {
                (Touch::Works(_), Some(true)) => format!(
                    "{name}: the top and the edge send different messages. Holding the edge sets \
                     off the touch sensor too, so Tandem goes by the messages alone: turning the \
                     top takes hold of the record."
                ),
                (Touch::Works(_), _) => format!(
                    "{name}: the top and the edge send different messages, and the touch sensor \
                     works. Turning the top scratches, and turning the edge nudges."
                ),
                (Touch::Missing, _) => format!(
                    "{name}: the top and the edge send different messages, and there's no touch \
                     sensor. Turning the top takes hold of the record, and turning the edge nudges."
                ),
                _ => format!(
                    "{name}: the top and the edge send different messages. The touch sensor can't \
                     be used ({}), so turning the top is what takes hold of the record.",
                    touch_fault(f.touch)
                ),
            },
            (Some(_), None, _) => format!(
                "{name}: turning it with a hand on top works, but by the edge it sends nothing \
                 Tandem can see."
            ),
            (None, Some(_), _) => format!(
                "{name}: turning it by the edge works, but with a hand on top it sends nothing \
                 Tandem can see."
            ),
        };
        s.push_str(place);
        let problems = self.problems(f, mapping);
        if problems.is_empty() {
            s.push_str(" Tandem's mapping has it right.");
        } else {
            s.push_str(&format!(
                " Tandem's mapping doesn't fit. {}",
                problems.join(" ")
            ));
        }
        if !mapping.vinyl && f.touch_used().is_some() {
            s.push_str(
                " (Vinyl mode is off in Settings, so a hand on top doesn't hold the record.)",
            );
        }
        s
    }

    /// What it found, for the DJ.
    pub fn summary(&self, mapping: &Mapping) -> Vec<String> {
        let (left, right) = (self.side(0), self.side(1));
        let mut out = vec![self.verdict(&left, mapping)];
        if right.like_left
            && self.problems(&left, mapping).is_empty() == self.problems(&right, mapping).is_empty()
        {
            out.push("The right jog wheel sends the same as the left, on its own channel.".into());
        } else {
            out.push(self.verdict(&right, mapping));
        }
        out
    }

    /// What the DJ should do next. `fixed`: "Fix the mapping" has been
    /// clicked.
    pub fn next_step(&self, mapping: &Mapping, fixed: bool) -> &'static str {
        let sides = [self.side(0), self.side(1)];
        let unclear = sides.iter().any(|f| !f.unclear.is_empty());
        if fixed {
            "Try the jog wheels. Copy the report and send it to us too, so the built-in mapping can \
             do the same."
        } else if self.fix(mapping).is_some() && unclear {
            "Click \"Fix the mapping\" below for the side that's clear, or \"Do it again\" for both. \
             Copy the report and send it to us too."
        } else if self.fix(mapping).is_some() {
            "Click \"Fix the mapping\" below, then try the jog wheels. Copy the report and send it \
             to us too, so the built-in mapping can do the same."
        } else if unclear {
            "Click \"Do it again\": it isn't clear yet."
        } else {
            "Copy the report and send it to us."
        }
    }

    /// What it heard, side by side, for us.
    pub fn details(&self, mapping: &Mapping) -> Vec<String> {
        let mut out = Vec::new();
        for side in 0..2u8 {
            let f = self.side(side);
            let name = f.name();
            if f.like_left {
                out.push(format!(
                    "{name}: the same as the left one in its short version; what it wasn't asked \
                     for is taken from the left."
                ));
            }
            out.push(format!(
                "{name}, touch: {}.",
                match f.touch {
                    Touch::Missing => "nothing came".into(),
                    Touch::Unclear => "not clear".into(),
                    Touch::Works(t) => format!("{}, let go with the hand", t.label()),
                    Touch::Stuck(t) => format!("{}, never let go", t.label()),
                    Touch::Flaky(t) =>
                        format!("{}, comes and goes with the hand on top", t.label()),
                }
            ));
            for (what, all) in [("platter", &f.platter_all), ("edge", &f.edge_all)] {
                if all.is_empty() {
                    out.push(format!("{name}, {what}: nothing read as a wheel."));
                }
                for w in all {
                    out.push(format!(
                        "{name}, {what}: {} {}, {} in {} messages at the clockwise step.",
                        w.trigger.label(),
                        w.rel.label(),
                        w.ticks,
                        w.n
                    ));
                }
            }
            if let Some(yes) = f.edge_touches {
                out.push(format!(
                    "{name}: holding the edge {} the touch sensor.",
                    if yes { "sets off" } else { "doesn't set off" }
                ));
            }
            for (w, n) in &f.turn {
                out.push(format!(
                    "{name}: one turn is {n} ticks of {}.",
                    w.trigger.label()
                ));
            }
            if let Some(r) = &f.ran_on {
                let on: Vec<String> = r.controls.iter().map(|t| t.label()).collect();
                out.push(if r.controls.is_empty() {
                    format!("{name}: let go, it sent nothing more.")
                } else {
                    format!(
                        "{name}: let go, it ran on for {:.2} s and {} ticks, on {}.",
                        r.secs,
                        r.ticks,
                        on.join(" and ")
                    )
                });
            }
            let mut channels: Vec<u8> = f.triggers().iter().map(|t| t.channel + 1).collect();
            channels.sort_unstable();
            channels.dedup();
            if channels.len() > 1 {
                out.push(format!(
                    "{name}: its messages come on more than one MIDI channel ({}).",
                    channels
                        .iter()
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            for u in &f.unclear {
                out.push(format!("{name}, not clear: {u}"));
            }
            for p in self.problems(&f, mapping) {
                out.push(format!("{name}, mapping: {p}"));
            }
        }
        out
    }

    /// The whole check as text: the mapping's jog wheels, what it found, then
    /// every step with what it heard.
    pub fn report(&self, info: &ReportInfo, mapping: &Mapping) -> String {
        let mut r = String::new();
        r.push_str("Tandem jog wheel check\n");
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
        r.push_str(&format!(
            "Jog wheels in the mapping: {} ticks per turn{}, vinyl mode {}, scratch speed {}, ring {}\n",
            mapping.ticks_per_rev(),
            if mapping.jog_ticks_per_rev.is_none() {
                " (not measured)"
            } else {
                ""
            },
            if mapping.vinyl { "on" } else { "off" },
            mapping.scratch_sensitivity,
            mapping.jog_sensitivity
        ));
        for line in jog_bindings(mapping) {
            r.push_str(&format!("  {line}\n"));
        }
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
        r.push_str(
            "\nStep by step (seconds, then the messages: \"B4 36=01\" is status B4, number 36, value \
             01, and \"02\" after it the same control again)\n",
        );
        for (i, step) in self.steps.iter().enumerate().take(self.at + 1) {
            r.push_str(&format!("{:2}. {}\n", i + 1, step.instruction()));
            let heard = self.heard(i);
            if heard.is_empty() {
                r.push_str("      (nothing)\n");
            }
            compact(&mut r, &heard[..heard.len().min(REPORT_MESSAGES)]);
            if heard.len() > REPORT_MESSAGES {
                r.push_str(&format!(
                    "      … and {} more messages\n",
                    heard.len() - REPORT_MESSAGES
                ));
            }
        }
        r
    }
}

/// Whether a mapping turns a deck's jog wheel by the message `b` does.
fn has(m: &Mapping, b: &Binding) -> bool {
    m.bindings
        .iter()
        .any(|x| (x.trigger, x.target, x.rel) == (b.trigger, b.target, b.rel))
}

/// How a wheel tells how far it turned, in words.
fn how(rel: RelMode) -> &'static str {
    match rel {
        RelMode::Offset64 => "counts around 64",
        RelMode::TwosComplement => "counts 1 and 127",
        RelMode::Absolute | RelMode::Absolute14 { .. } => "says where it is",
    }
}

fn touch_fault(t: Touch) -> &'static str {
    match t {
        Touch::Stuck(_) => "it never says your hand has gone",
        Touch::Flaky(_) => "it comes and goes while your hand stays on top",
        _ => "it isn't clear",
    }
}

/// The mapping's jog wheel bindings, for the report: "Deck A · Jog wheel,
/// both (touch decides) <- CC 0x36 · ch 5, counts 1 and 127".
fn jog_bindings(mapping: &Mapping) -> Vec<String> {
    let mut own: Vec<&Binding> = mapping
        .bindings
        .iter()
        .filter(|b| b.target.is_jog_turn() || matches!(b.target, Target::JogTouch { .. }))
        .collect();
    own.sort_by_key(|b| b.target.deck());
    if own.is_empty() {
        return vec!["(none)".into()];
    }
    own.iter()
        .map(|b| {
            let mut s = format!("{} <- {}", b.target.label(), b.label());
            if b.target.is_jog_turn() {
                s.push_str(&format!(", {}", b.rel.label()));
            }
            if b.invert {
                s.push_str(", inverted");
            }
            s
        })
        .collect()
}

/// A step's messages, 16 to a line.
fn compact(r: &mut String, heard: &[Heard]) {
    for line in heard.chunks(16) {
        r.push_str(&format!("    {:8.3} ", line[0].t));
        let mut last: Option<(u8, u8)> = None;
        for h in line {
            match h.bytes[..] {
                [status, number, value] if status < 0xF0 => {
                    if last == Some((status, number)) {
                        r.push_str(&format!(" {value:02X}"));
                    } else {
                        r.push_str(&format!("  {status:02X} {number:02X}={value:02X}"));
                    }
                    last = Some((status, number));
                }
                _ => {
                    let hex: Vec<String> = h.bytes.iter().map(|b| format!("{b:02X}")).collect();
                    r.push_str(&format!("  [{}]", hex.join(" ")));
                    last = None;
                }
            }
        }
        r.push('\n');
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

/// A pretend controller's jog wheels, for the tests and the app's
/// `--dev-jog-check`. Both sides are alike, each on a MIDI channel of its own,
/// but for [`Pretend::Mixed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pretend {
    /// A Denon SC Live 4 as Traktor's mapping has it: a touch sensor (note
    /// 0x21) and one message (CC 0x36) that counts 1 and 127.
    Counts,
    /// The same as Mixxx's mapping has it: one 14-bit position (CCs 0x11 and
    /// 0x31).
    Position,
    /// Both of those messages at once.
    Both,
    /// A platter and an edge with a message each (CCs 0x22 and 0x21, around
    /// 64), and a touch sensor (note 0x36), as on a DDJ-FLX6.
    Split,
    /// The same without a touch sensor.
    SplitNoTouch,
    /// The same, whose edge sets off the touch sensor.
    SplitEdgeTouches,
    /// [`Pretend::Counts`] without a touch sensor.
    NoTouch,
    /// Its touch sensor never says the hand has gone.
    Stuck,
    /// Its touch sensor comes and goes while the hand stays on top.
    Flaky,
    /// Its edge sets off the touch sensor (a metal wheel).
    EdgeTouches,
    /// The left like [`Pretend::Counts`], the right like
    /// [`Pretend::Position`].
    Mixed,
    /// [`Pretend::Counts`], turned by a DJ who goes the wrong way by the edge.
    WrongWay,
}

/// A [`Pretend`] controller being used.
#[derive(Clone, Debug)]
pub struct PretendDj {
    pub model: Pretend,
    /// The left side's MIDI channel (from 0); the right side's is the next.
    pub channel: u8,
    /// Where each wheel is, for one that says so.
    place: [i32; 2],
}

impl PretendDj {
    pub fn new(model: Pretend) -> Self {
        Self {
            model,
            channel: 4,
            place: [5_000, 9_000],
        }
    }

    /// What its controls are called, by name (for `--dev-jog-check`).
    pub fn named(name: &str) -> Option<Pretend> {
        Some(match name {
            "counts" => Pretend::Counts,
            "position" => Pretend::Position,
            "both" => Pretend::Both,
            "split" => Pretend::Split,
            "split-notouch" => Pretend::SplitNoTouch,
            "split-edge" => Pretend::SplitEdgeTouches,
            "notouch" => Pretend::NoTouch,
            "stuck" => Pretend::Stuck,
            "flaky" => Pretend::Flaky,
            "edge" => Pretend::EdgeTouches,
            "mixed" => Pretend::Mixed,
            "wrongway" => Pretend::WrongWay,
            _ => return None,
        })
    }

    /// What it sends for a step, with how long (s) after the message before.
    pub fn messages(&mut self, step: Step) -> Vec<(f64, Vec<u8>)> {
        use Pretend::*;
        let side = step.side as usize;
        let ch = self.channel + step.side;
        let model = match (self.model, step.side) {
            (Mixed, 0) => Counts,
            (Mixed, _) => Position,
            (m, _) => m,
        };
        let split = matches!(model, Split | SplitNoTouch | SplitEdgeTouches);
        let per_turn = if split { 2048 } else { 1000 };
        let note = if split { 0x36 } else { 0x21 };
        let senses = !matches!(model, SplitNoTouch | NoTouch)
            && (!step.what.edge() || matches!(model, SplitEdgeTouches | EdgeTouches));
        let mut out: Vec<(f64, Vec<u8>)> = Vec::new();
        let hand = |out: &mut Vec<(f64, Vec<u8>)>, dt: f64, on: bool| {
            if senses && (on || model != Stuck) {
                out.push((dt, vec![0x90 | ch, note, if on { 0x7F } else { 0 }]));
            }
        };
        let place = &mut self.place[side];
        let mut turn = |out: &mut Vec<(f64, Vec<u8>)>, dt: f64, ticks: i32, edge: bool| {
            *place = (*place + ticks).rem_euclid(1 << 14);
            let cc = 0xB0 | ch;
            let at = *place;
            let counts = vec![cc, 0x36, (ticks & 0x7F) as u8];
            let coarse = vec![cc, 0x11, (at >> 7) as u8];
            let fine = vec![cc, 0x31, (at & 0x7F) as u8];
            match model {
                Position => out.extend([(dt, coarse), (0.0003, fine)]),
                Both => out.extend([(dt, coarse), (0.0003, fine), (0.0003, counts)]),
                _ if split => {
                    let number = if edge { 0x21 } else { 0x22 };
                    out.push((dt, vec![cc, number, (64 + ticks) as u8]));
                }
                _ => out.push((dt, counts)),
            }
        };
        // `n` ticks in all, a few at a time, as a hand turns it.
        let mut by = |out: &mut Vec<(f64, Vec<u8>)>, n: i32, edge: bool, fast: i32| {
            let mut left = n.abs();
            let mut i = 0;
            while left > 0 {
                let t = (1 + (i % 5 == 3) as i32 * (fast - 1) + (i % 7 == 2) as i32).min(left);
                turn(out, 0.004, t * n.signum(), edge);
                left -= t;
                i += 1;
            }
        };
        let quarter = per_turn / 4;
        match step.what {
            Do::Touch => {
                hand(&mut out, 0.3, true);
                hand(&mut out, 1.6, false);
            }
            Do::PlatterCw | Do::PlatterCcw => {
                let way = if step.what == Do::PlatterCw { 1 } else { -1 };
                hand(&mut out, 0.3, true);
                if model == Flaky {
                    hand(&mut out, 0.01, false);
                }
                by(&mut out, way * quarter, false, 2);
                hand(&mut out, 0.4, false);
                // Lifting a hand flicks it on a little.
                by(&mut out, way * quarter / 5, true, 2);
            }
            Do::EdgeCw | Do::EdgeCcw => {
                let way = if step.what == Do::EdgeCw { 1 } else { -1 };
                let way = if model == WrongWay { -way } else { way };
                hand(&mut out, 0.3, true);
                by(&mut out, way * quarter, true, 2);
                hand(&mut out, 0.4, false);
            }
            Do::FullTurn => {
                hand(&mut out, 0.3, true);
                if model == Flaky {
                    hand(&mut out, 0.01, false);
                }
                by(&mut out, per_turn, false, 4);
                hand(&mut out, 0.4, false);
                by(&mut out, quarter / 5, true, 2);
            }
            Do::Flick => {
                hand(&mut out, 0.3, true);
                by(&mut out, -quarter, false, 9);
                hand(&mut out, 0.001, false);
                // It runs on by itself, more and more slowly.
                for i in 0..40 {
                    let t = (9 - i / 5).max(1);
                    turn(&mut out, 0.004 + i as f64 * 0.0005, -t, true);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{MidiEngine, MidiOut};
    use crate::model::{Action, RoomState, TrackId};
    use crate::render::LocalParams;

    /// The SC Live 4's built-in mapping, of decks that take turns, reading
    /// its jog wheels only as Traktor's mapping does.
    fn sc_live_4() -> Mapping {
        let mut m = builtin_preset("SC LIVE 4").unwrap();
        m.bindings.retain(|b| !b.rel.is_position());
        m
    }

    /// Goes through the check with a pretend controller, ticking as frames
    /// go by, until it's over or step `stop`.
    fn run_to(c: &mut JogCheck, model: Pretend, stop: usize) -> f64 {
        use_it(c, PretendDj::new(model), stop)
    }

    fn use_it(c: &mut JogCheck, mut dj: PretendDj, stop: usize) -> f64 {
        let mut t = 1.0;
        while let Some(step) = c.step().filter(|_| c.at < stop) {
            let at = c.at;
            for (dt, m) in dj.messages(step) {
                t += dt;
                c.hear(t, &m);
                assert!(!c.tick(t), "moved on mid-step: {step:?}");
            }
            // Frames go by.
            for _ in 0..40 {
                t += 0.1;
                c.tick(t);
                if c.at != at {
                    break;
                }
            }
            if c.at == at {
                // "That was one turn", or "go on".
                c.next();
            }
            t += 0.5;
        }
        t
    }

    fn run(mapping: &Mapping, model: Pretend) -> JogCheck {
        let mut c = JogCheck::new(mapping);
        run_to(&mut c, model, usize::MAX);
        assert!(c.done());
        c
    }

    fn info() -> ReportInfo {
        ReportInfo {
            version: "1.2.3".into(),
            port: "SC LIVE 4".into(),
            output: true,
            unix_secs: 1_795_500_000,
        }
    }

    /// The jog wheel bindings of a deck: (trigger, target, how it counts,
    /// inverted, takes turns).
    fn jogs(m: &Mapping, deck: u8) -> Vec<(Trigger, Target, RelMode, bool, bool)> {
        let mut v: Vec<_> = m
            .bindings
            .iter()
            .filter(|b| b.target.deck() == Some(deck))
            .filter(|b| b.target.is_jog_turn() || matches!(b.target, Target::JogTouch { .. }))
            .map(|b| (b.trigger, b.target, b.rel, b.invert, b.layered))
            .collect();
        v.sort_by_key(|x| (x.0.kind as u8, x.0.number));
        v
    }

    const TWOS: RelMode = RelMode::TwosComplement;
    const OFFS: RelMode = RelMode::Offset64;

    #[test]
    fn the_steps_and_what_they_say() {
        let c = JogCheck::new(&sc_live_4());
        let what: Vec<(u8, Do)> = c.steps.iter().map(|s| (s.side, s.what)).collect();
        assert_eq!(what.len(), 7 + 3);
        assert_eq!(what[0], (0, Do::Touch));
        assert_eq!(
            what[7..],
            [(1, Do::Touch), (1, Do::PlatterCw), (1, Do::EdgeCw)]
        );
        for s in &c.steps {
            let side = side_name(s.side);
            assert!(
                s.instruction().contains(side) && !s.hint().is_empty(),
                "{s:?}"
            );
            // Words a DJ uses.
            for word in ["MIDI", "CC", "note", "sensor"] {
                assert!(!s.instruction().contains(word), "{}", s.instruction());
            }
        }
    }

    /// The SC Live 4 as Traktor's mapping has it: one message and a touch
    /// sensor, which the built-in mapping fits.
    #[test]
    fn one_message_and_a_touch_sensor() {
        let m = sc_live_4();
        let c = run(&m, Pretend::Counts);
        // The right side sent the same: it was only asked for three steps.
        assert_eq!(c.steps.len(), 10);
        for side in 0..2u8 {
            let f = c.side(side);
            assert_eq!(f.unclear, [] as [String; 0], "side {side}");
            assert_eq!(f.touch, Touch::Works(Trigger::note(4 + side, 0x21)));
            assert!(f.one_message());
            let w = f.platter.unwrap();
            assert_eq!((w.trigger, w.rel), (Trigger::cc(4 + side, 0x36), TWOS));
            assert_eq!(f.edge_touches, Some(false));
            assert_eq!(f.ticks(f.platter), Some(1000));
            assert_eq!(f.like_left, side == 1);
        }
        let r = c.side(0).ran_on.unwrap();
        assert!(r.secs > 0.3 && r.ticks < -50, "{r:?}");
        let text = c.summary(&m).join("\n");
        assert!(
            text.contains("the top and the edge send the same message"),
            "{text}"
        );
        assert!(text.contains("the touch sensor tells them apart"), "{text}");
        assert!(text.contains("Tandem's mapping has it right"), "{text}");
        assert!(
            text.contains("The right jog wheel sends the same as the left"),
            "{text}"
        );
        assert!(c.fix(&m).is_none());
        assert_eq!(c.next_step(&m, false), "Copy the report and send it to us.");
        // No MIDI bytes in what the DJ reads.
        assert!(!text.contains("0x"), "{text}");
    }

    /// What its owner has: the wheel learned as counting around 64, so it
    /// goes the wrong way, much too far.
    #[test]
    fn a_wheel_learned_wrongly_is_found_and_put_right() {
        let mut m = sc_live_4();
        for b in m.bindings.iter_mut().filter(|b| b.target.is_jog_turn()) {
            b.rel = OFFS;
        }
        // And the left touch sensor learned from the wheel's turn.
        m.bindings
            .retain(|b| b.target != Target::JogTouch { deck: 0 });
        m.bind(Trigger::cc(4, 0x36), Target::JogTouch { deck: 0 })
            .layered = true;
        m.jog_ticks_per_rev = Some(720.0);
        let c = run(&m, Pretend::Counts);
        let text = c.summary(&m).join("\n");
        assert!(text.contains("doesn't fit"), "{text}");
        for want in [
            "Turning it is read wrongly (as a wheel that counts around 64, and it counts 1 and 127)",
            "On deck A, the touch sensor is mapped to what turning the wheel sends.",
            "A turn is 1000 ticks, and the mapping counts 720, so the record moves 1.4× as far",
        ] {
            assert!(text.contains(want), "{want:?} in {text}");
        }
        assert!(!text.contains("0x"), "{text}");
        // Once is enough.
        assert_eq!(text.matches("touch sensor").count(), 2, "{text}");
        assert!(c
            .next_step(&m, false)
            .starts_with("Click \"Fix the mapping\""));
        let fix = c.fix(&m).unwrap();
        assert_eq!(fix.mapping.jog_ticks_per_rev, Some(1000.0));
        // Both of each side's decks, taking turns.
        for deck in 0..4u8 {
            let ch = 4 + deck % 2;
            assert_eq!(
                jogs(&fix.mapping, deck),
                [
                    (
                        Trigger::note(ch, 0x21),
                        Target::JogTouch { deck },
                        OFFS,
                        false,
                        true
                    ),
                    (
                        Trigger::cc(ch, 0x36),
                        Target::Jog { deck },
                        TWOS,
                        false,
                        true
                    ),
                ],
                "deck {deck}"
            );
        }
        // Everything else is as it was, and nothing more to fix.
        let rest = |m: &Mapping| -> Vec<Binding> {
            m.bindings
                .iter()
                .filter(|b| !b.target.is_jog_turn() && !matches!(b.target, Target::JogTouch { .. }))
                .cloned()
                .collect()
        };
        assert_eq!(rest(&fix.mapping), rest(&m));
        assert!(c.fix(&fix.mapping).is_none());
        assert!(c.next_step(&m, true).starts_with("Try the jog wheels"));
        assert!(fix.changed.iter().any(|l| l.contains("CC 0x36 · ch 5 (side on deck A) -> Deck A · Jog wheel, both (touch decides) (counts 1 and 127)")), "{:?}", fix.changed);
        assert!(
            fix.changed
                .iter()
                .any(|l| l.contains("no longer Deck A · Jog wheel touch")),
            "{:?}",
            fix.changed
        );
        // And it plays: a hand on top holds the record, and turning it moves
        // it as far as the hand.
        let mut e = MidiEngine::new(fix.mapping, 0.08);
        let (mut room, local) = (RoomState::default(), LocalParams::default());
        room.decks[0].track = Some(TrackId([1; 32]));
        room.decks[0].playing = true;
        assert!(matches!(
            e.handle(&[0x94, 0x21, 0x7F], &room, &local)[..],
            [MidiOut::Action(Action::JogTouch {
                deck: 0,
                down: true,
                ..
            })]
        ));
        room.decks[0].scratching = true;
        for _ in 0..250 {
            assert!(e.handle(&[0xB4, 0x36, 0x01], &room, &local).is_empty());
        }
        let delta = match e.flush(None)[..] {
            [Action::Nudge { deck: 0, delta }] => delta,
            ref other => panic!("{other:?}"),
        };
        assert!((delta - 0.45).abs() < 1e-9, "{delta}");
    }

    #[test]
    fn the_report_says_what_the_mapping_had() {
        let mut m = sc_live_4();
        for b in m
            .bindings
            .iter_mut()
            .filter(|b| b.target == Target::Jog { deck: 0 })
        {
            (b.rel, b.invert) = (OFFS, true);
        }
        let c = run(&m, Pretend::Counts);
        let r = c.report(&info(), &m);
        for want in [
            "Tandem jog wheel check\nTandem 1.2.3 · 2026-11-24 06:00 UTC\nController: SC LIVE 4 (MIDI out: yes)",
            "Mapping: built-in Denon SC Live 4 (preset version 14, ",
            "Jog wheels in the mapping: 1000 ticks per turn, vinyl mode on, scratch speed 1, ring 1\n",
            "  Deck A · Jog wheel, both (touch decides) <- CC 0x36 · ch 5 (side on deck A), counts around 64, inverted\n",
            "  Deck B · Jog wheel touch (scratch) <- Note 0x21 · ch 6 (side on deck B)\n",
            "  Deck D · Jog wheel, both (touch decides) <- CC 0x36 · ch 6 (side on deck D), counts 1 and 127\n",
            " 1. Rest your hand flat on top of the left jog wheel",
            "  94 21=7F",
            "  B4 36=01 01 ",
            "10. Now hold the right jog wheel only by its edge",
            "one turn is 1000 ticks of CC 0x36 · ch 5",
            "let go, it ran on for",
        ] {
            assert!(r.contains(want), "{want:?} in {r}");
        }
        assert!(!r.contains("Stopped at step"));
        assert!(!r.contains("Click \""), "{r}");
        // Short enough to send: a few messages to the line.
        assert!(r.len() < 40_000, "{}", r.len());
        // Stopped half way, it says so and lists what there is.
        let mut c = JogCheck::new(&m);
        run_to(&mut c, Pretend::Counts, 3);
        let r = c.report(&info(), &m);
        assert!(r.contains("Stopped at step 4 of 10."), "{r}");
        // A mapping with no jog wheels says so.
        let r = c.report(&info(), &Mapping::empty("x"));
        assert!(
            r.contains("720 ticks per turn (not measured)") && r.contains("  (none)\n"),
            "{r}"
        );
    }

    /// The SC Live 4 as Mixxx's mapping has it.
    #[test]
    fn a_position_in_14_bits() {
        // The built-in mapping reads both, so it's right as it is.
        let preset = builtin_preset("SC LIVE 4").unwrap();
        let c = run(&preset, Pretend::Position);
        for side in 0..2u8 {
            let f = c.side(side);
            assert_eq!(f.unclear, [] as [String; 0], "side {side}");
            let w = f.platter.unwrap();
            assert_eq!(
                (w.trigger, w.rel),
                (
                    Trigger::cc(4 + side, 0x11),
                    RelMode::Absolute14 { fine: 0x31 }
                )
            );
            assert!(f.one_message() && w.ticks > 0);
            // (Its first message only says where it is.)
            let n = f.ticks(f.platter).unwrap();
            assert!((990..=1000).contains(&n), "{n}");
        }
        let text = c.summary(&preset).join("\n");
        assert!(
            text.contains("It says where the wheel is, instead of how far it turned"),
            "{text}"
        );
        assert!(text.contains("Tandem's mapping has it right"), "{text}");
        assert!(c.fix(&preset).is_none());
        // A mapping that only reads the other message is put right.
        let m = sc_live_4();
        let c = run(&m, Pretend::Position);
        assert!(
            c.summary(&m)[0].contains("isn't mapped"),
            "{:?}",
            c.summary(&m)
        );
        let fix = c.fix(&m).unwrap();
        for deck in 0..4u8 {
            let ch = 4 + deck % 2;
            let at = (
                Trigger::cc(ch, 0x11),
                Target::Jog { deck },
                RelMode::Absolute14 { fine: 0x31 },
                false,
                true,
            );
            assert!(
                jogs(&fix.mapping, deck).contains(&at),
                "{:?}",
                jogs(&fix.mapping, deck)
            );
        }
        assert!(c.fix(&fix.mapping).is_none());
        // So is its coarse half, learned as counting.
        let mut m = sc_live_4();
        m.bindings.retain(|b| b.target != Target::Jog { deck: 0 });
        m.bind(Trigger::cc(4, 0x11), Target::Jog { deck: 0 })
            .layered = true;
        let c = run(&m, Pretend::Position);
        let text = c.summary(&m).join("\n");
        assert!(text.contains("On deck A, turning it is read wrongly (as a wheel that counts around 64, and it says where it is)"), "{text}");
        assert!(c.fix(&c.fix(&m).unwrap().mapping).is_none());
    }

    /// A position with few ticks in a turn: a quarter turn doesn't move its
    /// coarse half, and a full turn does.
    #[test]
    fn a_coarse_position_is_read_from_the_full_turn() {
        let m = sc_live_4();
        let mut c = JogCheck::new(&m);
        let mut place = 5_000i32;
        let mut t = 1.0;
        while let Some(step) = c.step().filter(|s| s.side == 0) {
            let at = c.at;
            let hand = !step.what.edge();
            let ticks: i32 = match step.what {
                Do::Touch => 0,
                Do::PlatterCw | Do::EdgeCw => 50,
                Do::PlatterCcw | Do::EdgeCcw => -50,
                Do::FullTurn => 200,
                Do::Flick => -40,
            };
            if hand {
                c.hear(t, &[0x94, 0x21, 0x7F]);
            }
            for _ in 0..ticks.abs() {
                t += 0.01;
                place += ticks.signum();
                c.hear(t, &[0xB4, 0x11, (place >> 7) as u8]);
                c.hear(t + 0.0003, &[0xB4, 0x31, (place & 0x7F) as u8]);
            }
            t += 0.5;
            if hand {
                c.hear(t, &[0x94, 0x21, 0x00]);
            }
            t += 5.0;
            if !c.tick(t) && c.at == at {
                c.next();
            }
        }
        let f = c.side(0);
        assert_eq!(f.unclear, [] as [String; 0]);
        assert_eq!(
            f.platter.map(|w| (w.trigger, w.rel)),
            Some((Trigger::cc(4, 0x11), RelMode::Absolute14 { fine: 0x31 }))
        );
        assert!(f.one_message());
        assert_eq!(f.ticks(f.platter), Some(199));
    }

    #[test]
    fn a_controller_that_sends_both_is_bound_once() {
        // Bound to both, the built-in mapping moves the deck once: right.
        let preset = builtin_preset("SC LIVE 4").unwrap();
        let c = run(&preset, Pretend::Both);
        let f = c.side(0);
        assert_eq!(f.platter_all.len(), 2, "{:?}", f.platter_all);
        assert_eq!(f.unclear, [] as [String; 0]);
        assert!(
            c.fix(&preset).is_none(),
            "{:?}",
            c.fix(&preset).unwrap().changed
        );
        // Bound to nothing, it gets the one that counts.
        let mut m = preset.clone();
        m.bindings.retain(|b| !b.target.is_jog_turn());
        let fix = run(&m, Pretend::Both).fix(&m).unwrap();
        assert_eq!(
            jogs(&fix.mapping, 0)
                .iter()
                .filter(|b| b.1.is_jog_turn())
                .map(|b| (b.0, b.2))
                .collect::<Vec<_>>(),
            [(Trigger::cc(4, 0x36), TWOS)]
        );
    }

    /// A platter and an edge with a message each, as on the DDJ-FLX6: mapped
    /// from nothing, on a controller Tandem knows nothing about.
    #[test]
    fn a_platter_and_an_edge_of_their_own() {
        let mut m = Mapping::empty("something");
        m.bind(Trigger::note(4, 0x0B), Target::Play { deck: 0 });
        m.bind(Trigger::note(5, 0x0B), Target::Play { deck: 1 });
        let c = run(&m, Pretend::Split);
        let f = c.side(0);
        assert_eq!(f.unclear, [] as [String; 0]);
        assert!(!f.one_message());
        assert_eq!(
            f.platter.map(|w| (w.trigger, w.rel)),
            Some((Trigger::cc(4, 0x22), OFFS))
        );
        assert_eq!(
            f.edge.map(|w| (w.trigger, w.rel)),
            Some((Trigger::cc(4, 0x21), OFFS))
        );
        assert_eq!(f.ticks(f.platter), Some(2048));
        // After the flick, it's the edge that runs on.
        assert_eq!(f.ran_on.as_ref().unwrap().controls, [Trigger::cc(4, 0x21)]);
        let text = c.summary(&m).join("\n");
        assert!(
            text.contains("the top and the edge send different messages"),
            "{text}"
        );
        assert!(text.contains("isn't mapped"), "{text}");
        let fix = c.fix(&m).unwrap();
        for deck in 0..2u8 {
            let ch = 4 + deck;
            assert_eq!(
                jogs(&fix.mapping, deck),
                [
                    (
                        Trigger::note(ch, 0x36),
                        Target::JogTouch { deck },
                        OFFS,
                        false,
                        false
                    ),
                    (
                        Trigger::cc(ch, 0x21),
                        Target::JogEdge { deck },
                        OFFS,
                        false,
                        false
                    ),
                    (
                        Trigger::cc(ch, 0x22),
                        Target::JogPlatter { deck },
                        OFFS,
                        false,
                        false
                    ),
                ]
            );
        }
        assert_eq!(fix.mapping.jog_ticks_per_rev, Some(2048.0));
        assert!(c.fix(&fix.mapping).is_none());
        // One message for both, on both of them, works as well with a touch
        // sensor: it isn't called wrong.
        let mut old = fix.mapping.clone();
        for b in old.bindings.iter_mut().filter(|b| b.target.is_jog_turn()) {
            b.target = Target::Jog {
                deck: b.target.deck().unwrap(),
            };
        }
        assert!(run(&old, Pretend::Split).fix(&old).is_none());
    }

    #[test]
    fn without_a_touch_sensor() {
        // Two messages: the platter takes hold by turning.
        let m = Mapping::empty("something");
        let c = run(&m, Pretend::SplitNoTouch);
        let f = c.side(0);
        assert_eq!(
            (f.touch, f.unclear.len()),
            (Touch::Missing, 0),
            "{:?}",
            f.unclear
        );
        assert_eq!(f.ran_on, None);
        let text = c.summary(&m).join("\n");
        assert!(
            text.contains("there's no touch sensor. Turning the top takes hold of the record"),
            "{text}"
        );
        let fix = c.fix(&m).unwrap();
        assert_eq!(
            jogs(&fix.mapping, 1)
                .iter()
                .map(|b| b.1)
                .collect::<Vec<_>>(),
            [Target::JogEdge { deck: 1 }, Target::JogPlatter { deck: 1 }]
        );
        // One message: it can only nudge, and it's said.
        let c = run(&m, Pretend::NoTouch);
        let text = c.summary(&m).join("\n");
        assert!(
            text.contains("it can nudge and search, but not scratch"),
            "{text}"
        );
        let fix = c.fix(&m).unwrap();
        assert_eq!(
            jogs(&fix.mapping, 0)
                .iter()
                .map(|b| b.1)
                .collect::<Vec<_>>(),
            [Target::Jog { deck: 0 }]
        );
        // A touch sensor in the mapping that the wheel hasn't got goes.
        let m = sc_live_4();
        let c = run(&m, Pretend::NoTouch);
        let fix = c.fix(&m).unwrap();
        for deck in 0..4u8 {
            assert!(jogs(&fix.mapping, deck)
                .iter()
                .all(|b| b.1 == Target::Jog { deck }));
        }
    }

    #[test]
    fn a_touch_sensor_that_never_lets_go_isnt_used() {
        let m = sc_live_4();
        let c = run(&m, Pretend::Stuck);
        let f = c.side(0);
        assert_eq!(f.touch, Touch::Stuck(Trigger::note(4, 0x21)));
        assert_eq!(f.unclear, [] as [String; 0]);
        let text = c.summary(&m).join("\n");
        assert!(text.contains("it never says your hand has gone"), "{text}");
        assert!(text.contains("it can't be used here"), "{text}");
        let fix = c.fix(&m).unwrap();
        assert!(jogs(&fix.mapping, 0)
            .iter()
            .all(|b| b.1 == Target::Jog { deck: 0 }));
        assert!(c.fix(&fix.mapping).is_none());
    }

    #[test]
    fn a_touch_sensor_that_comes_and_goes_isnt_used() {
        let m = sc_live_4();
        let c = run(&m, Pretend::Flaky);
        assert_eq!(c.side(0).touch, Touch::Flaky(Trigger::note(4, 0x21)));
        let text = c.summary(&m).join("\n");
        assert!(
            text.contains("it comes and goes while your hand stays on top"),
            "{text}"
        );
        assert!(c
            .fix(&m)
            .unwrap()
            .mapping
            .binding_for(Target::JogTouch { deck: 0 })
            .is_none());
        // At one step of three, it's the hand that isn't clear.
        let mut c = JogCheck::new(&m);
        run_to(&mut c, Pretend::Counts, 2);
        let t = 100.0;
        c.hear(t, &[0x94, 0x21, 0x7F]);
        c.hear(t + 0.1, &[0x94, 0x21, 0x00]);
        for i in 0..250 {
            c.hear(t + 0.2 + i as f64 * 0.004, &[0xB4, 0x36, 127]);
        }
        assert!(c.tick(t + 5.0));
        run_to(&mut c, Pretend::Counts, usize::MAX);
        let f = c.side(0);
        assert_eq!(f.touch, Touch::Unclear);
        assert!(
            f.unclear[0].contains("only part of some turns"),
            "{:?}",
            f.unclear
        );
        assert!(!f.clear());
    }

    #[test]
    fn an_edge_that_sets_off_the_touch_sensor() {
        // One message for both: nothing can tell them apart, and it's said.
        let m = sc_live_4();
        let c = run(&m, Pretend::EdgeTouches);
        let f = c.side(0);
        assert_eq!(
            (f.edge_touches, f.unclear.len()),
            (Some(true), 0),
            "{:?}",
            f.unclear
        );
        let text = c.summary(&m).join("\n");
        assert!(
            text.contains("turning the edge scratches as well"),
            "{text}"
        );
        assert!(c.fix(&m).is_none());
        // A message each: the touch sensor is left out, so nudging by the
        // edge doesn't hold the record.
        let mut m = Mapping::empty("something");
        m.bind(Trigger::note(4, 0x36), Target::JogTouch { deck: 0 });
        let c = run(&m, Pretend::SplitEdgeTouches);
        let text = c.summary(&m).join("\n");
        assert!(text.contains("Tandem goes by the messages alone"), "{text}");
        let fix = c.fix(&m).unwrap();
        assert_eq!(
            jogs(&fix.mapping, 0)
                .iter()
                .map(|b| b.1)
                .collect::<Vec<_>>(),
            [Target::JogEdge { deck: 0 }, Target::JogPlatter { deck: 0 }]
        );
        assert!(c.fix(&fix.mapping).is_none());
    }

    #[test]
    fn the_right_side_is_asked_for_everything_when_it_isnt_like_the_left() {
        let m = Mapping::empty("something");
        let c = run(&m, Pretend::Mixed);
        assert_eq!(c.steps.len(), 14);
        let what: Vec<Do> = c.steps[7..].iter().map(|s| s.what).collect();
        assert_eq!(
            what,
            [
                Do::Touch,
                Do::PlatterCw,
                Do::EdgeCw,
                Do::PlatterCcw,
                Do::EdgeCcw,
                Do::FullTurn,
                Do::Flick
            ]
        );
        assert!(!c.side(1).like_left);
        assert_eq!(c.side(1).unclear, [] as [String; 0]);
        let fix = c.fix(&m).unwrap();
        assert_eq!(jogs(&fix.mapping, 0)[1].2, TWOS);
        assert_eq!(
            jogs(&fix.mapping, 1)[1].2,
            RelMode::Absolute14 { fine: 0x31 }
        );
    }

    #[test]
    fn turned_the_wrong_way_it_isnt_clear() {
        let m = sc_live_4();
        let c = run(&m, Pretend::WrongWay);
        let f = c.side(0);
        assert!(!f.clear());
        let text = c.summary(&m).join("\n");
        assert!(text.contains("Left jog wheel: not clear yet"), "{text}");
        assert!(text.contains("Turn it clockwise first each time"), "{text}");
        assert!(!text.contains("has it right"), "{text}");
        assert!(c.fix(&m).is_none());
        assert!(c.next_step(&m, false).contains("Do it again"));
    }

    /// Another control pressed or moved at a step isn't the wheel's answer,
    /// and doesn't move the check on.
    #[test]
    fn a_wrong_control_isnt_taken() {
        let m = sc_live_4();
        let mut c = JogCheck::new(&m);
        // PLAY, the right wheel touched, the pitch fader (both halves).
        c.hear(1.0, &[0x94, 0x0A, 0x7F]);
        assert_eq!(c.refused(), Some(Trigger::note(4, 0x0A)));
        c.hear(1.1, &[0x94, 0x0A, 0x00]);
        c.hear(1.5, &[0x95, 0x21, 0x7F]);
        assert_eq!(c.refused(), Some(Trigger::note(5, 0x21)));
        c.hear(1.6, &[0x95, 0x21, 0x00]);
        assert!(!c.tick(9.0) && !c.got());
        // The right control, and it goes on.
        c.hear(10.0, &[0x94, 0x21, 0x7F]);
        assert!(c.hand_on() && !c.tick(15.0), "not while the hand is on it");
        c.hear(15.0, &[0x94, 0x21, 0x00]);
        assert!(!c.tick(15.5) && c.tick(16.1));
        // At a turn: the fader isn't it, whichever half.
        let mut t = 20.0;
        for v in 0..60u8 {
            c.hear(t, &[0xB4, 0x1F, v]);
            c.hear(t + 0.0005, &[0xB4, 0x4B, v.wrapping_mul(37) & 0x7F]);
            t += 0.01;
        }
        assert_eq!(c.refused(), Some(Trigger::cc(4, 0x1F)));
        assert!(!c.got() && !c.tick(t + 5.0));
        // Nor the other wheel, nor a few stray ticks of this one.
        for _ in 0..30 {
            c.hear(t, &[0xB5, 0x36, 1]);
        }
        for _ in 0..5 {
            c.hear(t, &[0xB4, 0x36, 1]);
        }
        assert!(!c.got() && !c.tick(t + 5.0));
        c.again();
        assert!(c.heard(1).is_empty() && c.refused().is_none());
        // A control the mapping has as something else, and the DJ says it's
        // the one.
        c.hear(t, &[0x94, 0x0A, 0x7F]);
        c.take_refused();
        assert_eq!(c.refused(), None);
        assert!(c.hand_on());
    }

    /// An unknown button pressed for the touch sensor: what came when the
    /// hand was on top says it isn't one.
    #[test]
    fn a_button_pressed_for_the_touch_sensor_is_found_out() {
        let mut m = sc_live_4();
        m.bindings
            .retain(|b| !matches!(b.target, Target::JogTouch { .. }));
        let mut c = JogCheck::new(&m);
        // SLIP (not mapped) instead of the wheel, then the rest as asked, on
        // a wheel without a touch sensor.
        c.hear(1.0, &[0x94, 0x24, 0x7F]);
        c.hear(1.2, &[0x94, 0x24, 0x00]);
        assert!(c.tick(3.0));
        run_to(&mut c, Pretend::NoTouch, usize::MAX);
        let f = c.side(0);
        assert_eq!(f.touch, Touch::Unclear);
        let text = c.summary(&m).join("\n");
        assert!(text.contains("not clear yet. Note 0x24"), "{text}");
        assert!(text.contains("do the first step again"), "{text}");
        assert!(
            c.fix(&m).is_none()
                || !c
                    .fix(&m)
                    .unwrap()
                    .changed
                    .iter()
                    .any(|l| l.contains("Deck A"))
        );
        // With the real one at the steps after, that's the touch sensor.
        let mut c = JogCheck::new(&m);
        c.hear(1.0, &[0x94, 0x24, 0x7F]);
        c.hear(1.2, &[0x94, 0x24, 0x00]);
        assert!(c.tick(3.0));
        run_to(&mut c, Pretend::Counts, usize::MAX);
        assert_eq!(c.side(0).touch, Touch::Works(Trigger::note(4, 0x21)));
    }

    #[test]
    fn steps_can_be_done_again() {
        let m = sc_live_4();
        let mut c = JogCheck::new(&m);
        assert!(!c.can_back());
        run_to(&mut c, Pretend::Counts, 3);
        assert_eq!(c.at, 3);
        assert!(!c.heard(2).is_empty());
        c.back();
        assert_eq!(c.at, 2);
        assert!(c.heard(2).is_empty() && !c.heard(1).is_empty());
        run_to(&mut c, Pretend::Counts, usize::MAX);
        assert_eq!(c.side(0).unclear, [] as [String; 0]);
        // A full turn counts as it goes, and waits for the DJ.
        let mut c = JogCheck::new(&m);
        let t = run_to(&mut c, Pretend::Counts, 5);
        assert_eq!(c.step().map(|s| s.what), Some(Do::FullTurn));
        assert_eq!(c.count(), Some(0));
        for i in 0..300 {
            c.hear(t + i as f64 * 0.004, &[0xB4, 0x36, 2]);
        }
        assert_eq!(c.count(), Some(600));
        assert!(!c.tick(t + 60.0));
        c.again();
        assert_eq!(c.count(), Some(0));
        // Too short to be a turn: not clear, rather than measured.
        for i in 0..30 {
            c.hear(t + 10.0 + i as f64 * 0.004, &[0xB4, 0x36, 2]);
        }
        c.next();
        let f = c.side(0);
        assert!(
            f.unclear[0].contains("too short to be one"),
            "{:?}",
            f.unclear
        );
        assert!(f.turn.is_empty());
    }

    /// A wheel learned somewhere else entirely (the pitch fader's fine half,
    /// say) is found, and taken off there.
    #[test]
    fn a_wheel_learned_from_another_control_is_put_right() {
        let m = sc_live_4();
        let elsewhere = Trigger::cc(4, 0x4B);
        let mut wrong = m.clone();
        wrong
            .bindings
            .retain(|b| b.target != Target::Jog { deck: 0 });
        wrong.bind(elsewhere, Target::Jog { deck: 0 }).layered = true;
        // With the built-in mapping's beside it, as an upgrade leaves it, or
        // alone.
        let mut both = m.clone();
        both.bind(elsewhere, Target::Jog { deck: 0 }).layered = true;
        for (m, why) in [
            (wrong, "On deck A, turning it isn't mapped."),
            (
                both,
                "On deck A, turning it is also mapped to something the wheel didn't send.",
            ),
        ] {
            let c = run(&m, Pretend::Counts);
            let text = c.summary(&m).join("\n");
            assert!(text.contains(why), "{text}");
            let fix = c.fix(&m).unwrap();
            assert!(!fix.mapping.bindings.iter().any(|b| b.trigger == elsewhere));
            assert_eq!(jogs(&fix.mapping, 0).len(), 2);
            assert!(c.fix(&fix.mapping).is_none());
        }
    }

    /// What was also on the wheel's own messages goes, and Undo is the
    /// mapping as it was.
    #[test]
    fn what_else_was_on_the_wheels_messages_makes_way() {
        let mut m = sc_live_4();
        m.bindings
            .retain(|b| b.target != Target::JogTouch { deck: 1 });
        m.bind(Trigger::note(5, 0x21), Target::Play { deck: 3 });
        let c = run(&m, Pretend::Counts);
        let text = &c.summary(&m)[1];
        assert!(
            text.contains("On deck B, the touch sensor is mapped to Deck D · Play/Pause."),
            "{text}"
        );
        assert!(
            text.contains(
                "On deck D, the touch sensor also does something else: Deck D · Play/Pause."
            ),
            "{text}"
        );
        let fix = c.fix(&m).unwrap();
        assert!(
            fix.changed
                .iter()
                .any(|l| l.contains("no longer Deck D · Play/Pause")),
            "{:?}",
            fix.changed
        );
        assert!(!fix
            .mapping
            .bindings
            .iter()
            .any(|b| b.trigger == Trigger::note(5, 0x21)
                && !matches!(b.target, Target::JogTouch { .. })));
        // The left side was right: untouched.
        assert_eq!(jogs(&fix.mapping, 0), jogs(&m, 0));
        assert!(fix.changed.iter().all(|l| !l.contains("Deck A")));
    }

    /// The built-in FLX6 mapping, on an FLX6: right as it is, the message
    /// for the controller's own vinyl mode (which the check doesn't hear)
    /// included. When the ticks are off, only they change.
    #[test]
    fn the_flx6s_mapping_is_right_and_only_its_ticks_are_changed() {
        let mut m = builtin_preset("DDJ-FLX6").unwrap();
        let flx6 = |m: &Mapping| {
            let mut c = JogCheck::new(m);
            let dj = PretendDj {
                channel: 0,
                ..PretendDj::new(Pretend::Split)
            };
            use_it(&mut c, dj, usize::MAX);
            c
        };
        let c = flx6(&m);
        assert!(c.side(1).like_left);
        let text = c.summary(&m).join("\n");
        assert!(text.contains("Tandem's mapping has it right"), "{text}");
        assert!(c.fix(&m).is_none());
        m.jog_ticks_per_rev = Some(6387.0);
        let c = flx6(&m);
        let text = c.summary(&m)[0].clone();
        assert!(text.contains("A turn is 2048 ticks, and the mapping counts 6387, so the record moves 0.3× as far as your hand."), "{text}");
        let fix = c.fix(&m).unwrap();
        assert_eq!(fix.changed, ["Jog: 2048 ticks per turn, instead of 6387"]);
        assert_eq!(fix.mapping.bindings, m.bindings);
        assert!(c.fix(&fix.mapping).is_none());
        // Its edge unmapped, that comes back, and the rest stays.
        let mut gone = builtin_preset("DDJ-FLX6").unwrap();
        gone.bindings
            .retain(|b| b.target != Target::JogEdge { deck: 0 });
        let fix = flx6(&gone).fix(&gone).unwrap();
        assert_eq!(fix.changed.len(), 1, "{:?}", fix.changed);
        let jogs = jogs(&fix.mapping, 0);
        assert_eq!(
            jogs.iter().map(|b| b.0.number).collect::<Vec<_>>(),
            [0x36, 0x21, 0x22, 0x23]
        );
    }

    #[test]
    fn nothing_heard_is_said_so() {
        let m = sc_live_4();
        let mut c = JogCheck::new(&m);
        while !c.done() {
            c.next();
        }
        let text = c.summary(&m).join("\n");
        assert!(text.contains("Left jog wheel: nothing was heard"), "{text}");
        assert!(
            text.contains("Right jog wheel: nothing was heard"),
            "{text}"
        );
        assert!(c.fix(&m).is_none());
        // Nothing was asked of the right side beyond its few steps... but it
        // isn't called like the left either.
        assert!(!c.side(1).like_left);
    }

    #[test]
    fn every_pretend_controller_is_named() {
        for name in [
            "counts",
            "position",
            "both",
            "split",
            "split-notouch",
            "split-edge",
            "notouch",
            "stuck",
            "flaky",
            "edge",
            "mixed",
            "wrongway",
        ] {
            let model = PretendDj::named(name).unwrap();
            let m = builtin_preset("SC LIVE 4").unwrap();
            let c = run(&m, model);
            let r = c.report(&info(), &m);
            assert!(r.contains("What it found"), "{name}");
        }
        assert_eq!(PretendDj::named("x"), None);
    }
}
