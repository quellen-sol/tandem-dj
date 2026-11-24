//! The step-by-step controller setup: it asks for one control at a time
//! ("Press PLAY on the left deck"), listens to what the controller sends,
//! and writes the [`Mapping`].
//!
//! While it's open, [`crate::mapping::MidiEngine`] hands it every message
//! ([`Wizard::feed`]) and nothing reaches the room; the UI calls
//! [`Wizard::tick`] every frame, for the things that are decided by a
//! control having stopped moving. It works on a copy of the mapping, which
//! the UI takes with [`Wizard::finish`] when the DJ keeps it.
//!
//! What it works out from the messages, so the DJ never has to:
//!
//! - **Which way a fader or knob runs.** A step asks for it to be left at
//!   one end; a low value there means it's inverted.
//! - **How a jog wheel or encoder counts.** Turned clockwise, values just
//!   over 64 are [`RelMode::Offset64`] and small ones are
//!   [`RelMode::TwosComplement`]; going down instead means it's inverted.
//! - **What SHIFT does.** Held, a control that sends what it sends without
//!   SHIFT gets a SHIFT + binding ([`Binding::shift`]); one that sends
//!   something else gets a plain binding to that. If nothing needed a SHIFT
//!   + binding, Tandem's own SHIFT is dropped again.
//! - **What the pad modes do.** Pads that send the same in two pad modes
//!   keep a binding for each ([`Binding::mode`]); if they never do, the pad
//!   mode buttons are dropped again, so Tandem can't get out of step with
//!   the controller.
//! - **What a DECK button does**, on a controller with two decks' worth of
//!   controls for four decks. If deck C's controls send what deck A's do,
//!   the two take turns ([`Binding::layered`]) and the rest of deck C's
//!   controls are filled in from deck A's; otherwise they're bound as they
//!   come.
//!
//! A control that's already used for something else is refused, with the
//! reason shown and a way to use it here instead.

use crate::mapping::{
    beat_loop_beats, beats_label, roll_beats, side_of, Binding, Mapping, MidiKind, PadMode, Part,
    RawMidi, RelMode, Target, Trigger,
};
use crate::model::{deck_letter, DECKS, FX_SLOTS};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// How a controller's decks are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decks {
    /// Two decks: A and B.
    Two,
    /// Two decks, each with a DECK button that switches it to a second
    /// deck: the left side plays A or C, the right side B or D.
    TwoSwitched,
    /// Four decks, each with controls of its own.
    Four,
}

/// A part of the controller the DJ can leave out of the setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Jog,
    Transport,
    Tempo,
    Loops,
    Pads,
    Mixer,
    Browse,
    Effects,
}

impl Section {
    /// Every section, in the order the setup goes through them.
    pub const ALL: [Section; 8] = [
        Section::Jog,
        Section::Transport,
        Section::Tempo,
        Section::Loops,
        Section::Pads,
        Section::Mixer,
        Section::Browse,
        Section::Effects,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Section::Jog => "Jog wheels",
            Section::Transport => "Play, cue, sync and load",
            Section::Tempo => "Tempo faders and pitch bend",
            Section::Loops => "Loop buttons",
            Section::Pads => "Pads",
            Section::Mixer => "Mixer",
            Section::Browse => "Browse knob",
            Section::Effects => "Effects",
        }
    }

    fn of(part: Part) -> Option<Section> {
        Some(match part {
            Part::Shift => return None,
            Part::Jog => Section::Jog,
            Part::Transport => Section::Transport,
            Part::Tempo => Section::Tempo,
            Part::Loops => Section::Loops,
            Part::HotCues | Part::BeatLoops | Part::Rolls => Section::Pads,
            Part::Channel | Part::Mixer => Section::Mixer,
            Part::Browse => Section::Browse,
            Part::Fx => Section::Effects,
        })
    }
}

/// What the DJ said about their controller before starting.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    pub decks: Decks,
    /// Pads on each deck: 8 or 4.
    pub pads: u8,
    pub sections: Vec<Section>,
    /// Leave out the controls that are mapped already.
    pub skip_mapped: bool,
}

impl Default for Setup {
    /// Two decks of eight pads, everything but the effects.
    fn default() -> Self {
        Self {
            decks: Decks::Two,
            pads: 8,
            sections: Section::ALL
                .into_iter()
                .filter(|s| *s != Section::Effects)
                .collect(),
            skip_mapped: true,
        }
    }
}

/// The end of its travel a fader or knob is to be left at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// Up, right, fastest: where its value should be highest.
    Top,
    /// Down, left: the quiet end, for a level knob that would otherwise be
    /// left turned all the way up.
    Bottom,
}

/// What a step asks the DJ to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Press a button.
    Press,
    /// Hold SHIFT and press a button (or press one that does it by itself).
    ShiftPress,
    /// Rest a hand on the jog wheel.
    Touch,
    /// Turn a wheel or knob clockwise.
    Turn,
    /// Turn the jog wheel clockwise by its edge.
    TurnEdge,
    /// Turn the jog wheel one full turn, to count its ticks, then confirm.
    FullTurn,
    /// Move a fader or knob through its travel and leave it at this end.
    Move(End),
    /// Hold SHIFT and move the tempo fader.
    ShiftMove,
}

/// How far a step has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Still to do.
    Todo,
    /// Mapped in this run.
    Done,
    /// Mapped in this run without asking, from what another step found.
    Filled,
    /// Mapped before this run, and left alone.
    Kept,
    /// Passed over.
    Skipped,
}

impl State {
    /// The control is mapped.
    pub fn mapped(&self) -> bool {
        matches!(self, State::Done | State::Filled | State::Kept)
    }
}

#[derive(Clone, Debug)]
pub struct Step {
    pub target: Target,
    pub ask: Ask,
    /// Index into [`Wizard::groups`].
    pub group: usize,
    pub state: State,
    /// What to do, in a few words: "Press PLAY/PAUSE".
    pub title: String,
    /// What it's for, and when to skip it.
    pub hint: String,
    /// It adds to what its target is bound to already (the jog wheel's
    /// edge, a second SHIFT), where other steps replace it.
    adds: bool,
    /// The bindings of its target this step made: (trigger, SHIFT +).
    made: Vec<(Trigger, bool)>,
}

/// Steps the setup shows together: one part of one deck.
#[derive(Clone, Debug)]
pub struct Group {
    pub title: String,
    pub part: Part,
    /// The deck or FX unit it belongs to.
    pub n: Option<u8>,
    pub steps: Range<usize>,
}

/// How what the setup last heard went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Mapped.
    Good,
    /// Heard, and not what the step is waiting for.
    Info,
    /// Refused.
    Warn,
}

/// What the setup last heard, and what it made of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Said {
    pub text: String,
    pub tone: Tone,
    /// Something the DJ can do about it ([`Wizard::accept`]): the label
    /// for its button.
    pub action: Option<String>,
}

/// A binding waiting for the DJ to say yes ([`Wizard::accept`]).
#[derive(Clone, Debug)]
enum Offer {
    /// The control is used for these already: take it from them.
    Take(Binding, Vec<Target>),
    /// A deck C or D control sent what the same control on deck A or B
    /// sends: the two decks take turns on the same controls.
    Share(Binding),
}

/// A control's messages are left out after it's learned until it has been
/// quiet this long (us): a button let go, a fader coming to rest.
const QUIET_US: i64 = 250_000;
/// A fader or knob has stopped once it has sent nothing for this long.
const SETTLE_US: i64 = 300_000;
/// A button that sends control changes is told from a knob by sending
/// nothing more for this long.
const CC_PRESS_US: i64 = 120_000;
/// A 14-bit control's fine half follows its coarse half within this (us).
const FINE_HALF_US: i64 = 5_000;
/// How much of its travel a fader or knob has to cover.
const TRAVEL: f32 = 0.25;
/// How close to an end it has to be left.
const AT_END: f32 = 0.15;
/// Messages a wheel has to send before its counting style is read.
const TURN_MESSAGES: u32 = 8;
/// Fewest ticks a full turn can be (as the mapping screen's calibration).
pub const MIN_TURN_TICKS: i64 = 100;

/// What one control change or pitch bend control has sent during a step.
#[derive(Clone, Debug)]
struct Track {
    last_t: i64,
    n: u32,
    min: f32,
    max: f32,
    last: f32,
    /// How far it has travelled in all, back and forth.
    path: f32,
    /// The 7-bit values it has sent.
    seen: u128,
    last7: u8,
    /// A fine half arrived for it: it's a 14-bit fader or knob.
    fine: bool,
    /// SHIFT was held when it last sent something.
    shifted: bool,
    /// Messages that read as 1/127 style, and the ticks they'd be.
    twos: (u32, i32),
    /// Messages that read as 64-centred, and the ticks they'd be.
    offs: (u32, i32),
}

impl Track {
    fn new(t: i64, v: f32, v7: u8) -> Self {
        let mut tr = Track {
            last_t: t,
            n: 0,
            min: v,
            max: v,
            last: v,
            path: 0.0,
            seen: 0,
            last7: v7,
            fine: false,
            shifted: false,
            twos: (0, 0),
            offs: (0, 0),
        };
        tr.add(t, v, v7);
        tr
    }

    fn add(&mut self, t: i64, v: f32, v7: u8) {
        self.path += (v - self.last).abs();
        self.last_t = t;
        self.n += 1;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.last = v;
        self.seen |= 1u128 << v7.min(127);
        self.last7 = v7;
        match v7 {
            1..=30 => self.twos = (self.twos.0 + 1, self.twos.1 + v7 as i32),
            98..=127 => self.twos = (self.twos.0 + 1, self.twos.1 + v7 as i32 - 128),
            34..=63 | 65..=94 => self.offs = (self.offs.0 + 1, self.offs.1 + v7 as i32 - 64),
            _ => {}
        }
    }

    fn range(&self) -> f32 {
        self.max - self.min
    }

    fn distinct(&self) -> u32 {
        self.seen.count_ones()
    }

    /// It moved like a fader or knob: far enough, through the values in
    /// between (a wheel that counts 1 and 127 has none), and without the
    /// endless back and forth of a 14-bit control's fine half.
    fn swept(&self, travel: f32) -> bool {
        const MIDDLE: u128 = ((1u128 << 108) - 1) & !((1u128 << 20) - 1);
        self.range() >= travel
            && self.distinct() >= 4
            && self.seen & MIDDLE != 0
            && self.path <= 4.0 * self.range() + 0.1
    }

    /// It turned like a wheel or an endless knob: how it counts, and
    /// whether clockwise came out backwards. A wheel repeats a few values;
    /// a fader never sends the same value twice.
    fn turned(&self) -> Option<(RelMode, bool)> {
        if self.n < TURN_MESSAGES || self.fine || self.distinct() * 2 > self.n {
            return None;
        }
        let most =
            |(count, ticks): (u32, i32)| (count * 10 >= self.n * 9 && ticks != 0).then_some(ticks);
        if let Some(ticks) = most(self.twos) {
            Some((RelMode::TwosComplement, ticks < 0))
        } else {
            most(self.offs).map(|ticks| (RelMode::Offset64, ticks < 0))
        }
    }

    /// A button that sends a control change: one value at 64 or more for
    /// the press, and perhaps 0 for letting go.
    fn pressed(&self) -> bool {
        let press = self.seen & !1;
        press.count_ones() == 1 && press >> 64 != 0
    }
}

pub struct Wizard {
    setup: Setup,
    /// The mapping being made: a copy, until the DJ keeps it.
    draft: Mapping,
    /// The mapping before the run, to tell whether anything changed.
    before: Mapping,
    pub steps: Vec<Step>,
    pub groups: Vec<Group>,
    /// The step being asked; `steps.len()` once through.
    at: usize,
    now: i64,
    /// The control just learned, and when it last sent something.
    hush: Option<(Trigger, i64)>,
    /// Control changes 0 to 31 on each channel, and when each last came
    /// (see `is_fine_half`).
    coarse: HashMap<(u8, u8), i64>,
    /// Jog wheel touch sensors with a hand on them.
    touch: HashSet<Trigger>,
    /// What this step's faders, knobs and wheels have sent.
    tracks: HashMap<Trigger, Track>,
    /// SHIFT buttons held.
    shift: HashSet<Trigger>,
    /// Targets whose bindings from before the run have been replaced.
    cleared: HashSet<Target>,
    /// Sides (left, right) the DJ has said share their controls between
    /// two decks.
    shared: [bool; 2],
    /// Ticks counted so far in a full turn.
    count: i64,
    said: Option<Said>,
    offer: Option<Offer>,
    /// After the last step: what the control just touched would do.
    tried: Option<String>,
}

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

/// A SHIFT, pad mode or DECK button: it changes what other controls do.
fn modifier(t: Target) -> bool {
    t.is_modifier() || matches!(t, Target::DeckSelect { .. })
}

/// The pad mode a part's pads are in.
fn pad_mode(part: Part) -> Option<PadMode> {
    match part {
        Part::HotCues => Some(PadMode::HotCues),
        Part::BeatLoops => Some(PadMode::BeatLoops),
        Part::Rolls => Some(PadMode::Rolls),
        _ => None,
    }
}

/// The beat loop and roll sizes on a deck's pads, pad 1 first: all eight,
/// or on four pads the four in the middle (1 to 8 beats, 1/8 to 1 beat).
pub fn pad_sizes(pads: u8) -> (Vec<u8>, Vec<u8>) {
    if pads <= 4 {
        (vec![2, 3, 4, 5], vec![1, 2, 3, 4])
    } else {
        ((0..8).collect(), (0..8).collect())
    }
}

/// Builds the steps of a run.
struct Plan<'a> {
    setup: &'a Setup,
    mapping: &'a Mapping,
    groups: Vec<Group>,
    steps: Vec<Step>,
}

impl Plan<'_> {
    fn wants(&self, part: Part) -> bool {
        Section::of(part).is_none_or(|s| self.setup.sections.contains(&s))
    }

    fn group(&mut self, title: String, part: Part, n: Option<u8>) {
        let at = self.steps.len();
        self.groups.push(Group {
            title,
            part,
            n,
            steps: at..at,
        });
    }

    fn part(&mut self, part: Part, n: Option<u8>) -> bool {
        if self.wants(part) {
            self.group(part.title(n), part, n);
        }
        self.wants(part)
    }

    fn step(
        &mut self,
        target: Target,
        ask: Ask,
        title: impl Into<String>,
        hint: impl Into<String>,
    ) {
        let mapped = match ask {
            Ask::FullTurn => self.mapping.jog_ticks_per_rev.is_some(),
            _ => self.mapping.binding_for(target).is_some(),
        };
        let group = self.groups.len() - 1;
        self.steps.push(Step {
            target,
            ask,
            group,
            state: if self.setup.skip_mapped && mapped {
                State::Kept
            } else {
                State::Todo
            },
            title: title.into(),
            hint: hint.into(),
            adds: false,
            made: Vec::new(),
        });
        self.groups[group].steps.end = self.steps.len();
    }

    /// The step just added adds to its target's bindings.
    fn adds(&mut self) {
        if let Some(s) = self.steps.last_mut() {
            s.adds = true;
        }
    }

    fn deck(&mut self, deck: u8) {
        use Target::*;
        let n = Some(deck);
        let switched = self.setup.decks == Decks::TwoSwitched && deck >= 2;
        let letter = deck_letter(deck as usize);
        if switched {
            let side = if side_of(deck) == 0 { "left" } else { "right" };
            self.group(format!("Deck {letter} · DECK button"), Part::Transport, n);
            self.step(
                DeckSelect { deck },
                Ask::Press,
                format!(
                    "Press the DECK button that switches the {side} side to deck {}",
                    deck + 1
                ),
                format!(
                    "Often marked DECK {}, or just DECK. Leave the {side} side on deck {} \
                     (Tandem's deck {letter}) for the steps that follow. If the button does \
                     its job but nothing is heard here, skip this step.",
                    deck + 1,
                    deck + 1
                ),
            );
        }
        if self.part(Part::Jog, n) {
            self.step(
                JogTouch { deck },
                Ask::Touch,
                "Rest your hand on top of the jog wheel",
                "Touching the top holds the record, for scratching. Skip this if the wheel can't feel your hand.",
            );
            self.step(
                Jog { deck },
                Ask::Turn,
                "Turn the jog wheel slowly clockwise",
                "With your hand on top, a quarter turn or so.",
            );
            self.step(
                Jog { deck },
                Ask::TurnEdge,
                "Now turn it clockwise by its edge",
                "Without touching the top: many wheels send something else from the edge, for nudging.",
            );
            self.adds();
            if deck == 0 {
                self.step(
                    Jog { deck },
                    Ask::FullTurn,
                    "Turn the jog wheel exactly one full turn",
                    "Line up a mark on the wheel, turn it once round with your hand on top, then press Enter. \
                     This measures the wheel, so a scratch follows your hand exactly.",
                );
            }
        }
        if self.part(Part::Transport, n) {
            self.step(Play { deck }, Ask::Press, "Press PLAY/PAUSE", "");
            self.step(Cue { deck }, Ask::Press, "Press CUE", "");
            self.step(Sync { deck }, Ask::Press, "Press SYNC", "");
            self.step(
                BackToStart { deck },
                Ask::ShiftPress,
                "Hold SHIFT and press CUE",
                "Jumps back to the start of the track. If another button does that on your controller, press that one.",
            );
            self.step(
                SyncOff { deck },
                Ask::ShiftPress,
                "Hold SHIFT and press SYNC",
                "Turns sync off and puts the tempo back where the tempo fader is.",
            );
            self.step(
                Load { deck },
                Ask::Press,
                "Press LOAD",
                "The button that loads the selected track onto this deck.",
            );
        }
        if self.part(Part::Tempo, n) {
            self.step(
                Tempo { deck },
                Ask::Move(End::Top),
                "Slide the tempo fader all the way to −, then all the way to +",
                "Leave it at +, the faster end: that tells Tandem which way it runs.",
            );
            self.step(
                TempoShift { deck },
                Ask::ShiftMove,
                "Hold SHIFT and slide the tempo fader",
                "A good way along, then let go of SHIFT. With SHIFT the fader moves and the tempo stays, \
                 so you can slide it back and keep going. Put it back in the middle afterwards.",
            );
            self.step(
                BendDown { deck },
                Ask::Press,
                "Press PITCH BEND −",
                "Skip this if there are no pitch bend buttons.",
            );
            self.step(BendUp { deck }, Ask::Press, "Press PITCH BEND +", "");
        }
        if self.part(Part::Loops, n) {
            self.step(LoopIn { deck }, Ask::Press, "Press LOOP IN", "");
            self.step(LoopOut { deck }, Ask::Press, "Press LOOP OUT", "");
            self.step(Reloop { deck }, Ask::Press, "Press RELOOP / EXIT", "");
            self.step(
                LoopHalve { deck },
                Ask::Press,
                "Press the button that halves the loop",
                "Often ½, or CUE/LOOP CALL ◀.",
            );
            self.step(
                LoopDouble { deck },
                Ask::Press,
                "Press the button that doubles the loop",
                "Often ×2, or CUE/LOOP CALL ▶.",
            );
            self.step(
                BeatLoop { deck, size: 4 },
                Ask::Press,
                "Press AUTO LOOP",
                "A button, or a push of the loop knob, that sets a 4-beat loop. Skip this if there isn't one.",
            );
        }
        let pads = self.setup.pads.clamp(1, 8);
        let (loops, rolls) = pad_sizes(pads);
        if self.part(Part::HotCues, n) {
            self.step(
                PadMode {
                    deck,
                    mode: self::PadMode::HotCues,
                },
                Ask::Press,
                "Press the HOT CUE pad mode button",
                "It makes the pads hot cues. Leave the pads in that mode for the steps that follow.",
            );
            for slot in 0..pads {
                self.step(
                    HotCue { deck, slot },
                    Ask::Press,
                    format!("Press pad {}", slot + 1),
                    format!("Hot cue {}.", slot + 1),
                );
            }
            for slot in 0..pads {
                self.step(
                    HotCueClear { deck, slot },
                    Ask::ShiftPress,
                    format!("Hold SHIFT and press pad {}", slot + 1),
                    format!("Clears hot cue {}.", slot + 1),
                );
            }
        }
        if self.part(Part::BeatLoops, n) {
            self.step(
                PadMode {
                    deck,
                    mode: self::PadMode::BeatLoops,
                },
                Ask::Press,
                "Press the pad mode button for beat loops",
                "Often BEAT LOOP, LOOP or AUTO LOOP. Leave the pads in that mode for the steps that follow. \
                 If the pads have no such mode, skip this group.",
            );
            for (pad, &size) in loops.iter().enumerate() {
                self.step(
                    BeatLoop { deck, size },
                    Ask::Press,
                    format!("Press pad {}", pad + 1),
                    format!("A loop of {} beats.", beats_label(beat_loop_beats(size))),
                );
                // The AUTO LOOP button may be on the same loop.
                self.adds();
            }
        }
        if self.part(Part::Rolls, n) {
            self.step(
                PadMode {
                    deck,
                    mode: self::PadMode::Rolls,
                },
                Ask::Press,
                "Press the pad mode button for loop rolls",
                "Often ROLL or LOOP ROLL; any spare pad mode will do (Tandem's preset for Pioneer uses BEAT JUMP). \
                 Leave the pads in that mode for the steps that follow.",
            );
            for (pad, &size) in rolls.iter().enumerate() {
                self.step(
                    Roll { deck, size },
                    Ask::Press,
                    format!("Press pad {}", pad + 1),
                    format!(
                        "A roll of {} beats while it's held.",
                        beats_label(roll_beats(size))
                    ),
                );
            }
        }
        if self.part(Part::Channel, n) {
            let knob = "fully left, then fully right";
            self.step(
                Trim { deck },
                Ask::Move(End::Top),
                format!("Turn TRIM (GAIN) {knob}"),
                "Put it back where it was afterwards: nothing you move here reaches the room.",
            );
            for (band, name) in [(2, "HIGH"), (1, "MID"), (0, "LOW")] {
                self.step(
                    Eq { deck, band },
                    Ask::Move(End::Top),
                    format!("Turn EQ {name} {knob}"),
                    "",
                );
            }
            self.step(
                Filter { deck },
                Ask::Move(End::Top),
                format!("Turn FILTER {knob}"),
                "",
            );
            self.step(
                Volume { deck },
                Ask::Move(End::Top),
                "Slide the channel fader all the way down, then all the way up",
                "",
            );
            self.step(
                Pfl { deck },
                Ask::Press,
                "Press the headphone CUE button",
                "The one on this channel that sends it to your headphones.",
            );
        }
    }

    fn rest(&mut self) {
        use Target::*;
        if self.setup.decks == Decks::TwoSwitched {
            self.group("Back to decks A and B".into(), Part::Transport, None);
            for (deck, side) in [(0u8, "left"), (1, "right")] {
                self.step(
                    DeckSelect { deck },
                    Ask::Press,
                    format!("Switch the {side} side back to deck {}", deck + 1),
                    format!("Press its DECK button again, or DECK {}.", deck + 1),
                );
            }
        }
        if self.part(Part::Mixer, None) {
            self.step(
                Crossfader,
                Ask::Move(End::Top),
                "Slide the crossfader fully left, then fully right",
                "",
            );
            self.step(
                CueMix,
                Ask::Move(End::Top),
                "Turn the headphone MIX knob fully to CUE, then fully to MASTER",
                "Skip any knob here that your controller doesn't have.",
            );
            self.step(
                HeadphoneLevel,
                Ask::Move(End::Bottom),
                "Turn the headphone LEVEL knob halfway up, then all the way down",
                "Leave it all the way down: that tells Tandem which way it runs.",
            );
            self.step(
                MasterLevel,
                Ask::Move(End::Bottom),
                "Turn the MASTER level knob halfway up, then all the way down",
                "Leave it all the way down. If the controller plays the speakers itself, turn them down first.",
            );
        }
        if self.part(Part::Browse, None) {
            self.step(
                Browse,
                Ask::Turn,
                "Turn the BROWSE knob clockwise",
                "A few clicks.",
            );
            self.step(BrowsePress, Ask::Press, "Press the BROWSE knob", "");
            self.step(
                BrowseBack,
                Ask::Press,
                "Press BACK",
                "It gives the browse knob to the lists on the left. Skip this if there's no BACK button.",
            );
        }
        if self.wants(Part::Fx) {
            for unit in 0..2u8 {
                self.group(Part::Fx.title(Some(unit)), Part::Fx, Some(unit));
                for slot in 0..FX_SLOTS as u8 {
                    self.step(
                        FxOn { unit, slot },
                        Ask::Press,
                        format!("Press effect {}'s ON button", slot + 1),
                        "Skip what your controller doesn't have: a unit with one ON button and one knob works too.",
                    );
                }
                for slot in 0..FX_SLOTS as u8 {
                    self.step(
                        FxDepth { unit, slot },
                        Ask::Move(End::Top),
                        format!("Turn effect {}'s knob fully left, then fully right", slot + 1),
                        "If there's only one knob, map it here as effect 1 and skip the others: it will set all three.",
                    );
                }
                self.step(FxBeatsDown { unit }, Ask::Press, "Press BEATS ◀", "");
                self.step(FxBeatsUp { unit }, Ask::Press, "Press BEATS ▶", "");
            }
        }
    }
}

impl Wizard {
    /// A run over `mapping`, the controller's mapping as it is now.
    pub fn new(mapping: Mapping, setup: Setup) -> Self {
        let mut plan = Plan {
            setup: &setup,
            mapping: &mapping,
            groups: Vec::new(),
            steps: Vec::new(),
        };
        plan.group(Part::Shift.title(None), Part::Shift, None);
        plan.step(
            Target::Shift,
            Ask::Press,
            "Press SHIFT",
            "The left one, if there are two. Skip this if your controller has no SHIFT button.",
        );
        plan.step(
            Target::Shift,
            Ask::Press,
            "Press the other SHIFT",
            "The one on the right. Skip this if there's only one.",
        );
        plan.adds();
        let decks = if setup.decks == Decks::Two {
            2
        } else {
            DECKS as u8
        };
        for deck in 0..decks {
            plan.deck(deck);
        }
        plan.rest();
        let (groups, steps) = (plan.groups, plan.steps);
        let mut w = Self {
            setup,
            before: mapping.clone(),
            draft: mapping,
            steps,
            groups,
            at: 0,
            now: 0,
            hush: None,
            coarse: HashMap::new(),
            touch: HashSet::new(),
            tracks: HashMap::new(),
            shift: HashSet::new(),
            cleared: HashSet::new(),
            shared: [false; 2],
            count: 0,
            said: None,
            offer: None,
            tried: None,
        };
        // A side whose decks take turns already still does.
        for b in &w.draft.bindings {
            if let (true, Some(d)) = (b.layered, b.target.deck()) {
                w.shared[side_of(d)] = true;
            }
        }
        // SHIFT and a pad mode button are only there for the steps that
        // use them: with none of those to do, they aren't asked for.
        let todo = |s: &Step| s.state == State::Todo;
        let shifts = w
            .steps
            .iter()
            .any(|s| todo(s) && matches!(s.ask, Ask::ShiftPress | Ask::ShiftMove));
        for g in &w.groups {
            let steps = &mut w.steps[g.steps.clone()];
            let used = match g.part {
                Part::Shift => shifts,
                Part::HotCues | Part::BeatLoops | Part::Rolls => {
                    steps.iter().any(|s| todo(s) && !modifier(s.target))
                }
                _ => true,
            };
            for s in steps.iter_mut().filter(|s| todo(s) && modifier(s.target)) {
                if !used {
                    s.state = State::Skipped;
                }
            }
        }
        w.at = w.next_todo(0);
        w
    }

    pub fn setup(&self) -> &Setup {
        &self.setup
    }

    /// The mapping as the run has it so far.
    pub fn draft(&self) -> &Mapping {
        &self.draft
    }

    /// The step being asked, or `None` once the run is through.
    pub fn at(&self) -> Option<usize> {
        (self.at < self.steps.len()).then_some(self.at)
    }

    pub fn step(&self) -> Option<&Step> {
        self.steps.get(self.at)
    }

    /// Where the step's control is: "Deck C · the left deck, on deck 3".
    pub fn place(&self, step: &Step) -> String {
        let group = &self.groups[step.group];
        let Some(deck) = step.target.deck() else {
            return group.title.clone();
        };
        let side = if side_of(deck) == 0 { "left" } else { "right" };
        let channel = group.part == Part::Channel;
        let at = match (self.setup.decks, deck >= 2, channel) {
            (Decks::Four, _, _) => return group.title.clone(),
            (_, false, false) => format!("the {side} deck"),
            (_, false, true) => format!("the {side} deck's mixer channel"),
            (_, true, false) => format!("the {side} deck, switched to deck {}", deck + 1),
            (_, true, true) => format!(
                "mixer channel {}, or the {side} channel with its side on deck {}",
                deck + 1,
                deck + 1
            ),
        };
        format!("{} · {at}", group.title)
    }

    /// What the step's control is bound to now: "Note 0x0b · ch 1". A step
    /// that adds to what its control has (a wheel's edge, the pad on the
    /// same loop as a button) only shows what it made itself.
    pub fn bound(&self, step: &Step) -> Option<String> {
        if step.ask == Ask::FullTurn {
            return self
                .draft
                .jog_ticks_per_rev
                .map(|t| format!("{t:.0} ticks per turn"));
        }
        let mode = pad_mode(self.groups[step.group].part);
        let own = !step.made.is_empty() || (step.adds && step.state != State::Kept);
        let labels: Vec<String> = self
            .draft
            .bindings
            .iter()
            .filter(|b| b.target == step.target)
            .filter(|b| b.mode.is_none() || mode.is_none() || b.mode == mode)
            .filter(|b| !own || step.made.contains(&(b.trigger, b.shift)))
            .map(|b| b.label())
            .collect();
        (!labels.is_empty()).then(|| labels.join(", "))
    }

    pub fn said(&self) -> Option<&Said> {
        self.said.as_ref()
    }

    /// Ticks counted so far in a full turn of the jog wheel.
    pub fn count(&self) -> i64 {
        self.count.abs()
    }

    /// True while a SHIFT button the run knows is held.
    pub fn shift_held(&self) -> bool {
        !self.shift.is_empty()
    }

    /// After the last step: what the control just touched does.
    pub fn tried(&self) -> Option<&str> {
        self.tried.as_deref()
    }

    /// Steps mapped, and steps in all.
    pub fn progress(&self) -> (usize, usize) {
        (
            self.steps.iter().filter(|s| s.state.mapped()).count(),
            self.steps.len(),
        )
    }

    /// Whether the run has changed the mapping.
    pub fn changed(&self) -> bool {
        self.draft != self.before
    }

    /// A group's steps mapped, and its steps in all.
    pub fn group_progress(&self, group: usize) -> (usize, usize) {
        let steps = &self.steps[self.groups[group].steps.clone()];
        (
            steps.iter().filter(|s| s.state.mapped()).count(),
            steps.len(),
        )
    }

    fn next_todo(&self, from: usize) -> usize {
        (from..self.steps.len())
            .find(|&i| self.steps[i].state == State::Todo)
            .unwrap_or(self.steps.len())
    }

    /// Start on step `i` afresh.
    fn go(&mut self, i: usize) {
        self.at = i.min(self.steps.len());
        self.tracks.clear();
        self.count = 0;
        self.offer = None;
        if self.said.as_ref().is_some_and(|s| s.tone != Tone::Good) {
            self.said = None;
        }
        self.tried = None;
    }

    /// Pass over this step (a step that's mapped stays mapped).
    pub fn skip(&mut self) {
        if let Some(s) = self.steps.get_mut(self.at) {
            if s.state == State::Todo {
                s.state = State::Skipped;
            }
            self.said = None;
            self.go(self.next_todo(self.at + 1));
        }
    }

    /// Pass over what's left of this step's group.
    pub fn skip_group(&mut self) {
        let Some(group) = self.step().map(|s| s.group) else {
            return;
        };
        let end = self.groups[group].steps.end;
        for s in &mut self.steps[self.at..end] {
            if s.state == State::Todo {
                s.state = State::Skipped;
            }
        }
        self.said = None;
        self.go(self.next_todo(end));
    }

    /// Back to the step before: it's asked again, and keeps what it has
    /// until something else is heard.
    pub fn back(&mut self) {
        let before = (0..self.at.min(self.steps.len()))
            .rev()
            .find(|&i| self.steps[i].state != State::Kept);
        if let Some(i) = before {
            self.said = None;
            self.go(i);
        }
    }

    /// To a group's first step that isn't mapped, or its first.
    pub fn jump(&mut self, group: usize) {
        let Some(g) = self.groups.get(group) else {
            return;
        };
        let steps = g.steps.clone();
        let i = steps
            .clone()
            .find(|&i| !self.steps[i].state.mapped())
            .unwrap_or(steps.start);
        self.said = None;
        self.go(i);
    }

    /// Past the last step, to look over what was mapped and try it.
    pub fn end(&mut self) {
        self.said = None;
        self.go(self.steps.len());
    }

    /// Back among the steps: at the first one still to do, else the first
    /// that was passed over, else the last.
    pub fn resume(&mut self) {
        let first = |state| self.steps.iter().position(|s| s.state == state);
        let i = first(State::Todo)
            .or(first(State::Skipped))
            .unwrap_or(self.steps.len().saturating_sub(1));
        self.said = None;
        self.go(i);
    }

    /// The groups with controls that aren't mapped, and how many each.
    pub fn unmapped(&self) -> Vec<(usize, usize)> {
        (0..self.groups.len())
            .map(|g| {
                let (done, total) = self.group_progress(g);
                (g, total - done)
            })
            .filter(|(_, left)| *left > 0)
            .collect()
    }

    /// To step `i`.
    pub fn jump_to(&mut self, i: usize) {
        if i < self.steps.len() {
            self.said = None;
            self.go(i);
        }
    }

    /// Unmap this step's control and move on.
    pub fn clear(&mut self) {
        let Some(step) = self.steps.get_mut(self.at) else {
            return;
        };
        let target = step.target;
        step.made.clear();
        step.state = State::Skipped;
        if step.ask == Ask::FullTurn {
            self.draft.jog_ticks_per_rev = None;
        } else {
            self.draft.bindings.retain(|b| b.target != target);
            // Its other steps have nothing left either.
            for s in &mut self.steps {
                if s.target == target && s.ask != Ask::FullTurn && s.state.mapped() {
                    s.made.clear();
                    s.state = State::Skipped;
                }
            }
        }
        self.said = None;
        self.go(self.next_todo(self.at + 1));
    }

    /// Use the full turn just counted, if it's long enough to be one.
    pub fn confirm(&mut self) {
        let turning = self.step().is_some_and(|s| s.ask == Ask::FullTurn);
        if turning && self.count.abs() >= MIN_TURN_TICKS {
            self.draft.jog_ticks_per_rev = Some(self.count.unsigned_abs() as f32);
            self.steps[self.at].state = State::Done;
            self.said = Some(Said {
                text: format!(
                    "✔ The jog wheel counts {} ticks in a turn.",
                    self.count.abs()
                ),
                tone: Tone::Good,
                action: None,
            });
            self.go(self.next_todo(self.at + 1));
        }
    }

    /// Say yes to what [`Said::action`] offers.
    pub fn accept(&mut self) {
        let (Some(offer), Some(i)) = (self.offer.take(), self.at()) else {
            return;
        };
        match offer {
            Offer::Take(binding, from) => {
                self.draft
                    .bindings
                    .retain(|b| !(from.contains(&b.target) && b.overlaps(&binding)));
                // The steps it was taken from are to do again.
                for s in &mut self.steps {
                    if from.contains(&s.target)
                        && s.made.contains(&(binding.trigger, binding.shift))
                    {
                        s.made.retain(|m| *m != (binding.trigger, binding.shift));
                        if s.made.is_empty() {
                            s.state = State::Todo;
                        }
                    }
                }
                self.bind(i, binding);
            }
            Offer::Share(binding) => {
                if let Some(d) = binding.target.deck() {
                    self.shared[side_of(d)] = true;
                }
                let held = binding.shift;
                self.offered(i, binding, held);
            }
        }
    }

    /// The fine half of a 14-bit fader or knob: the control change 32
    /// above its coarse half, sent straight after it. (Told by the timing
    /// as well as the number, since plenty of controllers have plain
    /// buttons and knobs on those numbers too.)
    fn is_fine_half(&self, ch: u8, cc: u8, t: i64) -> bool {
        (32..64).contains(&cc)
            && self
                .coarse
                .get(&(ch, cc - 32))
                .is_some_and(|last| (0..=FINE_HALF_US).contains(&(t - last)))
    }

    fn say(&mut self, tone: Tone, text: impl Into<String>) {
        self.offer = None;
        self.said = Some(Said {
            text: text.into(),
            tone,
            action: None,
        });
    }

    /// The bindings a control has in the mapping so far.
    fn uses(&self, trig: Trigger) -> impl Iterator<Item = &Binding> {
        self.draft
            .bindings
            .iter()
            .filter(move |b| b.trigger == trig)
    }

    /// A message from the controller, at shared time `t` (us).
    pub fn feed(&mut self, raw: RawMidi, t: i64) {
        self.now = t;
        let trig = trigger_of(raw);
        let (value, v7) = match raw {
            RawMidi::NoteOn { vel, .. } => (1.0, vel),
            RawMidi::NoteOff { .. } => (0.0, 0),
            RawMidi::Cc { val, .. } => (val as f32 / 127.0, val),
            RawMidi::PitchBend { val, .. } => (val as f32 / 16383.0, (val >> 7) as u8),
        };
        // The fine half of a 14-bit fader or knob says nothing new.
        if let RawMidi::Cc { ch, cc, .. } = raw {
            if self.is_fine_half(ch, cc, t) {
                if let Some(tr) = self.tracks.get_mut(&Trigger::cc(ch, cc - 32)) {
                    tr.fine = true;
                }
                return;
            }
            if cc < 32 {
                self.coarse.insert((ch, cc), t);
            }
        }
        let down = match raw {
            RawMidi::NoteOn { .. } => Some(true),
            RawMidi::NoteOff { .. } => Some(false),
            RawMidi::Cc { val, .. } => Some(val >= 64),
            RawMidi::PitchBend { .. } => None,
        };
        if self.uses(trig).any(|b| b.target == Target::Shift) {
            match down {
                Some(true) => self.shift.insert(trig),
                _ => self.shift.remove(&trig),
            };
        }
        if self
            .uses(trig)
            .any(|b| matches!(b.target, Target::JogTouch { .. }))
        {
            match down {
                Some(true) => self.touch.insert(trig),
                _ => self.touch.remove(&trig),
            };
        }
        let Some(i) = self.at() else {
            self.tried = Some(self.describe(raw, trig, value));
            return;
        };
        // The control just learned, still being let go or coming to rest.
        if let Some((h, last)) = self.hush {
            if h == trig && t - last < QUIET_US {
                self.hush = Some((h, t));
                return;
            }
            self.hush = None;
        }
        let (target, ask) = (self.steps[i].target, self.steps[i].ask);
        // SHIFT, pad mode and DECK buttons do their job, unless one of them
        // is what's being asked for.
        if !modifier(target) {
            let button = self.uses(trig).map(|b| b.target).find(|t| modifier(*t));
            if let Some(m) = button {
                if m != Target::Shift && down == Some(true) {
                    let what = match m {
                        Target::PadMode { .. } => "a pad mode button",
                        _ => "a DECK button",
                    };
                    self.say(
                        Tone::Info,
                        format!("{} is {what}: it stays what it is.", trig.label()),
                    );
                }
                return;
            }
        }
        let knob = |w: &Self| {
            w.uses(trig)
                .any(|b| b.target.is_absolute() || b.target.is_relative())
        };
        let held = !self.shift.is_empty();
        let binding = |invert, rel| Binding {
            trigger: trig,
            target,
            invert,
            rel,
            shift: false,
            mode: None,
            light: None,
            layered: false,
        };
        match (ask, raw) {
            (Ask::Press | Ask::ShiftPress | Ask::Touch, RawMidi::NoteOn { .. }) => {
                self.offered(i, binding(false, RelMode::Offset64), held);
            }
            (Ask::Press | Ask::ShiftPress | Ask::Touch, RawMidi::Cc { val, .. }) => {
                // A button may send a control change: wait and see whether
                // it's one, or a knob going by.
                if knob(self) {
                    return;
                }
                if val > 0 || self.tracks.contains_key(&trig) {
                    self.track(trig, t, value, v7);
                }
            }
            (Ask::Press | Ask::ShiftPress | Ask::Touch, RawMidi::PitchBend { .. }) => {
                self.say(
                    Tone::Info,
                    format!(
                        "{} is a fader. This step is waiting for a button.",
                        trig.label()
                    ),
                );
            }
            (Ask::Move(_) | Ask::ShiftMove, RawMidi::NoteOn { .. }) => {
                // A brush of the jog wheel isn't worth a word.
                if !self
                    .uses(trig)
                    .any(|b| matches!(b.target, Target::JogTouch { .. }))
                {
                    self.say(
                        Tone::Info,
                        format!(
                            "{} is a button. This step is waiting for a fader or knob.",
                            trig.label()
                        ),
                    );
                }
            }
            (Ask::Move(_) | Ask::ShiftMove, RawMidi::Cc { .. } | RawMidi::PitchBend { .. }) => {
                if self.uses(trig).any(|b| b.target.is_relative()) {
                    return;
                }
                self.track(trig, t, value, v7);
            }
            (Ask::Turn | Ask::TurnEdge, RawMidi::Cc { .. }) => {
                // The edge is wanted, and a hand is on the top.
                let on_top = ask == Ask::TurnEdge
                    && self.draft.bindings.iter().any(|b| {
                        b.target.deck() == target.deck()
                            && matches!(b.target, Target::JogTouch { .. })
                            && self.touch.contains(&b.trigger)
                    });
                if on_top {
                    self.tracks.clear();
                    self.say(
                        Tone::Info,
                        "Your hand is on top of the wheel. Let go, and turn it by its edge.",
                    );
                    return;
                }
                let (turned, n) = self.track(trig, t, value, v7);
                if let Some((rel, invert)) = turned {
                    self.offered(i, binding(invert, rel), held);
                } else if n >= 3 * TURN_MESSAGES {
                    self.tracks.remove(&trig);
                    self.say(
                        Tone::Info,
                        format!(
                            "{} doesn't read as a wheel turning one way. Turn it slowly clockwise.",
                            trig.label()
                        ),
                    );
                }
            }
            (Ask::FullTurn, RawMidi::Cc { val, .. }) => {
                let jog = self.uses(trig).find(|b| b.target == target);
                if let Some(b) = jog {
                    let ticks = match b.rel {
                        RelMode::Offset64 => val as i64 - 64,
                        RelMode::TwosComplement if val < 64 => val as i64,
                        RelMode::TwosComplement => val as i64 - 128,
                    };
                    self.count += if b.invert { -ticks } else { ticks };
                }
            }
            _ => {}
        }
    }

    /// Note what a fader, knob or wheel sent. Returns how it reads as a
    /// wheel so far, and how many messages it has sent.
    fn track(&mut self, trig: Trigger, t: i64, v: f32, v7: u8) -> (Option<(RelMode, bool)>, u32) {
        let held = !self.shift.is_empty();
        let tr = self
            .tracks
            .entry(trig)
            .and_modify(|tr| tr.add(t, v, v7))
            .or_insert_with(|| Track::new(t, v, v7));
        tr.shifted = held;
        (tr.turned(), tr.n)
    }

    /// Called every frame with the shared time (us): decides what only a
    /// control having stopped can tell.
    pub fn tick(&mut self, t: i64) {
        self.now = t;
        let Some(i) = self.at() else {
            return;
        };
        let (target, ask) = (self.steps[i].target, self.steps[i].ask);
        let binding = |trigger, invert| Binding {
            trigger,
            target,
            invert,
            rel: RelMode::Offset64,
            shift: false,
            mode: None,
            light: None,
            layered: false,
        };
        match ask {
            Ask::Press | Ask::ShiftPress | Ask::Touch => {
                let quiet: Vec<(Trigger, bool, bool)> = self
                    .tracks
                    .iter()
                    .filter(|(_, tr)| t - tr.last_t >= CC_PRESS_US)
                    .map(|(k, tr)| (*k, tr.pressed(), tr.shifted))
                    .collect();
                for (trig, pressed, held) in quiet {
                    self.tracks.remove(&trig);
                    if pressed {
                        self.offered(i, binding(trig, false), held);
                        return;
                    }
                    self.say(
                        Tone::Info,
                        format!(
                            "{} is a knob or fader. This step is waiting for a button.",
                            trig.label()
                        ),
                    );
                }
            }
            Ask::Move(_) | Ask::ShiftMove => {
                let travel = if ask == Ask::ShiftMove {
                    TRAVEL * 0.8
                } else {
                    TRAVEL
                };
                let best = self
                    .tracks
                    .iter()
                    .filter(|(_, tr)| tr.swept(travel))
                    .max_by(|a, b| a.1.range().total_cmp(&b.1.range()))
                    .map(|(k, tr)| (*k, tr.clone()));
                let Some((trig, tr)) = best else {
                    // Something moved, and not far enough yet.
                    let near = self
                        .tracks
                        .iter()
                        .filter(|(_, tr)| tr.range() >= 0.05 && tr.distinct() >= 3)
                        .max_by(|a, b| a.1.range().total_cmp(&b.1.range()))
                        .map(|(k, _)| *k);
                    if let Some(trig) = near {
                        self.say(
                            Tone::Info,
                            format!(
                                "Hearing {}: keep going, through its whole travel.",
                                trig.label()
                            ),
                        );
                    }
                    return;
                };
                if t - tr.last_t < SETTLE_US {
                    return;
                }
                let end = match ask {
                    Ask::Move(end) => end,
                    _ => {
                        // SHIFT + tempo runs the way the tempo fader does.
                        let fader = target.deck().map(|deck| Target::Tempo { deck });
                        let invert = self
                            .draft
                            .bindings
                            .iter()
                            .find(|x| Some(x.target) == fader)
                            .is_some_and(|x| x.invert);
                        self.tracks.clear();
                        self.offered(i, binding(trig, invert), tr.shifted);
                        return;
                    }
                };
                let (top, bottom) = (tr.last >= 1.0 - AT_END, tr.last <= AT_END);
                if top || bottom {
                    self.tracks.clear();
                    let invert = match end {
                        End::Top => bottom,
                        End::Bottom => top,
                    };
                    self.offered(i, binding(trig, invert), tr.shifted);
                } else {
                    self.say(
                        Tone::Info,
                        format!(
                            "Hearing {}: now leave it at the end the step asks for.",
                            trig.label()
                        ),
                    );
                }
            }
            _ => {}
        }
    }

    /// The control on deck A or B that the step's control takes turns
    /// with, where a side's DECK button switches it between two decks.
    fn other_deck(&self, target: Target) -> Option<Target> {
        let deck = target.deck()?;
        (self.setup.decks == Decks::TwoSwitched && deck >= 2).then(|| target.with_deck(deck - 2))
    }

    /// A control was heard for step `i`, with SHIFT `held` or not: work out
    /// what else the binding needs, and bind it unless something stands in
    /// the way.
    fn offered(&mut self, i: usize, mut b: Binding, held: bool) {
        self.offer = None;
        let (target, ask) = (b.target, self.steps[i].ask);
        let part = self.groups[self.steps[i].group].part;
        let label = b.trigger.label();
        if !modifier(target) {
            // In a pad mode, where the deck has a button for it.
            b.mode = pad_mode(part).filter(|&mode| {
                target.is_pad()
                    && self
                        .draft
                        .binding_for(Target::PadMode {
                            deck: target.deck().unwrap_or(0),
                            mode,
                        })
                        .is_some()
            });
            // With SHIFT held, a control that sends what it sends without
            // SHIFT needs Tandem to keep track of SHIFT.
            b.shift = held
                && self
                    .uses(b.trigger)
                    .any(|x| !x.shift && x.target != target && !modifier(x.target));
        }
        // The same control as without SHIFT, and no SHIFT to tell by.
        if matches!(ask, Ask::ShiftPress | Ask::ShiftMove) && !b.shift {
            let plain = match target {
                Target::BackToStart { deck } => Some(Target::Cue { deck }),
                Target::SyncOff { deck } => Some(Target::Sync { deck }),
                Target::HotCueClear { deck, slot } => Some(Target::HotCue { deck, slot }),
                Target::TempoShift { deck } => Some(Target::Tempo { deck }),
                _ => None,
            };
            let same = self
                .uses(b.trigger)
                .any(|x| Some(x.target) == plain && !x.shift);
            if same {
                let known = self.draft.binding_for(Target::Shift).is_some();
                self.say(
                    Tone::Warn,
                    if known {
                        format!("{label} is what it sends without SHIFT. Hold SHIFT while you do it.")
                    } else {
                        format!(
                            "{label} is what it sends without SHIFT, and no SHIFT button is mapped, \
                             so Tandem can't tell the two apart. Skip this step."
                        )
                    },
                );
                return;
            }
        }
        // A wheel's edge that sends what its top does has nothing to add.
        let again = self
            .uses(b.trigger)
            .any(|x| x.target == target && x.shift == b.shift && x.mode == b.mode);
        if again && self.steps[i].adds && self.steps[i].made.is_empty() {
            let mine = self
                .steps
                .iter()
                .any(|s| s.target == target && s.made.contains(&(b.trigger, b.shift)));
            if mine || ask == Ask::TurnEdge || target == Target::Shift {
                self.steps[i].state = State::Done;
                self.say(Tone::Good, format!("✔ {label} again: nothing more to add."));
                self.hush = Some((b.trigger, self.now));
                self.go(self.next_todo(i + 1));
                return;
            }
        }
        // Deck C's control sending what deck A's does: the two take turns,
        // if the DJ says the side really is on deck C.
        if let Some(other) = self.other_deck(target) {
            let twin = self.uses(b.trigger).any(|x| {
                x.target == other
                    && x.shift == b.shift
                    && (x.mode.is_none() || b.mode.is_none() || x.mode == b.mode)
            });
            if twin {
                let deck = target.deck().unwrap_or(0);
                let (c, a) = (deck_letter(deck as usize), deck_letter(deck as usize - 2));
                if self
                    .draft
                    .binding_for(Target::DeckSelect { deck })
                    .is_none()
                {
                    self.say(
                        Tone::Warn,
                        format!(
                            "{label} is what deck {a} sends, and the DECK button for deck {c} isn't mapped, \
                             so Tandem can't tell the two decks apart. Go back and map the DECK button, \
                             or switch the side to deck {c} and try again."
                        ),
                    );
                    return;
                }
                b.layered = true;
                if !self.shared[side_of(deck)] {
                    self.said = Some(Said {
                        text: format!(
                            "{label} is what deck {a} sends. If the side isn't on deck {c} yet, switch it and \
                             do the step again. If it is, this controller sends the same for both decks, \
                             and Tandem will keep track of the DECK button itself."
                        ),
                        tone: Tone::Warn,
                        action: Some(format!("It's on deck {c}: decks {a} and {c} share their controls")),
                    });
                    self.offer = Some(Offer::Share(b));
                    return;
                }
            }
        }
        // Used for something else already?
        let toggles = |x: &Binding| {
            matches!(
                (x.target, target),
                (Target::DeckSelect { deck: a }, Target::DeckSelect { deck: b })
                    if a != b && side_of(a) == side_of(b)
            )
        };
        let twin = self.other_deck(target).filter(|_| b.layered);
        let others: Vec<Binding> = self
            .draft
            .bindings
            .iter()
            .filter(|x| x.target != target && Some(x.target) != twin && !toggles(x))
            .filter(|x| x.overlaps(&b))
            .cloned()
            .collect();
        let mut taken = Vec::new();
        for x in others {
            let made = self.made(&x);
            let pending = self
                .steps
                .iter()
                .any(|s| s.target == x.target && s.state == State::Todo);
            if pending && !made {
                // Bound before this run, and about to be asked again.
                self.draft.bindings.retain(|y| *y != x);
            } else if !taken.contains(&x.target) {
                taken.push(x.target);
            }
        }
        if !taken.is_empty() {
            let names: Vec<String> = taken.iter().map(|t| t.label()).collect();
            self.said = Some(Said {
                text: format!(
                    "{} is already used for {}. Press the control this step asks for.",
                    b.label(),
                    names.join(" and ")
                ),
                tone: Tone::Warn,
                action: Some("Use it here instead".into()),
            });
            self.offer = Some(Offer::Take(b, taken));
            return;
        }
        self.bind(i, b);
    }

    /// Whether a binding was made in this run.
    fn made(&self, b: &Binding) -> bool {
        self.steps
            .iter()
            .any(|s| s.target == b.target && s.made.contains(&(b.trigger, b.shift)))
    }

    /// Put a binding in the mapping for step `i`, and move on.
    fn bind(&mut self, i: usize, b: Binding) {
        let target = b.target;
        self.put(i, b.clone());
        self.steps[i].state = State::Done;
        let what = match self.steps[i].ask {
            Ask::Move(_) | Ask::ShiftMove if b.invert => " (runs backwards: inverted)",
            Ask::Turn | Ask::TurnEdge => match (b.rel, b.invert) {
                (RelMode::Offset64, false) => " (counts around 64)",
                (RelMode::Offset64, true) => " (counts around 64, backwards)",
                (RelMode::TwosComplement, false) => " (counts 1 and 127)",
                (RelMode::TwosComplement, true) => " (counts 1 and 127, backwards)",
            },
            _ => "",
        };
        self.say(
            Tone::Good,
            format!("✔ {} is {}{what}", target.label(), b.label()),
        );
        if target == Target::Shift {
            // It's still held.
            self.shift.insert(b.trigger);
        }
        self.hush = Some((b.trigger, self.now));
        // SHIFT + a hot cue pad clears it: so do the others.
        if let (Target::HotCueClear { deck, .. }, true) = (target, b.shift) {
            self.fill_clears(deck, b.mode, b.layered);
        }
        // A deck that shares its controls with another has the rest of
        // them filled in from that deck's.
        if b.layered {
            if let Some(deck) = target.deck().filter(|d| *d >= 2) {
                self.fill_deck(deck);
            }
        }
        self.go(self.next_todo(i + 1));
    }

    /// Put a binding in the mapping as step `i`'s, in place of what the
    /// step made before.
    fn put(&mut self, i: usize, b: Binding) {
        let target = b.target;
        let made = std::mem::take(&mut self.steps[i].made);
        self.draft
            .bindings
            .retain(|x| !(x.target == target && made.contains(&(x.trigger, x.shift))));
        let same = self
            .draft
            .bindings
            .iter_mut()
            .find(|x| x.target == target && x.trigger == b.trigger && x.shift == b.shift);
        match same {
            // The control it has already: what was found out about it now
            // counts, and its light stays.
            Some(x) => {
                (x.invert, x.rel, x.mode) = (b.invert, b.rel, b.mode);
                x.layered |= b.layered;
            }
            None => {
                if !self.steps[i].adds && self.cleared.insert(target) {
                    let kept: Vec<(Trigger, bool)> = self
                        .steps
                        .iter()
                        .filter(|s| s.target == target)
                        .flat_map(|s| s.made.iter().copied())
                        .collect();
                    self.draft
                        .bindings
                        .retain(|x| x.target != target || kept.contains(&(x.trigger, x.shift)));
                }
                self.draft.bindings.push(b.clone());
            }
        }
        self.steps[i].made = vec![(b.trigger, b.shift)];
        if b.layered {
            // Its twin on the side's other deck takes turns with it.
            for x in &mut self.draft.bindings {
                let twin = match (x.target.deck(), target.deck()) {
                    (Some(a), Some(d)) => {
                        a != d && side_of(a) == side_of(d) && x.target.with_deck(d) == target
                    }
                    _ => false,
                };
                if twin && x.trigger == b.trigger && x.shift == b.shift {
                    x.layered = true;
                }
            }
        }
    }

    /// SHIFT + pad cleared one hot cue of `deck`: the other pads clear
    /// theirs the same way.
    fn fill_clears(&mut self, deck: u8, mode: Option<PadMode>, layered: bool) {
        for i in 0..self.steps.len() {
            let Target::HotCueClear { deck: d, slot } = self.steps[i].target else {
                continue;
            };
            if d != deck || self.steps[i].state != State::Todo {
                continue;
            }
            let pad = self
                .draft
                .bindings
                .iter()
                .find(|b| b.target == Target::HotCue { deck, slot } && !b.shift)
                .map(|b| b.trigger);
            let Some(trigger) = pad else {
                continue;
            };
            let b = Binding {
                trigger,
                target: self.steps[i].target,
                invert: false,
                rel: RelMode::Offset64,
                shift: true,
                mode,
                light: None,
                layered,
            };
            if self.draft.bindings.iter().any(|x| x.overlaps(&b)) {
                continue;
            }
            self.put(i, b);
            self.steps[i].state = State::Filled;
        }
    }

    /// Deck C or D shares its controls with deck A or B: every control of
    /// its deck section still to do is bound as the other deck's is. Its
    /// mixer channel is asked for, since a mixer may well have four.
    fn fill_deck(&mut self, deck: u8) {
        let mut filled: Vec<Target> = Vec::new();
        for i in 0..self.steps.len() {
            let target = self.steps[i].target;
            let part = self.groups[self.steps[i].group].part;
            if target.deck() != Some(deck)
                || self.steps[i].state != State::Todo
                || part == Part::Channel
                || matches!(target, Target::DeckSelect { .. })
                || self.steps[i].ask == Ask::FullTurn
            {
                continue;
            }
            if filled.contains(&target) {
                // A wheel's edge, filled in with its top.
                self.steps[i].state = State::Filled;
                continue;
            }
            let other = target.with_deck(deck - 2);
            let mode = pad_mode(part);
            let theirs: Vec<Binding> = self
                .draft
                .bindings
                .iter()
                .filter(|b| {
                    b.target == other && (b.mode.is_none() || mode.is_none() || b.mode == mode)
                })
                .cloned()
                .collect();
            if self.draft.binding_for(target).is_some() {
                continue;
            }
            if theirs.is_empty() {
                // The other deck hasn't got one either.
                self.steps[i].state = State::Skipped;
                continue;
            }
            for x in &mut self.draft.bindings {
                if x.target == other {
                    x.layered = true;
                }
            }
            for mut b in theirs {
                b.target = target;
                b.layered = true;
                self.steps[i].made.push((b.trigger, b.shift));
                self.draft.bindings.push(b);
            }
            self.steps[i].state = State::Filled;
            filled.push(target);
        }
    }

    /// What a message does in the mapping as it is: "Note 0x0b · ch 1 →
    /// Deck A · Play/Pause".
    fn describe(&self, raw: RawMidi, trig: Trigger, value: f32) -> String {
        let shift = !self.shift.is_empty();
        let mut uses: Vec<&Binding> = self.uses(trig).filter(|b| shift || !b.shift).collect();
        if shift && uses.iter().any(|b| b.shift) {
            uses.retain(|b| b.shift);
        }
        if uses.is_empty() {
            return format!("{} → nothing is mapped to it", raw.label());
        }
        let mut names: Vec<String> = Vec::new();
        for b in uses {
            let mut name = b.target.label();
            if let Some(m) = b.mode {
                name.push_str(&format!(" (pad mode: {})", m.label()));
            }
            match (b.target.kind(), raw) {
                (crate::mapping::TargetKind::Absolute, _) => {
                    let v = if b.invert { 1.0 - value } else { value };
                    name.push_str(&format!(" at {:.0}%", v * 100.0));
                }
                (crate::mapping::TargetKind::Relative, RawMidi::Cc { val, .. }) => {
                    let ticks = match b.rel {
                        RelMode::Offset64 => val as i32 - 64,
                        RelMode::TwosComplement if val < 64 => val as i32,
                        RelMode::TwosComplement => val as i32 - 128,
                    };
                    let ticks = if b.invert { -ticks } else { ticks };
                    name.push_str(if ticks >= 0 {
                        ", clockwise"
                    } else {
                        ", anticlockwise"
                    });
                }
                _ => {}
            }
            if !names.contains(&name) {
                names.push(name);
            }
        }
        let shift = if shift { "SHIFT + " } else { "" };
        format!("{shift}{} → {}", trig.label(), names.join(" / "))
    }

    /// The mapping the run made, tidied: without what turned out not to be
    /// needed (see the module's notes).
    pub fn finish(&self) -> Mapping {
        let mut m = self.draft.clone();
        // Pads that never send the same in two pad modes need no pad mode
        // buttons: the controller keeps track of its pad modes itself.
        for deck in 0..DECKS as u8 {
            let pad = |b: &Binding| b.target.is_pad() && b.target.deck() == Some(deck);
            let pads: Vec<&Binding> = m.bindings.iter().filter(|b| pad(b)).collect();
            let shared = pads.iter().any(|a| {
                pads.iter().any(|b| {
                    a.trigger == b.trigger
                        && a.mode.is_some()
                        && b.mode.is_some()
                        && a.mode != b.mode
                })
            });
            if !shared {
                let had = pads.iter().any(|b| b.mode.is_some());
                for b in m.bindings.iter_mut().filter(|b| pad(b)) {
                    b.mode = None;
                }
                // The buttons this run mapped go, and any that only the
                // modes just dropped were for.
                m.bindings.retain(|b| {
                    !matches!(b.target, Target::PadMode { deck: d, .. } if d == deck)
                        || !(had || self.made(b))
                });
            }
        }
        // A SHIFT that changes what the controls send needs no SHIFT of
        // Tandem's: drop the one this run mapped, once a SHIFT + step has
        // shown that.
        let asked = self
            .steps
            .iter()
            .any(|s| matches!(s.ask, Ask::ShiftPress | Ask::ShiftMove) && s.state == State::Done);
        if asked && !m.bindings.iter().any(|b| b.shift) {
            m.bindings
                .retain(|b| b.target != Target::Shift || !self.made(b));
        }
        // One DECK button for a side that shares its controls: it switches
        // back as well.
        for (lower, upper) in [(0u8, 2u8), (1, 3)] {
            let layered = m.bindings.iter().any(|b| {
                b.layered
                    && b.target
                        .deck()
                        .is_some_and(|d| side_of(d) == side_of(lower))
            });
            let back = m.binding_for(Target::DeckSelect { deck: lower }).is_some();
            if let (true, false, Some(b)) = (
                layered,
                back,
                m.binding_for(Target::DeckSelect { deck: upper }).cloned(),
            ) {
                m.bindings.push(Binding {
                    target: Target::DeckSelect { deck: lower },
                    ..b
                });
            }
        }
        // An FX unit with one knob: it sets all three effects.
        for unit in 0..2u8 {
            let knob = |slot| Target::FxDepth { unit, slot };
            let alone = (1..FX_SLOTS as u8).all(|s| m.binding_for(knob(s)).is_none())
                && m.binding_for(Target::FxDepthAll { unit }).is_none();
            let mine = self
                .steps
                .iter()
                .any(|s| s.target == knob(0) && s.state == State::Done);
            if alone && mine {
                for b in m.bindings.iter_mut().filter(|b| b.target == knob(0)) {
                    b.target = Target::FxDepthAll { unit };
                }
            }
        }
        m
    }
}

/// A pretend controller that sends what a [`Mapping`] says its controls
/// send: for trying the setup without one (tests, screenshots).
pub struct Demo {
    pub controller: Mapping,
    /// Its faders and knobs are 14-bit: a fine half follows each move.
    pub fine: bool,
}

impl Demo {
    fn message(t: Trigger, v: u8) -> [u8; 3] {
        match t.kind {
            MidiKind::Note if v > 0 => [0x90 | t.channel, t.number, v],
            MidiKind::Note => [0x80 | t.channel, t.number, 0],
            MidiKind::Cc => [0xB0 | t.channel, t.number, v],
            MidiKind::PitchBend => [0xE0 | t.channel, 0, v],
        }
    }

    /// What the controller sends, and how long (us) after the message
    /// before, when a DJ does what step `i` of `wizard` asks. Nothing, for
    /// a control it doesn't have.
    pub fn play(&self, wizard: &Wizard, i: usize) -> Vec<(i64, [u8; 3])> {
        let step = &wizard.steps[i];
        let part = wizard.groups[step.group].part;
        let mode = pad_mode(part);
        let mut mine: Vec<&Binding> = self
            .controller
            .bindings
            .iter()
            .filter(|b| {
                b.target == step.target && (b.mode.is_none() || mode.is_none() || b.mode == mode)
            })
            .collect();
        // A loop button comes before the pad with the same loop, a SHIFT on
        // the left before one on the right, a wheel's top before its edge.
        let second = step.adds && mine.len() > 1;
        let b = match (mine.len(), second) {
            (0, _) => return Vec::new(),
            (_, true) => mine.remove(1),
            _ => mine.remove(0),
        };
        let shift = self
            .controller
            .bindings
            .iter()
            .filter(|x| x.target == Target::Shift)
            .min_by_key(|x| x.trigger.channel != b.trigger.channel)
            .map(|x| x.trigger);
        let mut out = Vec::new();
        let held = matches!(step.ask, Ask::ShiftPress | Ask::ShiftMove) || b.shift;
        if let (true, Some(s)) = (held, shift) {
            out.push((20_000, Self::message(s, 127)));
        }
        let value = |out: &mut Vec<(i64, [u8; 3])>, v: u8| {
            out.push((10_000, Self::message(b.trigger, v)));
            if self.fine && b.trigger.kind == MidiKind::Cc && b.trigger.number < 32 {
                let fine = Trigger::cc(b.trigger.channel, b.trigger.number + 32);
                out.push((0, Self::message(fine, v.wrapping_mul(37) & 0x7F)));
            }
        };
        let tick = |ticks: i32| {
            let ticks = if b.invert { -ticks } else { ticks };
            match b.rel {
                RelMode::Offset64 => (64 + ticks) as u8,
                RelMode::TwosComplement => (ticks & 0x7F) as u8,
            }
        };
        match step.ask {
            Ask::Press | Ask::ShiftPress | Ask::Touch => {
                out.push((20_000, Self::message(b.trigger, 127)));
                out.push((80_000, Self::message(b.trigger, 0)));
            }
            Ask::Turn | Ask::TurnEdge => {
                for _ in 0..12 {
                    out.push((8_000, Self::message(b.trigger, tick(1))));
                }
            }
            Ask::FullTurn => {
                let mut left = self.controller.ticks_per_rev() as i32;
                while left > 0 {
                    let n = left.min(16);
                    out.push((1_000, Self::message(b.trigger, tick(n))));
                    left -= n;
                }
            }
            Ask::Move(end) => {
                // Away from the end it's to be left at, then all the way.
                let up = (end == End::Top) != b.invert;
                let sweep: Vec<u8> = (0..=127).step_by(7).chain([127]).collect();
                let from = if up { 90 } else { 37 };
                let mut v: Vec<u8> = sweep.iter().rev().copied().filter(|&v| v <= from).collect();
                v.extend(sweep.iter().copied());
                for x in v {
                    value(&mut out, if up { x } else { 127 - x });
                }
            }
            Ask::ShiftMove => {
                for v in (40..=100).step_by(5) {
                    value(&mut out, v);
                }
            }
        }
        if let (true, Some(s)) = (held, shift) {
            out.push((400_000, Self::message(s, 0)));
        }
        out
    }

    /// Go through the rest of `wizard`'s steps as a DJ with this controller
    /// would, from shared time `t`: doing each step, saying yes when asked
    /// whether two decks share their controls, and skipping what the
    /// controller doesn't have or the setup won't take. Returns the time
    /// afterwards and the steps that were refused.
    pub fn run(&self, wizard: &mut Wizard, t: i64) -> (i64, Vec<usize>) {
        self.run_to(wizard, t, usize::MAX)
    }

    /// The same, stopping once the setup has reached step `stop`.
    pub fn run_to(&self, wizard: &mut Wizard, mut t: i64, stop: usize) -> (i64, Vec<usize>) {
        let mut refused = Vec::new();
        while let Some(i) = wizard.at().filter(|&i| i < stop) {
            let ask = wizard.steps[i].ask;
            let messages = self.play(wizard, i);
            if messages.is_empty() {
                wizard.skip();
                continue;
            }
            for (dt, bytes) in messages {
                // Frames go by while the control is still.
                for _ in 0..dt / 100_000 {
                    t += 100_000;
                    wizard.tick(t);
                }
                t += dt % 100_000;
                if let Some(raw) = crate::mapping::parse(&bytes) {
                    wizard.feed(raw, t);
                }
                wizard.tick(t);
            }
            for _ in 0..5 {
                t += 100_000;
                wizard.tick(t);
            }
            if matches!(wizard.offer, Some(Offer::Share(_))) {
                wizard.accept();
            }
            if ask == Ask::FullTurn {
                wizard.confirm();
            }
            if wizard.at() == Some(i) {
                refused.push(i);
                wizard.skip();
            }
            t += 300_000;
        }
        (t, refused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{builtin_preset, parse, MidiEngine, MidiOut};
    use crate::model::{Action, RoomState};
    use crate::render::LocalParams;

    /// What a binding does, without its light (the setup can't know what a
    /// controller's lights take).
    type Key = (Trigger, Target, bool, RelMode, bool, Option<PadMode>, bool);

    fn key(b: &Binding) -> Key {
        (
            b.trigger, b.target, b.invert, b.rel, b.shift, b.mode, b.layered,
        )
    }

    fn keys(m: &Mapping) -> Vec<Key> {
        let mut v: Vec<Key> = m.bindings.iter().map(key).collect();
        v.sort_by_key(|k| format!("{k:?}"));
        v
    }

    /// The bindings of `a` that `b` lacks, by name.
    fn missing(a: &Mapping, b: &Mapping) -> Vec<String> {
        let have = keys(b);
        keys(a)
            .into_iter()
            .filter(|k| !have.contains(k))
            .map(|k| format!("{} ← {:?}", k.1.label(), k))
            .collect()
    }

    /// Past the SHIFT steps, where a run asks for SHIFT at all.
    fn past_shift(w: &mut Wizard) {
        while w.step().is_some_and(|s| s.target == Target::Shift) {
            w.skip();
        }
    }

    fn all_sections() -> Vec<Section> {
        Section::ALL.to_vec()
    }

    /// Run the whole setup from nothing with a pretend controller.
    fn learn(controller: &Mapping, fine: bool, setup: Setup) -> (Wizard, Vec<usize>) {
        let demo = Demo {
            controller: controller.clone(),
            fine,
        };
        let mut w = Wizard::new(Mapping::empty(&controller.device), setup);
        let (_, refused) = demo.run(&mut w, 1_000_000);
        (w, refused)
    }

    fn refusals(w: &Wizard, refused: &[usize]) -> Vec<String> {
        refused
            .iter()
            .map(|&i| format!("{}: {}", w.steps[i].target.label(), w.steps[i].title))
            .collect()
    }

    #[test]
    fn a_pioneer_with_its_own_shift_and_pad_modes_is_learned_as_its_preset() {
        let preset = builtin_preset("DDJ-FLX4").unwrap();
        let (w, refused) = learn(&preset, true, Setup::default());
        assert_eq!(refusals(&w, &refused), [] as [String; 0]);
        let made = w.finish();
        // Everything the preset binds, but a third message the jog wheel
        // sends with vinyl mode off: nobody is asked for that.
        let left: Vec<String> = missing(&preset, &made);
        assert_eq!(
            left,
            [0u8, 1].map(|d| {
                let k = key(&Binding {
                    trigger: Trigger::cc(d, 0x23),
                    target: Target::Jog { deck: d },
                    invert: false,
                    rel: RelMode::Offset64,
                    shift: false,
                    mode: None,
                    light: None,
                    layered: false,
                });
                format!("{} ← {:?}", k.1.label(), k)
            })
        );
        assert_eq!(missing(&made, &preset), [] as [String; 0]);
        assert_eq!(made.jog_ticks_per_rev, Some(720.0));
        // SHIFT changes what its controls send, and the pads send other
        // notes in each mode: nothing is tied to SHIFT or a pad mode.
        assert!(made
            .bindings
            .iter()
            .all(|b| !b.shift && b.mode.is_none() && !b.layered));
        // The fourteen-bit faders' fine halves were never taken for
        // controls of their own.
        assert!(!made
            .bindings
            .iter()
            .any(|b| b.trigger.kind == MidiKind::Cc && (0x24..0x40).contains(&b.trigger.number)));
        let (done, total) = w.progress();
        assert!(done > 100 && done < total, "{done} of {total}");
        assert!(w.changed());
    }

    #[test]
    fn a_shift_button_that_also_changes_the_messages_is_dropped_again() {
        // A Pioneer's SHIFT sends a note of its own as well.
        let mut controller = builtin_preset("DDJ-FLX4").unwrap();
        for ch in 0..2 {
            controller.bind(Trigger::note(ch, 0x3F), Target::Shift);
        }
        let demo = Demo {
            controller: controller.clone(),
            fine: true,
        };
        let mut w = Wizard::new(Mapping::empty("test"), Setup::default());
        // It's mapped while the run needs it…
        let mut t = 0;
        for i in 0..2 {
            for (dt, bytes) in demo.play(&w, i) {
                t += dt;
                w.feed(parse(&bytes).unwrap(), t);
            }
            t += 500_000;
        }
        assert_eq!(w.draft().bindings.len(), 2);
        assert!(w.draft().bindings.iter().all(|b| b.target == Target::Shift));
        let (_, refused) = demo.run(&mut w, t);
        assert_eq!(refusals(&w, &refused), [] as [String; 0]);
        // …and gone from what's kept: no control needed it.
        let made = w.finish();
        assert!(w.draft().binding_for(Target::Shift).is_some());
        assert_eq!(made.binding_for(Target::Shift), None);
        assert_eq!(
            made.binding_for(Target::BackToStart { deck: 0 })
                .map(|b| (b.trigger, b.shift)),
            Some((Trigger::note(0, 0x48), false))
        );
        assert_eq!(
            missing(&made, &builtin_preset("DDJ-FLX4").unwrap()),
            [] as [String; 0]
        );
    }

    #[test]
    fn a_denon_with_tandems_shift_and_pad_modes_is_learned_as_its_preset() {
        let preset = builtin_preset("SC LIVE 4").unwrap();
        let (w, refused) = learn(&preset, false, Setup::default());
        assert_eq!(refusals(&w, &refused), [] as [String; 0]);
        let made = w.finish();
        assert_eq!(missing(&preset, &made), [] as [String; 0]);
        assert_eq!(missing(&made, &preset), [] as [String; 0]);
        assert_eq!(made.jog_ticks_per_rev, Some(1000.0));
        // SHIFT + pad 1 was asked for; pads 2 to 8 were filled in.
        let clears: Vec<State> = w
            .steps
            .iter()
            .filter(|s| matches!(s.target, Target::HotCueClear { deck: 0, .. }))
            .map(|s| s.state)
            .collect();
        assert_eq!(clears[0], State::Done);
        assert!(
            clears[1..].iter().all(|s| *s == State::Filled),
            "{clears:?}"
        );
        // The pad on the AUTO LOOP button's loop shows its own note, not
        // the button's.
        let pad = w
            .steps
            .iter()
            .rfind(|s| s.target == Target::BeatLoop { deck: 0, size: 4 })
            .unwrap();
        assert_eq!(
            w.bound(pad).as_deref(),
            Some("Note 0x13 · ch 5 (pad mode: beat loops)")
        );
        // The jog wheel's edge sends what its top does: one binding.
        let jogs = made
            .bindings
            .iter()
            .filter(|b| b.target == Target::Jog { deck: 0 })
            .count();
        assert_eq!(jogs, 1);
        // And it plays: SHIFT + CUE goes back to the start, and the pads
        // roll in ROLL mode.
        let mut e = MidiEngine::new(made, 0.08);
        let (room, local) = (RoomState::default(), LocalParams::default());
        e.handle(&[0x94, 0x1C, 0x7F], &room, &local);
        assert_eq!(
            e.handle(&[0x94, 0x09, 0x7F], &room, &local),
            [MidiOut::Action(Action::Seek { deck: 0, pos: 0.0 })]
        );
        e.handle(&[0x94, 0x09, 0x00], &room, &local);
        e.handle(&[0x94, 0x1C, 0x00], &room, &local);
        e.handle(&[0x94, 0x0D, 0x7F], &room, &local);
        assert!(matches!(
            e.handle(&[0x94, 0x0F, 0x7F], &room, &local)[..],
            [MidiOut::Action(Action::Roll {
                deck: 0,
                on: true,
                ..
            })]
        ));
    }

    #[test]
    fn a_hercules_with_four_pads_and_a_backwards_tempo_fader_is_learned_as_its_preset() {
        let preset = builtin_preset("DJControl Inpulse 200 MK2").unwrap();
        let setup = Setup {
            pads: 4,
            ..Setup::default()
        };
        let (w, refused) = learn(&preset, true, setup);
        assert_eq!(refusals(&w, &refused), [] as [String; 0]);
        let made = w.finish();
        assert_eq!(missing(&preset, &made), [] as [String; 0]);
        assert_eq!(missing(&made, &preset), [] as [String; 0]);
        let tempo = made.binding_for(Target::Tempo { deck: 0 }).unwrap();
        assert!(tempo.invert, "left at +, its value was low");
        assert!(
            made.binding_for(Target::TempoShift { deck: 0 })
                .unwrap()
                .invert
        );
        assert!(!made.binding_for(Target::Volume { deck: 0 }).unwrap().invert);
        assert!(
            !made.binding_for(Target::MasterLevel).unwrap().invert,
            "left at the bottom"
        );
        let jog: Vec<(u8, RelMode)> = made
            .bindings
            .iter()
            .filter(|b| b.target == Target::Jog { deck: 1 })
            .map(|b| (b.trigger.number, b.rel))
            .collect();
        assert_eq!(
            jog,
            [
                (0x0A, RelMode::TwosComplement),
                (0x09, RelMode::TwosComplement)
            ]
        );
        assert_eq!(made.jog_ticks_per_rev, Some(248.0));
        // Four pads: no steps for pads 5 to 8.
        assert!(!w
            .steps
            .iter()
            .any(|s| matches!(s.target, Target::HotCue { slot: 4.., .. })));
        assert_eq!(pad_sizes(4), (vec![2, 3, 4, 5], vec![1, 2, 3, 4]));
    }

    #[test]
    fn deck_buttons_that_change_the_messages_give_four_decks_of_plain_bindings() {
        let preset = builtin_preset("DDJ-FLX6").unwrap();
        let setup = Setup {
            decks: Decks::TwoSwitched,
            ..Setup::default()
        };
        let (w, refused) = learn(&preset, true, setup);
        assert_eq!(refusals(&w, &refused), [] as [String; 0]);
        let made = w.finish();
        // All of the preset but the effects (not asked for here) and the
        // jog wheels' third message.
        let left: Vec<Target> = keys(&preset)
            .into_iter()
            .filter(|k| !keys(&made).contains(k))
            .map(|k| k.1)
            .collect();
        assert_eq!(left.iter().filter(|t| t.part() != Part::Fx).count(), 4);
        assert!(left
            .iter()
            .all(|t| t.part() == Part::Fx || matches!(t, Target::Jog { .. })));
        assert_eq!(missing(&made, &preset), [] as [String; 0]);
        assert!(made.bindings.iter().all(|b| !b.layered));
        for deck in 0..4 {
            assert_eq!(
                made.binding_for(Target::DeckSelect { deck })
                    .map(|b| b.trigger),
                Some(Trigger::note(deck, 0x3C))
            );
        }
        // Each deck was asked for in full: nothing was filled in.
        assert!(!w.steps.iter().any(|s| s.state == State::Filled));
    }

    /// The SC Live 4 as it really is: its DECK buttons change nothing it
    /// sends, so decks C and D take turns with A and B on the same
    /// controls, and its mixer has four channels.
    fn sc_live_4_on_four_decks() -> Mapping {
        let mut m = builtin_preset("SC LIVE 4").unwrap();
        let mut more = Vec::new();
        for b in &mut m.bindings {
            let Some(deck) = b.target.deck() else {
                continue;
            };
            let mut c = b.clone();
            c.target = b.target.with_deck(deck + 2);
            if b.target.part() == Part::Channel {
                c.trigger.channel += 2;
            } else {
                (b.layered, c.layered) = (true, true);
            }
            more.push(c);
        }
        m.bindings.extend(more);
        for deck in 0..4u8 {
            m.bind(
                Trigger::note(4 + deck % 2, 0x1F),
                Target::DeckSelect { deck },
            );
        }
        m
    }

    #[test]
    fn deck_buttons_that_change_nothing_make_two_decks_take_turns() {
        let controller = sc_live_4_on_four_decks();
        let setup = Setup {
            decks: Decks::TwoSwitched,
            ..Setup::default()
        };
        let demo = Demo {
            controller: controller.clone(),
            fine: false,
        };
        let mut w = Wizard::new(Mapping::empty("test"), setup);
        // Up to deck C's first control, as any controller.
        let first = w
            .steps
            .iter()
            .position(|s| s.target == Target::JogTouch { deck: 2 })
            .unwrap();
        let (t, refused) = demo.run_to(&mut w, 0, first);
        assert_eq!(refusals(&w, &refused), [] as [String; 0]);
        assert_eq!(w.at(), Some(first));
        assert!(w
            .draft()
            .binding_for(Target::DeckSelect { deck: 2 })
            .is_some());
        assert!(w.draft().bindings.iter().all(|b| !b.layered));
        // Deck C's jog wheel sends what deck A's does: the DJ is asked
        // whether the side really is on deck C.
        w.feed(parse(&[0x94, 0x21, 0x7F]).unwrap(), t);
        assert_eq!(w.at(), Some(first), "not taken yet");
        let said = w.said().unwrap().clone();
        assert_eq!(said.tone, Tone::Warn);
        assert_eq!(
            said.action.as_deref(),
            Some("It's on deck C: decks A and C share their controls")
        );
        assert!(said.text.contains("is what deck A sends"), "{}", said.text);
        w.feed(parse(&[0x84, 0x21, 0x00]).unwrap(), t + 50_000);
        w.accept();
        // The rest of deck C's deck section is filled in from deck A's;
        // its mixer channel is asked for.
        let next = w.step().unwrap();
        assert_eq!(next.target, Target::Trim { deck: 2 });
        let c: Vec<State> = w
            .steps
            .iter()
            .filter(|s| s.target.deck() == Some(2) && s.target.part() != Part::Channel)
            .map(|s| s.state)
            .collect();
        assert!(
            c.iter()
                .all(|s| matches!(s, State::Done | State::Filled | State::Skipped)),
            "{c:?}"
        );
        assert!(c.iter().filter(|s| **s == State::Filled).count() > 40);
        // Deck D the same way, then back to A and B.
        let (_, refused) = demo.run(&mut w, t + 1_000_000);
        assert_eq!(refusals(&w, &refused), [] as [String; 0]);
        let made = w.finish();
        assert_eq!(missing(&controller, &made), [] as [String; 0]);
        assert_eq!(missing(&made, &controller), [] as [String; 0]);
        // And it plays: PLAY is deck A's, and after DECK it's deck C's.
        let mut e = MidiEngine::new(made, 0.08);
        let (room, local) = (RoomState::default(), LocalParams::default());
        let play = |deck| vec![MidiOut::Action(Action::Play { deck })];
        assert_eq!(e.handle(&[0x94, 0x0A, 0x7F], &room, &local), play(0));
        e.handle(&[0x94, 0x1F, 0x7F], &room, &local);
        assert_eq!(e.handle(&[0x94, 0x0A, 0x7F], &room, &local), play(2));
        e.handle(&[0x94, 0x1F, 0x7F], &room, &local);
        assert_eq!(e.handle(&[0x94, 0x0A, 0x7F], &room, &local), play(0));
        // The right side kept its own turns.
        assert_eq!(e.handle(&[0x95, 0x0A, 0x7F], &room, &local), play(1));
    }

    #[test]
    fn a_shared_control_is_refused_while_the_deck_button_is_unknown() {
        let setup = Setup {
            decks: Decks::TwoSwitched,
            sections: vec![Section::Transport],
            ..Setup::default()
        };
        let mut w = Wizard::new(Mapping::empty("test"), setup);
        let step = |w: &Wizard, t: Target| w.steps.iter().position(|s| s.target == t).unwrap();
        w.jump_to(step(&w, Target::Play { deck: 0 }));
        w.feed(parse(&[0x90, 0x0B, 0x7F]).unwrap(), 0);
        // The DECK step was skipped: deck C's PLAY can't be the same note.
        w.jump_to(step(&w, Target::Play { deck: 2 }));
        w.feed(parse(&[0x90, 0x0B, 0x7F]).unwrap(), 1_000_000);
        let said = w.said().unwrap();
        assert_eq!((said.tone, &said.action), (Tone::Warn, &None));
        assert!(said
            .text
            .contains("the DECK button for deck C isn't mapped"));
        assert_eq!(w.draft().bindings.len(), 1);
        // A note of its own is fine.
        w.feed(parse(&[0x92, 0x0B, 0x7F]).unwrap(), 2_000_000);
        assert_eq!(
            w.draft()
                .binding_for(Target::Play { deck: 2 })
                .map(|b| (b.trigger, b.layered)),
            Some((Trigger::note(2, 0x0B), false))
        );
    }

    #[test]
    fn one_deck_button_mapped_once_also_switches_back() {
        let setup = Setup {
            decks: Decks::TwoSwitched,
            sections: vec![Section::Transport],
            ..Setup::default()
        };
        let mut w = Wizard::new(Mapping::empty("test"), setup);
        let step = |w: &Wizard, t: Target| w.steps.iter().position(|s| s.target == t).unwrap();
        w.jump_to(step(&w, Target::Play { deck: 0 }));
        w.feed(parse(&[0x90, 0x0B, 0x7F]).unwrap(), 0);
        w.jump_to(step(&w, Target::DeckSelect { deck: 2 }));
        w.feed(parse(&[0x90, 0x1F, 0x7F]).unwrap(), 1_000_000);
        w.jump_to(step(&w, Target::Play { deck: 2 }));
        // The DECK button itself isn't taken for PLAY.
        w.feed(parse(&[0x90, 0x1F, 0x7F]).unwrap(), 2_000_000);
        assert!(w.said().unwrap().text.contains("is a DECK button"));
        w.feed(parse(&[0x90, 0x0B, 0x7F]).unwrap(), 3_000_000);
        w.accept();
        // The DJ never gets to "back to deck A".
        let made = w.finish();
        let deck = |d| {
            made.binding_for(Target::DeckSelect { deck: d })
                .map(|b| b.trigger)
        };
        assert_eq!(deck(2), Some(Trigger::note(0, 0x1F)));
        assert_eq!(deck(0), deck(2));
        assert_eq!(deck(1), None);
    }

    /// A run over the mixer only, at deck A's TRIM.
    fn mixer() -> Wizard {
        let setup = Setup {
            sections: vec![Section::Mixer],
            ..Setup::default()
        };
        let mut w = Wizard::new(Mapping::empty("test"), setup);
        past_shift(&mut w);
        assert_eq!(w.step().unwrap().target, Target::Trim { deck: 0 });
        w
    }

    /// Moves a control change through `values`, 10 ms apart, from `t`.
    fn sweep(w: &mut Wizard, t: &mut i64, ch: u8, cc: u8, values: impl IntoIterator<Item = u8>) {
        for v in values {
            *t += 10_000;
            w.feed(RawMidi::Cc { ch, cc, val: v }, *t);
            w.tick(*t);
        }
    }

    /// Lets `ms` go by, a frame at a time.
    fn wait(w: &mut Wizard, t: &mut i64, ms: i64) {
        for _ in 0..ms / 20 {
            *t += 20_000;
            w.tick(*t);
        }
    }

    #[test]
    fn a_knob_is_taken_once_it_rests_at_the_end_it_was_asked_for() {
        let mut w = mixer();
        let mut t = 0;
        // A worn pot twitching by itself isn't a move.
        sweep(&mut w, &mut t, 0, 0x13, [64, 65, 64, 65, 64]);
        wait(&mut w, &mut t, 500);
        assert_eq!(w.said(), None);
        // Not far enough.
        sweep(&mut w, &mut t, 0, 0x04, 60..70);
        wait(&mut w, &mut t, 500);
        let said = w.said().unwrap();
        assert_eq!(said.tone, Tone::Info);
        assert!(said.text.contains("keep going"), "{}", said.text);
        // Down, then up, and left in the middle: not yet.
        sweep(&mut w, &mut t, 0, 0x04, (0..70).rev().chain(0..80));
        wait(&mut w, &mut t, 500);
        assert!(w.said().unwrap().text.contains("leave it at the end"));
        assert_eq!(w.draft().bindings.len(), 0);
        // All the way up: taken only once it has stopped.
        sweep(&mut w, &mut t, 0, 0x04, 80..=127);
        assert_eq!(w.draft().bindings.len(), 0);
        wait(&mut w, &mut t, 200);
        assert_eq!(w.draft().bindings.len(), 0);
        wait(&mut w, &mut t, 200);
        let b = w.draft().binding_for(Target::Trim { deck: 0 }).unwrap();
        assert_eq!((b.trigger, b.invert), (Trigger::cc(0, 0x04), false));
        assert_eq!(w.said().unwrap().tone, Tone::Good);
        assert_eq!(w.step().unwrap().target, Target::Eq { deck: 0, band: 2 });
    }

    #[test]
    fn a_knob_still_coming_to_rest_isnt_taken_for_the_next_control() {
        let mut w = mixer();
        let mut t = 0;
        sweep(&mut w, &mut t, 0, 0x04, (0..100).rev().chain(0..=120));
        wait(&mut w, &mut t, 320);
        assert_eq!(w.step().unwrap().target, Target::Eq { deck: 0, band: 2 });
        // The DJ's hand is still on TRIM, and takes it back down.
        sweep(&mut w, &mut t, 0, 0x04, (0..=120).rev());
        sweep(&mut w, &mut t, 0, 0x04, 0..=127);
        wait(&mut w, &mut t, 240);
        assert_eq!(w.step().unwrap().target, Target::Eq { deck: 0, band: 2 });
        assert_eq!(w.draft().bindings.len(), 1);
        // Once it has been still, moving it again is a mistake worth a word.
        wait(&mut w, &mut t, 100);
        sweep(&mut w, &mut t, 0, 0x04, (0..=127).rev().chain(0..=127));
        wait(&mut w, &mut t, 400);
        let said = w.said().unwrap();
        assert_eq!(said.tone, Tone::Warn);
        assert!(
            said.text.contains("is already used for Deck A · Trim"),
            "{}",
            said.text
        );
        assert_eq!(said.action.as_deref(), Some("Use it here instead"));
        // The right knob.
        sweep(&mut w, &mut t, 0, 0x07, (0..=127).rev().chain(0..=127));
        wait(&mut w, &mut t, 400);
        assert_eq!(w.step().unwrap().target, Target::Eq { deck: 0, band: 1 });
        assert_eq!(w.draft().bindings.len(), 2);
    }

    #[test]
    fn a_fader_whose_fine_half_isnt_32_up_is_learned_by_its_coarse_half() {
        // The Denon SC Live 4's pitch fader: coarse on CC 0x1F, fine on
        // 0x4B, which runs through its values over and over.
        let setup = Setup {
            sections: vec![Section::Tempo],
            ..Setup::default()
        };
        let mut w = Wizard::new(Mapping::empty("test"), setup);
        past_shift(&mut w);
        let mut t = 0;
        for coarse in (0..=60u8).rev().chain(0..=127) {
            for fine in [5u8, 40, 75, 110] {
                t += 2_000;
                w.feed(
                    RawMidi::Cc {
                        ch: 4,
                        cc: 0x1F,
                        val: coarse,
                    },
                    t,
                );
                w.feed(
                    RawMidi::Cc {
                        ch: 4,
                        cc: 0x4B,
                        val: fine,
                    },
                    t,
                );
            }
        }
        wait(&mut w, &mut t, 400);
        assert_eq!(
            w.draft()
                .binding_for(Target::Tempo { deck: 0 })
                .map(|b| b.trigger),
            Some(Trigger::cc(4, 0x1F))
        );
    }

    #[test]
    fn the_wrong_kind_of_control_is_named_and_not_taken() {
        let mut w = mixer();
        let mut t = 0;
        // A button, while a knob is asked for.
        w.feed(parse(&[0x90, 0x0B, 0x7F]).unwrap(), t);
        assert!(w.said().unwrap().text.contains("is a button"));
        // A jog wheel going by reads as neither end of a knob.
        sweep(&mut w, &mut t, 0, 0x22, [1, 2, 3, 127, 126, 125, 1, 127]);
        wait(&mut w, &mut t, 500);
        assert_eq!(w.draft().bindings.len(), 0);
        // A knob, while a button is asked for.
        let setup = Setup {
            sections: vec![Section::Transport],
            ..Setup::default()
        };
        let mut w = Wizard::new(Mapping::empty("test"), setup);
        past_shift(&mut w);
        assert_eq!(w.step().unwrap().target, Target::Play { deck: 0 });
        sweep(&mut w, &mut t, 0, 0x13, 100..=127);
        wait(&mut w, &mut t, 200);
        assert!(w.said().unwrap().text.contains("is a knob or fader"));
        assert_eq!(w.draft().bindings.len(), 0);
        // A button that sends a control change is one, once it has shown
        // it isn't a knob going by.
        w.feed(
            RawMidi::Cc {
                ch: 0,
                cc: 0x40,
                val: 127,
            },
            t,
        );
        assert_eq!(w.draft().bindings.len(), 0);
        wait(&mut w, &mut t, 200);
        assert_eq!(
            w.draft()
                .binding_for(Target::Play { deck: 0 })
                .map(|b| b.trigger),
            Some(Trigger::cc(0, 0x40))
        );
        // Letting go of it isn't CUE.
        w.feed(
            RawMidi::Cc {
                ch: 0,
                cc: 0x40,
                val: 0,
            },
            t,
        );
        wait(&mut w, &mut t, 400);
        assert_eq!(w.step().unwrap().target, Target::Cue { deck: 0 });
        assert_eq!(w.draft().bindings.len(), 1);
        // A button on control change 33 is one, though a slider on control
        // change 1 was moved a while ago: it's no 14-bit pair.
        sweep(&mut w, &mut t, 0, 0x01, 100..=127);
        wait(&mut w, &mut t, 400);
        w.feed(
            RawMidi::Cc {
                ch: 0,
                cc: 0x21,
                val: 127,
            },
            t,
        );
        wait(&mut w, &mut t, 200);
        assert_eq!(
            w.draft()
                .binding_for(Target::Cue { deck: 0 })
                .map(|b| b.trigger),
            Some(Trigger::cc(0, 0x21))
        );
    }

    /// A run over the transport only, at deck A's PLAY.
    fn transport() -> Wizard {
        let setup = Setup {
            sections: vec![Section::Transport],
            ..Setup::default()
        };
        let mut w = Wizard::new(Mapping::empty("test"), setup);
        past_shift(&mut w);
        assert_eq!(w.step().unwrap().target, Target::Play { deck: 0 });
        w
    }

    fn press(w: &mut Wizard, t: &mut i64, ch: u8, note: u8) {
        *t += 400_000;
        w.feed(RawMidi::NoteOn { ch, note, vel: 127 }, *t);
        *t += 60_000;
        w.feed(RawMidi::NoteOff { ch, note }, *t);
    }

    #[test]
    fn a_button_used_already_is_refused_and_can_be_moved() {
        let mut w = transport();
        let mut t = 0;
        press(&mut w, &mut t, 0, 0x0B);
        assert_eq!(w.step().unwrap().target, Target::Cue { deck: 0 });
        // A bounce of the same button straight away isn't a second press.
        w.feed(
            RawMidi::NoteOn {
                ch: 0,
                note: 0x0B,
                vel: 127,
            },
            t + 20_000,
        );
        assert_eq!(w.said().unwrap().tone, Tone::Good);
        // Pressed again for CUE: refused, and still asking.
        press(&mut w, &mut t, 0, 0x0B);
        let said = w.said().unwrap().clone();
        assert_eq!(said.tone, Tone::Warn);
        assert!(
            said.text.contains("already used for Deck A · Play/Pause"),
            "{}",
            said.text
        );
        assert_eq!(w.step().unwrap().target, Target::Cue { deck: 0 });
        // The right button clears the offer.
        press(&mut w, &mut t, 0, 0x0C);
        assert_eq!(w.step().unwrap().target, Target::Sync { deck: 0 });
        w.accept();
        assert_eq!(w.draft().bindings.len(), 2);
        // "No, this one is SYNC": PLAY is to do again.
        press(&mut w, &mut t, 0, 0x0B);
        w.accept();
        assert_eq!(
            w.draft()
                .binding_for(Target::Sync { deck: 0 })
                .map(|b| b.trigger),
            Some(Trigger::note(0, 0x0B))
        );
        assert_eq!(w.draft().binding_for(Target::Play { deck: 0 }), None);
        assert_eq!(w.steps[2].state, State::Todo);
        let (done, _) = w.progress();
        assert_eq!(done, 2);
    }

    #[test]
    fn shift_steps_tell_a_shift_of_tandems_from_one_of_the_controllers() {
        // No SHIFT mapped: the same note as CUE can't be SHIFT + CUE.
        let mut w = transport();
        let mut t = 0;
        for note in [0x0B, 0x0C, 0x58] {
            press(&mut w, &mut t, 0, note);
        }
        assert_eq!(w.step().unwrap().target, Target::BackToStart { deck: 0 });
        press(&mut w, &mut t, 0, 0x0C);
        let said = w.said().unwrap();
        assert_eq!((said.tone, &said.action), (Tone::Warn, &None));
        assert!(
            said.text.contains("no SHIFT button is mapped"),
            "{}",
            said.text
        );
        // A note of its own is a plain binding.
        press(&mut w, &mut t, 0, 0x48);
        let b = w
            .draft()
            .binding_for(Target::BackToStart { deck: 0 })
            .unwrap();
        assert_eq!((b.trigger, b.shift), (Trigger::note(0, 0x48), false));

        // With SHIFT mapped, the same note is SHIFT + CUE while it's held.
        let setup = Setup {
            sections: vec![Section::Transport],
            ..Setup::default()
        };
        let mut w = Wizard::new(Mapping::empty("test"), setup);
        press(&mut w, &mut t, 0, 0x1C);
        w.skip();
        for note in [0x0B, 0x0C, 0x58] {
            press(&mut w, &mut t, 0, note);
        }
        press(&mut w, &mut t, 0, 0x0C);
        assert!(w
            .said()
            .unwrap()
            .text
            .contains("Hold SHIFT while you do it"));
        t += 400_000;
        w.feed(
            RawMidi::NoteOn {
                ch: 0,
                note: 0x1C,
                vel: 127,
            },
            t,
        );
        assert!(w.shift_held());
        press(&mut w, &mut t, 0, 0x0C);
        w.feed(RawMidi::NoteOff { ch: 0, note: 0x1C }, t);
        assert!(!w.shift_held());
        let b = w
            .draft()
            .binding_for(Target::BackToStart { deck: 0 })
            .unwrap();
        assert_eq!((b.trigger, b.shift), (Trigger::note(0, 0x0C), true));
        assert_eq!(
            w.finish().binding_for(Target::Shift).map(|b| b.trigger),
            Some(Trigger::note(0, 0x1C))
        );
    }

    #[test]
    fn back_skip_clear_and_jump() {
        let mut w = transport();
        let mut t = 0;
        press(&mut w, &mut t, 0, 0x0B);
        press(&mut w, &mut t, 0, 0x0C);
        assert_eq!(w.step().unwrap().target, Target::Sync { deck: 0 });
        // Back to CUE: it keeps its button until another is pressed.
        w.back();
        let cue = w.step().unwrap().clone();
        assert_eq!(
            (cue.target, cue.state),
            (Target::Cue { deck: 0 }, State::Done)
        );
        assert_eq!(w.bound(&cue).as_deref(), Some("Note 0x0c · ch 1"));
        // A step that hasn't mapped anything says nothing of its own.
        assert_eq!(w.bound(&w.steps[4]), None);
        assert_eq!(w.place(&cue), "Deck A · Transport · the left deck");
        press(&mut w, &mut t, 0, 0x0D);
        assert_eq!(w.draft().bindings.len(), 2);
        assert_eq!(
            w.draft()
                .binding_for(Target::Cue { deck: 0 })
                .map(|b| b.trigger),
            Some(Trigger::note(0, 0x0D))
        );
        // Skipping a step that's mapped leaves it mapped.
        w.back();
        w.skip();
        assert_eq!(w.steps[3].state, State::Done);
        // Skipping one that isn't marks it.
        assert_eq!(w.step().unwrap().target, Target::Sync { deck: 0 });
        w.skip();
        assert_eq!(w.steps[4].state, State::Skipped);
        // Clearing unmaps.
        w.back();
        w.back();
        w.clear();
        assert_eq!(w.draft().binding_for(Target::Cue { deck: 0 }), None);
        assert_eq!(w.steps[3].state, State::Skipped);
        // The rest of the group, then deck B's.
        w.skip_group();
        assert_eq!(w.step().unwrap().target, Target::Play { deck: 1 });
        assert_eq!(w.group_progress(1), (1, 6));
        // Jumping to a group lands on what it still lacks.
        w.jump(1);
        assert_eq!(w.step().unwrap().target, Target::Cue { deck: 0 });
        w.jump(2);
        w.skip_group();
        assert_eq!(w.at(), None);
        assert_eq!(w.progress(), (1, 14));
    }

    #[test]
    fn controls_mapped_already_are_left_out_or_asked_again() {
        let preset = builtin_preset("DDJ-FLX4").unwrap();
        let demo = Demo {
            controller: preset.clone(),
            fine: true,
        };
        // Skipping them: only what the preset lacks is asked for, and the
        // controller has none of it.
        let mut w = Wizard::new(preset.clone(), Setup::default());
        let asked: Vec<Target> = w
            .steps
            .iter()
            .filter(|s| s.state == State::Todo)
            .map(|s| s.target)
            .collect();
        assert_eq!(
            asked,
            [
                Target::BendDown { deck: 0 },
                Target::BendUp { deck: 0 },
                Target::BendDown { deck: 1 },
                Target::BendUp { deck: 1 },
                Target::HeadphoneLevel,
                Target::MasterLevel,
            ]
        );
        // Not SHIFT or the pad mode buttons: no step is left that would
        // use them.
        assert_eq!(w.steps[0].state, State::Skipped);
        assert_eq!(w.step().unwrap().target, Target::BendDown { deck: 0 });
        let (_, refused) = demo.run(&mut w, 0);
        assert!(refused.is_empty());
        assert!(!w.changed());
        assert_eq!(w.finish(), preset);
        // A control that's mapped is refused for another.
        let mut w = Wizard::new(preset.clone(), Setup::default());
        w.feed(parse(&[0x90, 0x0B, 0x7F]).unwrap(), 0);
        let said = w.said().unwrap();
        assert!(
            said.text.contains("already used for Deck A · Play/Pause"),
            "{}",
            said.text
        );
        // A pad mode button mapped on the way is dropped again when the
        // pads turn out not to need it.
        let mut bare = preset.clone();
        bare.bindings.retain(|b| b.target.part() != Part::Rolls);
        let mut w = Wizard::new(bare, Setup::default());
        let button = Target::PadMode {
            deck: 0,
            mode: PadMode::Rolls,
        };
        w.jump_to(w.steps.iter().position(|s| s.target == button).unwrap());
        let roll = w.step().unwrap().clone();
        assert_eq!(roll.state, State::Todo);
        w.feed(parse(&[0x90, 0x69, 0x7F]).unwrap(), 0);
        assert!(w.draft().binding_for(roll.target).is_some());
        let (_, refused) = demo.run(&mut w, 1_000_000);
        assert!(refused.is_empty());
        assert_eq!(w.finish(), {
            // The rolls are back, at the end of the list.
            let mut m = preset.clone();
            let (rolls, rest): (Vec<_>, Vec<_>) = m
                .bindings
                .into_iter()
                .partition(|b| b.target.part() == Part::Rolls);
            m.bindings = rest.into_iter().chain(rolls).collect();
            m
        });

        // Going through everything again with the same controller changes
        // nothing: not the lights' settings, nor the jog wheel's third
        // message, which nobody is asked for.
        let setup = Setup {
            skip_mapped: false,
            ..Setup::default()
        };
        let mut w = Wizard::new(preset.clone(), setup.clone());
        assert!(w.steps.iter().all(|s| s.state == State::Todo));
        let (_, refused) = demo.run(&mut w, 0);
        assert_eq!(refusals(&w, &refused), [] as [String; 0]);
        assert_eq!(w.finish(), preset);
        // With another controller on the same mapping, a control it moved
        // is bound afresh, and the one that had its message gives it up.
        let mut other = preset.clone();
        for b in &mut other.bindings {
            match b.target {
                Target::Play { deck: 0 } => b.trigger = Trigger::note(0, 0x0C),
                Target::Cue { deck: 0 } => b.trigger = Trigger::note(0, 0x0B),
                _ => {}
            }
        }
        let demo = Demo {
            controller: other.clone(),
            fine: true,
        };
        let mut w = Wizard::new(preset.clone(), setup);
        let (_, refused) = demo.run(&mut w, 0);
        assert_eq!(refusals(&w, &refused), [] as [String; 0]);
        assert_eq!(keys(&w.finish()), keys(&other));
    }

    #[test]
    fn a_full_turn_measures_the_jog_wheel() {
        let setup = Setup {
            sections: vec![Section::Jog],
            ..Setup::default()
        };
        let mut w = Wizard::new(Mapping::empty("test"), setup);
        past_shift(&mut w);
        // The platter's touch isn't its turn.
        w.jump_to(3);
        assert_eq!(w.step().unwrap().ask, Ask::Turn);
        let mut t = 0;
        w.feed(parse(&[0x90, 0x36, 0x7F]).unwrap(), t);
        assert_eq!(w.draft().bindings.len(), 0);
        // Turned clockwise, it counts down from 64: it runs backwards.
        sweep(&mut w, &mut t, 0, 0x22, [63; 12]);
        let b = w.draft().binding_for(Target::Jog { deck: 0 }).unwrap();
        assert_eq!((b.rel, b.invert), (RelMode::Offset64, true));
        // Its edge sends something else, counting the same way.
        assert_eq!(w.step().unwrap().ask, Ask::TurnEdge);
        wait(&mut w, &mut t, 400);
        sweep(
            &mut w,
            &mut t,
            0,
            0x21,
            [63, 62, 63, 63, 62, 63, 63, 63, 63],
        );
        assert_eq!(w.draft().bindings.len(), 2);
        // With the touch sensor mapped, a hand still on top isn't taken
        // for the edge sending the same as the top.
        w.jump_to(2);
        w.feed(parse(&[0x90, 0x36, 0x7F]).unwrap(), t);
        w.feed(parse(&[0x80, 0x36, 0x00]).unwrap(), t);
        w.jump_to(4);
        wait(&mut w, &mut t, 400);
        w.feed(parse(&[0x90, 0x36, 0x7F]).unwrap(), t);
        sweep(&mut w, &mut t, 0, 0x22, [63; 12]);
        assert_eq!(w.step().unwrap().ask, Ask::TurnEdge);
        assert!(w.said().unwrap().text.contains("Your hand is on top"));
        w.feed(parse(&[0x80, 0x36, 0x00]).unwrap(), t);
        sweep(&mut w, &mut t, 0, 0x21, [63; 12]);
        assert_eq!(w.step().unwrap().ask, Ask::FullTurn);
        assert_eq!(w.draft().bindings.len(), 3);
        // A full turn, a little there and back on the way.
        let turn = w.step().unwrap().clone();
        assert_eq!(turn.ask, Ask::FullTurn);
        assert_eq!(w.bound(&turn), None);
        wait(&mut w, &mut t, 400);
        sweep(&mut w, &mut t, 0, 0x22, [60; 20]);
        w.confirm();
        assert_eq!(
            w.step().unwrap().ask,
            Ask::FullTurn,
            "too short to be a turn"
        );
        sweep(&mut w, &mut t, 0, 0x22, [66; 10]);
        sweep(&mut w, &mut t, 0, 0x22, [44; 100]);
        assert_eq!(w.count(), 80 - 20 + 2000);
        w.confirm();
        assert_eq!(w.finish().jog_ticks_per_rev, Some(2060.0));
        assert_eq!(w.step().unwrap().target, Target::JogTouch { deck: 1 });
        // An endless knob that counts 1 and 127.
        let setup = Setup {
            sections: vec![Section::Browse],
            ..Setup::default()
        };
        let mut w = Wizard::new(Mapping::empty("test"), setup);
        past_shift(&mut w);
        sweep(&mut w, &mut t, 6, 0x40, [1; 8]);
        let b = w.draft().binding_for(Target::Browse).unwrap();
        assert_eq!((b.rel, b.invert), (RelMode::TwosComplement, false));
        // A fader moved instead of the knob isn't a wheel.
        let mut w = Wizard::new(
            Mapping::empty("test"),
            Setup {
                sections: vec![Section::Browse],
                ..Setup::default()
            },
        );
        past_shift(&mut w);
        sweep(&mut w, &mut t, 6, 0x1F, 40..90);
        assert_eq!(w.draft().bindings.len(), 0);
        assert!(w.said().unwrap().text.contains("doesn't read as a wheel"));
    }

    #[test]
    fn one_effect_knob_sets_all_three() {
        let setup = Setup {
            sections: vec![Section::Effects],
            ..Setup::default()
        };
        let mut w = Wizard::new(Mapping::empty("test"), setup);
        past_shift(&mut w);
        let mut t = 0;
        press(&mut w, &mut t, 4, 0x47);
        w.skip();
        w.skip();
        assert_eq!(
            w.step().unwrap().target,
            Target::FxDepth { unit: 0, slot: 0 }
        );
        sweep(&mut w, &mut t, 4, 0x02, (0..=127).rev().chain(0..=127));
        wait(&mut w, &mut t, 400);
        while w.at().is_some() {
            w.skip();
        }
        let made = w.finish();
        assert_eq!(
            made.binding_for(Target::FxDepthAll { unit: 0 })
                .map(|b| b.trigger),
            Some(Trigger::cc(4, 0x02))
        );
        assert_eq!(made.binding_for(Target::FxDepth { unit: 0, slot: 0 }), None);
        assert!(made
            .binding_for(Target::FxOn { unit: 0, slot: 0 })
            .is_some());
    }

    #[test]
    fn after_the_last_step_a_control_says_what_it_does() {
        let mut w = Wizard::new(builtin_preset("SC LIVE 4").unwrap(), Setup::default());
        while w.at().is_some() {
            w.skip();
        }
        assert_eq!(w.tried(), None);
        w.feed(parse(&[0x94, 0x0A, 0x7F]).unwrap(), 0);
        assert_eq!(w.tried(), Some("Note 0x0a · ch 5 → Deck A · Play/Pause"));
        w.feed(parse(&[0x94, 0x1C, 0x7F]).unwrap(), 0);
        w.feed(parse(&[0x94, 0x09, 0x7F]).unwrap(), 0);
        assert_eq!(
            w.tried(),
            Some("SHIFT + Note 0x09 · ch 5 → Deck A · Back to start")
        );
        w.feed(parse(&[0x94, 0x1C, 0x00]).unwrap(), 0);
        w.feed(parse(&[0xB0, 0x0E, 0x7F]).unwrap(), 0);
        assert_eq!(
            w.tried(),
            Some("CC 0x0e · ch 1 → Deck A · Channel fader at 100%")
        );
        w.feed(parse(&[0xB4, 0x36, 0x7F]).unwrap(), 0);
        assert_eq!(
            w.tried(),
            Some("CC 0x36 · ch 5 → Deck A · Jog wheel turn, anticlockwise")
        );
        w.feed(parse(&[0x94, 0x0F, 0x7F]).unwrap(), 0);
        assert!(w
            .tried()
            .unwrap()
            .contains("Deck A · Hot cue 1 (pad mode: hot cues) / "));
        w.feed(parse(&[0x99, 0x01, 0x7F]).unwrap(), 0);
        assert_eq!(
            w.tried(),
            Some("Note 0x01 on (127) · ch 10 → nothing is mapped to it")
        );
        // Back among the steps, it's quiet again.
        w.jump(0);
        assert_eq!(w.tried(), None);
    }

    #[test]
    fn the_setup_takes_the_controllers_messages_and_the_room_gets_none() {
        let preset = builtin_preset("DDJ-FLX4").unwrap();
        let mut e = MidiEngine::new(preset.clone(), 0.08);
        let (room, local) = (RoomState::default(), LocalParams::default());
        let setup = Setup {
            skip_mapped: false,
            sections: vec![Section::Transport],
            ..Setup::default()
        };
        let mut w = Wizard::new(e.mapping.clone(), setup);
        past_shift(&mut w);
        e.wizard = Some(w);
        // PLAY doesn't play: the setup hears it.
        assert_eq!(e.handle_at(&[0x90, 0x0B, 0x7F], 0, &room, &local), []);
        let w = e.wizard.as_ref().unwrap();
        assert_eq!(w.step().unwrap().target, Target::Cue { deck: 0 });
        assert_eq!(e.last_message.as_deref(), Some("Note 0x0b on (127) · ch 1"));
        // The mapping in use is untouched until the DJ keeps the new one.
        assert_eq!(e.mapping, preset);
        let made = e.wizard.as_ref().unwrap().finish();
        e.load(made);
        assert!(e.wizard.is_none());
        assert_eq!(
            e.handle_at(&[0x90, 0x0B, 0x7F], 0, &room, &local),
            [MidiOut::Action(Action::Play { deck: 0 })]
        );
    }

    #[test]
    fn steps_say_where_their_control_is() {
        let setup = Setup {
            decks: Decks::TwoSwitched,
            sections: all_sections(),
            ..Setup::default()
        };
        let w = Wizard::new(Mapping::empty("test"), setup);
        let place = |t: Target| {
            let s = w.steps.iter().find(|s| s.target == t).unwrap();
            w.place(s)
        };
        assert_eq!(place(Target::Shift), "SHIFT");
        assert_eq!(
            place(Target::Play { deck: 1 }),
            "Deck B · Transport · the right deck"
        );
        assert_eq!(
            place(Target::Play { deck: 2 }),
            "Deck C · Transport · the left deck, switched to deck 3"
        );
        assert_eq!(
            place(Target::Volume { deck: 3 }),
            "Deck D · Mixer channel · mixer channel 4, or the right channel with its side on deck 4"
        );
        assert_eq!(place(Target::Crossfader), "Mixer");
        assert_eq!(place(Target::FxOn { unit: 1, slot: 0 }), "FX unit 2");
        let titles: Vec<&str> = w.groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(
            titles[..3],
            ["SHIFT", "Deck A · Jog wheel", "Deck A · Transport"]
        );
        assert!(titles.contains(&"Deck C · DECK button"));
        assert!(titles.contains(&"Back to decks A and B"));
        assert_eq!(
            titles[titles.len() - 4..],
            ["Mixer", "Browse", "FX unit 1", "FX unit 2"]
        );
        // Every group has steps, and every step a title.
        assert!(w.groups.iter().all(|g| !g.steps.is_empty()));
        assert!(w.steps.iter().all(|s| !s.title.is_empty()));
        // Four decks of their own: no DECK buttons.
        let four = Wizard::new(
            Mapping::empty("test"),
            Setup {
                decks: Decks::Four,
                ..Setup::default()
            },
        );
        assert!(!four
            .steps
            .iter()
            .any(|s| matches!(s.target, Target::DeckSelect { .. })));
        assert!(four
            .steps
            .iter()
            .any(|s| s.target == Target::Play { deck: 3 }));
        let s = four
            .steps
            .iter()
            .find(|s| s.target == Target::Play { deck: 3 })
            .unwrap();
        assert_eq!(four.place(s), "Deck D · Transport");
    }
}
