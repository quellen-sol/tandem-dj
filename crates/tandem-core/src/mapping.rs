//! MIDI controller mapping ("MIDI learn"), built-in presets, soft takeover
//! and LED feedback.
//!
//! A binding can need SHIFT held, its deck's pads in a given pad mode, or
//! its deck to have its side of the controller, for controllers that tell
//! none of these in their messages (see [`Target::Shift`], [`PadMode`] and
//! [`Binding::layered`]).
//!
//! [`crate::wizard`] walks a DJ through a controller control by control and
//! writes the mapping for them.
//!
//! Each DJ maps their own controller; mappings are local files and never
//! shared. Because two people control one mixer, absolute controls (faders,
//! knobs) use *soft takeover*: once the room value has been changed, your
//! hardware control only takes effect after it reaches that value, so it
//! never jumps over what your partner set. The tempo fader is the exception:
//! SYNC moves tempos without moving the fader (and SHIFT moves the fader
//! without moving the tempo), so when the two disagree the fader works
//! *relatively* (see [`MidiEngine::tempo_fader`]).

use crate::meter::{to_db, Levels};
use crate::model::{deck_letter, Action, RoomState, BEAT_LOOPS, DECKS, FX_BEATS, FX_SLOTS, ROLLS};
use crate::render::LocalParams;
use crate::wizard::Wizard;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MidiKind {
    Note,
    Cc,
    PitchBend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Trigger {
    pub kind: MidiKind,
    pub channel: u8,
    pub number: u8,
}

impl Trigger {
    pub fn note(channel: u8, number: u8) -> Self {
        Self {
            kind: MidiKind::Note,
            channel,
            number,
        }
    }

    pub fn cc(channel: u8, number: u8) -> Self {
        Self {
            kind: MidiKind::Cc,
            channel,
            number,
        }
    }

    pub fn label(&self) -> String {
        match self.kind {
            MidiKind::Note => format!("Note {:#04x} · ch {}", self.number, self.channel + 1),
            MidiKind::Cc => format!("CC {:#04x} · ch {}", self.number, self.channel + 1),
            MidiKind::PitchBend => format!("Pitch bend · ch {}", self.channel + 1),
        }
    }
}

/// What a deck's pads do, on a controller whose pads send the same notes in
/// every pad mode (the Denon SC Live 4). Its pad mode buttons are bound to
/// [`Target::PadMode`], and each pad has a binding per mode, marked with
/// [`Binding::mode`]. Controllers whose pads send other notes in each mode
/// (Pioneer) don't need it. Every deck starts on hot cues.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PadMode {
    #[default]
    HotCues,
    BeatLoops,
    Rolls,
}

impl PadMode {
    pub const ALL: [PadMode; 3] = [PadMode::HotCues, PadMode::BeatLoops, PadMode::Rolls];

    pub fn label(&self) -> &'static str {
        match self {
            PadMode::HotCues => "hot cues",
            PadMode::BeatLoops => "beat loops",
            PadMode::Rolls => "rolls",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Button,
    Absolute,
    Relative,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Target {
    Play {
        deck: u8,
    },
    Cue {
        deck: u8,
    },
    /// SHIFT + CUE: back to the start of the track.
    BackToStart {
        deck: u8,
    },
    Sync {
        deck: u8,
    },
    /// SHIFT + SYNC: sync off, tempo back to where the tempo fader sits.
    SyncOff {
        deck: u8,
    },
    HotCue {
        deck: u8,
        slot: u8,
    },
    HotCueClear {
        deck: u8,
        slot: u8,
    },
    LoopIn {
        deck: u8,
    },
    LoopOut {
        deck: u8,
    },
    /// RELOOP/EXIT.
    Reloop {
        deck: u8,
    },
    LoopHalve {
        deck: u8,
    },
    LoopDouble {
        deck: u8,
    },
    /// A beat loop of [`BEAT_LOOPS`]`[size]` beats (¼ to 32).
    BeatLoop {
        deck: u8,
        size: u8,
    },
    /// A loop roll of [`ROLLS`]`[size]` beats (1/16 to 8) while held: one
    /// per pad, so pads that send the same notes in every pad mode can use
    /// them too.
    Roll {
        deck: u8,
        size: u8,
    },
    Tempo {
        deck: u8,
    },
    /// The tempo fader moved with SHIFT held: the fader moves, the tempo
    /// doesn't (for controllers that send another message with SHIFT, or
    /// as a SHIFT + binding with [`Target::Shift`]).
    TempoShift {
        deck: u8,
    },
    /// A four-deck controller's DECK button gave its side to this deck (the
    /// DDJ-FLX6 says so with a message of its own). The tempo fader may have
    /// moved while it drove the other deck, so it starts again from wherever
    /// it is (see [`MidiEngine::tempo_fader`]). On a controller whose
    /// controls send the same messages for both of a side's decks, this is
    /// also how Tandem knows which deck they drive now (see
    /// [`Binding::layered`]); one button bound to both of a side's decks
    /// switches to the other one each time it's pressed.
    DeckSelect {
        deck: u8,
    },
    Volume {
        deck: u8,
    },
    Eq {
        deck: u8,
        band: u8,
    },
    Trim {
        deck: u8,
    },
    Filter {
        deck: u8,
    },
    Pfl {
        deck: u8,
    },
    Jog {
        deck: u8,
    },
    JogTouch {
        deck: u8,
    },
    BendDown {
        deck: u8,
    },
    BendUp {
        deck: u8,
    },
    Load {
        deck: u8,
    },
    Crossfader,
    CueMix,
    /// This booth's headphone level (never the room's).
    HeadphoneLevel,
    /// This booth's master level (never the room's).
    MasterLevel,
    /// Turning the browse knob: moves through the tracks, or through the
    /// lists on the left when they have the knob.
    Browse,
    /// Pressing the browse knob: gives the knob to the lists on the left,
    /// or back to the tracks.
    BrowsePress,
    /// BACK: gives the browse knob to the lists on the left.
    BrowseBack,
    /// A pad mode button: from now on the deck's pads do what their
    /// bindings for this mode say (see [`PadMode`]).
    PadMode {
        deck: u8,
        mode: PadMode,
    },
    /// A SHIFT button that sends a message of its own and doesn't change
    /// what the other controls send: while it's held, a control's SHIFT +
    /// bindings ([`Binding::shift`]) replace its others. Any SHIFT bound to
    /// it shifts the whole controller.
    Shift,
    /// An effect slot on or off, on the deck the FX unit works (see
    /// [`MidiEngine::fx_unit_deck`]). Lit while it's on.
    FxOn {
        unit: u8,
        slot: u8,
    },
    /// An effect slot's depth. A slot that's off takes the knob as it is;
    /// one that's on waits for the knob to pick it up.
    FxDepth {
        unit: u8,
        slot: u8,
    },
    /// All three slots' depths at once, for a controller with one knob.
    FxDepthAll {
        unit: u8,
    },
    /// BEATS one step shorter, or longer.
    FxBeatsDown {
        unit: u8,
    },
    FxBeatsUp {
        unit: u8,
    },
    /// The next effect in a slot.
    FxNext {
        unit: u8,
        slot: u8,
    },
    /// The FX unit works this deck's bank from now on (the DDJ-FLX6's CH
    /// SELECT).
    FxAssign {
        unit: u8,
        deck: u8,
    },
    /// The FX unit works nothing (CH SELECT on MST: there's no master bank).
    FxAssignNone {
        unit: u8,
    },
    /// The controller's shared knob and ON/OFF button now belong to this
    /// slot (the DDJ-FLX6's FX SELECT). The knob's next reading only says
    /// where it is, and the slot's lights are sent again.
    FxFocus {
        unit: u8,
        slot: u8,
    },
}

impl Target {
    pub fn label(&self) -> String {
        let deck = |d: u8| deck_letter(d as usize);
        match *self {
            Target::Play { deck: d } => format!("Deck {} · Play/Pause", deck(d)),
            Target::Cue { deck: d } => format!("Deck {} · Cue", deck(d)),
            Target::BackToStart { deck: d } => format!("Deck {} · Back to start", deck(d)),
            Target::Sync { deck: d } => format!("Deck {} · Sync", deck(d)),
            Target::SyncOff { deck: d } => {
                format!("Deck {} · Sync off (back to tempo fader)", deck(d))
            }
            Target::HotCue { deck: d, slot } => format!("Deck {} · Hot cue {}", deck(d), slot + 1),
            Target::HotCueClear { deck: d, slot } => {
                format!("Deck {} · Clear hot cue {}", deck(d), slot + 1)
            }
            Target::LoopIn { deck: d } => format!("Deck {} · Loop in", deck(d)),
            Target::LoopOut { deck: d } => format!("Deck {} · Loop out", deck(d)),
            Target::Reloop { deck: d } => format!("Deck {} · Reloop / exit", deck(d)),
            Target::LoopHalve { deck: d } => format!("Deck {} · Loop ½", deck(d)),
            Target::LoopDouble { deck: d } => format!("Deck {} · Loop ×2", deck(d)),
            Target::BeatLoop { deck: d, size } => {
                format!(
                    "Deck {} · Beat loop {}",
                    deck(d),
                    beats_label(beat_loop_beats(size))
                )
            }
            Target::Roll { deck: d, size } => {
                format!("Deck {} · Roll {}", deck(d), beats_label(roll_beats(size)))
            }
            Target::Tempo { deck: d } => format!("Deck {} · Tempo fader", deck(d)),
            Target::TempoShift { deck: d } => {
                format!("Deck {} · SHIFT + tempo fader (tempo stays)", deck(d))
            }
            Target::DeckSelect { deck: d } => {
                format!("Deck {} · DECK button (this side switches to it)", deck(d))
            }
            Target::Volume { deck: d } => format!("Deck {} · Channel fader", deck(d)),
            Target::Eq { deck: d, band } => {
                format!(
                    "Deck {} · EQ {}",
                    deck(d),
                    ["low", "mid", "high"][band.min(2) as usize]
                )
            }
            Target::Trim { deck: d } => format!("Deck {} · Trim", deck(d)),
            Target::Filter { deck: d } => format!("Deck {} · Filter", deck(d)),
            Target::Pfl { deck: d } => format!("Deck {} · Headphone cue", deck(d)),
            Target::Jog { deck: d } => format!("Deck {} · Jog wheel turn", deck(d)),
            Target::JogTouch { deck: d } => format!("Deck {} · Jog wheel touch (scratch)", deck(d)),
            Target::BendDown { deck: d } => format!("Deck {} · Pitch bend −", deck(d)),
            Target::BendUp { deck: d } => format!("Deck {} · Pitch bend +", deck(d)),
            Target::Load { deck: d } => format!("Deck {} · Load selected track", deck(d)),
            Target::Crossfader => "Crossfader".into(),
            Target::CueMix => "Headphone cue/master mix".into(),
            Target::HeadphoneLevel => "Headphone level".into(),
            Target::MasterLevel => "Master level".into(),
            Target::Browse => "Library browse encoder".into(),
            Target::BrowsePress => "Library browse press (lists ↔ tracks)".into(),
            Target::BrowseBack => "Library back (to the lists)".into(),
            Target::PadMode { deck: d, mode } => {
                format!("Deck {} · Pad mode: {}", deck(d), mode.label())
            }
            Target::Shift => "SHIFT (for SHIFT + controls)".into(),
            Target::FxOn { unit, slot } => {
                format!("FX unit {} · Slot {} on/off", unit + 1, slot + 1)
            }
            Target::FxDepth { unit, slot } => {
                format!("FX unit {} · Slot {} depth", unit + 1, slot + 1)
            }
            Target::FxDepthAll { unit } => {
                format!("FX unit {} · Depth (all three slots)", unit + 1)
            }
            Target::FxBeatsDown { unit } => format!("FX unit {} · BEATS ◀", unit + 1),
            Target::FxBeatsUp { unit } => format!("FX unit {} · BEATS ▶", unit + 1),
            Target::FxNext { unit, slot } => {
                format!("FX unit {} · Slot {}: next effect", unit + 1, slot + 1)
            }
            Target::FxAssign { unit, deck: d } => {
                format!("FX unit {} · Works deck {}", unit + 1, deck(d))
            }
            Target::FxAssignNone { unit } => format!("FX unit {} · Works no deck", unit + 1),
            Target::FxFocus { unit, slot } => {
                format!(
                    "FX unit {} · Select slot {} (FX SELECT)",
                    unit + 1,
                    slot + 1
                )
            }
        }
    }

    /// The deck this control acts on, if it's a deck's (or channel's).
    pub fn deck(&self) -> Option<u8> {
        let mut t = *self;
        t.with_deck_checked(None)
    }

    /// The same control on deck `d` (controls with no deck are unchanged).
    pub fn with_deck(&self, d: u8) -> Target {
        let mut t = *self;
        t.with_deck_checked(Some(d));
        t
    }

    /// Reads the deck, and sets it to `set` if given.
    fn with_deck_checked(&mut self, set: Option<u8>) -> Option<u8> {
        use Target::*;
        let deck = match self {
            Play { deck }
            | Cue { deck }
            | BackToStart { deck }
            | Sync { deck }
            | SyncOff { deck }
            | HotCue { deck, .. }
            | HotCueClear { deck, .. }
            | LoopIn { deck }
            | LoopOut { deck }
            | Reloop { deck }
            | LoopHalve { deck }
            | LoopDouble { deck }
            | BeatLoop { deck, .. }
            | Roll { deck, .. }
            | Tempo { deck }
            | TempoShift { deck }
            | DeckSelect { deck }
            | Volume { deck }
            | Eq { deck, .. }
            | Trim { deck }
            | Filter { deck }
            | Pfl { deck }
            | Jog { deck }
            | JogTouch { deck }
            | BendDown { deck }
            | BendUp { deck }
            | Load { deck }
            | PadMode { deck, .. } => deck,
            // An FX unit works whichever deck it's pointed at.
            Crossfader
            | CueMix
            | HeadphoneLevel
            | MasterLevel
            | Browse
            | BrowsePress
            | BrowseBack
            | Shift
            | FxOn { .. }
            | FxDepth { .. }
            | FxDepthAll { .. }
            | FxBeatsDown { .. }
            | FxBeatsUp { .. }
            | FxNext { .. }
            | FxAssign { .. }
            | FxAssignNone { .. }
            | FxFocus { .. } => return None,
        };
        if let Some(d) = set {
            *deck = d;
        }
        Some(*deck)
    }

    pub fn kind(&self) -> TargetKind {
        match self {
            Target::Tempo { .. }
            | Target::TempoShift { .. }
            | Target::Volume { .. }
            | Target::Eq { .. }
            | Target::Trim { .. }
            | Target::Filter { .. }
            | Target::Crossfader
            | Target::CueMix
            | Target::HeadphoneLevel
            | Target::MasterLevel
            | Target::FxDepth { .. }
            | Target::FxDepthAll { .. } => TargetKind::Absolute,
            Target::Jog { .. } | Target::Browse => TargetKind::Relative,
            _ => TargetKind::Button,
        }
    }

    pub fn is_absolute(&self) -> bool {
        self.kind() == TargetKind::Absolute
    }

    pub fn is_relative(&self) -> bool {
        self.kind() == TargetKind::Relative
    }

    /// Something the pads do, so its binding can depend on the pad mode.
    pub fn is_pad(&self) -> bool {
        matches!(
            self,
            Target::HotCue { .. }
                | Target::HotCueClear { .. }
                | Target::BeatLoop { .. }
                | Target::Roll { .. }
        )
    }

    /// SHIFT or a pad mode button: changes what other controls do.
    pub fn is_modifier(&self) -> bool {
        matches!(self, Target::Shift | Target::PadMode { .. })
    }

    /// The FX unit this control belongs to, if it's one's.
    pub fn unit(&self) -> Option<u8> {
        use Target::*;
        match *self {
            FxOn { unit, .. }
            | FxDepth { unit, .. }
            | FxDepthAll { unit }
            | FxBeatsDown { unit }
            | FxBeatsUp { unit }
            | FxNext { unit, .. }
            | FxAssign { unit, .. }
            | FxAssignNone { unit }
            | FxFocus { unit, .. } => Some(unit),
            _ => None,
        }
    }

    /// The part of the controller this control sits in.
    pub fn part(&self) -> Part {
        use Target::*;
        match self {
            Shift => Part::Shift,
            JogTouch { .. } | Jog { .. } => Part::Jog,
            Play { .. }
            | Cue { .. }
            | BackToStart { .. }
            | Sync { .. }
            | SyncOff { .. }
            | Load { .. }
            | DeckSelect { .. } => Part::Transport,
            Tempo { .. } | TempoShift { .. } | BendDown { .. } | BendUp { .. } => Part::Tempo,
            LoopIn { .. }
            | LoopOut { .. }
            | Reloop { .. }
            | LoopHalve { .. }
            | LoopDouble { .. } => Part::Loops,
            HotCue { .. } | HotCueClear { .. } => Part::HotCues,
            BeatLoop { .. } => Part::BeatLoops,
            Roll { .. } => Part::Rolls,
            PadMode { mode, .. } => match mode {
                self::PadMode::HotCues => Part::HotCues,
                self::PadMode::BeatLoops => Part::BeatLoops,
                self::PadMode::Rolls => Part::Rolls,
            },
            Trim { .. } | Eq { .. } | Filter { .. } | Volume { .. } | Pfl { .. } => Part::Channel,
            Crossfader | CueMix | HeadphoneLevel | MasterLevel => Part::Mixer,
            Browse | BrowsePress | BrowseBack => Part::Browse,
            FxOn { .. }
            | FxDepth { .. }
            | FxDepthAll { .. }
            | FxBeatsDown { .. }
            | FxBeatsUp { .. }
            | FxNext { .. }
            | FxAssign { .. }
            | FxAssignNone { .. }
            | FxFocus { .. } => Part::Fx,
        }
    }

    /// The group the mapping screen and the step-by-step setup show this
    /// control in: its part of the controller, on its deck or FX unit.
    pub fn group(&self) -> (Part, Option<u8>) {
        (self.part(), self.deck().or(self.unit()))
    }

    /// Every mappable target, in the order the mapping screen shows them:
    /// group by group (see [`Target::group`]).
    pub fn all() -> Vec<Target> {
        let mut v = vec![Target::Shift];
        for d in 0..DECKS as u8 {
            v.push(Target::JogTouch { deck: d });
            v.push(Target::Jog { deck: d });
            v.push(Target::Play { deck: d });
            v.push(Target::Cue { deck: d });
            v.push(Target::Sync { deck: d });
            v.push(Target::BackToStart { deck: d });
            v.push(Target::SyncOff { deck: d });
            v.push(Target::Load { deck: d });
            v.push(Target::DeckSelect { deck: d });
            v.push(Target::Tempo { deck: d });
            v.push(Target::TempoShift { deck: d });
            v.push(Target::BendDown { deck: d });
            v.push(Target::BendUp { deck: d });
            v.push(Target::LoopIn { deck: d });
            v.push(Target::LoopOut { deck: d });
            v.push(Target::Reloop { deck: d });
            v.push(Target::LoopHalve { deck: d });
            v.push(Target::LoopDouble { deck: d });
            for mode in PadMode::ALL {
                v.push(Target::PadMode { deck: d, mode });
                match mode {
                    PadMode::HotCues => {
                        for slot in 0..8 {
                            v.push(Target::HotCue { deck: d, slot });
                        }
                        for slot in 0..8 {
                            v.push(Target::HotCueClear { deck: d, slot });
                        }
                    }
                    PadMode::BeatLoops => {
                        for size in 0..BEAT_LOOPS.len() as u8 {
                            v.push(Target::BeatLoop { deck: d, size });
                        }
                    }
                    PadMode::Rolls => {
                        for size in 0..ROLLS.len() as u8 {
                            v.push(Target::Roll { deck: d, size });
                        }
                    }
                }
            }
            v.push(Target::Trim { deck: d });
            for band in (0..3).rev() {
                v.push(Target::Eq { deck: d, band });
            }
            v.push(Target::Filter { deck: d });
            v.push(Target::Volume { deck: d });
            v.push(Target::Pfl { deck: d });
        }
        v.push(Target::Crossfader);
        v.push(Target::CueMix);
        v.push(Target::HeadphoneLevel);
        v.push(Target::MasterLevel);
        v.push(Target::Browse);
        v.push(Target::BrowsePress);
        v.push(Target::BrowseBack);
        for unit in 0..FX_UNITS as u8 {
            for slot in 0..FX_SLOTS as u8 {
                v.push(Target::FxFocus { unit, slot });
            }
            for slot in 0..FX_SLOTS as u8 {
                v.push(Target::FxOn { unit, slot });
                v.push(Target::FxDepth { unit, slot });
                v.push(Target::FxNext { unit, slot });
            }
            v.push(Target::FxDepthAll { unit });
            v.push(Target::FxBeatsDown { unit });
            v.push(Target::FxBeatsUp { unit });
            for deck in 0..DECKS as u8 {
                v.push(Target::FxAssign { unit, deck });
            }
            v.push(Target::FxAssignNone { unit });
        }
        v
    }
}

/// A part of a controller: what the mapping screen and the step-by-step
/// setup group its controls by (see [`Target::group`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Shift,
    Jog,
    Transport,
    Tempo,
    Loops,
    HotCues,
    BeatLoops,
    Rolls,
    /// A deck's mixer channel.
    Channel,
    /// The crossfader, and the headphone and master knobs.
    Mixer,
    Browse,
    Fx,
}

impl Part {
    /// The name of this part on deck or FX unit `n`: "Deck A · Jog wheel".
    pub fn title(&self, n: Option<u8>) -> String {
        let name = match self {
            Part::Shift => return "SHIFT".into(),
            Part::Mixer => return "Mixer".into(),
            Part::Browse => return "Browse".into(),
            Part::Fx => return format!("FX unit {}", n.unwrap_or(0) + 1),
            Part::Jog => "Jog wheel",
            Part::Transport => "Transport",
            Part::Tempo => "Tempo",
            Part::Loops => "Loops",
            Part::HotCues => "Pads: hot cues",
            Part::BeatLoops => "Pads: beat loops",
            Part::Rolls => "Pads: rolls",
            Part::Channel => "Mixer channel",
        };
        format!("Deck {} · {name}", deck_letter(n.unwrap_or(0) as usize))
    }
}

/// Beats in beat loop `size` (an index into [`BEAT_LOOPS`]).
pub fn beat_loop_beats(size: u8) -> f64 {
    BEAT_LOOPS[(size as usize).min(BEAT_LOOPS.len() - 1)]
}

/// Beats in loop roll `size` (an index into [`ROLLS`]).
pub fn roll_beats(size: u8) -> f64 {
    ROLLS[(size as usize).min(ROLLS.len() - 1)]
}

/// "¼", "½", "1", "16"… for a loop length in beats.
pub fn beats_label(beats: f64) -> String {
    match beats {
        b if (b - 0.25).abs() < 1e-9 => "¼".into(),
        b if (b - 0.5).abs() < 1e-9 => "½".into(),
        b if (b - 0.75).abs() < 1e-9 => "¾".into(),
        b if b >= 1.0 && b.fract() == 0.0 => format!("{b}"),
        b if b < 1.0 && (1.0 / b).fract().abs() < 1e-9 => format!("1/{}", 1.0 / b),
        b => format!("{b:.2}"),
    }
}

/// How a relative (endless) control encodes direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelMode {
    /// 64 = still, 65 = +1, 63 = -1 (Pioneer jogs and many others).
    #[default]
    Offset64,
    /// 1 = +1, 127 = -1 (Pioneer browse knob, Numark, Hercules...).
    TwosComplement,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub trigger: Trigger,
    pub target: Target,
    #[serde(default)]
    pub invert: bool,
    #[serde(default)]
    pub rel: RelMode,
    /// SHIFT + this control: only while a [`Target::Shift`] button is held.
    /// Then it replaces the control's bindings without SHIFT; a control
    /// with no SHIFT + binding does the same with SHIFT held.
    #[serde(default, skip_serializing_if = "is_false")]
    pub shift: bool,
    /// Only while the target's deck is in this pad mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PadMode>,
    /// What its light is sent, lit and unlit, instead of 127 and 0 (Denon
    /// buttons are dim at 1, and their pads take a colour number).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<(u8, u8)>,
    /// Only while the target's deck has its side of the controller: for a
    /// controller with two decks' worth of controls whose DECK buttons
    /// ([`Target::DeckSelect`]) don't change what the controls send. The
    /// left side starts on deck A, the right side on deck B.
    #[serde(default, skip_serializing_if = "is_false")]
    pub layered: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Binding {
    /// Whether both can answer the same message: the same button or knob,
    /// both with SHIFT or both without, in a pad mode they share, and not
    /// on two decks that take turns on one side of the controller.
    pub fn overlaps(&self, other: &Binding) -> bool {
        let turns = self.layered
            && other.layered
            && matches!(
                (self.target.deck(), other.target.deck()),
                (Some(a), Some(b)) if a != b && side_of(a) == side_of(b)
            );
        self.trigger == other.trigger
            && self.shift == other.shift
            && (self.mode.is_none() || other.mode.is_none() || self.mode == other.mode)
            && !turns
    }

    /// The control, and what else it needs: "SHIFT + Note 0x0f · ch 5 (pad
    /// mode: rolls)".
    pub fn label(&self) -> String {
        let shift = if self.shift { "SHIFT + " } else { "" };
        let mut s = match self.mode {
            Some(m) => format!("{shift}{} (pad mode: {})", self.trigger.label(), m.label()),
            None => format!("{shift}{}", self.trigger.label()),
        };
        if let (true, Some(d)) = (self.layered, self.target.deck()) {
            s.push_str(&format!(" (side on deck {})", deck_letter(d as usize)));
        }
        s
    }
}

/// The side of a controller a deck plays on: 0 for the left (decks A and
/// C), 1 for the right (B and D).
pub fn side_of(deck: u8) -> usize {
    (deck % 2) as usize
}

/// A level meter on the controller: a control change whose value sets how
/// many of its lights are lit (Pioneer: CC 0x02 on the deck's channel).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeterLight {
    pub channel: u8,
    pub cc: u8,
    /// The deck whose channel level it shows.
    pub deck: u8,
}

/// An FX unit's BEATS indicator: a control change whose value lights the
/// beat count of the deck the unit works (the DDJ-FLX6: CC 0x64 on the
/// unit's channel).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxBeatLight {
    pub channel: u8,
    pub cc: u8,
    pub unit: u8,
    /// The value for each of [`FX_BEATS`].
    pub values: Vec<u8>,
    /// The value while the unit works no deck, and when Tandem lets go.
    pub dark: u8,
}

/// How many FX units a controller can have.
pub const FX_UNITS: usize = 4;

/// Unit 1 works deck A … unit 4 deck D, unless a preset says otherwise.
fn default_fx_unit_decks() -> [Option<u8>; FX_UNITS] {
    std::array::from_fn(|u| Some(u as u8))
}

fn one() -> f32 {
    1.0
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mapping {
    pub device: String,
    pub bindings: Vec<Binding>,
    /// Pitch-bend strength when turning the jog ring while playing.
    #[serde(default = "one")]
    pub jog_sensitivity: f32,
    /// Scratch distance multiplier (1.0 = one platter turn moves 1.8 s,
    /// like a 33⅓ rpm record).
    #[serde(default = "one")]
    pub scratch_sensitivity: f32,
    /// Jog ticks in one full turn of the platter (measured with the
    /// calibration tool). `None` = unknown, use 720.
    #[serde(default)]
    pub jog_ticks_per_rev: Option<f32>,
    /// Touching the jog platter holds the record (vinyl mode).
    #[serde(default = "yes")]
    pub vinyl: bool,
    /// Name of the built-in preset this mapping started from, if any.
    #[serde(default)]
    pub preset: Option<String>,
    /// Version of that preset when this mapping was saved.
    #[serde(default)]
    pub preset_version: u32,
    /// Channel level meters on the controller (built-in presets only).
    #[serde(default)]
    pub meters: Vec<MeterLight>,
    /// The deck each FX unit works when the controller connects. The
    /// controller can't be asked, so this is how it powers on.
    #[serde(default = "default_fx_unit_decks")]
    pub fx_unit_decks: [Option<u8>; FX_UNITS],
    /// FX units' BEATS indicators (built-in presets only).
    #[serde(default)]
    pub fx_beat_lights: Vec<FxBeatLight>,
}

/// Bump when built-in presets gain controls; saved mappings pick them up.
pub const PRESET_VERSION: u32 = 13;
/// The first preset version with effects: older mappings start their FX
/// units where the preset says.
const FIRST_FX_PRESET: u32 = 13;
/// Up to this preset version, four-deck controllers drove Tandem's two
/// decks: decks 1 and 3 drove A, decks 2 and 4 drove B.
const LAST_TWO_DECK_PRESET: u32 = 6;

impl Mapping {
    pub fn empty(device: &str) -> Self {
        Self {
            device: device.to_string(),
            bindings: Vec::new(),
            jog_sensitivity: 1.0,
            scratch_sensitivity: 1.0,
            jog_ticks_per_rev: None,
            vinyl: true,
            preset: None,
            preset_version: 0,
            meters: Vec::new(),
            fx_unit_decks: default_fx_unit_decks(),
            fx_beat_lights: Vec::new(),
        }
    }

    pub fn ticks_per_rev(&self) -> f64 {
        self.jog_ticks_per_rev.unwrap_or(720.0).max(16.0) as f64
    }

    pub fn binding_for(&self, target: Target) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.target == target)
    }

    /// A mapping saved from an older built-in preset: add the controls the
    /// preset has gained since (only on buttons/knobs the user hasn't bound
    /// to something else, with or without SHIFT and in the same pad mode)
    /// and its level meters, keeping calibration and customisations.
    /// Returns how many controls were added.
    pub fn upgrade_from(&mut self, preset: &Mapping) -> usize {
        if self.preset.is_none() || self.preset_version >= preset.preset_version {
            return 0;
        }
        let mut added = 0;
        if self.preset_version <= LAST_TWO_DECK_PRESET {
            added += self.unfold_decks(preset);
        }
        for b in &preset.bindings {
            let trigger_used = self.bindings.iter().any(|x| x.overlaps(b));
            if !trigger_used {
                self.bindings.push(b.clone());
                added += 1;
            }
        }
        for m in &preset.meters {
            if !self
                .meters
                .iter()
                .any(|x| (x.channel, x.cc) == (m.channel, m.cc))
            {
                self.meters.push(*m);
                added += 1;
            }
        }
        for l in &preset.fx_beat_lights {
            if !self
                .fx_beat_lights
                .iter()
                .any(|x| (x.channel, x.cc) == (l.channel, l.cc))
            {
                self.fx_beat_lights.push(l.clone());
                added += 1;
            }
        }
        if self.preset_version < FIRST_FX_PRESET {
            // Saved before there were effects: start the units the way the
            // controller powers on.
            self.fx_unit_decks = preset.fx_unit_decks;
        }
        self.preset_version = preset.preset_version;
        added
    }

    /// A mapping made before Tandem had a built-in preset for this
    /// controller (learned, or written by hand, like the first Denon SC
    /// Live 4 mapping), when at least half its bindings are the preset's:
    /// it becomes the preset, so it picks up the preset's controls now and
    /// later. It keeps the jog calibration and settings, and its invert and
    /// encoder style on the preset's controls. A button or knob it binds to
    /// something the preset doesn't keeps all its bindings as they were,
    /// instead of the preset's. Returns whether it did.
    pub fn adopt(&mut self, preset: &Mapping) -> bool {
        if self.preset.is_some() || preset.preset.is_none() || self.bindings.is_empty() {
            return false;
        }
        let same = |a: &Binding, b: &Binding| a.trigger == b.trigger && a.target == b.target;
        let known = |b: &Binding| preset.bindings.iter().any(|p| same(p, b));
        if self.bindings.iter().filter(|b| known(b)).count() * 2 < self.bindings.len() {
            return false;
        }
        let own: HashSet<Trigger> = self
            .bindings
            .iter()
            .filter(|b| !known(b))
            .map(|b| b.trigger)
            .collect();
        let mut bindings: Vec<Binding> = preset
            .bindings
            .iter()
            .filter(|p| !own.contains(&p.trigger))
            .map(|p| {
                let mut p = p.clone();
                if let Some(b) = self.bindings.iter().find(|b| same(b, &p)) {
                    p.invert = b.invert;
                    p.rel = b.rel;
                }
                p
            })
            .collect();
        bindings.extend(
            self.bindings
                .iter()
                .filter(|b| own.contains(&b.trigger))
                .cloned(),
        );
        *self = Mapping {
            device: std::mem::take(&mut self.device),
            bindings,
            jog_sensitivity: self.jog_sensitivity,
            scratch_sensitivity: self.scratch_sensitivity,
            jog_ticks_per_rev: self.jog_ticks_per_rev.or(preset.jog_ticks_per_rev),
            vinyl: self.vinyl,
            ..preset.clone()
        };
        true
    }

    /// A four-deck controller's decks 3 and 4 used to drive Tandem's decks
    /// A and B. Point every binding still doing exactly that at deck C or D,
    /// as the preset now does. Bindings the DJ changed are left alone.
    /// Returns how many moved.
    fn unfold_decks(&mut self, preset: &Mapping) -> usize {
        let mut moved = 0;
        for b in &mut self.bindings {
            let Some(p) = preset.bindings.iter().find(|p| p.trigger == b.trigger) else {
                continue;
            };
            let (Some(old), Some(new)) = (b.target.deck(), p.target.deck()) else {
                continue;
            };
            if new >= 2 && old == new - 2 && b.target.with_deck(new) == p.target {
                b.target = p.target;
                moved += 1;
            }
        }
        moved
    }

    pub(crate) fn bind(&mut self, trigger: Trigger, target: Target) -> &mut Binding {
        self.bindings.push(Binding {
            trigger,
            target,
            invert: false,
            rel: RelMode::Offset64,
            shift: false,
            mode: None,
            light: None,
            layered: false,
        });
        self.bindings.last_mut().unwrap()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawMidi {
    NoteOn { ch: u8, note: u8, vel: u8 },
    NoteOff { ch: u8, note: u8 },
    Cc { ch: u8, cc: u8, val: u8 },
    PitchBend { ch: u8, val: u16 },
}

pub fn parse(bytes: &[u8]) -> Option<RawMidi> {
    let status = *bytes.first()?;
    let ch = status & 0x0f;
    let d1 = *bytes.get(1).unwrap_or(&0) & 0x7f;
    let d2 = *bytes.get(2).unwrap_or(&0) & 0x7f;
    match status & 0xf0 {
        0x90 if d2 > 0 => Some(RawMidi::NoteOn {
            ch,
            note: d1,
            vel: d2,
        }),
        0x90 | 0x80 => Some(RawMidi::NoteOff { ch, note: d1 }),
        0xb0 => Some(RawMidi::Cc {
            ch,
            cc: d1,
            val: d2,
        }),
        0xe0 => Some(RawMidi::PitchBend {
            ch,
            val: ((d2 as u16) << 7) | d1 as u16,
        }),
        _ => None,
    }
}

impl RawMidi {
    pub fn label(&self) -> String {
        match *self {
            RawMidi::NoteOn { ch, note, vel } => {
                format!("Note {note:#04x} on ({vel}) · ch {}", ch + 1)
            }
            RawMidi::NoteOff { ch, note } => format!("Note {note:#04x} off · ch {}", ch + 1),
            RawMidi::Cc { ch, cc, val } => format!("CC {cc:#04x} = {val} · ch {}", ch + 1),
            RawMidi::PitchBend { ch, val } => format!("Pitch bend {val} · ch {}", ch + 1),
        }
    }
}

/// What the app should do in response to a MIDI message.
#[derive(Clone, Debug, PartialEq)]
pub enum MidiOut {
    Action(Action),
    TogglePfl(u8),
    CueMix(f32),
    HeadphoneLevel(f32),
    MasterLevel(f32),
    Browse(i32),
    BrowsePress,
    BrowseBack,
    LoadSelected(u8),
    Learned(Binding),
}

/// Which of this booth's outputs play through the controller's own sound
/// card. A controller's MASTER and HEADPHONE LEVEL knobs also turn its own
/// outputs up and down, in hardware: there they leave Tandem's levels
/// alone, or the sound would be turned down twice. Through any other sound
/// card (the PC's speakers, a headset), they set Tandem's levels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ControllerCard {
    /// The master output plays through it.
    pub master: bool,
    /// The headphone output plays through it.
    pub phones: bool,
    /// The headphones play through its jack, and it blends cue and master
    /// for them itself: its HEADPHONES MIX knob works on the sound card
    /// (see [`mixes_own_phones`]). The app sets it from the audio routing.
    pub phones_mix: bool,
}

impl ControllerCard {
    /// For the controller on MIDI port `port`, with the master and the
    /// headphones on the sound cards named `master` and `phones`.
    pub fn find(port: Option<&str>, master: Option<&str>, phones: Option<&str>) -> Self {
        let on = |device: Option<&str>| match (port, device) {
            (Some(p), Some(d)) => is_controller_card(p, d),
            _ => false,
        };
        Self {
            master: on(master),
            phones: on(phones),
            phones_mix: false,
        }
    }

    /// The controller sets this level (or mix) itself, on its own sound card.
    pub fn sets(&self, target: Target) -> bool {
        match target {
            Target::MasterLevel => self.master,
            Target::HeadphoneLevel => self.phones,
            Target::CueMix => self.phones_mix,
            _ => false,
        }
    }
}

/// The stereo pair a DJ controller's headphone jack plays (channels 3/4).
pub const CONTROLLER_PHONES_PAIR: u16 = 1;

/// If `device` is the sound card of a controller that mixes its own
/// headphones, the controller's name. These play the jack from channels 3/4
/// and blend in the master themselves: the HEADPHONES MIX knob turns
/// channels 3/4 down in hardware and MASTER CUE adds the master, so Tandem
/// sends them only the cued decks. Measured on a DDJ-FLX6; Mixxx's manuals
/// say the same of the DDJ-400 and FLX4.
pub fn mixes_own_phones(device: &str) -> Option<&'static str> {
    ["DDJ-FLX6", "DDJ-FLX4", "DDJ-400"]
        .into_iter()
        .find(|model| is_controller_card(model, device))
}

/// Whether the sound card `device` is the one built into the controller on
/// MIDI port `port`. Windows names a controller's sound card after it: the
/// DDJ-FLX6's MIDI port is "DDJ-FLX6" and its sound card "Line (DDJ-FLX6)".
/// So it is when the card's name contains the port's, ignoring case,
/// spaces and punctuation, Windows' "2- " prefix and a "MIDI" at the end.
pub fn is_controller_card(port: &str, device: &str) -> bool {
    let squash = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let port = match port.split_once("- ") {
        Some((n, rest)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => rest,
        _ => port,
    };
    // "… MIDI" or "… MIDI 1": the sound card won't say MIDI.
    let mut words: Vec<&str> = port.split_whitespace().collect();
    let midi = |w: &str| w.eq_ignore_ascii_case("midi");
    let number = |w: &str| w.chars().all(|c| c.is_ascii_digit());
    match words[..] {
        [_, .., m, n] if midi(m) && number(n) => words.truncate(words.len() - 2),
        [_, .., m] if midi(m) => words.truncate(words.len() - 1),
        _ => {}
    }
    let key = squash(&words.concat());
    // Long enough not to be a generic word ("USB MIDI" is "usb").
    key.len() >= 4 && squash(device).contains(&key)
}

#[derive(Clone, Copy, Debug, Default)]
struct Takeover {
    engaged: bool,
    last_sent: Option<f32>,
    last_hw: Option<f32>,
    /// Tempo fader only: how far (in tempo) it has moved past the room's
    /// limit, up to one coarse step (see [`MidiEngine::tempo_fader`]).
    past_limit: f64,
    /// Readings still to come that only say where the control is: the
    /// tempo fader's after a DECK button gave it this deck back, an effect
    /// knob's after FX SELECT moved onto its slot.
    relearn: u8,
    /// Effect knobs: the last relearned reading was a coarse half, so a
    /// fine half straight after it is part of it. (Right after connecting,
    /// a 14-bit knob isn't known to be one until its first fine half.)
    relearn_fine: bool,
}

pub struct MidiEngine {
    pub mapping: Mapping,
    /// Set by the mapping screen; the next matching control you touch gets bound.
    pub learn: Option<Target>,
    /// The step-by-step setup, while it's open: it takes every message, and
    /// nothing reaches the room.
    pub wizard: Option<Wizard>,
    pub tempo_range: f64,
    /// LOOP IN and OUT snap to the beatgrid (this DJ's setting).
    pub quantize_loops: bool,
    pub last_message: Option<String>,
    /// Which outputs play through the controller's own sound card (the app
    /// keeps it up to date).
    pub card: ControllerCard,
    takeover: HashMap<Target, Takeover>,
    msb: HashMap<(u8, u8), u8>,
    lsb: HashMap<(u8, u8), u8>,
    /// (channel, coarse CC) pairs seen sending a fine half on CC+32.
    hires: HashSet<(u8, u8)>,
    /// Scratch / search movement (seconds) not yet sent, per deck.
    jog_pos: [f64; DECKS],
    /// Jog ring ticks since the last pitch-bend update, per deck.
    ring_ticks: [i32; DECKS],
    /// Pitch bend currently applied from the jog ring, per deck.
    ring_bend: [f64; DECKS],
    /// While calibrating: net jog ticks counted so far.
    pub calibration: Option<i64>,
    /// Shared time (us) of the MIDI message being handled.
    now: i64,
    /// Scratch movement in the last moments, per deck: (time, seconds of
    /// track). How fast the hand was going when it let go sets the spin.
    hand: [VecDeque<(i64, f64)>; DECKS],
    /// Per deck, after the hand let go of the platter: (when, last tick
    /// since). The platter turns on by itself for a moment.
    free: [Option<(i64, i64)>; DECKS],
    /// Roll pads held, per deck, the latest last (sizes, see [`ROLLS`]).
    rolls: [Vec<u8>; DECKS],
    /// Each deck's pad mode, set by [`Target::PadMode`] buttons.
    pad_modes: [PadMode; DECKS],
    /// The deck each side of the controller is on (left, right), set by
    /// [`Target::DeckSelect`] buttons.
    sides: [u8; 2],
    /// [`Target::Shift`] buttons held.
    shift: HashSet<Trigger>,
    /// Buttons held, and what pressing them did: letting go goes to the
    /// same targets, even if SHIFT or the pad mode changed in between (a
    /// roll always ends when its pad is let go).
    held: HashMap<Trigger, Vec<Target>>,
    /// The deck whose effects each FX unit works (None: nothing). The room
    /// never sees units: only the actions they send.
    fx_deck: [Option<u8>; FX_UNITS],
    /// Lights (status, number) the controller thread should send again
    /// even if they haven't changed: a controller that shows only the
    /// selected slot's lights may have ignored them (see
    /// [`MidiEngine::take_resend`]).
    resend: Vec<(u8, u8)>,
}

/// Where a mapping's FX units start (decks past D work nothing).
fn mapping_fx_decks(m: &Mapping) -> [Option<u8>; FX_UNITS] {
    m.fx_unit_decks.map(|d| d.filter(|&d| (d as usize) < DECKS))
}

const BEND: f64 = 0.04;
/// One platter turn at 33⅓ rpm: 1.8 s of audio.
const SECONDS_PER_TURN: f64 = 60.0 / (100.0 / 3.0);
/// Jog-ring pitch bend: turning the ring at one turn per second speeds the
/// track up by this fraction (10%). A gentle quarter-turn nudge over a
/// second shifts the track by about 25 ms.
const BEND_PER_TURN_PER_SEC: f64 = 0.10;
/// Letting go of the platter, the hand's speed is taken over this long.
const HAND_WINDOW_US: i64 = 20_000;
/// A platter that has sent nothing for this long has stopped.
const PLATTER_STILL_US: i64 = 50_000;
/// Longest a released platter is left to turn by itself.
const FREEWHEEL_MAX_US: i64 = 2_000_000;

impl MidiEngine {
    pub fn new(mapping: Mapping, tempo_range: f64) -> Self {
        Self {
            learn: None,
            wizard: None,
            tempo_range,
            quantize_loops: true,
            last_message: None,
            card: ControllerCard::default(),
            takeover: HashMap::new(),
            msb: HashMap::new(),
            lsb: HashMap::new(),
            hires: HashSet::new(),
            jog_pos: [0.0; DECKS],
            ring_ticks: [0; DECKS],
            ring_bend: [0.0; DECKS],
            calibration: None,
            now: 0,
            hand: std::array::from_fn(|_| VecDeque::new()),
            free: [None; DECKS],
            rolls: std::array::from_fn(|_| Vec::new()),
            pad_modes: [PadMode::default(); DECKS],
            sides: [0, 1],
            shift: HashSet::new(),
            held: HashMap::new(),
            fx_deck: mapping_fx_decks(&mapping),
            resend: Vec::new(),
            mapping,
        }
    }

    /// Use another mapping (a controller just connected): nothing held,
    /// every deck's pads on hot cues, the sides on decks A and B, the FX
    /// units where it powers on.
    pub fn load(&mut self, mapping: Mapping) {
        self.fx_deck = mapping_fx_decks(&mapping);
        self.mapping = mapping;
        self.learn = None;
        self.wizard = None;
        self.pad_modes = [PadMode::default(); DECKS];
        self.sides = [0, 1];
        self.shift.clear();
        self.held.clear();
        self.resend.clear();
    }

    /// The deck FX unit `unit` works, if any.
    pub fn fx_unit_deck(&self, unit: u8) -> Option<u8> {
        self.fx_deck.get(unit as usize).copied().flatten()
    }

    /// Lights to send again whether or not they've changed: the controller
    /// thread forgets what it last sent them.
    pub fn take_resend(&mut self) -> Vec<(u8, u8)> {
        std::mem::take(&mut self.resend)
    }

    /// Deck `deck`'s pad mode.
    pub fn pad_mode(&self, deck: u8) -> PadMode {
        self.pad_modes
            .get(deck as usize)
            .copied()
            .unwrap_or_default()
    }

    /// True while a [`Target::Shift`] button is held.
    pub fn shift_held(&self) -> bool {
        !self.shift.is_empty()
    }

    /// Let go of every button held, as if it were let go now: SHIFT, a jog
    /// wheel touched, a cue or a roll held. For when the controller's
    /// messages stop reaching the engine for a while (the DECK button
    /// check), so nothing stays held in the room meanwhile.
    pub fn let_go(&mut self, room: &RoomState) -> Vec<MidiOut> {
        let mut out = Vec::new();
        let held: Vec<Trigger> = self.held.keys().copied().collect();
        for t in held {
            self.button(t, false, room, &mut out);
        }
        out
    }

    /// The deck a side of the controller (0 left, 1 right) is on.
    pub fn side_deck(&self, side: usize) -> u8 {
        self.sides[side.min(1)]
    }

    /// Whether a binding's pad mode, if it has one, is its deck's, and its
    /// deck has its side of the controller, if it needs that.
    fn in_mode(&self, b: &Binding) -> bool {
        let Some(deck) = b.target.deck() else {
            return true;
        };
        b.mode.is_none_or(|mode| self.pad_mode(deck) == mode)
            && (!b.layered || self.sides[side_of(deck)] == deck)
    }

    /// The bindings a message on `trig` drives now: those whose deck is in
    /// their pad mode, and while SHIFT is held, the SHIFT + ones if the
    /// control has any (otherwise SHIFT doesn't change what it does).
    fn active(&self, trig: Trigger) -> Vec<Binding> {
        let shift = self.shift_held();
        let mut v: Vec<Binding> = self
            .mapping
            .bindings
            .iter()
            .filter(|b| b.trigger == trig && (shift || !b.shift) && self.in_mode(b))
            .cloned()
            .collect();
        if shift && v.iter().any(|b| b.shift) {
            v.retain(|b| b.shift);
        }
        v
    }

    /// While learning, SHIFT and the pad mode buttons keep working, so the
    /// DJ can hold SHIFT or pick the pad mode for the control being learned
    /// (unless one of them is what's being learned).
    fn modifier_message(&self, raw: RawMidi, learning: Target) -> bool {
        let trig = match raw {
            RawMidi::NoteOn { ch, note, .. } | RawMidi::NoteOff { ch, note } => {
                Trigger::note(ch, note)
            }
            RawMidi::Cc { ch, cc, .. } => Trigger::cc(ch, cc),
            RawMidi::PitchBend { .. } => return false,
        };
        !learning.is_modifier()
            && self
                .mapping
                .bindings
                .iter()
                .any(|b| b.trigger == trig && b.target.is_modifier())
    }

    /// Called every few milliseconds by the controller thread. Turns the
    /// jog movement gathered since last time into actions: scratch and
    /// search movement as one playhead shift (batched so a 1 kHz jog doesn't
    /// flood the room), and, when `bend_window` (seconds) is given, the jog
    /// ring's speed as a pitch bend that stops when the ring stops.
    pub fn flush(&mut self, bend_window: Option<f64>) -> Vec<Action> {
        let mut out = Vec::new();
        let per_rev = self.mapping.ticks_per_rev();
        for d in 0..DECKS {
            let deck = d as u8;
            if self.jog_pos[d] != 0.0 {
                out.push(Action::Nudge {
                    deck,
                    delta: self.jog_pos[d],
                });
                self.jog_pos[d] = 0.0;
            }
            if let Some(dt) = bend_window {
                let ticks = std::mem::take(&mut self.ring_ticks[d]);
                let prev = self.ring_bend[d];
                let next = if ticks == 0 {
                    let decayed = prev * 0.3;
                    if decayed.abs() < 0.002 {
                        0.0
                    } else {
                        decayed
                    }
                } else {
                    let turns_per_sec = ticks as f64 / per_rev / dt.max(0.001);
                    let target = (turns_per_sec
                        * BEND_PER_TURN_PER_SEC
                        * self.mapping.jog_sensitivity as f64)
                        .clamp(-0.5, 0.5);
                    prev * 0.4 + target * 0.6
                };
                if (next - prev).abs() > 0.001 || (next == 0.0 && prev != 0.0) {
                    self.ring_bend[d] = next;
                    out.push(Action::Bend { deck, bend: next });
                }
            }
        }
        out
    }

    /// Tempo the hardware tempo fader is physically set to, if we've seen it.
    pub fn hardware_tempo(&self, deck: u8) -> Option<f64> {
        let v = self.takeover.get(&Target::Tempo { deck })?.last_hw?;
        Some(1.0 + (v as f64 - 0.5) * 2.0 * self.tempo_range)
    }

    /// Targets whose hardware control hasn't "picked up" the room value yet.
    /// (Never the tempo fader: it works relatively instead of waiting.)
    pub fn waiting_for_pickup(&self) -> Vec<Target> {
        self.takeover
            .iter()
            .filter(|(k, _)| !matches!(k, Target::Tempo { .. }))
            .filter(|(_, t)| !t.engaged && t.last_hw.is_some())
            .map(|(k, _)| *k)
            .collect()
    }

    /// The same, for showing the DJ in `room`: without the effect knobs of
    /// slots that are off, which take the knob as it is (see
    /// [`MidiEngine::fx_depth`]).
    pub fn waiting_for_pickup_in(&self, room: &RoomState) -> Vec<Target> {
        let mut waiting = self.waiting_for_pickup();
        waiting.retain(|t| match *t {
            Target::FxDepth { unit, slot } => self
                .fx_unit_deck(unit)
                .and_then(|d| room.decks.get(d as usize))
                .and_then(|d| d.fx.slots.get(slot as usize))
                .is_some_and(|s| s.on),
            _ => true,
        });
        waiting
    }

    fn is_fine_half(&self, ch: u8, cc: u8) -> bool {
        (32..64).contains(&cc)
            && (self.msb.contains_key(&(ch, cc - 32)) || self.hires.contains(&(ch, cc - 32)))
    }

    fn learn_trigger(&self, raw: RawMidi, kind: TargetKind) -> Option<Trigger> {
        match (raw, kind) {
            (RawMidi::NoteOn { ch, note, .. }, TargetKind::Button) => Some(Trigger::note(ch, note)),
            (RawMidi::Cc { ch, cc, val }, TargetKind::Button)
                if val > 0 && !self.is_fine_half(ch, cc) =>
            {
                Some(Trigger::cc(ch, cc))
            }
            (RawMidi::Cc { ch, cc, .. }, TargetKind::Absolute | TargetKind::Relative)
                if !self.is_fine_half(ch, cc) =>
            {
                Some(Trigger::cc(ch, cc))
            }
            (RawMidi::PitchBend { ch, .. }, TargetKind::Absolute) => Some(Trigger {
                kind: MidiKind::PitchBend,
                channel: ch,
                number: 0,
            }),
            _ => None,
        }
    }

    pub fn handle(&mut self, bytes: &[u8], room: &RoomState, local: &LocalParams) -> Vec<MidiOut> {
        self.handle_at(bytes, self.now, room, local)
    }

    /// Handle a message that arrived at shared time `t` (us).
    pub fn handle_at(
        &mut self,
        bytes: &[u8],
        t: i64,
        room: &RoomState,
        local: &LocalParams,
    ) -> Vec<MidiOut> {
        self.now = t;
        let Some(raw) = parse(bytes) else {
            return Vec::new();
        };
        self.last_message = Some(raw.label());

        if let Some(w) = self.wizard.as_mut() {
            w.feed(raw, t);
            return Vec::new();
        }

        if let Some(target) = self.learn.filter(|&t| !self.modifier_message(raw, t)) {
            // Only accept a message that suits the target: a fader can't be
            // learned from a button press, a jog not from its touch sensor.
            let trigger = self.learn_trigger(raw, target.kind());
            if let RawMidi::Cc { ch, cc, val } = raw {
                if !(32..64).contains(&cc) {
                    self.msb.insert((ch, cc), val);
                }
            }
            let Some(trigger) = trigger else {
                return Vec::new();
            };
            self.learn = None;
            let rel = if target == Target::Browse {
                RelMode::TwosComplement
            } else {
                RelMode::Offset64
            };
            // Learned with SHIFT held, it's SHIFT + the control. A pad on a
            // deck with pad mode buttons works in the mode the deck is in.
            let mode = target.deck().filter(|&d| {
                target.is_pad()
                    && self
                        .mapping
                        .bindings
                        .iter()
                        .any(|b| matches!(b.target, Target::PadMode { deck, .. } if deck == d))
            });
            // A control that already takes turns between a side's two decks
            // (the setup found it sends the same for both) still does.
            let layered = target.deck().is_some()
                && self
                    .mapping
                    .bindings
                    .iter()
                    .any(|b| b.layered && b.trigger == trigger);
            let binding = Binding {
                trigger,
                target,
                invert: false,
                rel,
                shift: self.shift_held() && target != Target::Shift,
                mode: mode.map(|d| self.pad_mode(d)),
                light: None,
                layered,
            };
            self.mapping.bindings.retain(|b| b.target != target);
            self.mapping.bindings.push(binding.clone());
            return vec![MidiOut::Learned(binding)];
        }

        let mut out = Vec::new();
        match raw {
            RawMidi::NoteOn { ch, note, .. } => {
                self.button(Trigger::note(ch, note), true, room, &mut out)
            }
            RawMidi::NoteOff { ch, note } => {
                self.button(Trigger::note(ch, note), false, room, &mut out)
            }
            RawMidi::PitchBend { ch, val } => {
                let trig = Trigger {
                    kind: MidiKind::PitchBend,
                    channel: ch,
                    number: 0,
                };
                self.value(trig, val as f32 / 16383.0, None, room, local, &mut out);
            }
            RawMidi::Cc { ch, cc, val } => {
                // 14-bit pair: coarse on CC n, fine on CC n+32.
                if (32..64).contains(&cc) {
                    let coarse = Trigger::cc(ch, cc - 32);
                    if let Some(&msb) = self.msb.get(&(ch, cc - 32)) {
                        if self
                            .mapping
                            .bindings
                            .iter()
                            .any(|b| b.trigger == coarse && b.target.is_absolute())
                        {
                            self.hires.insert((ch, cc - 32));
                            self.lsb.insert((ch, cc - 32), val);
                            let v = (((msb as u16) << 7) | val as u16) as f32 / 16383.0;
                            self.value(coarse, v, None, room, local, &mut out);
                            return out;
                        }
                    }
                }
                self.msb.insert((ch, cc), val);
                let v = if self.hires.contains(&(ch, cc)) {
                    let lsb = *self.lsb.get(&(ch, cc)).unwrap_or(&0);
                    (((val as u16) << 7) | lsb as u16) as f32 / 16383.0
                } else {
                    val as f32 / 127.0
                };
                self.value(Trigger::cc(ch, cc), v, Some(val), room, local, &mut out);
            }
        }
        out
    }

    fn button(&mut self, trig: Trigger, down: bool, room: &RoomState, out: &mut Vec<MidiOut>) {
        let mut targets: Vec<Target> = match (down, self.held.remove(&trig)) {
            (false, Some(pressed)) => pressed,
            _ => self.active(trig).iter().map(|b| b.target).collect(),
        };
        if down {
            // One DECK button for both of a side's decks: each press goes
            // to the one the side isn't on.
            for (side, &deck) in self.sides.iter().enumerate() {
                let on = Target::DeckSelect { deck };
                let other = |t: &Target| matches!(t, Target::DeckSelect { deck: d } if *d != deck && side_of(*d) == side);
                if targets.contains(&on) && targets.iter().any(other) {
                    targets.retain(|t| *t != on);
                }
            }
            self.held.insert(trig, targets.clone());
        }
        for target in targets {
            match target {
                Target::Play { deck } if down => {
                    let d = &room.decks[deck as usize];
                    let a = if d.playing && !d.cue_preview {
                        Action::Pause { deck }
                    } else {
                        Action::Play { deck }
                    };
                    out.push(MidiOut::Action(a));
                }
                Target::Cue { deck } => out.push(MidiOut::Action(if down {
                    Action::CueDown { deck }
                } else {
                    Action::CueUp { deck }
                })),
                Target::BackToStart { deck } if down => {
                    out.push(MidiOut::Action(Action::Seek { deck, pos: 0.0 }))
                }
                Target::Sync { deck } if down => {
                    let on = !room.decks[deck as usize].synced;
                    out.push(MidiOut::Action(Action::Sync { deck, on }));
                }
                Target::SyncOff { deck } if down => {
                    out.push(MidiOut::Action(Action::Sync { deck, on: false }));
                    let tempo = self.hardware_tempo(deck).unwrap_or(1.0);
                    out.push(MidiOut::Action(Action::Tempo { deck, tempo }));
                    // The fader is in control again straight away, no pickup.
                    if let Some(t) = self.takeover.get_mut(&Target::Tempo { deck }) {
                        t.engaged = true;
                        t.last_sent = t.last_hw;
                    }
                }
                Target::DeckSelect { deck } if down => self.deck_selected(deck),
                Target::HotCue { deck, slot } => out.push(MidiOut::Action(if down {
                    Action::HotCue { deck, slot }
                } else {
                    Action::HotCueUp { deck, slot }
                })),
                Target::HotCueClear { deck, slot } if down => {
                    out.push(MidiOut::Action(Action::ClearHotCue { deck, slot }))
                }
                Target::LoopIn { deck } if down => out.push(MidiOut::Action(Action::LoopIn {
                    deck,
                    quantize: self.quantize_loops,
                })),
                Target::LoopOut { deck } if down => out.push(MidiOut::Action(Action::LoopOut {
                    deck,
                    quantize: self.quantize_loops,
                })),
                Target::Reloop { deck } if down => {
                    out.push(MidiOut::Action(Action::Reloop { deck }))
                }
                Target::LoopHalve { deck } if down => {
                    out.push(MidiOut::Action(Action::LoopHalve { deck }))
                }
                Target::LoopDouble { deck } if down => {
                    out.push(MidiOut::Action(Action::LoopDouble { deck }))
                }
                Target::BeatLoop { deck, size } if down => {
                    out.push(MidiOut::Action(Action::BeatLoop {
                        deck,
                        beats: beat_loop_beats(size),
                    }))
                }
                Target::Roll { deck, size } => {
                    if let Some(a) = self.roll_pad(deck, size, down) {
                        out.push(MidiOut::Action(a));
                    }
                }
                Target::Pfl { deck } if down => out.push(MidiOut::TogglePfl(deck)),
                Target::JogTouch { deck } => {
                    let d = deck as usize;
                    let scratching = room.decks[d].scratching;
                    if down && self.mapping.vinyl && !scratching {
                        self.hand[d].clear();
                        self.free[d] = None;
                        out.push(MidiOut::Action(Action::JogTouch {
                            deck,
                            down,
                            spin: 0.0,
                        }));
                    } else if !down && scratching {
                        // Movement not sent yet goes first, so the record is
                        // let go exactly where the hand left it.
                        let moved = std::mem::take(&mut self.jog_pos[d]);
                        if moved != 0.0 {
                            out.push(MidiOut::Action(Action::Nudge { deck, delta: moved }));
                        }
                        let spin = self.hand_speed(d) as f32;
                        self.hand[d].clear();
                        self.free[d] = Some((self.now, self.now));
                        out.push(MidiOut::Action(Action::JogTouch { deck, down, spin }));
                    }
                }
                Target::BendDown { deck } => out.push(MidiOut::Action(Action::Bend {
                    deck,
                    bend: if down { -BEND } else { 0.0 },
                })),
                Target::BendUp { deck } => out.push(MidiOut::Action(Action::Bend {
                    deck,
                    bend: if down { BEND } else { 0.0 },
                })),
                Target::Load { deck } if down => out.push(MidiOut::LoadSelected(deck)),
                Target::BrowsePress if down => out.push(MidiOut::BrowsePress),
                Target::BrowseBack if down => out.push(MidiOut::BrowseBack),
                Target::PadMode { deck, mode } if down => {
                    if let Some(m) = self.pad_modes.get_mut(deck as usize) {
                        *m = mode;
                    }
                }
                Target::Shift => {
                    if down {
                        self.shift.insert(trig);
                    } else {
                        self.shift.remove(&trig);
                    }
                }
                Target::FxOn { unit, slot } if down => self.fx_on(unit, slot, room, out),
                Target::FxBeatsDown { unit } | Target::FxBeatsUp { unit } if down => {
                    let Some(deck) = self.fx_unit_deck(unit) else {
                        continue;
                    };
                    let now = room.decks[deck as usize].fx.beats;
                    let beats = if matches!(target, Target::FxBeatsUp { .. }) {
                        (now + 1).min(FX_BEATS.len() as u8 - 1)
                    } else {
                        now.saturating_sub(1)
                    };
                    if beats != now {
                        out.push(MidiOut::Action(Action::FxBeats { deck, beats }));
                    }
                }
                Target::FxNext { unit, slot } if down => {
                    let Some(deck) = self.fx_unit_deck(unit) else {
                        continue;
                    };
                    if let Some(s) = room.decks[deck as usize].fx.slots.get(slot as usize) {
                        out.push(MidiOut::Action(Action::FxSelect {
                            deck,
                            slot,
                            kind: s.kind.next(),
                        }));
                    }
                }
                Target::FxAssign { unit, deck } if down => {
                    if let Some(d) = self.fx_deck.get_mut(unit as usize) {
                        *d = Some(deck).filter(|&d| (d as usize) < DECKS);
                    }
                }
                Target::FxAssignNone { unit } if down => {
                    if let Some(d) = self.fx_deck.get_mut(unit as usize) {
                        *d = None;
                    }
                }
                Target::FxFocus { unit, slot } if down => self.fx_focus(unit, slot),
                _ => {}
            }
        }
    }

    /// An FX unit's ON/OFF for `slot`. Switching a slot on first sets its
    /// depth to where the knob is, when that's known, and the knob is in
    /// control from there: knob at zero, press ON, turn up, and the effect
    /// comes in from nothing.
    fn fx_on(&mut self, unit: u8, slot: u8, room: &RoomState, out: &mut Vec<MidiOut>) {
        let Some(deck) = self.fx_unit_deck(unit) else {
            return;
        };
        let Some(s) = room.decks[deck as usize].fx.slots.get(slot as usize) else {
            return;
        };
        let on = !s.on;
        if on {
            let t = self
                .takeover
                .entry(Target::FxDepth { unit, slot })
                .or_default();
            if let Some(v) = t.last_hw {
                t.engaged = true;
                t.last_sent = Some(v);
                if v != s.depth {
                    out.push(MidiOut::Action(Action::FxDepth { deck, slot, v }));
                }
            }
        }
        out.push(MidiOut::Action(Action::FxOn { deck, slot, on }));
    }

    /// FX SELECT moved onto `slot` of `unit`. The knob, shared between the
    /// slots, may have moved since this slot last heard from it, and the
    /// controller now says where it is: that reading (both halves of a
    /// 14-bit knob) only says where it is, so selecting a slot never
    /// changes its depth. The controller shows only the selected slot's
    /// lights, so they go out again.
    fn fx_focus(&mut self, unit: u8, slot: u8) {
        let t = self
            .takeover
            .entry(Target::FxDepth { unit, slot })
            .or_default();
        t.relearn = 1;
        t.relearn_fine = false;
        // It may have been turned for another slot: a slot that's on waits
        // for it to come back.
        t.engaged = false;
        for b in &self.mapping.bindings {
            if b.target == (Target::FxOn { unit, slot }) && b.trigger.kind == MidiKind::Note {
                self.resend
                    .push((0x90 | b.trigger.channel, b.trigger.number));
            }
        }
        for l in &self.mapping.fx_beat_lights {
            if l.unit == unit {
                self.resend.push((0xB0 | l.channel, l.cc));
            }
        }
    }

    /// A roll pad pressed or let go. The latest pad held rolls: letting go
    /// of it while another is still held goes back to that one's size, and
    /// letting go of one that isn't rolling does nothing.
    fn roll_pad(&mut self, deck: u8, size: u8, down: bool) -> Option<Action> {
        let held = self.rolls.get_mut(deck as usize)?;
        let roll = |size: u8, on: bool| Action::Roll {
            deck,
            beats: roll_beats(size),
            on,
        };
        if down {
            held.retain(|&s| s != size);
            held.push(size);
            return Some(roll(size, true));
        }
        if held.last() != Some(&size) {
            held.retain(|&s| s != size);
            return None;
        }
        held.pop();
        Some(match held.last() {
            Some(&back) => roll(back, true),
            None => roll(size, false),
        })
    }

    /// Remember scratch movement, to know how fast the hand let go.
    fn track_hand(&mut self, d: usize, moved: f64) {
        let now = self.now;
        let h = &mut self.hand[d];
        h.push_back((now, moved));
        while h.front().is_some_and(|&(t, _)| t < now - HAND_WINDOW_US) {
            h.pop_front();
        }
    }

    /// How fast the hand was moving the record just now (track seconds per
    /// second, negative = backwards).
    fn hand_speed(&self, d: usize) -> f64 {
        let from = self.now - HAND_WINDOW_US;
        let moved: f64 = self.hand[d]
            .iter()
            .filter(|&&(t, _)| t >= from)
            .map(|&(_, m)| m)
            .sum();
        moved / (HAND_WINDOW_US as f64 * 1e-6)
    }

    /// True while a platter the hand just let go of is still turning by
    /// itself: until it has been still for a moment.
    fn freewheeling(&mut self, d: usize) -> bool {
        let now = self.now;
        match self.free[d] {
            Some((since, last))
                if now - last < PLATTER_STILL_US && now - since < FREEWHEEL_MAX_US =>
            {
                self.free[d] = Some((since, now));
                true
            }
            _ => {
                self.free[d] = None;
                false
            }
        }
    }

    fn value(
        &mut self,
        trig: Trigger,
        v: f32,
        raw7: Option<u8>,
        room: &RoomState,
        local: &LocalParams,
        out: &mut Vec<MidiOut>,
    ) {
        let bindings = self.active(trig);
        let mut pressed = false;
        for b in &bindings {
            match b.target.kind() {
                TargetKind::Relative => {
                    let Some(raw) = raw7 else { continue };
                    let ticks = match b.rel {
                        RelMode::Offset64 => raw as i32 - 64,
                        RelMode::TwosComplement => {
                            if raw < 64 {
                                raw as i32
                            } else {
                                raw as i32 - 128
                            }
                        }
                    };
                    let ticks = if b.invert { -ticks } else { ticks };
                    if ticks == 0 {
                        continue;
                    }
                    match b.target {
                        Target::Jog { deck } => {
                            if let Some(c) = self.calibration.as_mut() {
                                *c += ticks as i64;
                            }
                            let d = deck as usize;
                            let st = &room.decks[d];
                            let turns = ticks as f64 / self.mapping.ticks_per_rev();
                            if st.scratching {
                                // Hand on the record: it moves exactly with the platter.
                                let moved = turns
                                    * SECONDS_PER_TURN
                                    * self.mapping.scratch_sensitivity as f64;
                                self.jog_pos[d] += moved;
                                self.track_hand(d, moved);
                            } else if self.freewheeling(d) {
                                // Just let go: the platter turns on by itself
                                // (even a still hand flicks it as it lifts off).
                                // That isn't the DJ moving it; a spinning
                                // record runs on in the room model instead.
                            } else if !st.playing {
                                // Paused: turning searches through the track.
                                self.jog_pos[d] += turns * SECONDS_PER_TURN;
                            } else {
                                // Playing: the ring bends the speed while it turns.
                                self.ring_ticks[d] += ticks;
                            }
                        }
                        Target::Browse => out.push(MidiOut::Browse(ticks)),
                        _ => {}
                    }
                }
                TargetKind::Button => {
                    // A button sent as CC (127 press / 0 release), once
                    // for all its bindings.
                    if !std::mem::replace(&mut pressed, true) {
                        let down = raw7.map(|r| r >= 64).unwrap_or(v >= 0.5);
                        self.button(trig, down, room, out);
                    }
                }
                TargetKind::Absolute => {
                    if self.card.sets(b.target) {
                        // The controller already turns its own output up
                        // or down: Tandem's level stays where it is.
                        continue;
                    }
                    let v = if b.invert { 1.0 - v } else { v };
                    if let Target::Tempo { deck } = b.target {
                        if let Some(tempo) = self.tempo_fader(deck, v, room) {
                            out.push(MidiOut::Action(Action::Tempo { deck, tempo }));
                        }
                        continue;
                    }
                    if let Target::TempoShift { deck } = b.target {
                        // A controller that sends the same message with
                        // SHIFT held can't tell the two apart: the plain
                        // fader wins.
                        if !bindings.iter().any(|x| x.target == Target::Tempo { deck }) {
                            let t = self.tempo_takeover(deck);
                            t.last_hw = Some(v);
                            t.past_limit = 0.0;
                        }
                        continue;
                    }
                    let fine = raw7.is_none() && trig.kind == MidiKind::Cc;
                    match b.target {
                        Target::FxDepth { unit, slot } => {
                            self.fx_depth(unit, slot, v, fine, room, out);
                            continue;
                        }
                        Target::FxDepthAll { unit } => {
                            for slot in 0..FX_SLOTS as u8 {
                                self.fx_depth(unit, slot, v, fine, room, out);
                            }
                            continue;
                        }
                        _ => {}
                    }
                    let current = self.current(b.target, room, local);
                    if !self.pickup(b.target, v, current) {
                        continue;
                    }
                    let range = self.tempo_range;
                    out.push(match b.target {
                        Target::Tempo { deck } => MidiOut::Action(Action::Tempo {
                            deck,
                            tempo: 1.0 + (v as f64 - 0.5) * 2.0 * range,
                        }),
                        Target::Volume { deck } => MidiOut::Action(Action::Volume { deck, v }),
                        Target::Eq { deck, band } => MidiOut::Action(Action::Eq { deck, band, v }),
                        Target::Trim { deck } => MidiOut::Action(Action::Trim { deck, v }),
                        Target::Filter { deck } => MidiOut::Action(Action::Filter { deck, v }),
                        Target::Crossfader => MidiOut::Action(Action::Crossfader { v }),
                        Target::CueMix => MidiOut::CueMix(v),
                        Target::HeadphoneLevel => MidiOut::HeadphoneLevel(v),
                        Target::MasterLevel => MidiOut::MasterLevel(v),
                        _ => continue,
                    });
                }
            }
        }
    }

    /// The tempo fader at `v` (0..1). It responds at once, never waits for
    /// pickup and never makes the tempo jump:
    ///
    /// - While the deck's tempo is where the fader says, the fader sets it
    ///   directly.
    /// - When they disagree (SYNC changed the tempo, the other deck's fader
    ///   carried it, your partner moved it, or you moved the fader with
    ///   SHIFT held), moving the fader changes the tempo by exactly as much
    ///   as the fader moved, from wherever the deck is. SHIFT + SYNC puts
    ///   the deck back on the fader.
    ///
    /// So a DJ who runs out of travel can SHIFT-move the fader back and keep
    /// going the same way, as far as the room's limit (±50%), past the
    /// fader's own range. Whether the deck is on the fader is judged by
    /// where the fader was before this move: a quick move whose new reading
    /// happens to land on the deck's tempo mustn't grab it (and throw away
    /// what SHIFT gained). The deck is back on the fader only when SHIFT
    /// left the fader exactly at the deck's tempo, the deck was moved to
    /// where the fader is, or the tempo stopped at the limit and the fader
    /// caught up.
    ///
    /// A deck nobody has touched (tempo 0%) takes the fader's position
    /// straight away, like the other controls.
    ///
    /// On a four-deck controller, one fader drives decks 1 and 3 (or 2 and
    /// 4). After its DECK button switches back, the fader may be anywhere:
    /// the first reading (both halves, on a 14-bit fader) only says where it
    /// is, and from there it works as above. So switching decks never makes
    /// a tempo jump, and the deck is usually off the fader afterwards.
    fn tempo_fader(&mut self, deck: u8, v: f32, room: &RoomState) -> Option<f64> {
        /// Closer than this (0.05%) counts as "on the fader".
        const MATCH: f64 = 0.0005;
        let range = self.tempo_range;
        let at = |v: f32| 1.0 + (v as f64 - 0.5) * 2.0 * range;
        let current = room.decks.get(deck as usize)?.tempo;
        let t = self.tempo_takeover(deck);
        let prev = t.last_hw.replace(v);
        if t.relearn > 0 {
            t.relearn -= 1;
            return None;
        }
        let past_limit = std::mem::take(&mut t.past_limit);
        let hw = at(v);
        let next = match prev {
            Some(p) if (at(p) - current).abs() < MATCH => hw,
            // First reading from this fader: it takes the deck if it's
            // already there or nobody has touched the deck; otherwise we
            // only learn where it is.
            None if (hw - current).abs() < MATCH => hw,
            None if t.last_sent.is_none() && (current - 1.0).abs() < 1e-9 => hw,
            None => return None,
            Some(p) => {
                // Held at the limit, the fader may run on past it by up to
                // one coarse step (1/128 of its travel), taken up first on
                // the way back. A 14-bit fader's coarse half arrives before
                // its fine half, and that dip and return mustn't creep the
                // tempo off the limit while the DJ pushes against it.
                let at_limit = current <= 0.5 || current >= 1.5;
                let want = current + hw - at(p) + if at_limit { past_limit } else { 0.0 };
                let next = want.clamp(0.5, 1.5);
                let step = 2.0 * range / 128.0;
                t.past_limit = (want - next).clamp(-step, step);
                // Caught up with a tempo held at the limit: on the fader
                // from here (snapped, so rounding can't build up).
                if (next - hw).abs() < MATCH {
                    t.past_limit = 0.0;
                    hw
                } else {
                    next
                }
            }
        };
        t.engaged = next == hw;
        t.last_sent = Some(v);
        (next != current).then_some(next)
    }

    /// An effect knob at `v` for `slot` of `unit` (`fine`: the fine half of
    /// a 14-bit knob). A reading that only says where the knob is (after FX
    /// SELECT) or repeats the last one isn't a move. Otherwise a slot
    /// that's off takes the knob as it is: nobody hears it, so there's
    /// nothing to wait for. One that's on waits for the knob to pick its
    /// depth up, and never takes it just because nobody has moved it, so
    /// touching the knob never makes a running effect jump.
    fn fx_depth(
        &mut self,
        unit: u8,
        slot: u8,
        v: f32,
        fine: bool,
        room: &RoomState,
        out: &mut Vec<MidiOut>,
    ) {
        let target = Target::FxDepth { unit, slot };
        let t = self.takeover.entry(target).or_default();
        if t.relearn > 0 || (t.relearn_fine && fine) {
            if fine {
                t.relearn_fine = false;
            } else {
                t.relearn -= 1;
                t.relearn_fine = true;
            }
            t.last_hw = Some(v);
            return;
        }
        t.relearn_fine = false;
        if t.last_hw == Some(v) {
            return;
        }
        let Some(deck) = self.fx_unit_deck(unit) else {
            self.takeover.entry(target).or_default().last_hw = Some(v);
            return;
        };
        let Some(s) = room.decks[deck as usize].fx.slots.get(slot as usize) else {
            return;
        };
        let send = if s.on {
            self.pickup(target, v, s.depth)
        } else {
            let t = self.takeover.entry(target).or_default();
            (t.last_hw, t.last_sent, t.engaged) = (Some(v), Some(v), true);
            true
        };
        if send && v != s.depth {
            out.push(MidiOut::Action(Action::FxDepth { deck, slot, v }));
        }
    }

    /// Deck `deck`'s tempo fader. Its `last_hw` is where the fader
    /// physically is, whichever message (with or without SHIFT) said so last.
    fn tempo_takeover(&mut self, deck: u8) -> &mut Takeover {
        self.takeover.entry(Target::Tempo { deck }).or_default()
    }

    /// A DECK button gave its side to `deck`. The side's tempo fader drove
    /// the other deck meanwhile, so where it was for this one is forgotten:
    /// its next reading (both halves of it, if it's a 14-bit fader) only
    /// says where it is now. The coarse half seen last on this deck's
    /// channel is stale too, so a fine half on its own waits for a new one.
    /// A deck whose fader hasn't sent anything yet is taken to be 14-bit
    /// when another deck's is.
    fn deck_selected(&mut self, deck: u8) {
        self.sides[side_of(deck)] = deck;
        let tempo_cc = |b: &Binding| {
            matches!(b.target, Target::Tempo { .. } | Target::TempoShift { .. })
                && b.trigger.kind == MidiKind::Cc
        };
        let hires = self
            .mapping
            .bindings
            .iter()
            .filter(|b| tempo_cc(b))
            .any(|b| self.hires.contains(&(b.trigger.channel, b.trigger.number)));
        for b in &self.mapping.bindings {
            if tempo_cc(b) && b.target.deck() == Some(deck) {
                self.msb.remove(&(b.trigger.channel, b.trigger.number));
            }
        }
        let t = self.tempo_takeover(deck);
        t.last_hw = None;
        t.past_limit = 0.0;
        t.relearn = if hires { 2 } else { 1 };
    }

    /// Where the control currently is in the room, in 0..1 fader units.
    fn current(&self, target: Target, room: &RoomState, local: &LocalParams) -> f32 {
        match target {
            Target::Tempo { deck } => {
                ((room.decks[deck as usize].tempo - 1.0) / (2.0 * self.tempo_range) + 0.5)
                    .clamp(0.0, 1.0) as f32
            }
            Target::Volume { deck } => room.decks[deck as usize].volume,
            Target::Eq { deck, band } => room.decks[deck as usize].eq[band as usize],
            Target::Trim { deck } => room.decks[deck as usize].trim,
            Target::Filter { deck } => room.decks[deck as usize].filter,
            Target::Crossfader => room.crossfader,
            Target::CueMix => local.cue_mix,
            Target::HeadphoneLevel => local.hp_level,
            Target::MasterLevel => local.master_level,
            _ => 0.0,
        }
    }

    /// The value nobody has changed yet, so the hardware can take it directly.
    fn pristine(target: Target, current: f32) -> bool {
        if matches!(target, Target::FxDepth { .. } | Target::FxDepthAll { .. }) {
            // A running effect never jumps to the knob.
            return false;
        }
        let local = LocalParams::default();
        let default = match target {
            Target::Volume { .. } => 1.0,
            Target::CueMix => local.cue_mix,
            Target::HeadphoneLevel => local.hp_level,
            Target::MasterLevel => local.master_level,
            _ => 0.5,
        };
        (current - default).abs() < 1e-6
    }

    fn pickup(&mut self, target: Target, v: f32, current: f32) -> bool {
        let t = self.takeover.entry(target).or_default();
        if t.engaged {
            if let Some(sent) = t.last_sent {
                if (current - sent).abs() > 0.02 {
                    t.engaged = false; // someone else moved it
                }
            }
        }
        if !t.engaged {
            let crossed = t
                .last_hw
                .map(|h| (h - current).signum() != (v - current).signum())
                .unwrap_or(false);
            if (v - current).abs() < 0.04
                || crossed
                || (t.last_sent.is_none() && Self::pristine(target, current))
            {
                t.engaged = true;
            }
        }
        t.last_hw = Some(v);
        if t.engaged {
            t.last_sent = Some(v);
        }
        t.engaged
    }

    /// Desired state of every light we can drive: (status, note) -> on.
    pub fn leds(&self, room: &RoomState, local: &LocalParams) -> Vec<((u8, u8), bool)> {
        self.light_states(room, local)
            .into_iter()
            .map(|(key, on, _)| (key, on))
            .collect()
    }

    /// What to send every light we can drive: (status, note) -> value
    /// (127 lit and 0 unlit, unless the binding says otherwise).
    pub fn lights(&self, room: &RoomState, local: &LocalParams) -> Vec<((u8, u8), u8)> {
        self.light_states(room, local)
            .into_iter()
            .map(|(key, _, value)| (key, value))
            .collect()
    }

    /// Every light: (status, note), whether it's lit, and its value. A
    /// control's lights show what it does without SHIFT, and a pad shows
    /// its binding for the deck's pad mode (off if it has none).
    fn light_states(&self, room: &RoomState, local: &LocalParams) -> Vec<((u8, u8), bool, u8)> {
        let mut out: Vec<((u8, u8), bool, u8)> = Vec::new();
        // Lights with a binding in use: the first one sets the light, and a
        // later one only to light it.
        let mut in_use = HashSet::new();
        for b in &self.mapping.bindings {
            if b.trigger.kind != MidiKind::Note || b.shift {
                continue;
            }
            let Some(on) = self.lit(b, room, local) else {
                continue;
            };
            let key = (0x90 | b.trigger.channel, b.trigger.number);
            let i = match out.iter().position(|l| l.0 == key) {
                Some(i) => i,
                None => {
                    out.push((key, false, 0));
                    out.len() - 1
                }
            };
            if !self.in_mode(b) {
                continue;
            }
            let (lit, unlit) = b.light.unwrap_or((0x7F, 0x00));
            if in_use.insert(key) || (on && !out[i].1) {
                out[i] = (key, on, if on { lit } else { unlit });
            }
        }
        out
    }

    /// Whether a control's light is lit, if it has one.
    fn lit(&self, b: &Binding, room: &RoomState, local: &LocalParams) -> Option<bool> {
        let target = b.target;
        Some(match target {
            Target::Play { deck } => {
                let d = &room.decks[deck as usize];
                d.playing && !d.cue_preview
            }
            Target::Cue { deck } => {
                let d = &room.decks[deck as usize];
                d.track.is_some() && (!d.playing || d.cue_preview)
            }
            Target::HotCue { deck, slot } => {
                room.decks[deck as usize].hot_cues[slot as usize].is_some()
            }
            Target::Sync { deck } => room.decks[deck as usize].synced,
            Target::Pfl { deck } => local.pfl[deck as usize],
            // Lit while looping; LOOP IN also while it waits for LOOP OUT.
            Target::LoopIn { deck } => {
                let d = &room.decks[deck as usize];
                d.looping || (d.loop_in.is_some() && d.loop_out.is_none())
            }
            Target::LoopOut { deck } | Target::Reloop { deck } => room.decks[deck as usize].looping,
            // The pad of the beat loop that's playing.
            Target::BeatLoop { deck, size } => {
                let d = &room.decks[deck as usize];
                d.looping
                    && d.loop_beats
                        .is_some_and(|b| (b - beat_loop_beats(size)).abs() < 1e-9)
            }
            // The pad of the roll that's held.
            Target::Roll { deck, size } => room.decks[deck as usize]
                .roll
                .is_some_and(|r| (r.beats - roll_beats(size)).abs() < 1e-9),
            // The deck's pad mode, and SHIFT while it's held.
            Target::PadMode { deck, mode } => self.pad_mode(deck) == mode,
            Target::Shift => self.shift_held(),
            // A DECK button, where Tandem keeps track of the decks (a
            // controller that does it itself lights its own): lit for the
            // deck the side is on, and one button for both decks while the
            // side is on deck C or D.
            Target::DeckSelect { deck } if self.mapping.bindings.iter().any(|x| x.layered) => {
                let shared = self.mapping.bindings.iter().any(|x| {
                    x.trigger == b.trigger
                        && matches!(x.target, Target::DeckSelect { deck: d } if d != deck)
                });
                self.sides[side_of(deck)] == deck && (!shared || deck >= 2)
            }
            // The slot on the deck the unit works, if it works one.
            Target::FxOn { unit, slot } => self.fx_unit_deck(unit).is_some_and(|d| {
                room.decks[d as usize]
                    .fx
                    .slots
                    .get(slot as usize)
                    .is_some_and(|s| s.on)
            }),
            _ => return None,
        })
    }

    /// Desired value of every FX unit's BEATS indicator: (status, CC) ->
    /// value, for the BEATS of the deck the unit works.
    pub fn fx_beat_lights(&self, room: &RoomState) -> Vec<((u8, u8), u8)> {
        self.mapping
            .fx_beat_lights
            .iter()
            .map(|l| {
                let value = self
                    .fx_unit_deck(l.unit)
                    .and_then(|d| l.values.get(room.decks[d as usize].fx.beats as usize))
                    .copied()
                    .unwrap_or(l.dark);
                ((0xB0 | l.channel, l.cc), value)
            })
            .collect()
    }

    /// Every FX unit's BEATS indicator, dark: for when Tandem lets go.
    pub fn fx_beat_lights_dark(&self) -> Vec<((u8, u8), u8)> {
        self.mapping
            .fx_beat_lights
            .iter()
            .map(|l| ((0xB0 | l.channel, l.cc), l.dark))
            .collect()
    }

    /// Desired value of every level meter: (status, CC) -> value.
    pub fn meters(&self, levels: &Levels) -> Vec<((u8, u8), u8)> {
        self.mapping
            .meters
            .iter()
            .map(|m| {
                let level = levels.channels[(m.deck as usize).min(DECKS - 1)];
                ((0xB0 | m.channel, m.cc), pioneer_meter_value(level))
            })
            .collect()
    }
}

/// A Pioneer channel meter's value (0–127) for a level.
///
/// Measured on a DDJ-FLX6, whose meters have five lights (two green, two
/// orange, red): they come on at values of about 37–40, 65–69, 85–88,
/// 101–106 and exactly 119. Each anchor below sits at the top of one of
/// those ranges, so each light comes on at a level the on-screen meter
/// marks too: green from -24 and -14 dBFS, orange from -8 and -3, and red
/// only over full scale. In between, the value climbs evenly in dB, so a
/// meter with more lights also climbs smoothly.
pub fn pioneer_meter_value(level: f32) -> u8 {
    let db = to_db(level);
    if db >= 0.0 {
        return 127;
    }
    let mut prev = PIONEER_METER[0];
    for &(at, v) in &PIONEER_METER[1..] {
        if db < at {
            let x = ((db - prev.0) / (at - prev.0)).clamp(0.0, 1.0);
            return (prev.1 + x * (v - prev.1)).floor() as u8;
        }
        prev = (at, v);
    }
    PIONEER_METER[PIONEER_METER.len() - 1].1 as u8
}

/// (dBFS, value) anchors for [`pioneer_meter_value`], below full scale.
const PIONEER_METER: [(f32, f32); 6] = [
    (-48.0, 0.0),
    (-24.0, 40.0),
    (-14.0, 69.0),
    (-8.0, 88.0),
    (-3.0, 106.0),
    (0.0, 118.0),
];

/// Mapping file name for a MIDI port. Windows prefixes ports with "2- " etc.,
/// which can change between sessions, so strip it.
pub fn mapping_key(port_name: &str) -> String {
    let trimmed = match port_name.split_once("- ") {
        Some((n, rest)) if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => rest,
        _ => port_name,
    };
    trimmed
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

/// A ready-made mapping for controllers we know, if this port is one.
pub fn builtin_preset(port_name: &str) -> Option<Mapping> {
    let n = port_name.to_uppercase();
    let letters: String = n.chars().filter(|c| c.is_alphanumeric()).collect();
    if letters.contains("SCLIVE4") {
        Some(denon_sc_live_4(port_name))
    } else if letters.contains("INPULSE200MK2") {
        // Not the first Inpulse 200 or the MK3: other labels, other maps.
        Some(hercules_inpulse_200_mk2(port_name))
    } else if n.contains("DDJ-FLX6") {
        Some(ddj_flx6(port_name))
    } else if n.contains("DDJ-FLX4") || n.contains("DDJ-400") {
        Some(pioneer_ddj(
            port_name,
            "Pioneer DDJ-FLX4 / DDJ-400",
            2,
            720.0,
        ))
    } else {
        None
    }
}

/// The DDJ-FLX6: the Pioneer DDJ layout on four decks, and its MASTER LEVEL
/// and HEADPHONE LEVEL knobs, 14-bit CCs on channel 7 (from Pioneer's FLX6
/// MIDI list; HEADPHONE LEVEL also recorded from one).
///
/// Its DECK buttons switch a side between decks 1 and 3 (or 2 and 4), and
/// it says which deck has the side now: note 0x3C on the deck's channel,
/// on (127) for the deck it switched to and off for the other one (from
/// the MIDI list). It doesn't say where the tempo fader is.
fn ddj_flx6(device: &str) -> Mapping {
    // Large jogs: same resolution as the DDJ-SX family until calibrated.
    let mut m = pioneer_ddj(device, "Pioneer DDJ-FLX6", 4, 2048.0);
    m.bind(Trigger::cc(6, 0x08), Target::MasterLevel);
    m.bind(Trigger::cc(6, 0x0D), Target::HeadphoneLevel);
    for deck in 0..4 {
        m.bind(Trigger::note(deck, 0x3C), Target::DeckSelect { deck });
    }
    flx6_beat_fx(&mut m);
    m
}

/// The DDJ-FLX6's BEAT FX values for its BEATS lights (CC 0x64), for each
/// of [`FX_BEATS`]: 1/16 and 1/8 blink the 1/4 light, 3/4 is 0x21 (not in
/// Pioneer's list) and 2 lights 1 and 4. Measured on the unit; 0x03, 0x04
/// and 0x07–0x0A are also in Pioneer's list.
const FLX6_BEAT_LIGHTS: [u8; 11] = [
    0x01, 0x02, 0x03, 0x04, 0x21, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A,
];
/// A value that lights none of them (measured; 0x00's isn't known).
const FLX6_BEATS_DARK: u8 = 0x0B;

/// The DDJ-FLX6's Beat FX strip: two FX units, FX1 on MIDI channel 5 and
/// FX2 on channel 6, each three slots (recorded and light-probed on the
/// unit, and in Pioneer's MIDI list). Only the FLX6: the FLX4 and DDJ-400
/// share [`pioneer_ddj`] and their strips send other messages.
///
/// - FX SELECT moves one cursor across both units' slots: `0x70 + slot`,
///   on (127) for the slot selected and off for the old one, then the
///   LEVEL/DEPTH knob's position on the new slot's CC pair. Moving onto
///   the other unit also sends that unit's channel.
/// - LEVEL/DEPTH and ON/OFF send the selected slot's messages only: CC
///   `0x02 + 2 × slot` (14-bit, coarse half first) and note `0x47 + slot`.
///   The ON/OFF light flashes with 127 on that note and is steady with 0;
///   the unit ignores the other slots' notes.
/// - BEAT ◀ ▶: notes 0x06 and 0x07. The BEATS lights are CC 0x64.
/// - CH SELECT points the selected unit at a channel: notes 0x1C–0x1F for
///   CH1–4, 0x14 for MST, on (127) for the new one. Both units power on at
///   CH1, so both start on deck A.
///
/// SHIFT + ON/OFF (0x43), SHIFT + BEAT (0x66, 0x6B), MERGE FX, the PAD FX
/// pads and JOG CUTTER are left alone.
fn flx6_beat_fx(m: &mut Mapping) {
    for unit in 0..2u8 {
        let ch = 4 + unit;
        for slot in 0..FX_SLOTS as u8 {
            m.bind(Trigger::note(ch, 0x47 + slot), Target::FxOn { unit, slot });
            m.bind(
                Trigger::cc(ch, 0x02 + 2 * slot),
                Target::FxDepth { unit, slot },
            );
            m.bind(
                Trigger::note(ch, 0x70 + slot),
                Target::FxFocus { unit, slot },
            );
        }
        m.bind(Trigger::note(ch, 0x06), Target::FxBeatsDown { unit });
        m.bind(Trigger::note(ch, 0x07), Target::FxBeatsUp { unit });
        for deck in 0..4u8 {
            m.bind(
                Trigger::note(ch, 0x1C + deck),
                Target::FxAssign { unit, deck },
            );
        }
        m.bind(Trigger::note(ch, 0x14), Target::FxAssignNone { unit });
        m.fx_beat_lights.push(FxBeatLight {
            channel: ch,
            cc: 0x64,
            unit,
            values: FLX6_BEAT_LIGHTS.to_vec(),
            dark: FLX6_BEATS_DARK,
        });
    }
    m.fx_unit_decks = [Some(0), Some(0), Some(2), Some(3)];
}

/// Pioneer DDJ layout (DDJ-400 / FLX4 / FLX6 generation). Deck N and mixer
/// channel N use MIDI channel N; the crossfader, filters and browser use
/// channel 7; performance pads use channel 8 (deck 1), 10 (deck 2), 12
/// (deck 3), 14 (deck 4), with SHIFT on the next channel up. Decks 1–4
/// drive Tandem's decks A–D: on a four-deck controller, the DECK buttons
/// pick which one each side plays.
///
/// The pads send a different note range in each pad mode: HOT CUE 0x00,
/// PAD FX1 0x10, BEAT JUMP 0x20, SAMPLER 0x30, and with SHIFT + the mode
/// button KEYBOARD 0x40, PAD FX2 0x50, BEAT LOOP 0x60, KEY SHIFT 0x70
/// (recorded from a DDJ-FLX6). Tandem has no beat jump: the BEAT JUMP pads
/// roll.
fn pioneer_ddj(device: &str, name: &str, hw_decks: u8, ticks_per_rev: f32) -> Mapping {
    let mut m = Mapping::empty(device);
    m.preset = Some(name.to_string());
    m.preset_version = PRESET_VERSION;
    m.jog_ticks_per_rev = Some(ticks_per_rev);
    for hw in 0..hw_decks.min(DECKS as u8) {
        let ch = hw;
        let deck = hw;
        m.bind(Trigger::note(ch, 0x0B), Target::Play { deck });
        m.bind(Trigger::note(ch, 0x0C), Target::Cue { deck });
        m.bind(Trigger::note(ch, 0x48), Target::BackToStart { deck });
        m.bind(Trigger::note(ch, 0x58), Target::Sync { deck });
        m.bind(Trigger::note(ch, 0x60), Target::SyncOff { deck });
        m.bind(Trigger::note(ch, 0x36), Target::JogTouch { deck });
        for cc in [0x21, 0x22, 0x23] {
            m.bind(Trigger::cc(ch, cc), Target::Jog { deck });
        }
        m.bind(Trigger::cc(ch, 0x00), Target::Tempo { deck });
        // With SHIFT held, TEMPO sends CC 0x05 (fine half 0x25) instead.
        m.bind(Trigger::cc(ch, 0x05), Target::TempoShift { deck });
        m.bind(Trigger::note(6, 0x46 + hw), Target::Load { deck });
        // Loop section: IN, OUT, RELOOP/EXIT; SHIFT + IN is a 4-beat loop;
        // CUE/LOOP CALL ◀ / ▶ halve and double the loop.
        m.bind(Trigger::note(ch, 0x10), Target::LoopIn { deck });
        m.bind(Trigger::note(ch, 0x11), Target::LoopOut { deck });
        m.bind(Trigger::note(ch, 0x4D), Target::Reloop { deck });
        m.bind(Trigger::note(ch, 0x4C), Target::BeatLoop { deck, size: 4 });
        m.bind(Trigger::note(ch, 0x51), Target::LoopHalve { deck });
        m.bind(Trigger::note(ch, 0x53), Target::LoopDouble { deck });
        // Mixer channel N is deck N's channel.
        m.bind(Trigger::note(ch, 0x54), Target::Pfl { deck });
        m.bind(Trigger::cc(ch, 0x13), Target::Volume { deck });
        m.bind(Trigger::cc(ch, 0x04), Target::Trim { deck });
        m.bind(Trigger::cc(ch, 0x07), Target::Eq { deck, band: 2 });
        m.bind(Trigger::cc(ch, 0x0B), Target::Eq { deck, band: 1 });
        m.bind(Trigger::cc(ch, 0x0F), Target::Eq { deck, band: 0 });
        m.bind(Trigger::cc(6, 0x17 + hw), Target::Filter { deck });
        // Its level meter.
        m.meters.push(MeterLight {
            channel: ch,
            cc: 0x02,
            deck,
        });
        let pads = 7 + 2 * hw;
        for slot in 0..8u8 {
            m.bind(Trigger::note(pads, slot), Target::HotCue { deck, slot });
            m.bind(
                Trigger::note(pads + 1, slot),
                Target::HotCueClear { deck, slot },
            );
            // BEAT LOOP pad mode: ¼, ½, 1, 2, 4, 8, 16, 32 beats.
            m.bind(
                Trigger::note(pads, 0x60 + slot),
                Target::BeatLoop { deck, size: slot },
            );
            // BEAT JUMP pad mode: loop rolls while held, 1/16 to ½ beat on
            // the top row and 1 to 8 beats on the bottom.
            m.bind(
                Trigger::note(pads, 0x20 + slot),
                Target::Roll { deck, size: slot },
            );
        }
    }
    m.bind(Trigger::cc(6, 0x1F), Target::Crossfader);
    m.bind(Trigger::cc(6, 0x0C), Target::CueMix);
    // Browse section (Pioneer's DDJ-FLX6 MIDI list): turning the knob,
    // pressing it, and BACK. SHIFT + press/BACK (0x42, 0x66) and VIEW
    // (0x7A, 0x67, 0x68) are left alone.
    m.bind(Trigger::cc(6, 0x40), Target::Browse).rel = RelMode::TwosComplement;
    m.bind(Trigger::note(6, 0x41), Target::BrowsePress);
    m.bind(Trigger::note(6, 0x65), Target::BrowseBack);
    m
}

/// Lights on Denon controllers: buttons are dim at 1 and lit at 127.
const DENON_BUTTON: Option<(u8, u8)> = Some((127, 1));
/// Denon pads take a colour number, two bits each of red, green and blue
/// (red 48, green 12, blue 3, white 63, off 0). The hot cue pads light in
/// the colours the hot cues have on screen: red, orange, yellow, green,
/// cyan, blue, purple and pink.
const DENON_HOT_CUES: [u8; 8] = [48, 56, 60, 12, 15, 3, 35, 51];
/// Beat loop pads: white while playing, otherwise dark green.
const DENON_BEAT_LOOP_PAD: Option<(u8, u8)> = Some((63, 4));
/// Roll pads: white while held, otherwise dark violet.
const DENON_ROLL_PAD: Option<(u8, u8)> = Some((63, 17));

/// Denon SC Live 4, in computer mode. Denon publishes no MIDI list for it:
/// this comes from two Traktor mappings for it and Mixxx's Denon Prime 4
/// mapping (the same family and messages), and hasn't been tried on the
/// unit yet. The left side sends on MIDI channel 5 and drives deck A, the
/// right side channel 6 and deck B (its DECK 3/4 button only switches a
/// layer in software, so C and D aren't on it); mixer channels 1 and 2 are
/// A and B's, and the browse section, crossfader, MASTER and the headphone
/// knobs are on channel 16.
///
/// The pads send notes 0x0F–0x16 in every pad mode, and SHIFT doesn't
/// change what anything sends, so Tandem keeps track of both: SHIFT (note
/// 0x1C on either side) is [`Target::Shift`], and the pad mode buttons HOT
/// CUE (0x0B), LOOP (0x0C) and ROLL (0x0D) are [`Target::PadMode`]: hot
/// cues (SHIFT + pad clears one), beat loops of ¼ to 32 beats, and loop
/// rolls of 1/16 to 8 beats. SLICER (0x0E) is left alone: Tandem has no
/// slicer, so pressing it keeps the pads as they were.
fn denon_sc_live_4(device: &str) -> Mapping {
    let mut m = Mapping::empty(device);
    m.preset = Some("Denon SC Live 4".to_string());
    m.preset_version = PRESET_VERSION;
    // Mixxx's Prime 4 mapping counts 1000 a turn; calibrate to be sure.
    m.jog_ticks_per_rev = Some(1000.0);
    for side in 0..2u8 {
        let ch = 4 + side;
        let deck = side;
        m.bind(Trigger::note(ch, 0x1C), Target::Shift).light = DENON_BUTTON;
        m.bind(Trigger::note(ch, 0x0A), Target::Play { deck }).light = DENON_BUTTON;
        m.bind(Trigger::note(ch, 0x09), Target::Cue { deck }).light = DENON_BUTTON;
        m.bind(Trigger::note(ch, 0x09), Target::BackToStart { deck })
            .shift = true;
        m.bind(Trigger::note(ch, 0x08), Target::Sync { deck }).light = DENON_BUTTON;
        m.bind(Trigger::note(ch, 0x08), Target::SyncOff { deck })
            .shift = true;
        m.bind(Trigger::note(ch, 0x21), Target::JogTouch { deck });
        m.bind(Trigger::cc(ch, 0x36), Target::Jog { deck }).rel = RelMode::TwosComplement;
        m.bind(Trigger::note(ch, 0x1D), Target::BendDown { deck });
        m.bind(Trigger::note(ch, 0x1E), Target::BendUp { deck });
        // The pitch fader's coarse half (its fine half, CC 0x4B, isn't on
        // CC 0x1F + 32, so Tandem reads it as 7-bit). Its value rises
        // towards +, as on Pioneers.
        m.bind(Trigger::cc(ch, 0x1F), Target::Tempo { deck });
        m.bind(Trigger::cc(ch, 0x1F), Target::TempoShift { deck })
            .shift = true;
        m.bind(Trigger::note(15, 0x01 + side), Target::Load { deck });
        // Loop section: IN, OUT, and pushing the loop knob makes a 4-beat
        // loop (Engine's auto loop).
        m.bind(Trigger::note(ch, 0x25), Target::LoopIn { deck })
            .light = DENON_BUTTON;
        m.bind(Trigger::note(ch, 0x26), Target::LoopOut { deck })
            .light = DENON_BUTTON;
        m.bind(Trigger::note(ch, 0x27), Target::BeatLoop { deck, size: 4 });
        // Mixer channel 1 or 2.
        let mx = side;
        m.bind(Trigger::cc(mx, 0x03), Target::Trim { deck });
        m.bind(Trigger::cc(mx, 0x04), Target::Eq { deck, band: 2 });
        m.bind(Trigger::cc(mx, 0x06), Target::Eq { deck, band: 1 });
        m.bind(Trigger::cc(mx, 0x08), Target::Eq { deck, band: 0 });
        m.bind(Trigger::cc(mx, 0x0B), Target::Filter { deck });
        m.bind(Trigger::cc(mx, 0x0E), Target::Volume { deck });
        m.bind(Trigger::note(mx, 0x0D), Target::Pfl { deck }).light = DENON_BUTTON;
        // Pad mode buttons, and the pads in each mode.
        for (note, mode) in [
            (0x0B, PadMode::HotCues),
            (0x0C, PadMode::BeatLoops),
            (0x0D, PadMode::Rolls),
        ] {
            m.bind(Trigger::note(ch, note), Target::PadMode { deck, mode })
                .light = DENON_BUTTON;
        }
        for i in 0..8u8 {
            let pad = Trigger::note(ch, 0x0F + i);
            let b = m.bind(pad, Target::HotCue { deck, slot: i });
            b.mode = Some(PadMode::HotCues);
            b.light = Some((DENON_HOT_CUES[i as usize], 0));
            let b = m.bind(pad, Target::HotCueClear { deck, slot: i });
            b.mode = Some(PadMode::HotCues);
            b.shift = true;
            let b = m.bind(pad, Target::BeatLoop { deck, size: i });
            b.mode = Some(PadMode::BeatLoops);
            b.light = DENON_BEAT_LOOP_PAD;
            let b = m.bind(pad, Target::Roll { deck, size: i });
            b.mode = Some(PadMode::Rolls);
            b.light = DENON_ROLL_PAD;
        }
    }
    m.bind(Trigger::cc(15, 0x0E), Target::Crossfader);
    m.bind(Trigger::cc(15, 0x12), Target::CueMix);
    m.bind(Trigger::cc(15, 0x14), Target::MasterLevel);
    m.bind(Trigger::cc(15, 0x13), Target::HeadphoneLevel);
    m.bind(Trigger::cc(15, 0x05), Target::Browse).rel = RelMode::TwosComplement;
    m.bind(Trigger::note(15, 0x06), Target::BrowsePress);
    m
}

/// Hercules DJControl Inpulse 200 MK2. Hercules publishes no MIDI list for
/// it: this comes from a djay mapping learned on one, two programs written
/// against one, MIDI recorded from one, and Hercules' list for the first
/// Inpulse 200, whose messages the MK2 kept under new labels. It hasn't
/// been tried on the unit yet, and four things are inferred from its
/// siblings, not shown for the MK2: SHIFT + tempo fader, SHIFT + BROWSER,
/// the tempo fader's direction and the jog's ticks per turn. Where each
/// message came from and how sure it is:
/// docs/controllers/hercules-djcontrol-inpulse-200-mk2.md.
///
/// Global controls send on MIDI channel 1, the left deck on channel 2 and
/// the right deck on channel 3, driving decks A and B. A deck's controls
/// move three channels up while its SHIFT is held, so each SHIFT function
/// is a message of its own. The pads send on channel 7 (left) and 8
/// (right), a different note range in each pad mode, which the unit keeps
/// track of itself: HOT CUE 0x00, STEMS 0x10, FX 0x20, SAMPLER 0x30, each
/// + 8 with SHIFT. Only the hot cue pads have something to do here.
///
/// Every fader and knob is 14-bit, fine half on CC + 32. The unit has no
/// level meters, and its lights are plain: 127 lit, 0 off.
fn hercules_inpulse_200_mk2(device: &str) -> Mapping {
    let mut m = Mapping::empty(device);
    m.preset = Some("Hercules DJControl Inpulse 200 MK2".to_string());
    m.preset_version = PRESET_VERSION;
    // The first Inpulse 200's figure (Mixxx's mapping); calibrate to be
    // sure, with a hand on top: the ring may count more slowly.
    m.jog_ticks_per_rev = Some(248.0);
    for side in 0..2u8 {
        let ch = 1 + side;
        let shift = 4 + side;
        let deck = side;
        m.bind(Trigger::note(ch, 0x07), Target::Play { deck });
        m.bind(Trigger::note(ch, 0x06), Target::Cue { deck });
        m.bind(Trigger::note(shift, 0x06), Target::BackToStart { deck });
        m.bind(Trigger::note(ch, 0x05), Target::Sync { deck });
        m.bind(Trigger::note(shift, 0x05), Target::SyncOff { deck });
        // The platter: turned with a hand on top (CC 0x0A, with the touch
        // sensor's note) or by its ring (CC 0x09).
        m.bind(Trigger::note(ch, 0x08), Target::JogTouch { deck });
        for cc in [0x0A, 0x09] {
            m.bind(Trigger::cc(ch, cc), Target::Jog { deck }).rel = RelMode::TwosComplement;
        }
        // The tempo fader's value falls towards +, the bottom end.
        m.bind(Trigger::cc(ch, 0x08), Target::Tempo { deck }).invert = true;
        m.bind(Trigger::cc(shift, 0x08), Target::TempoShift { deck })
            .invert = true;
        m.bind(Trigger::note(ch, 0x0D), Target::Load { deck });
        // Loop section: LOOP is a 4-beat loop, on and off; ½ and X2 halve
        // and double it; with SHIFT they are IN and OUT.
        m.bind(Trigger::note(ch, 0x03), Target::BeatLoop { deck, size: 4 });
        m.bind(Trigger::note(ch, 0x09), Target::LoopHalve { deck });
        m.bind(Trigger::note(ch, 0x0A), Target::LoopDouble { deck });
        m.bind(Trigger::note(shift, 0x09), Target::LoopIn { deck });
        m.bind(Trigger::note(shift, 0x0A), Target::LoopOut { deck });
        // Mixer channel: GAIN, a two-band EQ, FILTER, headphone cue.
        m.bind(Trigger::cc(ch, 0x00), Target::Volume { deck });
        m.bind(Trigger::cc(ch, 0x05), Target::Trim { deck });
        m.bind(Trigger::cc(ch, 0x04), Target::Eq { deck, band: 2 });
        m.bind(Trigger::cc(ch, 0x02), Target::Eq { deck, band: 0 });
        m.bind(Trigger::cc(ch, 0x01), Target::Filter { deck });
        m.bind(Trigger::note(ch, 0x0C), Target::Pfl { deck });
        // Four pads, in HOT CUE mode; SHIFT + pad clears.
        let pads = 6 + side;
        for slot in 0..4u8 {
            m.bind(Trigger::note(pads, slot), Target::HotCue { deck, slot });
            m.bind(
                Trigger::note(pads, 0x08 + slot),
                Target::HotCueClear { deck, slot },
            );
        }
    }
    m.bind(Trigger::cc(0, 0x00), Target::Crossfader);
    // MASTER and HEADPHONES also turn the unit's own outputs up and down.
    m.bind(Trigger::cc(0, 0x03), Target::MasterLevel);
    m.bind(Trigger::cc(0, 0x04), Target::HeadphoneLevel);
    m.bind(Trigger::cc(0, 0x01), Target::Browse).rel = RelMode::TwosComplement;
    m.bind(Trigger::note(0, 0x00), Target::BrowsePress);
    // SHIFT + pressing BROWSER.
    m.bind(Trigger::note(3, 0x00), Target::BrowseBack);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> MidiEngine {
        MidiEngine::new(Mapping::empty("test"), 0.08)
    }

    #[test]
    fn learn_then_play() {
        let mut e = engine();
        let room = RoomState::default();
        let local = LocalParams::default();
        e.learn = Some(Target::Play { deck: 0 });
        let out = e.handle(&[0x90, 11, 127], &room, &local);
        assert!(matches!(out[0], MidiOut::Learned(_)));
        let out = e.handle(&[0x90, 11, 127], &room, &local);
        assert_eq!(out, vec![MidiOut::Action(Action::Play { deck: 0 })]);
    }

    #[test]
    fn learn_ignores_messages_of_the_wrong_kind() {
        let mut e = engine();
        let (room, local) = (RoomState::default(), LocalParams::default());
        e.learn = Some(Target::Jog { deck: 0 });
        // Touching the platter sends a note first: not a jog turn.
        assert!(e.handle(&[0x90, 0x36, 127], &room, &local).is_empty());
        assert_eq!(e.learn, Some(Target::Jog { deck: 0 }));
        let out = e.handle(&[0xb0, 0x22, 65], &room, &local);
        assert!(matches!(&out[0], MidiOut::Learned(b) if b.trigger == Trigger::cc(0, 0x22)));
        // A fader's fine half (CC 32+) is never learned on its own.
        e.handle(&[0xb0, 0x13, 10], &room, &local);
        e.learn = Some(Target::Volume { deck: 0 });
        assert!(e.handle(&[0xb0, 0x33, 5], &room, &local).is_empty());
        let out = e.handle(&[0xb0, 0x13, 11], &room, &local);
        assert!(matches!(&out[0], MidiOut::Learned(b) if b.trigger == Trigger::cc(0, 0x13)));
    }

    #[test]
    fn soft_takeover_waits_for_pickup() {
        let mut e = engine();
        let local = LocalParams::default();
        let mut room = RoomState::default();
        room.decks[0].volume = 0.9; // someone already set it
        e.mapping
            .bind(Trigger::cc(0, 19), Target::Volume { deck: 0 });
        // Hardware fader is at the bottom: ignored.
        assert!(e.handle(&[0xb0, 19, 0], &room, &local).is_empty());
        assert!(e.handle(&[0xb0, 19, 60], &room, &local).is_empty());
        // Reaches the room value: picks up.
        assert_eq!(e.handle(&[0xb0, 19, 115], &room, &local).len(), 1);
        room.decks[0].volume = 115.0 / 127.0;
        assert_eq!(e.handle(&[0xb0, 19, 120], &room, &local).len(), 1);
        room.decks[0].volume = 120.0 / 127.0;
        // Partner pulls the fader down to 0.2: we release until we cross it again.
        room.decks[0].volume = 0.2;
        assert!(e.handle(&[0xb0, 19, 110], &room, &local).is_empty());
        assert_eq!(e.handle(&[0xb0, 19, 20], &room, &local).len(), 1);
    }

    #[test]
    fn untouched_controls_take_over_immediately() {
        let mut e = engine();
        let (room, local) = (RoomState::default(), LocalParams::default());
        e.mapping.bind(Trigger::cc(6, 0x1F), Target::Crossfader);
        // Crossfader still at its default centre: the hardware position wins.
        assert_eq!(
            e.handle(&[0xb6, 0x1F, 0], &room, &local),
            vec![MidiOut::Action(Action::Crossfader { v: 0.0 })]
        );
    }

    #[test]
    fn fourteen_bit_faders_and_jog() {
        let mut e = engine();
        let (room, local) = (RoomState::default(), LocalParams::default());
        e.mapping.bind(Trigger::cc(0, 0), Target::Tempo { deck: 0 });
        e.mapping
            .bind(Trigger::cc(0, 0x21), Target::Jog { deck: 0 });
        let _ = e.handle(&[0xb0, 0, 64], &room, &local);
        let out = e.handle(&[0xb0, 32, 0], &room, &local);
        match &out[..] {
            [MidiOut::Action(Action::Tempo { tempo, .. })] => assert!((tempo - 1.0).abs() < 0.001),
            other => panic!("{other:?}"),
        }
        // Once known as 14-bit, the coarse half is combined with the last fine half.
        let out = e.handle(&[0xb0, 0, 65], &room, &local);
        match &out[..] {
            [MidiOut::Action(Action::Tempo { tempo, .. })] => {
                let want = 1.0 + ((65.0 * 128.0) / 16383.0 - 0.5) * 0.16;
                assert!((tempo - want).abs() < 1e-6, "{tempo} vs {want}");
            }
            other => panic!("{other:?}"),
        }
        // Paused deck: the jog searches through the track.
        assert!(e.handle(&[0xb0, 0x21, 66], &room, &local).is_empty());
        assert!(matches!(e.flush(None)[..], [Action::Nudge { .. }]));
    }

    #[test]
    fn jog_scratches_while_touched() {
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let local = LocalParams::default();
        let mut room = RoomState::default();
        room.decks[0].track = Some(crate::model::TrackId([1; 32]));
        let out = e.handle(&[0x90, 0x36, 127], &room, &local);
        assert_eq!(
            out,
            vec![MidiOut::Action(Action::JogTouch {
                deck: 0,
                down: true,
                spin: 0.0,
            })]
        );
        room.decks[0].scratching = true;
        // Jog messages are gathered and sent as one movement per flush.
        assert!(e.handle(&[0xb0, 0x22, 64 + 10], &room, &local).is_empty());
        assert!(e.handle(&[0xb0, 0x22, 64 + 6], &room, &local).is_empty());
        match &e.flush(None)[..] {
            [Action::Nudge { delta, .. }] => {
                assert!((delta - 16.0 / 2048.0 * SECONDS_PER_TURN).abs() < 1e-12)
            }
            other => panic!("{other:?}"),
        }
        assert!(e.flush(None).is_empty());
        // Let go a while after the hand stopped: no spin.
        let out = e.handle_at(&[0x80, 0x36, 0], 100_000, &room, &local);
        assert_eq!(
            out,
            vec![MidiOut::Action(Action::JogTouch {
                deck: 0,
                down: false,
                spin: 0.0,
            })]
        );
        // Deck 3 on the FLX6 is deck C.
        room.decks[2].track = Some(crate::model::TrackId([2; 32]));
        let out = e.handle(&[0x92, 0x36, 127], &room, &local);
        assert_eq!(
            out,
            vec![MidiOut::Action(Action::JogTouch {
                deck: 2,
                down: true,
                spin: 0.0,
            })]
        );
    }

    /// The last 30 ms of a hard backspin on a DDJ-FLX6 before the hand let
    /// go (CC 0x22, two messages every 2 ms, as Windows delivers them).
    const FLX6_BACKSPIN: [u8; 30] = [
        0x2C, 0x2B, 0x2C, 0x2B, 0x2C, 0x2B, 0x2B, 0x2B, 0x2B, 0x2B, 0x2A, 0x2B, 0x2A, 0x2A, 0x2B,
        0x2A, 0x2A, 0x2A, 0x2B, 0x2A, 0x2A, 0x2A, 0x2B, 0x2A, 0x2B, 0x2A, 0x2B, 0x2B, 0x2B, 0x2B,
    ];

    /// Touch, play the recorded backspin and let go (at 30 ms). Returns what
    /// letting go sent.
    fn backspin(e: &mut MidiEngine, room: &mut RoomState) -> Vec<MidiOut> {
        let local = LocalParams::default();
        room.decks[0].scratching = false;
        e.handle_at(&[0x90, 0x36, 127], 0, room, &local);
        room.decks[0].scratching = true;
        for (i, &v) in FLX6_BACKSPIN.iter().enumerate() {
            let t = (i as i64 / 2) * 2_000;
            assert!(e.handle_at(&[0xb0, 0x22, v], t, room, &local).is_empty());
        }
        let out = e.handle_at(&[0x90, 0x36, 0], 30_000, room, &local);
        room.decks[0].scratching = false;
        out
    }

    #[test]
    fn letting_go_mid_backspin_passes_on_the_hands_speed() {
        // The user's FLX6 measures 6387 ticks a turn.
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 6387.0), 0.08);
        let mut room = RoomState::default();
        room.decks[0].track = Some(crate::model::TrackId([1; 32]));
        let out = backspin(&mut e, &mut room);
        // Movement not sent yet goes out first, then the release with the
        // hand's speed: about 6x normal speed, backwards.
        match &out[..] {
            [MidiOut::Action(Action::Nudge { delta, .. }), MidiOut::Action(Action::JogTouch {
                down: false, spin, ..
            })] => {
                assert!(*delta < 0.0);
                assert!(*spin < -5.5 && *spin > -6.5, "let go at {spin}x");
            }
            other => panic!("{other:?}"),
        }
        assert!(e.flush(None).is_empty());
    }

    #[test]
    fn a_platter_let_go_turns_on_by_itself_without_moving_the_deck() {
        let local = LocalParams::default();
        for playing in [false, true] {
            let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 6387.0), 0.08);
            let mut room = RoomState::default();
            room.decks[0].track = Some(crate::model::TrackId([1; 32]));
            room.decks[0].playing = playing;
            backspin(&mut e, &mut room);
            e.flush(Some(0.025));
            // The platter spins on for 0.4 s after the hand left it (the
            // record's own spin is the room model's job): no search, no bend.
            for ms in 31..430 {
                let out = e.handle_at(&[0xb0, 0x21, 35], ms * 1_000, &room, &local);
                assert!(out.is_empty());
                if ms % 5 == 0 {
                    let acts = e.flush((ms % 25 == 0).then_some(0.025));
                    assert!(acts.is_empty(), "playing {playing}: {acts:?}");
                }
            }
            // Once it has been still for a moment, the jog works as usual.
            e.handle_at(&[0xb0, 0x21, 70], 500_000, &room, &local);
            let acts = e.flush(Some(0.025));
            if playing {
                assert!(
                    matches!(acts[..], [Action::Bend { bend, .. }] if bend > 0.0),
                    "{acts:?}"
                );
            } else {
                assert!(
                    matches!(acts[..], [Action::Nudge { delta, .. }] if delta > 0.0),
                    "{acts:?}"
                );
            }
        }
    }

    #[test]
    fn letting_go_of_a_still_record_leaves_the_cue_alone() {
        // Lifting the hand flicks an FLX6 platter about 400 ticks on (seen in
        // recordings): a paused deck must not search with it.
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 6387.0), 0.08);
        let local = LocalParams::default();
        let mut room = RoomState::default();
        room.decks[0].track = Some(crate::model::TrackId([1; 32]));
        e.handle_at(&[0x90, 0x36, 127], 0, &room, &local);
        room.decks[0].scratching = true;
        let out = e.handle_at(&[0x90, 0x36, 0], 500_000, &room, &local);
        assert!(
            matches!(out[..], [MidiOut::Action(Action::JogTouch { down: false, spin, .. })] if spin == 0.0)
        );
        room.decks[0].scratching = false;
        for ms in 520..720 {
            e.handle_at(&[0xb0, 0x21, 66], ms * 1_000, &room, &local);
        }
        assert!(e.flush(None).is_empty());
    }

    #[test]
    fn jog_ring_bends_speed_only_while_turning() {
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let local = LocalParams::default();
        let mut room = RoomState::default();
        room.decks[0].track = Some(crate::model::TrackId([1; 32]));
        room.decks[0].playing = true;
        // A gentle nudge: ~0.25 turns per second for 25 ms (13 ticks).
        for _ in 0..13 {
            e.handle(&[0xb0, 0x21, 65], &room, &local);
        }
        let bend = match &e.flush(Some(0.025))[..] {
            [Action::Bend { bend, .. }] => *bend,
            other => panic!("{other:?}"),
        };
        assert!(
            bend > 0.005 && bend < 0.03,
            "gentle nudge bent speed by {bend}"
        );
        // Ring stops: the bend falls back to exactly zero within a few updates.
        let mut last = bend;
        for _ in 0..6 {
            for a in e.flush(Some(0.025)) {
                if let Action::Bend { bend, .. } = a {
                    last = bend;
                }
            }
        }
        assert_eq!(last, 0.0);
    }

    #[test]
    fn flx6_decks_and_mixer_channels_3_and_4_are_decks_c_and_d() {
        let m = pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0);
        let target = |t: Trigger| m.bindings.iter().find(|b| b.trigger == t).map(|b| b.target);
        assert_eq!(
            target(Trigger::note(2, 0x0B)),
            Some(Target::Play { deck: 2 })
        );
        assert_eq!(
            target(Trigger::note(3, 0x0B)),
            Some(Target::Play { deck: 3 })
        );
        assert_eq!(
            target(Trigger::note(2, 0x54)),
            Some(Target::Pfl { deck: 2 })
        );
        assert_eq!(
            target(Trigger::cc(3, 0x13)),
            Some(Target::Volume { deck: 3 })
        );
        assert_eq!(
            target(Trigger::cc(6, 0x19)),
            Some(Target::Filter { deck: 2 })
        );
        assert_eq!(
            target(Trigger::note(6, 0x49)),
            Some(Target::Load { deck: 3 })
        );
        assert_eq!(
            target(Trigger::note(11, 0x00)),
            Some(Target::HotCue { deck: 2, slot: 0 }),
            "deck 3's pads"
        );
        // Every deck control, on all four decks.
        for d in 0..4 {
            let n = m
                .bindings
                .iter()
                .filter(|b| b.target.deck() == Some(d))
                .count();
            assert_eq!(
                n,
                m.bindings
                    .iter()
                    .filter(|b| b.target.deck() == Some(0))
                    .count()
            );
        }
        // A two-deck controller only has A and B.
        let small = pioneer_ddj("DDJ-FLX4", "test", 2, 720.0);
        assert!(small
            .bindings
            .iter()
            .all(|b| b.target.deck().is_none_or(|d| d < 2)));
    }

    /// The FLX6 mapping as preset version 6 (Tandem v0.8) saved it: decks 3
    /// and 4 drove A and B, and mixer channels 3 and 4 weren't mapped.
    fn flx6_v6() -> Mapping {
        let mut m = pioneer_ddj("DDJ-FLX6", "Pioneer DDJ-FLX6", 4, 2048.0);
        m.preset_version = 6;
        let mixer = |t: &Target| {
            matches!(
                t,
                Target::Pfl { .. }
                    | Target::Volume { .. }
                    | Target::Trim { .. }
                    | Target::Eq { .. }
                    | Target::Filter { .. }
            )
        };
        m.bindings
            .retain(|b| !(b.target.deck().is_some_and(|d| d >= 2) && mixer(&b.target)));
        // SHIFT + TEMPO came with preset version 9, and the roll pads
        // with 10.
        m.bindings
            .retain(|b| !matches!(b.target, Target::TempoShift { .. } | Target::Roll { .. }));
        for b in &mut m.bindings {
            if let Some(d) = b.target.deck() {
                b.target = b.target.with_deck(d % 2);
            }
        }
        m.meters.retain(|l| l.deck < 2);
        m
    }

    #[test]
    fn saved_v6_mappings_move_decks_3_and_4_to_c_and_d() {
        let preset = pioneer_ddj("DDJ-FLX6", "Pioneer DDJ-FLX6", 4, 2048.0);
        let mut saved = flx6_v6();
        saved.jog_ticks_per_rev = Some(6387.0);
        // The DJ put deck 3's first pad on PLAY: that stays as it is.
        let custom = Trigger::note(11, 0x00);
        saved.bindings.retain(|b| b.trigger != custom);
        saved.bind(custom, Target::Play { deck: 0 });
        let moved_and_added = saved.upgrade_from(&preset);
        // 41 controls on each of decks 3 and 4 (less the custom pad) move;
        // 7 mixer controls on each of channels 3 and 4, and their meters, are
        // new, and so are SHIFT + TEMPO and 8 roll pads on all four decks.
        assert_eq!(moved_and_added, 2 * 41 - 1 + 2 * 7 + 2 + 4 + 4 * 8);
        assert_eq!(saved.preset_version, PRESET_VERSION);
        assert_eq!(saved.jog_ticks_per_rev, Some(6387.0));
        assert_eq!(saved.meters, preset.meters);
        let mut want: Vec<_> = preset
            .bindings
            .iter()
            .filter(|b| b.trigger != custom)
            .map(|b| (b.trigger, b.target))
            .chain([(custom, Target::Play { deck: 0 })])
            .collect();
        let mut got: Vec<_> = saved
            .bindings
            .iter()
            .map(|b| (b.trigger, b.target))
            .collect();
        let key = |x: &(Trigger, Target)| format!("{x:?}");
        want.sort_by_key(key);
        got.sort_by_key(key);
        assert_eq!(got, want);
        // Saved again, it's left alone.
        assert_eq!(saved.upgrade_from(&preset), 0);
    }

    #[test]
    fn saved_preset_mappings_gain_new_controls() {
        let preset = pioneer_ddj("DDJ-FLX6", "Pioneer DDJ-FLX6", 4, 2048.0);
        // A mapping saved before SYNC existed, with a calibrated jog and one
        // custom binding on a pad.
        let mut saved = preset.clone();
        saved.preset_version = 0;
        saved.jog_ticks_per_rev = Some(6387.0);
        saved
            .bindings
            .retain(|b| !matches!(b.target, Target::Sync { .. } | Target::SyncOff { .. }));
        saved.bindings.retain(|b| b.trigger != Trigger::note(7, 0));
        saved.bind(Trigger::note(7, 0), Target::Play { deck: 0 });
        let added = saved.upgrade_from(&preset);
        assert_eq!(added, 8, "SYNC and SHIFT+SYNC on decks 1-4");
        assert!(saved
            .bindings
            .iter()
            .any(|b| b.trigger == Trigger::note(0, 0x58)));
        assert_eq!(saved.jog_ticks_per_rev, Some(6387.0));
        let pad1: Vec<_> = saved
            .bindings
            .iter()
            .filter(|b| b.trigger == Trigger::note(7, 0))
            .collect();
        assert_eq!(pad1.len(), 1, "custom binding kept, not doubled");
        assert_eq!(saved.upgrade_from(&preset), 0, "only once");
    }

    #[test]
    fn shift_sync_turns_sync_off_and_returns_to_the_fader() {
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let local = LocalParams::default();
        let mut room = RoomState::default();
        // Tempo fader physically at +4% (fader 0.75 with an 8% range).
        let _ = e.handle(&[0xb0, 0x00, 96], &room, &local);
        let _ = e.handle(&[0xb0, 0x20, 0], &room, &local);
        room.decks[0].synced = true;
        room.decks[0].tempo = 0.93; // sync pulled it elsewhere
        let out = e.handle(&[0x90, 0x60, 127], &room, &local);
        assert_eq!(out[0], MidiOut::Action(Action::Sync { deck: 0, on: false }));
        match out[1] {
            MidiOut::Action(Action::Tempo { tempo, .. }) => {
                assert!((tempo - 1.04).abs() < 0.001, "{tempo}")
            }
            ref other => panic!("{other:?}"),
        }
    }

    /// Feed MIDI to the engine and apply what it sends to the room, the
    /// way the app does. Returns the tempos it sent.
    fn drive(e: &mut MidiEngine, room: &mut RoomState, msgs: &[[u8; 3]]) -> Vec<f64> {
        let local = LocalParams::default();
        let mut tempos = Vec::new();
        for m in msgs {
            for o in e.handle(m, room, &local) {
                if let MidiOut::Action(a) = o {
                    if let Action::Tempo { tempo, .. } = a {
                        tempos.push(tempo);
                    }
                    let seq = room.decks[0].gen + room.decks[1].gen + tempos.len() as u64;
                    room.apply(&crate::model::Event {
                        time: 1_000_000 + seq as i64 * 1000,
                        origin: 1,
                        seq,
                        action: a,
                    });
                }
            }
        }
        tempos
    }

    #[test]
    fn tempo_fader_responds_at_once_after_sync() {
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let mut room = RoomState::default();
        for (d, bpm) in [(0, 128.0), (1, 115.0)] {
            let deck = &mut room.decks[d];
            deck.track = Some(crate::model::TrackId([d as u8 + 1; 32]));
            deck.duration = 300.0;
            deck.grid = crate::grid::BeatGrid::new(0.0, bpm);
        }
        // Both tempo faders sit at 0%.
        let centre = [
            [0xb0, 0x00, 64],
            [0xb0, 0x20, 0],
            [0xb1, 0x00, 64],
            [0xb1, 0x20, 0],
        ];
        drive(&mut e, &mut room, &centre);
        room.decks[0].tempo = 1.0;
        room.decks[1].tempo = 1.0;

        // SYNC deck A onto deck B: 128 -> 115 BPM, far below where the fader is.
        let out = e.handle(&[0x90, 0x58, 127], &room, &LocalParams::default());
        let [MidiOut::Action(a)] = &out[..] else {
            panic!("{out:?}")
        };
        room.apply(&crate::model::Event {
            time: 2_000_000,
            origin: 1,
            seq: 99,
            action: a.clone(),
        });
        let synced = room.decks[0].tempo;
        assert!((synced - 115.0 / 128.0).abs() < 1e-9);
        assert!(e.waiting_for_pickup().is_empty());

        // Nudge the fader up 1%: the very first message moves the tempo by 1%,
        // and deck B (still in sync) comes along.
        let step = 0.01 / 0.16 * 16383.0;
        let up = (8192.0 + step) as u16;
        let tempos = drive(
            &mut e,
            &mut room,
            &[
                [0xb0, 0x00, (up >> 7) as u8],
                [0xb0, 0x20, (up & 0x7f) as u8],
            ],
        );
        assert!(!tempos.is_empty(), "the fader must respond straight away");
        let moved = room.decks[0].tempo - synced;
        assert!((moved - 0.01).abs() < 0.0002, "moved {moved}");
        assert!((room.decks[1].tempo * 115.0 - room.decks[0].tempo * 128.0).abs() < 1e-6);

        // Deck B's own fader also moves from where sync left it, no jump.
        let b_before = room.decks[1].tempo;
        let tempos = drive(&mut e, &mut room, &[[0xb1, 0x00, 63], [0xb1, 0x20, 0]]);
        assert!(!tempos.is_empty());
        let moved = room.decks[1].tempo - b_before;
        assert!(
            (moved + 0.16 * 128.0 / 16383.0).abs() < 0.0002,
            "moved {moved}"
        );

        // Back to the centre: deck A returns to its synced tempo (B carried it).
        let a_before = room.decks[0].tempo;
        drive(&mut e, &mut room, &[[0xb0, 0x00, 64], [0xb0, 0x20, 0]]);
        assert!((room.decks[0].tempo - (a_before - 0.01)).abs() < 0.0002);

        // SYNC off (plain SYNC press): tempo stays, the fader keeps working relatively.
        drive(&mut e, &mut room, &[[0x90, 0x58, 127]]);
        assert!(!room.decks[0].synced);
        let kept = room.decks[0].tempo;
        assert!((kept - 1.0).abs() > 0.05);
        drive(
            &mut e,
            &mut room,
            &[
                [0xb0, 0x00, (up >> 7) as u8],
                [0xb0, 0x20, (up & 0x7f) as u8],
            ],
        );
        assert!((room.decks[0].tempo - kept - 0.01).abs() < 0.0002);

        // SHIFT + SYNC puts the deck back on the fader (+1%), and from then on
        // the fader sets the tempo directly again.
        drive(&mut e, &mut room, &[[0x90, 0x60, 127]]);
        assert!((room.decks[0].tempo - 1.01).abs() < 0.0002);
        drive(&mut e, &mut room, &[[0xb0, 0x00, 72], [0xb0, 0x20, 0]]);
        let want = 1.0 + (72.0 * 128.0 / 16383.0 - 0.5) * 0.16;
        assert!((room.decks[0].tempo - want).abs() < 1e-9);
    }

    /// A DDJ-FLX6 tempo fader on MIDI channel `ch` moving from `from` to
    /// `to` (14-bit positions) in `steps` moves, sent the way the FLX6 sends
    /// it: the coarse half on CC `cc`, then the fine half on CC `cc + 32`,
    /// every time (checked against a recording). CC 0x00 is the plain
    /// fader, CC 0x05 the fader with SHIFT held.
    fn fader(ch: u8, cc: u8, from: u16, to: u16, steps: i32) -> Vec<[u8; 3]> {
        (1..=steps)
            .flat_map(|i| {
                let p = (from as i32 + (to as i32 - from as i32) * i / steps) as u16;
                [
                    [0xB0 | ch, cc, (p >> 7) as u8],
                    [0xB0 | ch, cc + 32, (p & 0x7F) as u8],
                ]
            })
            .collect()
    }

    const TOP: u16 = 16383;
    const CENTRE: u16 = 8192;
    const BOTTOM: u16 = 0;

    /// Deck `d`'s tempo after each MSB/LSB pair (the controller sends the
    /// pair together; in between, the coarse half sits with the old fine half).
    fn tempo_after_pairs(
        e: &mut MidiEngine,
        room: &mut RoomState,
        d: usize,
        msgs: &[[u8; 3]],
    ) -> Vec<f64> {
        msgs.chunks(2)
            .map(|pair| {
                drive(e, room, pair);
                room.decks[d].tempo
            })
            .collect()
    }

    /// Two decks with tracks and beatgrids (128 and 115 BPM), both tempo
    /// faders seen at the centre.
    fn two_decks(e: &mut MidiEngine) -> RoomState {
        let mut room = RoomState::default();
        for (d, bpm) in [(0, 128.0), (1, 115.0)] {
            let deck = &mut room.decks[d];
            deck.track = Some(crate::model::TrackId([d as u8 + 1; 32]));
            deck.duration = 300.0;
            deck.grid = crate::grid::BeatGrid::new(0.0, bpm);
        }
        drive(e, &mut room, &fader(0, 0x00, CENTRE, CENTRE, 1));
        drive(e, &mut room, &fader(1, 0x00, CENTRE, CENTRE, 1));
        room
    }

    #[test]
    fn shift_tempo_moves_the_fader_not_the_tempo() {
        // The DJ's report: move the tempo fader down, SHIFT + move it back
        // up to 0: nothing happens. Move it down again: lower still.
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let mut room = RoomState::default();
        drive(&mut e, &mut room, &fader(0, 0x00, CENTRE, CENTRE, 1));
        let centre = room.decks[0].tempo;
        assert!((centre - 1.0).abs() < 1e-5);

        // Down to the bottom: -8%.
        let down = tempo_after_pairs(&mut e, &mut room, 0, &fader(0, 0x00, CENTRE, BOTTOM, 37));
        assert!(down.windows(2).all(|w| w[1] < w[0]));
        assert!((room.decks[0].tempo - 0.92).abs() < 1e-9);

        // SHIFT + back up to the centre: no tempo change at all.
        let shifted = fader(0, 0x05, BOTTOM, CENTRE, 37);
        assert!(drive(&mut e, &mut room, &shifted).is_empty());
        assert_eq!(room.decks[0].tempo, 0.92);
        let hw = e.hardware_tempo(0).unwrap();
        assert!((hw - 1.0).abs() < 1e-5, "the fader is at the centre: {hw}");

        // Down again: from -8% to -16%, smoothly, starting at once.
        let down = tempo_after_pairs(&mut e, &mut room, 0, &fader(0, 0x00, CENTRE, BOTTOM, 37));
        assert!(down[0] < 0.92 && down[0] > 0.91, "first move: {}", down[0]);
        assert!(down.windows(2).all(|w| w[1] < w[0] && w[0] - w[1] < 0.005));
        let want = 0.92 - (centre - 0.92);
        assert!((room.decks[0].tempo - want).abs() < 1e-9);

        // And back up, all the way: the fader is 8% above the deck now.
        tempo_after_pairs(&mut e, &mut room, 0, &fader(0, 0x00, BOTTOM, TOP, 37));
        assert!((room.decks[0].tempo - (want + 0.16)).abs() < 1e-9);
        // Deck B and the others never moved.
        assert!(room.decks[1..].iter().all(|d| d.tempo == 1.0));
    }

    #[test]
    fn shift_tempo_goes_on_to_the_rooms_limit() {
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let mut room = RoomState::default();
        // Top to bottom, SHIFT back to the top, again and again: 16% a
        // time, down to the room's -50%, and no further.
        drive(&mut e, &mut room, &fader(0, 0x00, TOP, TOP, 1));
        assert!((room.decks[0].tempo - 1.08).abs() < 1e-9);
        for want in [0.92, 0.76, 0.60, 0.5, 0.5] {
            let tempos = tempo_after_pairs(&mut e, &mut room, 0, &fader(0, 0x00, TOP, BOTTOM, 20));
            assert!(tempos.iter().all(|&t| t >= 0.5));
            assert!((room.decks[0].tempo - want).abs() < 1e-9, "{want}");
            drive(&mut e, &mut room, &fader(0, 0x05, BOTTOM, TOP, 20));
        }
        // Pushing on against the limit, with the fine halves all over the
        // place, leaves the tempo exactly there.
        let pushed = tempo_after_pairs(&mut e, &mut room, 0, &fader(0, 0x00, TOP, BOTTOM, 37));
        assert!(pushed.iter().all(|&t| t == 0.5), "{pushed:?}");
        drive(&mut e, &mut room, &fader(0, 0x05, BOTTOM, TOP, 20));
        // Coming back starts at once: the moves past the limit aren't owed
        // back (beyond one coarse step, 1/128 of the fader's travel).
        let up_1pct = (TOP as f64 - 0.01 / 0.16 * 16383.0).round() as u16;
        drive(&mut e, &mut room, &fader(0, 0x00, TOP, up_1pct, 1));
        assert_eq!(room.decks[0].tempo, 0.5);
        drive(&mut e, &mut room, &fader(0, 0x00, up_1pct, TOP, 1));
        assert!((room.decks[0].tempo - 0.51).abs() < 1e-4);
        // The same upwards, to +50%.
        drive(&mut e, &mut room, &fader(0, 0x05, TOP, BOTTOM, 20));
        for _ in 0..8 {
            drive(&mut e, &mut room, &fader(0, 0x00, BOTTOM, TOP, 20));
            drive(&mut e, &mut room, &fader(0, 0x05, TOP, BOTTOM, 20));
        }
        assert_eq!(room.decks[0].tempo, 1.5);
    }

    #[test]
    fn a_quick_move_onto_the_decks_tempo_doesnt_grab_it() {
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let mut room = RoomState::default();
        // Fader at -2%, then SHIFT up to the centre: the deck is 2% below it.
        const MINUS_2: u16 = 6144;
        drive(&mut e, &mut room, &fader(0, 0x00, MINUS_2, MINUS_2, 1));
        let at_minus_2 = room.decks[0].tempo;
        assert!((at_minus_2 - 0.98).abs() < 1e-4);
        drive(&mut e, &mut room, &fader(0, 0x05, MINUS_2, CENTRE, 10));
        // One quick move straight back to -2%: the fader now reads the
        // deck's tempo, but it moved 2%, so the tempo moves 2%.
        drive(&mut e, &mut room, &fader(0, 0x00, CENTRE, MINUS_2, 1));
        let quick = room.decks[0].tempo;
        assert!((quick - (at_minus_2 - 0.02)).abs() < 1e-4, "{quick}");

        // SHIFT-moving the fader exactly onto the deck's tempo puts it back
        // on the fader: from then on it sets the tempo directly.
        let on = ((quick - 1.0) / 0.16 + 0.5) * 16383.0;
        drive(
            &mut e,
            &mut room,
            &fader(0, 0x05, MINUS_2, on.round() as u16, 5),
        );
        assert_eq!(room.decks[0].tempo, quick);
        drive(
            &mut e,
            &mut room,
            &fader(0, 0x00, on.round() as u16, 2000, 5),
        );
        let want = 1.0 + (2000.0 / 16383.0 - 0.5) * 0.16;
        assert!((room.decks[0].tempo - want).abs() < 1e-9);
    }

    #[test]
    fn shift_sync_after_shift_tempo_goes_to_where_the_fader_is() {
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let mut room = two_decks(&mut e);
        // -4% on the fader, then SHIFT moves the fader to +4%.
        drive(&mut e, &mut room, &fader(0, 0x00, CENTRE, 4096, 10));
        drive(&mut e, &mut room, &fader(0, 0x05, 4096, 12288, 10));
        assert!((room.decks[0].tempo - 0.96).abs() < 1e-4);
        // Right-clicking SYNC on screen uses the same position.
        assert!((e.hardware_tempo(0).unwrap() - 1.04).abs() < 1e-4);
        // SYNC, then SHIFT + SYNC: back on the fader, at +4%.
        drive(&mut e, &mut room, &[[0x90, 0x58, 127]]);
        assert!(room.decks[0].synced);
        drive(&mut e, &mut room, &[[0x90, 0x60, 127]]);
        assert!(!room.decks[0].synced);
        assert!((room.decks[0].tempo - 1.04).abs() < 1e-4);
        // The fader sets the tempo directly again.
        drive(&mut e, &mut room, &fader(0, 0x00, 12288, 10000, 3));
        let want = 1.0 + (10000.0 / 16383.0 - 0.5) * 0.16;
        assert!((room.decks[0].tempo - want).abs() < 1e-9);
    }

    #[test]
    fn flx6_deck_switch_doesnt_make_the_tempo_jump() {
        // The DJ's report: move the fader on deck 1, switch to deck 3 and
        // move it there, switch back: deck 1's first move jumped, by as far
        // as the fader went on deck 3.
        let mut e = MidiEngine::new(builtin_preset("DDJ-FLX6").unwrap(), 0.08);
        let mut room = two_decks(&mut e);
        let at = |p: u16| 1.0 + (p as f64 / 16383.0 - 0.5) * 0.16;
        let left = |deck: u8| [[0x90 | deck, 0x3C, 0x7F], [0x90 | (deck ^ 2), 0x3C, 0x00]];
        // Deck A to -4%, on the fader.
        drive(&mut e, &mut room, &fader(0, 0x00, CENTRE, 4096, 10));
        let a = room.decks[0].tempo;
        assert!((a - at(4096)).abs() < 1e-9);

        // DECK: the left side plays deck C (nobody has touched it). Up to
        // +4%: C starts from 0% and moves by as much as the fader does
        // after its first reading, which only says where the fader is.
        assert!(drive(&mut e, &mut room, &left(2)).is_empty());
        let up = fader(2, 0x00, 4096, 12288, 10);
        let c = tempo_after_pairs(&mut e, &mut room, 2, &up);
        assert_eq!(c[0], 1.0, "switching alone changes nothing");
        assert!(
            c.windows(2).all(|w| w[1] > w[0] && w[1] - w[0] < 0.01),
            "{c:?}"
        );
        let first = 4096 + (12288 - 4096) / 10;
        assert!((room.decks[2].tempo - (1.0 + at(12288) - at(first))).abs() < 1e-6);
        assert_eq!(room.decks[0].tempo, a, "deck A stays put");

        // DECK again: back to deck A. The fader is at +4% now, A at -4%.
        // A lone fine half first (its coarse half is the one from before
        // the switch) changes nothing either.
        assert!(drive(&mut e, &mut room, &left(0)).is_empty());
        assert!(drive(&mut e, &mut room, &[[0xB0, 0x20, 0x11]]).is_empty());
        let a_moves = tempo_after_pairs(&mut e, &mut room, 0, &fader(0, 0x00, 12288, 14000, 5));
        assert_eq!(a_moves[0], a, "no jump");
        assert!(a_moves
            .windows(2)
            .all(|w| w[1] > w[0] && w[1] - w[0] < 0.005));
        let first = 12288 + (14000 - 12288) / 5;
        assert!((room.decks[0].tempo - (a + at(14000) - at(first))).abs() < 1e-6);
        assert_eq!(room.decks[2].tempo, c[9], "deck C stays put");
        // The fader position is known again: SHIFT + SYNC puts A on it.
        drive(&mut e, &mut room, &[[0x90, 0x60, 127]]);
        assert!((room.decks[0].tempo - at(14000)).abs() < 1e-6);
    }

    #[test]
    fn shift_tempo_on_synced_decks() {
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let mut room = two_decks(&mut e);
        // A syncs to B: B is the master, A plays at B's 115 BPM.
        drive(&mut e, &mut room, &[[0x90, 0x58, 127]]);
        assert_eq!(room.master, Some(1));
        let locked = |room: &RoomState| {
            (room.decks[0].tempo * 128.0 - room.decks[1].tempo * 115.0).abs() < 1e-6
        };
        let synced = room.decks[0].tempo;
        assert!(locked(&room));

        // A's fader down 4%: A moves 4% and carries the master.
        drive(&mut e, &mut room, &fader(0, 0x00, CENTRE, 4096, 15));
        assert!((room.decks[0].tempo - (synced - 0.04)).abs() < 1e-4);
        assert!(locked(&room));
        // SHIFT + A's fader back to the centre: neither deck moves.
        let before = (room.decks[0].tempo, room.decks[1].tempo);
        assert!(drive(&mut e, &mut room, &fader(0, 0x05, 4096, CENTRE, 15)).is_empty());
        assert_eq!((room.decks[0].tempo, room.decks[1].tempo), before);
        // Down 4% again: both go lower still, still locked.
        drive(&mut e, &mut room, &fader(0, 0x00, CENTRE, 4096, 15));
        assert!((room.decks[0].tempo - (synced - 0.08)).abs() < 1e-4);
        assert!(locked(&room));

        // The master's own fader: SHIFT down, then up 2% from where B is.
        let b = room.decks[1].tempo;
        assert!(drive(&mut e, &mut room, &fader(1, 0x05, CENTRE, 0, 15)).is_empty());
        drive(&mut e, &mut room, &fader(1, 0x00, 0, 2048, 15));
        assert!((room.decks[1].tempo - (b + 0.02)).abs() < 1e-4);
        assert!(locked(&room));
        assert!(room.decks[0].synced);
    }

    #[test]
    fn shift_tempo_then_the_partner_moves_the_deck() {
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let mut room = RoomState::default();
        let partner = |room: &mut RoomState, tempo: f64| {
            room.apply(&crate::model::Event {
                time: 5_000_000,
                origin: 2,
                seq: 1,
                action: Action::Tempo { deck: 0, tempo },
            })
        };
        drive(&mut e, &mut room, &fader(0, 0x00, CENTRE, 4096, 10));
        drive(&mut e, &mut room, &fader(0, 0x05, 4096, CENTRE, 10));
        // The partner sets the deck to +2% on their screen. Moving our
        // fader down 2% starts from there.
        partner(&mut room, 1.02);
        drive(&mut e, &mut room, &fader(0, 0x00, CENTRE, 6144, 10));
        assert!((room.decks[0].tempo - 1.0).abs() < 1e-4);
        // The partner puts the deck where our fader sits: it's on the fader
        // again, and sets the tempo directly.
        let here = e.hardware_tempo(0).unwrap();
        partner(&mut room, here);
        drive(&mut e, &mut room, &fader(0, 0x00, 6144, 3000, 10));
        let want = 1.0 + (3000.0 / 16383.0 - 0.5) * 0.16;
        assert!((room.decks[0].tempo - want).abs() < 1e-9);
    }

    #[test]
    fn learned_shift_tempo_and_a_controller_that_cant_tell() {
        let (room, local) = (RoomState::default(), LocalParams::default());
        // Learn SHIFT + tempo fader from the FLX6's shifted pair.
        let mut e = engine();
        e.learn = Some(Target::TempoShift { deck: 0 });
        let out = e.handle(&[0xb0, 0x05, 70], &room, &local);
        assert!(matches!(&out[0], MidiOut::Learned(b) if b.trigger == Trigger::cc(0, 0x05)));
        assert!(e.handle(&[0xb0, 0x25, 3], &room, &local).is_empty());

        // A controller that sends the same message with SHIFT held: if both
        // get learned on it, it stays a plain tempo fader.
        let mut e = engine();
        e.mapping
            .bind(Trigger::cc(0, 0x1F), Target::TempoShift { deck: 0 });
        e.mapping
            .bind(Trigger::cc(0, 0x1F), Target::Tempo { deck: 0 });
        let mut room = RoomState::default();
        drive(&mut e, &mut room, &[[0xb0, 0x1F, 64]]);
        drive(&mut e, &mut room, &[[0xb0, 0x1F, 30]]);
        let want = 1.0 + (30.0 / 127.0 - 0.5) * 0.16;
        assert!((room.decks[0].tempo - want).abs() < 1e-6);
    }

    #[test]
    fn saved_v7_and_v8_mappings_gain_shift_tempo() {
        let preset = pioneer_ddj("DDJ-FLX6", "Pioneer DDJ-FLX6", 4, 2048.0);
        let browse = |t: &Target| matches!(t, Target::BrowsePress | Target::BrowseBack);
        let shift_tempo = |t: &Target| matches!(t, Target::TempoShift { .. });
        // Saved by preset 8 (v0.11.2 to v0.11.4): SHIFT + TEMPO on all four
        // decks. Saved by preset 7: also the browse knob's press and BACK.
        for (version, added) in [(8, 4), (7, 4 + 2)] {
            let mut saved = preset.clone();
            saved.preset_version = version;
            saved.jog_ticks_per_rev = Some(6387.0);
            saved
                .bindings
                .retain(|b| !shift_tempo(&b.target) && (version > 7 || !browse(&b.target)));
            assert_eq!(saved.upgrade_from(&preset), added, "preset {version}");
            assert_eq!(saved.preset_version, PRESET_VERSION);
            assert_eq!(saved.bindings.len(), preset.bindings.len());
            for deck in 0..4 {
                let b = saved.binding_for(Target::TempoShift { deck }).unwrap();
                assert_eq!(b.trigger, Trigger::cc(deck, 0x05));
            }
            assert_eq!(saved.jog_ticks_per_rev, Some(6387.0));
            assert_eq!(saved.upgrade_from(&preset), 0, "only once");
            // And it works: SHIFT + TEMPO moves the fader, not the tempo.
            let mut e = MidiEngine::new(saved, 0.08);
            let mut room = RoomState::default();
            drive(&mut e, &mut room, &fader(0, 0x00, CENTRE, BOTTOM, 10));
            assert!(drive(&mut e, &mut room, &fader(0, 0x05, BOTTOM, CENTRE, 10)).is_empty());
            drive(&mut e, &mut room, &fader(0, 0x00, CENTRE, 4096, 10));
            assert!(
                (room.decks[0].tempo - 0.88).abs() < 1e-4,
                "preset {version}"
            );
        }
    }

    #[test]
    fn leds_follow_the_room() {
        let e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let mut room = RoomState::default();
        room.decks[1].playing = true;
        room.decks[0].hot_cues[2] = Some(1.0);
        let leds = e.leds(&room, &LocalParams::default());
        let on = |k: (u8, u8)| leds.iter().find(|(kk, _)| *kk == k).map(|x| x.1);
        assert_eq!(on((0x91, 0x0B)), Some(true)); // deck B play
        assert_eq!(on((0x90, 0x0B)), Some(false)); // deck A play
        assert_eq!(on((0x97, 0x02)), Some(true)); // deck A pad 3
        assert_eq!(on((0x97, 0x03)), Some(false));
    }

    #[test]
    fn shift_cue_and_held_pads() {
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let (room, local) = (RoomState::default(), LocalParams::default());
        let act = |a| vec![MidiOut::Action(a)];
        // SHIFT + CUE on deck 2, and on deck 4: back to the start.
        let start = |deck| act(Action::Seek { deck, pos: 0.0 });
        assert_eq!(e.handle(&[0x91, 0x48, 127], &room, &local), start(1));
        assert!(e.handle(&[0x91, 0x48, 0], &room, &local).is_empty());
        assert_eq!(e.handle(&[0x93, 0x48, 127], &room, &local), start(3));
        // Pads report their release too, for gated hot cues.
        assert_eq!(
            e.handle(&[0x97, 0x02, 127], &room, &local),
            act(Action::HotCue { deck: 0, slot: 2 })
        );
        assert_eq!(
            e.handle(&[0x87, 0x02, 0], &room, &local),
            act(Action::HotCueUp { deck: 0, slot: 2 })
        );
        // SHIFT + pad still clears, and its release does nothing.
        assert_eq!(
            e.handle(&[0x98, 0x02, 127], &room, &local),
            act(Action::ClearHotCue { deck: 0, slot: 2 })
        );
        assert!(e.handle(&[0x98, 0x02, 0], &room, &local).is_empty());
    }

    #[test]
    fn flx6_loop_controls_as_recorded() {
        // Messages recorded from a DDJ-FLX6 with Tandem's MIDI monitor.
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let (room, local) = (RoomState::default(), LocalParams::default());
        let act = |a| vec![MidiOut::Action(a)];
        let mut press = |msg: [u8; 3]| {
            let out = e.handle(&msg, &room, &local);
            // Every button and pad's release does nothing.
            assert!(e.handle(&[msg[0], msg[1], 0], &room, &local).is_empty());
            out
        };
        // Quantized unless the DJ turned it off.
        let loop_in = |deck| Action::LoopIn {
            deck,
            quantize: true,
        };
        assert_eq!(press([0x90, 0x10, 0x7F]), act(loop_in(0)));
        assert_eq!(
            press([0x90, 0x11, 0x7F]),
            act(Action::LoopOut {
                deck: 0,
                quantize: true
            })
        );
        assert_eq!(press([0x90, 0x4D, 0x7F]), act(Action::Reloop { deck: 0 }));
        assert_eq!(
            press([0x90, 0x4C, 0x7F]),
            act(Action::BeatLoop {
                deck: 0,
                beats: 4.0
            }),
            "SHIFT + IN"
        );
        assert_eq!(
            press([0x90, 0x51, 0x7F]),
            act(Action::LoopHalve { deck: 0 })
        );
        assert_eq!(
            press([0x90, 0x53, 0x7F]),
            act(Action::LoopDouble { deck: 0 })
        );
        assert_eq!(press([0x91, 0x10, 0x7F]), act(loop_in(1)));
        assert_eq!(press([0x91, 0x4D, 0x7F]), act(Action::Reloop { deck: 1 }));
        // BEAT LOOP pads (SHIFT + BEAT JUMP): ¼ to 32 beats.
        for (i, beats) in BEAT_LOOPS.iter().enumerate() {
            let pad = 0x60 + i as u8;
            assert_eq!(
                press([0x97, pad, 0x7F]),
                act(Action::BeatLoop {
                    deck: 0,
                    beats: *beats
                })
            );
            assert_eq!(
                press([0x99, pad, 0x7F]),
                act(Action::BeatLoop {
                    deck: 1,
                    beats: *beats
                })
            );
        }
        // Left alone: SHIFT + OUT, SHIFT + RELOOP/EXIT, SHIFT + ◀/▶, and
        // SHIFT + BEAT JUMP pads.
        for msg in [
            [0x90, 0x77, 0x7F],
            [0x90, 0x50, 0x7F],
            [0x90, 0x61, 0x7F],
            [0x90, 0x62, 0x7F],
            [0x98, 0x20, 0x7F],
            [0x9E, 0x27, 0x7F],
        ] {
            assert!(press(msg).is_empty(), "{msg:02X?}");
        }
        // Quantize turned off in Settings: IN and OUT go to the playhead.
        e.quantize_loops = false;
        assert_eq!(
            e.handle(&[0x90, 0x10, 0x7F], &room, &local),
            act(Action::LoopIn {
                deck: 0,
                quantize: false
            })
        );
    }

    #[test]
    fn flx6_beat_jump_pads_roll_while_held() {
        // Pioneer's MIDI list: BEAT JUMP pads 1–8 send notes 0x20–0x27 on
        // the pad channel (8, 10, 12, 14 for decks 1–4), 0x7F pressed and
        // 0x00 let go.
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let (room, local) = (RoomState::default(), LocalParams::default());
        let roll = |deck, beats, on| vec![MidiOut::Action(Action::Roll { deck, beats, on })];
        for (deck, status) in [(0u8, 0x97u8), (1, 0x99), (2, 0x9B), (3, 0x9D)] {
            for (i, beats) in ROLLS.iter().enumerate() {
                let pad = 0x20 + i as u8;
                assert_eq!(
                    e.handle(&[status, pad, 0x7F], &room, &local),
                    roll(deck, *beats, true),
                    "deck {deck} pad {}",
                    i + 1
                );
                assert_eq!(
                    e.handle(&[status, pad, 0x00], &room, &local),
                    roll(deck, *beats, false)
                );
            }
        }
        assert_eq!(ROLLS[0], 1.0 / 16.0, "top row, short to long");
        assert_eq!(ROLLS[7], 8.0);
        // Two pads held: the latest rolls, and letting go of it goes back
        // to the other one's size.
        let half = [0x97, 0x23, 0x7F];
        let quarter = [0x97, 0x22, 0x7F];
        let up = |m: [u8; 3]| [m[0], m[1], 0];
        assert_eq!(e.handle(&half, &room, &local), roll(0, 0.5, true));
        assert_eq!(e.handle(&quarter, &room, &local), roll(0, 0.25, true));
        assert_eq!(e.handle(&up(quarter), &room, &local), roll(0, 0.5, true));
        assert_eq!(e.handle(&up(half), &room, &local), roll(0, 0.5, false));
        // Let go of the earlier one first: the latest keeps rolling.
        assert_eq!(e.handle(&half, &room, &local), roll(0, 0.5, true));
        assert_eq!(e.handle(&quarter, &room, &local), roll(0, 0.25, true));
        assert!(e.handle(&up(half), &room, &local).is_empty());
        assert_eq!(e.handle(&up(quarter), &room, &local), roll(0, 0.25, false));
        // A note-off message lets go too; a release never pressed does nothing.
        assert_eq!(
            e.handle(&[0x99, 0x24, 0x7F], &room, &local),
            roll(1, 1.0, true)
        );
        assert_eq!(
            e.handle(&[0x89, 0x24, 0x40], &room, &local),
            roll(1, 1.0, false)
        );
        assert!(e.handle(&[0x89, 0x24, 0x40], &room, &local).is_empty());
    }

    #[test]
    fn roll_pads_light_while_held() {
        let e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let mut room = RoomState::default();
        let on = |room: &RoomState, k: (u8, u8)| {
            let leds = e.leds(room, &LocalParams::default());
            leds.iter().find(|(kk, _)| *kk == k).map(|x| x.1)
        };
        room.decks[0] = crate::model::DeckState {
            track: Some(crate::model::TrackId([1; 32])),
            duration: 100.0,
            playing: true,
            ..Default::default()
        };
        room.apply(&crate::model::Event {
            time: 1_000_000,
            origin: 1,
            seq: 1,
            action: Action::Roll {
                deck: 0,
                beats: 0.5,
                on: true,
            },
        });
        assert!(room.decks[0].roll.is_some());
        assert_eq!(on(&room, (0x97, 0x23)), Some(true), "pad 4: ½ beat");
        for k in [(0x97, 0x22), (0x97, 0x24), (0x99, 0x23), (0x9B, 0x23)] {
            assert_eq!(on(&room, k), Some(false), "{k:02X?}");
        }
        room.decks[0].roll = None;
        assert_eq!(on(&room, (0x97, 0x23)), Some(false));
    }

    #[test]
    fn saved_v7_v8_and_v9_mappings_gain_the_roll_pads() {
        let preset = pioneer_ddj("DDJ-FLX6", "Pioneer DDJ-FLX6", 4, 2048.0);
        let rolls = |t: &Target| matches!(t, Target::Roll { .. });
        let shift_tempo = |t: &Target| matches!(t, Target::TempoShift { .. });
        let browse = |t: &Target| matches!(t, Target::BrowsePress | Target::BrowseBack);
        // Saved by preset 9 (v0.11.5 and v0.11.6): just the roll pads are
        // new. Preset 8: SHIFT + TEMPO on all four decks too. Preset 7: and
        // the browse knob's press and BACK.
        for (version, added) in [(9, 32), (8, 32 + 4), (7, 32 + 4 + 2)] {
            let mut saved = preset.clone();
            saved.preset_version = version;
            saved.jog_ticks_per_rev = Some(6387.0);
            saved.bindings.retain(|b| {
                !rolls(&b.target)
                    && (version > 8 || !shift_tempo(&b.target))
                    && (version > 7 || !browse(&b.target))
            });
            // The DJ learned deck 2's BEAT JUMP pad 8 onto PLAY: that stays.
            let custom = Trigger::note(9, 0x27);
            saved.bind(custom, Target::Play { deck: 1 });
            assert_eq!(saved.upgrade_from(&preset), added - 1, "preset {version}");
            assert_eq!(saved.preset_version, PRESET_VERSION);
            assert_eq!(saved.jog_ticks_per_rev, Some(6387.0));
            assert_eq!(saved.bindings.len(), preset.bindings.len());
            let at = |t: Trigger| {
                saved
                    .bindings
                    .iter()
                    .filter(|b| b.trigger == t)
                    .map(|b| b.target)
                    .collect::<Vec<_>>()
            };
            assert_eq!(at(custom), [Target::Play { deck: 1 }]);
            assert_eq!(
                at(Trigger::note(13, 0x20)),
                [Target::Roll { deck: 3, size: 0 }]
            );
            assert_eq!(saved.upgrade_from(&preset), 0, "only once");
            // And they roll.
            let mut e = MidiEngine::new(saved, 0.08);
            let (room, local) = (RoomState::default(), LocalParams::default());
            assert_eq!(
                e.handle(&[0x97, 0x23, 0x7F], &room, &local),
                vec![MidiOut::Action(Action::Roll {
                    deck: 0,
                    beats: 0.5,
                    on: true
                })]
            );
        }
        // Two-deck Pioneers get them on A and B.
        let small = pioneer_ddj("DDJ-FLX4", "test", 2, 720.0);
        assert_eq!(
            small.bindings.iter().filter(|b| rolls(&b.target)).count(),
            16
        );
    }

    #[test]
    fn saved_v4_mappings_gain_the_loop_controls() {
        let preset = pioneer_ddj("DDJ-FLX6", "Pioneer DDJ-FLX6", 4, 2048.0);
        let mut saved = preset.clone();
        saved.preset_version = 4;
        saved.bindings.retain(|b| {
            !matches!(
                b.target,
                Target::LoopIn { .. }
                    | Target::LoopOut { .. }
                    | Target::Reloop { .. }
                    | Target::LoopHalve { .. }
                    | Target::LoopDouble { .. }
                    | Target::BeatLoop { .. }
            )
        });
        // Six loop buttons and eight pads on each of the four hardware decks.
        assert_eq!(saved.upgrade_from(&preset), 4 * (6 + 8));
        assert_eq!(saved.bindings.len(), preset.bindings.len());
    }

    #[test]
    fn saved_v5_mappings_gain_the_level_meters() {
        let preset = pioneer_ddj("DDJ-FLX6", "Pioneer DDJ-FLX6", 4, 2048.0);
        let mut json = serde_json::to_value(&preset).unwrap();
        json["preset_version"] = 5.into();
        json.as_object_mut().unwrap().remove("meters");
        let mut saved: Mapping = serde_json::from_value(json).unwrap();
        assert!(saved.meters.is_empty());
        assert_eq!(saved.upgrade_from(&preset), 4);
        assert_eq!(saved.meters, preset.meters);
        assert_eq!(saved.bindings.len(), preset.bindings.len());
    }

    #[test]
    fn flx6_browse_section() {
        // Messages from Pioneer's DDJ-FLX6 MIDI list, section 4 (BROWSE),
        // all on MIDI channel 7.
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let (room, local) = (RoomState::default(), LocalParams::default());
        let mut h = |msg: [u8; 3]| e.handle(&msg, &room, &local);
        // Turning: 1/127 style.
        assert_eq!(h([0xB6, 0x40, 0x01]), vec![MidiOut::Browse(1)]);
        assert_eq!(h([0xB6, 0x40, 0x03]), vec![MidiOut::Browse(3)]);
        assert_eq!(h([0xB6, 0x40, 0x7F]), vec![MidiOut::Browse(-1)]);
        // Pressing the knob, and BACK; their releases do nothing.
        assert_eq!(h([0x96, 0x41, 0x7F]), vec![MidiOut::BrowsePress]);
        assert!(h([0x96, 0x41, 0x00]).is_empty());
        assert!(h([0x86, 0x41, 0x00]).is_empty());
        assert_eq!(h([0x96, 0x65, 0x7F]), vec![MidiOut::BrowseBack]);
        assert!(h([0x96, 0x65, 0x00]).is_empty());
        // LOAD still loads the selected track, on all four decks.
        for deck in 0..4 {
            assert_eq!(
                h([0x96, 0x46 + deck, 0x7F]),
                vec![MidiOut::LoadSelected(deck)]
            );
        }
        // Left alone: SHIFT + turn, SHIFT + press, SHIFT + BACK, VIEW.
        for msg in [
            [0xB6, 0x64, 0x01],
            [0x96, 0x42, 0x7F],
            [0x96, 0x66, 0x7F],
            [0x96, 0x7A, 0x7F],
            [0x96, 0x67, 0x7F],
            [0x96, 0x68, 0x7F],
        ] {
            assert!(h(msg).is_empty(), "{msg:02X?}");
        }
    }

    #[test]
    fn saved_v7_mappings_gain_the_browse_press_and_back() {
        let preset = pioneer_ddj("DDJ-FLX6", "Pioneer DDJ-FLX6", 4, 2048.0);
        let new = |t: &Target| matches!(t, Target::BrowsePress | Target::BrowseBack);
        let mut saved = preset.clone();
        saved.preset_version = 7;
        saved.jog_ticks_per_rev = Some(6387.0);
        saved.bindings.retain(|b| !new(&b.target));
        assert_eq!(saved.upgrade_from(&preset), 2);
        assert_eq!(saved.bindings.len(), preset.bindings.len());
        assert_eq!(saved.jog_ticks_per_rev, Some(6387.0));
        let mut e = MidiEngine::new(saved, 0.08);
        let (room, local) = (RoomState::default(), LocalParams::default());
        assert_eq!(
            e.handle(&[0x96, 0x41, 0x7F], &room, &local),
            vec![MidiOut::BrowsePress]
        );
        assert_eq!(
            e.handle(&[0x96, 0x65, 0x7F], &room, &local),
            vec![MidiOut::BrowseBack]
        );

        // A DJ who had put BACK on something else keeps it.
        let mut saved = preset.clone();
        saved.preset_version = 7;
        saved.bindings.retain(|b| !new(&b.target));
        saved.bind(Trigger::note(6, 0x65), Target::Play { deck: 0 });
        assert_eq!(saved.upgrade_from(&preset), 1);
        let back: Vec<_> = saved
            .bindings
            .iter()
            .filter(|b| b.trigger == Trigger::note(6, 0x65))
            .map(|b| b.target)
            .collect();
        assert_eq!(back, vec![Target::Play { deck: 0 }]);
    }

    #[test]
    fn browse_press_and_back_can_be_learned() {
        // A Denon SC Live 4 (user mapping): browse push is note 0x06 on
        // channel 16.
        let mut e = engine();
        let (room, local) = (RoomState::default(), LocalParams::default());
        assert!(Target::all().contains(&Target::BrowsePress));
        assert!(Target::all().contains(&Target::BrowseBack));
        e.learn = Some(Target::BrowsePress);
        let out = e.handle(&[0x9F, 0x06, 0x7F], &room, &local);
        assert!(
            matches!(&out[..], [MidiOut::Learned(b)] if b.trigger == Trigger::note(15, 0x06)),
            "{out:?}"
        );
        assert!(e.handle(&[0x8F, 0x06, 0x00], &room, &local).is_empty());
        assert_eq!(
            e.handle(&[0x9F, 0x06, 0x7F], &room, &local),
            vec![MidiOut::BrowsePress]
        );
    }

    #[test]
    fn level_meters_follow_the_channels() {
        let e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let db = |db: f32| 10f32.powf(db / 20.0);
        let levels = Levels {
            channels: [db(-6.0), 0.0, db(-20.0), db(1.0)],
            master: [db(-3.0); 2],
        };
        let out = e.meters(&levels);
        // Mixer channels 1–4 show decks A–D.
        assert_eq!(
            out,
            vec![
                ((0xB0, 0x02), pioneer_meter_value(db(-6.0))),
                ((0xB1, 0x02), 0),
                ((0xB2, 0x02), pioneer_meter_value(db(-20.0))),
                ((0xB3, 0x02), 127),
            ]
        );
        // A two-deck controller has two.
        let e = MidiEngine::new(pioneer_ddj("DDJ-FLX4", "test", 2, 720.0), 0.08);
        assert_eq!(e.meters(&levels).len(), 2);
        // The FLX6's lights come on at 37–40, 65–69, 85–88, 101–106 and
        // 119 (measured): count them at a level.
        let lights = |db_fs: f32| {
            let v = pioneer_meter_value(db(db_fs));
            [(37, 40), (65, 69), (85, 88), (101, 106), (119, 119)]
                .iter()
                .map(|&(lo, hi)| {
                    assert!(v < lo || v >= hi, "{db_fs} dB: {v} is ambiguous");
                    (v >= hi) as u8
                })
                .sum::<u8>()
        };
        for (db_fs, want) in [
            (-60.0, 0),
            (-26.0, 0),
            (-24.0, 1),
            (-16.0, 1),
            (-14.0, 2),
            (-9.0, 2),
            (-8.0, 3),
            (-5.0, 3),
            (-3.0, 4),
            (-0.01, 4),
            (0.0, 5),
            (6.0, 5),
        ] {
            assert_eq!(lights(db_fs), want, "{db_fs} dB");
        }
        assert_eq!(pioneer_meter_value(0.0), 0);
        assert_eq!(pioneer_meter_value(1.0), 127);
        let mut last = 0;
        for i in 0..600 {
            let v = pioneer_meter_value(db(-60.0 + i as f32 * 0.1));
            assert!(v >= last && v <= 118);
            last = v;
        }
    }

    #[test]
    fn loop_lights() {
        let e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let mut room = RoomState::default();
        let on = |room: &RoomState, k: (u8, u8)| {
            let leds = e.leds(room, &LocalParams::default());
            leds.iter().find(|(kk, _)| *kk == k).map(|x| x.1)
        };
        // LOOP IN pressed: its light waits for LOOP OUT.
        room.decks[0].loop_in = Some(10.0);
        assert_eq!(on(&room, (0x90, 0x10)), Some(true));
        assert_eq!(on(&room, (0x90, 0x11)), Some(false));
        // A 4-beat loop playing: IN, OUT, RELOOP/EXIT and pad 5 light up.
        room.decks[0].loop_out = Some(12.0);
        room.decks[0].looping = true;
        room.decks[0].loop_beats = Some(4.0);
        for k in [(0x90, 0x10), (0x90, 0x11), (0x90, 0x4D), (0x97, 0x64)] {
            assert_eq!(on(&room, k), Some(true), "{k:02X?}");
        }
        assert_eq!(on(&room, (0x97, 0x63)), Some(false));
        assert_eq!(on(&room, (0x99, 0x64)), Some(false), "deck B");
        assert_eq!(on(&room, (0x9B, 0x64)), Some(false), "deck C");
        room.decks[2] = room.decks[0];
        assert_eq!(on(&room, (0x9B, 0x64)), Some(true), "deck C");
        assert_eq!(on(&room, (0x92, 0x4D)), Some(true), "deck C");
        // Left the loop: all off.
        room.decks[0].looping = false;
        for k in [(0x90, 0x10), (0x90, 0x11), (0x90, 0x4D), (0x97, 0x64)] {
            assert_eq!(on(&room, k), Some(false), "{k:02X?}");
        }
    }

    /// HEADPHONE LEVEL on a DDJ-FLX6 turned down to zero and back up, as
    /// recorded: (MSB on CC 0x0D, LSB on CC 0x2D), channel 7.
    #[rustfmt::skip]
    const FLX6_HEADPHONE_SWEEP: [(u8, u8); 85] = [
        (0x16, 0x40), (0x13, 0x39), (0x0E, 0x0E), (0x0A, 0x67), (0x08, 0x11), (0x05, 0x5C),
        (0x04, 0x5A), (0x03, 0x68), (0x02, 0x76), (0x02, 0x25), (0x01, 0x74), (0x01, 0x43),
        (0x01, 0x12), (0x00, 0x62), (0x00, 0x31), (0x00, 0x00), (0x00, 0x31), (0x00, 0x62),
        (0x01, 0x33), (0x02, 0x35), (0x03, 0x27), (0x03, 0x78), (0x04, 0x7A), (0x06, 0x0D),
        (0x07, 0x40), (0x0A, 0x46), (0x0D, 0x1C), (0x0F, 0x72), (0x13, 0x5A), (0x16, 0x40),
        (0x19, 0x05), (0x1D, 0x2E), (0x20, 0x24), (0x23, 0x0A), (0x27, 0x54), (0x2B, 0x3C),
        (0x2F, 0x04), (0x33, 0x6E), (0x36, 0x23), (0x38, 0x27), (0x3A, 0x2C), (0x3B, 0x6F),
        (0x3D, 0x53), (0x40, 0x49), (0x42, 0x7E), (0x45, 0x53), (0x49, 0x5C), (0x4C, 0x42),
        (0x4F, 0x59), (0x53, 0x00), (0x55, 0x25), (0x58, 0x0A), (0x5C, 0x03), (0x5E, 0x38),
        (0x60, 0x0B), (0x62, 0x10), (0x62, 0x41), (0x62, 0x71), (0x60, 0x3C), (0x5D, 0x15),
        (0x59, 0x3D), (0x53, 0x00), (0x4E, 0x67), (0x49, 0x6C), (0x42, 0x2C), (0x3D, 0x22),
        (0x38, 0x48), (0x31, 0x39), (0x2C, 0x4E), (0x28, 0x56), (0x23, 0x4B), (0x1F, 0x74),
        (0x1C, 0x0B), (0x16, 0x0F), (0x12, 0x06), (0x0D, 0x6D), (0x08, 0x42), (0x06, 0x2D),
        (0x05, 0x1B), (0x03, 0x68), (0x02, 0x56), (0x02, 0x25), (0x01, 0x23), (0x00, 0x41),
        (0x00, 0x00),
    ];

    /// 14-bit knob moves on channel 7: MSB on `cc`, then LSB on `cc + 32`.
    fn knob(cc: u8, moves: &[(u8, u8)]) -> Vec<[u8; 3]> {
        moves
            .iter()
            .flat_map(|&(msb, lsb)| [[0xB6, cc, msb], [0xB6, cc + 32, lsb]])
            .collect()
    }

    /// Plays messages in as the MIDI thread does, applying this booth's
    /// levels to `local` as the session would. Returns every output.
    fn play_levels(e: &mut MidiEngine, local: &mut LocalParams, msgs: &[[u8; 3]]) -> Vec<MidiOut> {
        let room = RoomState::default();
        let mut all = Vec::new();
        for m in msgs {
            for o in e.handle(m, &room, local) {
                match o {
                    MidiOut::MasterLevel(v) => local.master_level = v,
                    MidiOut::HeadphoneLevel(v) => local.hp_level = v,
                    _ => {}
                }
                all.push(o);
            }
        }
        all
    }

    #[test]
    fn flx6_level_knobs_set_this_booths_levels() {
        let mut e = MidiEngine::new(builtin_preset("DDJ-FLX6").unwrap(), 0.08);
        let mut local = LocalParams::default();
        // The recorded HEADPHONE LEVEL sweep: down to zero, up to about
        // three quarters, and back down.
        let out = play_levels(&mut e, &mut local, &knob(0x0D, &FLX6_HEADPHONE_SWEEP));
        assert!(out.len() >= 2 * 84, "every move counts: {}", out.len());
        assert!(
            out.iter().all(|o| matches!(o, MidiOut::HeadphoneLevel(_))),
            "only this booth's headphone level, never the room: {out:?}"
        );
        let levels: Vec<f32> = out
            .iter()
            .map(|o| match o {
                MidiOut::HeadphoneLevel(v) => *v,
                _ => unreachable!(),
            })
            .collect();
        let top = levels.iter().cloned().fold(0.0, f32::max);
        let want = ((0x62 << 7) | 0x71) as f32 / 16383.0;
        assert!((top - want).abs() < 1e-6, "{top} vs {want}: 14-bit");
        assert_eq!(local.hp_level, 0.0);
        assert_eq!(local.master_level, LocalParams::default().master_level);

        // MASTER LEVEL, per Pioneer's list: CC 0x08 and 0x28.
        let out = play_levels(
            &mut e,
            &mut local,
            &knob(0x08, &[(0x40, 0x00), (0x7F, 0x7F)]),
        );
        assert!(out.iter().all(|o| matches!(o, MidiOut::MasterLevel(_))));
        assert_eq!(local.master_level, 1.0);
        let out = play_levels(&mut e, &mut local, &knob(0x08, &[(0x20, 0x00)]));
        assert_eq!(out.last(), Some(&MidiOut::MasterLevel(4096.0 / 16383.0)));

        // Moved on screen: the knob picks it up again once it gets there.
        local.master_level = 0.6;
        assert!(play_levels(&mut e, &mut local, &knob(0x08, &[(0x30, 0x00)])).is_empty());
        assert_eq!(e.waiting_for_pickup(), vec![Target::MasterLevel]);
        let out = play_levels(&mut e, &mut local, &knob(0x08, &[(0x50, 0x00)]));
        assert!(!out.is_empty());
        assert!((local.master_level - (0x50 << 7) as f32 / 16383.0).abs() < 1e-6);
        assert!(e.waiting_for_pickup().is_empty());
    }

    #[test]
    fn level_knobs_leave_the_controllers_own_sound_card_to_it() {
        let mut e = MidiEngine::new(builtin_preset("DDJ-FLX6").unwrap(), 0.08);
        let mut local = LocalParams::default();
        // The master plays through the FLX6, the headphones through a headset.
        e.card = ControllerCard::find(
            Some("DDJ-FLX6"),
            Some("Line (DDJ-FLX6)"),
            Some("Speakers (G733 Gaming Headset)"),
        );
        assert_eq!(
            e.card,
            ControllerCard {
                master: true,
                phones: false,
                phones_mix: false,
            }
        );
        // The FLX6 turns its own output down: Tandem's level stays put.
        let master = knob(0x08, &[(0x10, 0x00), (0x08, 0x00)]);
        assert!(play_levels(&mut e, &mut local, &master).is_empty());
        assert_eq!(local.master_level, LocalParams::default().master_level);
        assert!(e.waiting_for_pickup().is_empty(), "nothing to pick up");
        let out = play_levels(&mut e, &mut local, &knob(0x0D, &[(0x20, 0x00)]));
        assert!(matches!(out.last(), Some(MidiOut::HeadphoneLevel(_))));
        // The master moved to the PC's speakers: the knob sets it again.
        e.card = ControllerCard::find(
            Some("DDJ-FLX6"),
            Some("Speakers (Realtek(R) Audio)"),
            Some("Speakers (G733 Gaming Headset)"),
        );
        assert_eq!(e.card, ControllerCard::default());
        play_levels(&mut e, &mut local, &master);
        assert_eq!(local.master_level, (0x08 << 7) as f32 / 16383.0);
    }

    #[test]
    fn phones_mix_knob_leaves_a_controller_that_mixes_its_own_alone() {
        let mut e = MidiEngine::new(builtin_preset("DDJ-FLX6").unwrap(), 0.08);
        let mut local = LocalParams::default();
        // HEADPHONE MIX, per Pioneer's list: CC 0x0C and 0x2C on channel 7.
        let mix = knob(0x0C, &[(0x7F, 0x7F)]);
        // Headphones on the FLX6's jack: its knob blends them in hardware.
        e.card = ControllerCard::find(
            Some("DDJ-FLX6"),
            Some("Line (DDJ-FLX6)"),
            Some("Line (DDJ-FLX6)"),
        );
        e.card.phones_mix = true;
        assert!(play_levels(&mut e, &mut local, &mix)
            .iter()
            .all(|o| !matches!(o, MidiOut::CueMix(_))));
        // Headphones on a headset: the knob sets Tandem's mix.
        e.card = ControllerCard::find(
            Some("DDJ-FLX6"),
            Some("Line (DDJ-FLX6)"),
            Some("Speakers (G733 Gaming Headset)"),
        );
        let out = play_levels(&mut e, &mut local, &mix);
        assert!(
            out.iter().any(|o| matches!(o, MidiOut::CueMix(_))),
            "{out:?}"
        );
    }

    #[test]
    fn controllers_that_mix_their_own_headphones() {
        // Windows names the FLX6's sound card like this.
        assert_eq!(mixes_own_phones("Line (2- DDJ-FLX6)"), Some("DDJ-FLX6"));
        assert_eq!(mixes_own_phones("Speakers (DDJ-FLX4)"), Some("DDJ-FLX4"));
        assert_eq!(mixes_own_phones("DDJ-400"), Some("DDJ-400"));
        assert_eq!(mixes_own_phones("Speakers (G733 Gaming Headset)"), None);
        // Not known to: its knob still sets Tandem's mix.
        assert_eq!(mixes_own_phones("Speakers (SC LIVE 4)"), None);
    }

    #[test]
    fn controller_sound_cards_by_name() {
        // Names as Windows shows them for a DDJ-FLX6 and the user's other cards.
        assert!(is_controller_card("DDJ-FLX6", "Line (DDJ-FLX6)"));
        assert!(is_controller_card("2- DDJ-FLX6", "Line (DDJ-FLX6)"));
        assert!(is_controller_card("DDJ-FLX6", "Speakers (2- DDJ-FLX6)"));
        assert!(!is_controller_card(
            "DDJ-FLX6",
            "Speakers (G733 Gaming Headset)"
        ));
        assert!(!is_controller_card(
            "DDJ-FLX6",
            "CABLE Input (VB-Audio Virtual Cable)"
        ));
        assert!(!is_controller_card("DDJ-FLX4", "Line (DDJ-FLX6)"));
        assert!(is_controller_card("SC LIVE 4", "Speakers (SC LIVE 4)"));
        assert!(is_controller_card(
            "Mixtrack Pro FX MIDI 1",
            "Speakers (Mixtrack Pro FX)"
        ));
        // A generic interface isn't every USB sound card.
        assert!(!is_controller_card(
            "USB MIDI",
            "Speakers (USB Audio Device)"
        ));
        assert_eq!(
            ControllerCard::find(None, Some("Line (DDJ-FLX6)"), None),
            ControllerCard::default()
        );
    }

    #[test]
    fn saved_v7_to_v11_flx6_mappings_gain_the_level_knobs() {
        let preset = builtin_preset("DDJ-FLX6").unwrap();
        assert_eq!(preset.preset_version, 13);
        let level = |t: &Target| matches!(t, Target::MasterLevel | Target::HeadphoneLevel);
        let rolls = |t: &Target| matches!(t, Target::Roll { .. });
        let shift_tempo = |t: &Target| matches!(t, Target::TempoShift { .. });
        let browse = |t: &Target| matches!(t, Target::BrowsePress | Target::BrowseBack);
        let deck_select = |t: &Target| matches!(t, Target::DeckSelect { .. });
        // Saved by preset 11 (v0.11.9 to v0.11.13): the four DECK messages
        // are new (see `flx6_deck_switch_doesnt_make_the_tempo_jump`).
        let mut saved = preset.clone();
        saved.preset_version = 11;
        saved.bindings.retain(|b| !deck_select(&b.target));
        assert_eq!(saved.upgrade_from(&preset), 4);
        assert_eq!(saved.bindings.len(), preset.bindings.len());
        // Preset 10 (v0.11.7 and v0.11.8): and the two knobs. Preset 9: the
        // roll pads too. Preset 8: and SHIFT + TEMPO on all four decks.
        // Preset 7: and the browse knob's press and BACK.
        for (version, added) in [
            (10, 4 + 2),
            (9, 4 + 2 + 32),
            (8, 4 + 2 + 32 + 4),
            (7, 4 + 2 + 32 + 4 + 2),
        ] {
            let mut saved = preset.clone();
            saved.preset_version = version;
            saved.jog_ticks_per_rev = Some(6387.0);
            saved.bindings.retain(|b| {
                !level(&b.target)
                    && !deck_select(&b.target)
                    && (version > 9 || !rolls(&b.target))
                    && (version > 8 || !shift_tempo(&b.target))
                    && (version > 7 || !browse(&b.target))
            });
            let mut custom = saved.clone();
            assert_eq!(saved.upgrade_from(&preset), added, "preset {version}");
            assert_eq!(saved.preset_version, PRESET_VERSION);
            assert_eq!(saved.bindings.len(), preset.bindings.len());
            assert_eq!(
                saved.binding_for(Target::MasterLevel).map(|b| b.trigger),
                Some(Trigger::cc(6, 0x08))
            );
            assert_eq!(
                saved.binding_for(Target::HeadphoneLevel).map(|b| b.trigger),
                Some(Trigger::cc(6, 0x0D))
            );
            assert_eq!(saved.jog_ticks_per_rev, Some(6387.0));
            assert_eq!(saved.upgrade_from(&preset), 0, "only once");
            // A DJ who'd put HEADPHONE LEVEL's CC on something else keeps it.
            custom.bind(Trigger::cc(6, 0x0D), Target::CueMix);
            assert_eq!(custom.upgrade_from(&preset), added - 1, "preset {version}");
            assert_eq!(custom.binding_for(Target::HeadphoneLevel), None);
            // And the knob works.
            let mut e = MidiEngine::new(saved, 0.08);
            let mut local = LocalParams::default();
            play_levels(&mut e, &mut local, &knob(0x08, &[(0x20, 0x00)]));
            assert_eq!(local.master_level, (0x20 << 7) as f32 / 16383.0);
        }
        // Two-deck Pioneers don't get them (their knobs may not send MIDI),
        // but keep everything else the family has.
        let small = builtin_preset("DDJ-FLX4").unwrap();
        assert!(!small.bindings.iter().any(|b| level(&b.target)));
        assert!(!small.bindings.iter().any(|b| deck_select(&b.target)));
        assert_eq!(
            small.bindings.iter().filter(|b| rolls(&b.target)).count(),
            16
        );
        assert!(small.binding_for(Target::BrowsePress).is_some());
        // The FLX6 is the family's four-deck layout plus the two knobs, the
        // four DECK messages and the Beat FX strip's 32.
        let family = pioneer_ddj("DDJ-FLX6", "Pioneer DDJ-FLX6", 4, 2048.0);
        assert_eq!(preset.bindings.len(), family.bindings.len() + 2 + 4 + 32);
        assert!(family.bindings.iter().all(|b| preset.bindings.contains(b)));
    }

    #[test]
    fn beat_loop_labels() {
        let labels: Vec<String> = BEAT_LOOPS.iter().map(|b| beats_label(*b)).collect();
        assert_eq!(labels, ["¼", "½", "1", "2", "4", "8", "16", "32"]);
        let labels: Vec<String> = ROLLS.iter().map(|b| beats_label(*b)).collect();
        assert_eq!(labels, ["1/16", "1/8", "¼", "½", "1", "2", "4", "8"]);
        assert_eq!(
            Target::Roll { deck: 1, size: 0 }.label(),
            "Deck B · Roll 1/16"
        );
        assert_eq!(beats_label(0.125), "1/8");
        assert_eq!(beats_label(64.0), "64");
    }

    #[test]
    fn port_name_keys() {
        assert_eq!(mapping_key("2- DDJ-FLX4"), "DDJ-FLX4");
        assert_eq!(mapping_key("DDJ-400"), "DDJ-400");
        assert_eq!(mapping_key("Mixtrack Pro FX"), "Mixtrack_Pro_FX");
        assert!(builtin_preset("DDJ-FLX6").is_some());
        assert!(builtin_preset("Mixtrack Pro FX").is_none());
        for port in ["SC LIVE 4", "2- SC LIVE 4", "SC Live 4 MIDI"] {
            let p = builtin_preset(port).expect(port);
            assert_eq!(p.preset.as_deref(), Some("Denon SC Live 4"));
        }
        assert_eq!(mapping_key("2- SC LIVE 4"), "SC_LIVE_4");
        assert!(builtin_preset("SC LIVE 2").is_none());
    }

    fn sc_live_4() -> MidiEngine {
        MidiEngine::new(builtin_preset("SC LIVE 4").unwrap(), 0.08)
    }

    /// Plays SC Live 4 messages; the left side sends on MIDI channel 5
    /// (0x94 note on, 0xB4 CC) and the right side on channel 6.
    fn send(e: &mut MidiEngine, msg: [u8; 3]) -> Vec<MidiOut> {
        e.handle(&msg, &RoomState::default(), &LocalParams::default())
    }

    fn act(a: Action) -> Vec<MidiOut> {
        vec![MidiOut::Action(a)]
    }

    fn roll(deck: u8, beats: f64, on: bool) -> Vec<MidiOut> {
        act(Action::Roll { deck, beats, on })
    }

    /// Presses and lets go of a button (Denon lets go with velocity 0).
    fn tap(e: &mut MidiEngine, status: u8, note: u8) -> Vec<MidiOut> {
        let mut out = send(e, [status, note, 0x7F]);
        out.extend(send(e, [status, note, 0x00]));
        out
    }

    #[test]
    fn sc_live_4_roll_mode_pads_roll() {
        let mut e = sc_live_4();
        // Every deck starts on hot cues.
        assert_eq!(
            tap(&mut e, 0x94, 0x0F),
            [
                MidiOut::Action(Action::HotCue { deck: 0, slot: 0 }),
                MidiOut::Action(Action::HotCueUp { deck: 0, slot: 0 }),
            ]
        );
        // ROLL (note 0x0D) on the left: its pads roll 1/16 to 8 beats
        // while held.
        assert!(tap(&mut e, 0x94, 0x0D).is_empty());
        assert_eq!(e.pad_mode(0), PadMode::Rolls);
        for (i, beats) in ROLLS.iter().enumerate() {
            let pad = 0x0F + i as u8;
            assert_eq!(
                send(&mut e, [0x94, pad, 0x7F]),
                roll(0, *beats, true),
                "pad {}",
                i + 1
            );
            assert_eq!(send(&mut e, [0x94, pad, 0x00]), roll(0, *beats, false));
        }
        // Two pads held: the latest rolls, and letting go of it goes back
        // to the other one's size. A note off lets go too.
        assert_eq!(send(&mut e, [0x94, 0x12, 0x7F]), roll(0, 0.5, true));
        assert_eq!(send(&mut e, [0x94, 0x11, 0x7F]), roll(0, 0.25, true));
        assert_eq!(send(&mut e, [0x94, 0x11, 0x00]), roll(0, 0.5, true));
        assert_eq!(send(&mut e, [0x84, 0x12, 0x40]), roll(0, 0.5, false));
        // The right side keeps its own mode: still hot cues.
        assert_eq!(e.pad_mode(1), PadMode::HotCues);
        assert_eq!(
            send(&mut e, [0x95, 0x0F, 0x7F]),
            act(Action::HotCue { deck: 1, slot: 0 })
        );
        send(&mut e, [0x95, 0x0F, 0x00]);
        // ROLL on the right rolls deck B.
        tap(&mut e, 0x95, 0x0D);
        assert_eq!(send(&mut e, [0x95, 0x16, 0x7F]), roll(1, 8.0, true));
        assert_eq!(send(&mut e, [0x95, 0x16, 0x00]), roll(1, 8.0, false));
        // LOOP on the left: beat loops of ¼ to 32 beats.
        tap(&mut e, 0x94, 0x0C);
        assert_eq!(e.pad_mode(0), PadMode::BeatLoops);
        for (i, beats) in BEAT_LOOPS.iter().enumerate() {
            let pad = 0x0F + i as u8;
            let beats = *beats;
            assert_eq!(
                tap(&mut e, 0x94, pad),
                act(Action::BeatLoop { deck: 0, beats })
            );
        }
        // SLICER isn't mapped: the pads stay as they were.
        assert!(tap(&mut e, 0x94, 0x0E).is_empty());
        assert_eq!(e.pad_mode(0), PadMode::BeatLoops);
        // HOT CUE: hot cues again; the right side still rolls.
        tap(&mut e, 0x94, 0x0B);
        assert_eq!(
            send(&mut e, [0x94, 0x16, 0x7F]),
            act(Action::HotCue { deck: 0, slot: 7 })
        );
        assert_eq!(send(&mut e, [0x95, 0x12, 0x7F]), roll(1, 0.5, true));
    }

    #[test]
    fn sc_live_4_shift() {
        let mut e = sc_live_4();
        // SHIFT on either side shifts the whole controller: SHIFT + pad
        // clears the hot cue, and its release does nothing.
        assert!(send(&mut e, [0x94, 0x1C, 0x7F]).is_empty());
        assert!(e.shift_held());
        assert_eq!(
            send(&mut e, [0x95, 0x10, 0x7F]),
            act(Action::ClearHotCue { deck: 1, slot: 1 })
        );
        assert!(send(&mut e, [0x95, 0x10, 0x00]).is_empty());
        // SHIFT + CUE goes back to the start, SHIFT + SYNC turns sync off.
        assert_eq!(
            tap(&mut e, 0x94, 0x09),
            act(Action::Seek { deck: 0, pos: 0.0 })
        );
        assert_eq!(
            tap(&mut e, 0x94, 0x08)[0],
            MidiOut::Action(Action::Sync { deck: 0, on: false })
        );
        // A control with no SHIFT binding does the same with SHIFT held.
        assert_eq!(tap(&mut e, 0x94, 0x0A), act(Action::Play { deck: 0 }));
        tap(&mut e, 0x94, 0x0D);
        assert_eq!(tap(&mut e, 0x94, 0x0F).len(), 2, "SHIFT + pad still rolls");
        // Both SHIFTs held and one let go: still shifted.
        send(&mut e, [0x95, 0x1C, 0x7F]);
        send(&mut e, [0x94, 0x1C, 0x00]);
        assert!(e.shift_held());
        send(&mut e, [0x95, 0x1C, 0x00]);
        assert!(!e.shift_held());
        assert_eq!(
            send(&mut e, [0x95, 0x10, 0x7F]),
            act(Action::HotCue { deck: 1, slot: 1 })
        );
        assert_eq!(
            send(&mut e, [0x94, 0x09, 0x7F]),
            act(Action::CueDown { deck: 0 })
        );
    }

    #[test]
    fn sc_live_4_shift_tempo_moves_the_fader_not_the_tempo() {
        let mut e = sc_live_4();
        let mut room = RoomState::default();
        let local = LocalParams::default();
        let at = |v: u8| 1.0 + ((v as f32 / 127.0) as f64 - 0.5) * 0.16;
        let tempo = |out: Vec<MidiOut>| match out[..] {
            [MidiOut::Action(Action::Tempo { deck: 0, tempo })] => tempo,
            ref other => panic!("{other:?}"),
        };
        room.decks[0].tempo = tempo(e.handle(&[0xB4, 0x1F, 96], &room, &local));
        assert!((room.decks[0].tempo - at(96)).abs() < 1e-9);
        // SHIFT + fader back to the centre: the tempo stays.
        e.handle(&[0x94, 0x1C, 0x7F], &room, &local);
        assert!(e.handle(&[0xB4, 0x1F, 64], &room, &local).is_empty());
        e.handle(&[0x94, 0x1C, 0x00], &room, &local);
        // Then up again: the tempo goes on from where it was.
        let next = tempo(e.handle(&[0xB4, 0x1F, 72], &room, &local));
        let want = at(96) + at(72) - at(64);
        assert!((next - want).abs() < 1e-9, "{next} vs {want}");
    }

    #[test]
    fn letting_go_goes_where_pressing_went() {
        let mut e = sc_live_4();
        // A roll held while switching to HOT CUE still ends when let go.
        tap(&mut e, 0x94, 0x0D);
        assert_eq!(send(&mut e, [0x94, 0x13, 0x7F]), roll(0, 1.0, true));
        tap(&mut e, 0x94, 0x0B);
        assert_eq!(send(&mut e, [0x94, 0x13, 0x00]), roll(0, 1.0, false));
        // SHIFT + pad, SHIFT let go first: the pad's release isn't a hot
        // cue's.
        send(&mut e, [0x94, 0x1C, 0x7F]);
        assert_eq!(
            send(&mut e, [0x94, 0x0F, 0x7F]),
            act(Action::ClearHotCue { deck: 0, slot: 0 })
        );
        send(&mut e, [0x94, 0x1C, 0x00]);
        assert!(send(&mut e, [0x94, 0x0F, 0x00]).is_empty());
        // And a hot cue held while SHIFT goes down is let go as a hot cue.
        assert_eq!(
            send(&mut e, [0x94, 0x0F, 0x7F]),
            act(Action::HotCue { deck: 0, slot: 0 })
        );
        send(&mut e, [0x94, 0x1C, 0x7F]);
        assert_eq!(
            send(&mut e, [0x94, 0x0F, 0x00]),
            act(Action::HotCueUp { deck: 0, slot: 0 })
        );
    }

    #[test]
    fn sc_live_4_lights() {
        let mut e = sc_live_4();
        let local = LocalParams::default();
        let mut room = RoomState::default();
        room.decks[0].hot_cues[2] = Some(1.0);
        let light = |e: &MidiEngine, room: &RoomState, k: (u8, u8)| {
            let lights = e.lights(room, &local);
            lights.iter().find(|(kk, _)| *kk == k).map(|x| x.1)
        };
        // HOT CUE lit, LOOP and ROLL dim; SLICER left alone.
        assert_eq!(light(&e, &room, (0x94, 0x0B)), Some(127));
        assert_eq!(light(&e, &room, (0x94, 0x0C)), Some(1));
        assert_eq!(light(&e, &room, (0x94, 0x0D)), Some(1));
        assert_eq!(light(&e, &room, (0x94, 0x0E)), None);
        // Hot cue pads: a set one in its colour (3: yellow), empty ones off.
        assert_eq!(light(&e, &room, (0x94, 0x11)), Some(60));
        assert_eq!(light(&e, &room, (0x94, 0x0F)), Some(0));
        // Buttons are dim, not off; nothing is sent to the jog or bends.
        for k in [(0x94, 0x0A), (0x94, 0x09), (0x94, 0x1C), (0x90, 0x0D)] {
            assert_eq!(light(&e, &room, k), Some(1), "{k:02X?}");
        }
        for k in [(0x94, 0x21), (0x94, 0x1D), (0x94, 0x1E)] {
            assert_eq!(light(&e, &room, k), None, "{k:02X?}");
        }
        // ROLL on the left: its button lit, its pads dark violet; the right
        // side's lights don't change.
        tap(&mut e, 0x94, 0x0D);
        assert_eq!(light(&e, &room, (0x94, 0x0D)), Some(127));
        assert_eq!(light(&e, &room, (0x94, 0x0B)), Some(1));
        assert_eq!(light(&e, &room, (0x94, 0x11)), Some(17));
        assert_eq!(light(&e, &room, (0x95, 0x0B)), Some(127));
        assert_eq!(light(&e, &room, (0x95, 0x0F)), Some(0));
        // The held roll's pad lights white.
        room.decks[0] = crate::model::DeckState {
            track: Some(crate::model::TrackId([1; 32])),
            duration: 100.0,
            playing: true,
            ..Default::default()
        };
        room.apply(&crate::model::Event {
            time: 1_000_000,
            origin: 1,
            seq: 1,
            action: Action::Roll {
                deck: 0,
                beats: 0.5,
                on: true,
            },
        });
        assert_eq!(light(&e, &room, (0x94, 0x12)), Some(63), "pad 4: ½ beat");
        assert_eq!(light(&e, &room, (0x94, 0x13)), Some(17));
        let leds = e.leds(&room, &local);
        assert!(leds.contains(&((0x94, 0x12), true)));
        assert!(leds.contains(&((0x94, 0x13), false)));
        // LOOP: beat loop pads dark green.
        tap(&mut e, 0x94, 0x0C);
        assert_eq!(light(&e, &room, (0x94, 0x12)), Some(4));
        // SHIFT lights both SHIFT buttons while held.
        send(&mut e, [0x95, 0x1C, 0x7F]);
        assert_eq!(light(&e, &room, (0x94, 0x1C)), Some(127));
        assert_eq!(light(&e, &room, (0x95, 0x1C)), Some(127));
    }

    /// The Inpulse 200 MK2's messages below are the ones in
    /// docs/controllers/hercules-djcontrol-inpulse-200-mk2.md: global
    /// controls on MIDI channel 1 (0x90 / 0xB0), the left deck on channel 2
    /// and the right on channel 3, each three channels up while its SHIFT
    /// is held, and the pads on channels 7 and 8.
    fn inpulse_200_mk2() -> MidiEngine {
        MidiEngine::new(builtin_preset("DJControl Inpulse 200 Mk2").unwrap(), 0.08)
    }

    /// Plays messages in and lets the room follow, as a session does.
    /// Returns everything they sent.
    fn follow(e: &mut MidiEngine, room: &mut RoomState, msgs: &[[u8; 3]]) -> Vec<MidiOut> {
        let local = LocalParams::default();
        let mut all = Vec::new();
        for m in msgs {
            for o in e.handle(m, room, &local) {
                if let MidiOut::Action(a) = &o {
                    let seq = all.len() as u64 + 1;
                    room.apply(&crate::model::Event {
                        time: 1_000_000 + seq as i64 * 1000,
                        origin: 1,
                        seq,
                        action: a.clone(),
                    });
                }
                all.push(o);
            }
        }
        all
    }

    #[test]
    fn inpulse_200_mk2_port_names() {
        // The name its sources give (on Windows, macOS and Linux), and with
        // the number Windows may put in front.
        for port in ["DJControl Inpulse 200 Mk2", "2- DJControl Inpulse 200 Mk2"] {
            let p = builtin_preset(port).expect(port);
            assert_eq!(
                p.preset.as_deref(),
                Some("Hercules DJControl Inpulse 200 MK2")
            );
        }
        assert_eq!(
            mapping_key("2- DJControl Inpulse 200 Mk2"),
            "DJControl_Inpulse_200_Mk2"
        );
        // The first Inpulse 200, the MK3 and the bigger models have other
        // labels and other maps.
        for port in [
            "DJControl Inpulse 200",
            "DJControl Inpulse 200 MK3",
            "DJControl Inpulse 300 Mk2",
            "DJControl Inpulse 500",
        ] {
            assert!(builtin_preset(port).is_none(), "{port}");
        }
    }

    #[test]
    fn inpulse_200_mk2_transport_and_shift() {
        let mut e = inpulse_200_mk2();
        for (deck, st) in [(0u8, 0x91u8), (1, 0x92)] {
            // PLAY 0x07, CUE 0x06, SYNC 0x05, LOAD 0x0D, headphone cue 0x0C.
            // Buttons let go with a note-on of velocity 0.
            assert_eq!(tap(&mut e, st, 0x07), act(Action::Play { deck }));
            assert_eq!(
                tap(&mut e, st, 0x06),
                [
                    MidiOut::Action(Action::CueDown { deck }),
                    MidiOut::Action(Action::CueUp { deck }),
                ]
            );
            assert_eq!(tap(&mut e, st, 0x05), act(Action::Sync { deck, on: true }));
            assert_eq!(tap(&mut e, st, 0x0D), [MidiOut::LoadSelected(deck)]);
            assert_eq!(tap(&mut e, st, 0x0C), [MidiOut::TogglePfl(deck)]);
            // With SHIFT held the same buttons arrive three channels up:
            // SHIFT + CUE goes back to the start, SHIFT + SYNC turns sync
            // off.
            let shift = st + 3;
            assert_eq!(
                tap(&mut e, shift, 0x06),
                act(Action::Seek { deck, pos: 0.0 })
            );
            assert_eq!(
                tap(&mut e, shift, 0x05)[0],
                MidiOut::Action(Action::Sync { deck, on: false })
            );
            // SHIFT's own note does nothing, nor do SHIFT + PLAY (stutter),
            // SHIFT + LOOP (vinyl mode), SHIFT + LOAD or SHIFT + the
            // headphone cue.
            assert!(tap(&mut e, st, 0x04).is_empty());
            for note in [0x07, 0x03, 0x0D, 0x0C] {
                assert!(tap(&mut e, shift, note).is_empty(), "{note:02X}");
            }
        }
    }

    #[test]
    fn inpulse_200_mk2_loop_section() {
        let mut e = inpulse_200_mk2();
        let quantize = e.quantize_loops;
        for (deck, st) in [(0u8, 0x91u8), (1, 0x92)] {
            // LOOP (0x03) is a 4-beat loop; ½ (0x09) and X2 (0x0A) halve
            // and double it.
            assert_eq!(
                tap(&mut e, st, 0x03),
                act(Action::BeatLoop { deck, beats: 4.0 })
            );
            assert_eq!(tap(&mut e, st, 0x09), act(Action::LoopHalve { deck }));
            assert_eq!(tap(&mut e, st, 0x0A), act(Action::LoopDouble { deck }));
            // With SHIFT they are IN and OUT, as printed under them.
            assert_eq!(
                tap(&mut e, st + 3, 0x09),
                act(Action::LoopIn { deck, quantize })
            );
            assert_eq!(
                tap(&mut e, st + 3, 0x0A),
                act(Action::LoopOut { deck, quantize })
            );
            // Holding ½ also sends note 0x0B, and holding LOAD 0x0E.
            assert!(tap(&mut e, st, 0x0B).is_empty());
            assert!(tap(&mut e, st, 0x0E).is_empty());
        }
    }

    #[test]
    fn inpulse_200_mk2_pads() {
        let mut e = inpulse_200_mk2();
        for (deck, st) in [(0u8, 0x96u8), (1, 0x97)] {
            for slot in 0..4u8 {
                // HOT CUE mode: notes 0-3, and 8-11 with SHIFT (clear).
                assert_eq!(
                    tap(&mut e, st, slot),
                    [
                        MidiOut::Action(Action::HotCue { deck, slot }),
                        MidiOut::Action(Action::HotCueUp { deck, slot }),
                    ]
                );
                assert_eq!(
                    tap(&mut e, st, 0x08 + slot),
                    act(Action::ClearHotCue { deck, slot })
                );
                // STEMS (0x10), FX (0x20) and SAMPLER (0x30) pads, plain
                // and with SHIFT (+ 8): Tandem has nothing for them.
                for base in [0x10, 0x18, 0x20, 0x28, 0x30, 0x38] {
                    assert!(tap(&mut e, st, base + slot).is_empty(), "{base:02X}");
                }
            }
        }
        // The pad mode buttons (HOT CUE 0x0F, STEMS 0x10, and with SHIFT FX
        // 0x11 and SAMPLER 0x12) switch the pads inside the unit.
        for st in [0x91, 0x92] {
            for note in [0x0F, 0x10, 0x11, 0x12] {
                assert!(tap(&mut e, st, note).is_empty(), "{note:02X}");
            }
        }
    }

    #[test]
    fn inpulse_200_mk2_jog() {
        let mut e = inpulse_200_mk2();
        let local = LocalParams::default();
        let mut room = RoomState::default();
        room.decks[1].track = Some(crate::model::TrackId([1; 32]));
        let nudge = |e: &mut MidiEngine| match e.flush(None)[..] {
            [Action::Nudge { deck: 1, delta }] => delta,
            ref other => panic!("{other:?}"),
        };
        // Turned by the ring (CC 0x09) while paused: it searches, forwards
        // when clockwise (1 a tick).
        assert!(e.handle(&[0xB2, 0x09, 0x01], &room, &local).is_empty());
        assert!((nudge(&mut e) - SECONDS_PER_TURN / 248.0).abs() < 1e-12);
        // A hand on top of the right platter (note 0x08) holds the record.
        assert_eq!(
            e.handle(&[0x92, 0x08, 0x7F], &room, &local),
            act(Action::JogTouch {
                deck: 1,
                down: true,
                spin: 0.0,
            })
        );
        room.decks[1].scratching = true;
        // Turned with the hand on it: CC 0x0A, bigger steps when faster.
        e.handle(&[0xB2, 0x0A, 0x01], &room, &local);
        e.handle(&[0xB2, 0x0A, 0x03], &room, &local);
        assert!((nudge(&mut e) - 4.0 / 248.0 * SECONDS_PER_TURN).abs() < 1e-12);
        // Anticlockwise counts down from 127: backwards.
        e.handle(&[0xB2, 0x0A, 0x7F], &room, &local);
        e.handle(&[0xB2, 0x0A, 0x7E], &room, &local);
        assert!((nudge(&mut e) + 3.0 / 248.0 * SECONDS_PER_TURN).abs() < 1e-12);
        assert_eq!(
            e.handle_at(&[0x92, 0x08, 0x00], 100_000, &room, &local),
            act(Action::JogTouch {
                deck: 1,
                down: false,
                spin: 0.0,
            })
        );
        room.decks[1].scratching = false;
        // Playing, a second later: the ring bends the speed, up when
        // turned clockwise and down when anticlockwise.
        room.decks[1].playing = true;
        for _ in 0..4 {
            e.handle_at(&[0xB2, 0x09, 0x01], 1_100_000, &room, &local);
        }
        match e.flush(Some(0.025))[..] {
            [Action::Bend { deck: 1, bend }] => assert!(bend > 0.0, "{bend}"),
            ref other => panic!("{other:?}"),
        }
        for _ in 0..12 {
            e.handle_at(&[0xB2, 0x09, 0x7F], 1_125_000, &room, &local);
        }
        match e.flush(Some(0.025))[..] {
            [Action::Bend { deck: 1, bend }] => assert!(bend < 0.0, "{bend}"),
            ref other => panic!("{other:?}"),
        }
        // SHIFT + jog (fast search in other software) arrives on channel
        // 6, touch and turn: left alone.
        assert!(e.handle(&[0x95, 0x08, 0x7F], &room, &local).is_empty());
        assert!(e.handle(&[0xB5, 0x0A, 0x01], &room, &local).is_empty());
        assert!(e.handle(&[0xB5, 0x09, 0x01], &room, &local).is_empty());
        assert!(e.flush(None).is_empty());
    }

    #[test]
    fn inpulse_200_mk2_tempo_fader_falls_towards_plus() {
        let mut e = inpulse_200_mk2();
        let mut room = RoomState::default();
        // 14-bit: CC 0x08, then its fine half on CC 0x28.
        drive(&mut e, &mut room, &fader(1, 0x08, CENTRE, CENTRE, 1));
        let centre = room.decks[0].tempo;
        assert!((centre - 1.0).abs() < 1e-5);
        // Down, towards "+": the value falls and the tempo rises, to +8%.
        let up = tempo_after_pairs(&mut e, &mut room, 0, &fader(1, 0x08, CENTRE, BOTTOM, 37));
        assert!(up.windows(2).all(|w| w[1] > w[0]));
        assert!((room.decks[0].tempo - 1.08).abs() < 1e-9);
        // SHIFT + fader (channel 5) back to the centre: the tempo stays.
        let shifted = fader(4, 0x08, BOTTOM, CENTRE, 37);
        assert!(drive(&mut e, &mut room, &shifted).is_empty());
        assert_eq!(room.decks[0].tempo, 1.08);
        let hw = e.hardware_tempo(0).unwrap();
        assert!((hw - 1.0).abs() < 1e-5, "the fader is at the centre: {hw}");
        // Down again: on from +8%, past the fader's own range.
        tempo_after_pairs(&mut e, &mut room, 0, &fader(1, 0x08, CENTRE, BOTTOM, 37));
        let want = 1.08 + (1.08 - centre);
        assert!((room.decks[0].tempo - want).abs() < 1e-9);
        // The right deck, up to the top ("−"): 8% slower.
        drive(&mut e, &mut room, &fader(2, 0x08, CENTRE, CENTRE, 1));
        let down = tempo_after_pairs(&mut e, &mut room, 1, &fader(2, 0x08, CENTRE, TOP, 37));
        assert!(down.windows(2).all(|w| w[1] < w[0]));
        assert!((room.decks[1].tempo - 0.92).abs() < 1e-9);
    }

    #[test]
    fn inpulse_200_mk2_mixer_and_browse() {
        let mut e = inpulse_200_mk2();
        let mut room = RoomState::default();
        let last = |out: Vec<MidiOut>| out.last().cloned();
        for (deck, st) in [(0u8, 0xB1u8), (1, 0xB2)] {
            let d = deck as usize;
            // Each fader and knob sends its coarse half, then its fine half
            // 32 higher: volume 0x00, FILTER 0x01, LOW 0x02, HIGH 0x04,
            // GAIN 0x05.
            let out = follow(&mut e, &mut room, &[[st, 0x00, 0x40], [st, 0x20, 0x00]]);
            let v = 8192.0 / 16383.0;
            assert_eq!(last(out), Some(MidiOut::Action(Action::Volume { deck, v })));
            follow(&mut e, &mut room, &[[st, 0x01, 0x7F], [st, 0x21, 0x7F]]);
            follow(&mut e, &mut room, &[[st, 0x02, 0x00], [st, 0x22, 0x00]]);
            follow(&mut e, &mut room, &[[st, 0x04, 0x7F], [st, 0x24, 0x7F]]);
            follow(&mut e, &mut room, &[[st, 0x05, 0x20], [st, 0x25, 0x00]]);
            assert_eq!(room.decks[d].volume, v);
            assert_eq!(room.decks[d].filter, 1.0);
            assert_eq!(
                room.decks[d].eq,
                [0.0, 0.5, 1.0],
                "low, (no mid knob), high"
            );
            assert_eq!(room.decks[d].trim, 4096.0 / 16383.0);
            // With SHIFT held the mixer arrives three channels up: left
            // alone.
            for cc in [0x00, 0x01, 0x02, 0x04, 0x05] {
                let msgs = [[st + 3, cc, 0x10], [st + 3, cc + 32, 0x00]];
                assert!(follow(&mut e, &mut room, &msgs).is_empty(), "{cc:02X}");
            }
        }
        // The crossfader: 0 is full left, deck A's side.
        follow(&mut e, &mut room, &[[0xB0, 0x00, 0x00], [0xB0, 0x20, 0x00]]);
        assert_eq!(room.crossfader, 0.0);
        follow(&mut e, &mut room, &[[0xB0, 0x00, 0x7F], [0xB0, 0x20, 0x7F]]);
        assert_eq!(room.crossfader, 1.0);

        let local = LocalParams::default();
        let mut h = |msg: [u8; 3]| e.handle(&msg, &room, &local);
        // BROWSER: clockwise (1) moves down the list, anticlockwise (127)
        // up; pressing it, and SHIFT + pressing it (channel 4).
        assert_eq!(h([0xB0, 0x01, 0x01]), vec![MidiOut::Browse(1)]);
        assert_eq!(h([0xB0, 0x01, 0x03]), vec![MidiOut::Browse(3)]);
        assert_eq!(h([0xB0, 0x01, 0x7F]), vec![MidiOut::Browse(-1)]);
        assert_eq!(h([0x90, 0x00, 0x7F]), vec![MidiOut::BrowsePress]);
        assert!(h([0x90, 0x00, 0x00]).is_empty());
        assert_eq!(h([0x93, 0x00, 0x7F]), vec![MidiOut::BrowseBack]);
        assert!(h([0x93, 0x00, 0x00]).is_empty());
        // Left alone: SHIFT + turn, turning with ASSIST./PREP. held,
        // BEATMATCH GUIDE, MASTER and ASSIST./PREP.
        for msg in [
            [0xB3, 0x01, 0x01],
            [0xB0, 0x02, 0x01],
            [0x90, 0x01, 0x7F],
            [0x90, 0x02, 0x7F],
            [0x90, 0x03, 0x7F],
        ] {
            assert!(h(msg).is_empty(), "{msg:02X?}");
        }
    }

    #[test]
    fn inpulse_200_mk2_level_knobs() {
        let mut e = inpulse_200_mk2();
        let mut local = LocalParams::default();
        // MASTER is CC 0x03 and HEADPHONES CC 0x04 on channel 1, 14-bit.
        let master = [[0xB0, 0x03, 0x7F], [0xB0, 0x23, 0x7F]];
        let out = play_levels(&mut e, &mut local, &master);
        assert!(!out.is_empty());
        assert!(out.iter().all(|o| matches!(o, MidiOut::MasterLevel(_))));
        assert_eq!(local.master_level, 1.0);
        let phones = [[0xB0, 0x04, 0x00], [0xB0, 0x24, 0x00]];
        let out = play_levels(&mut e, &mut local, &phones);
        assert!(!out.is_empty());
        assert!(out.iter().all(|o| matches!(o, MidiOut::HeadphoneLevel(_))));
        assert_eq!(local.hp_level, 0.0);
        assert_eq!(local.master_level, 1.0);
    }

    #[test]
    fn inpulse_200_mk2_lights() {
        let e = inpulse_200_mk2();
        let mut local = LocalParams::default();
        let mut room = RoomState::default();
        let light = |room: &RoomState, local: &LocalParams, k: (u8, u8)| {
            let lights = e.lights(room, local);
            lights.iter().find(|(kk, _)| *kk == k).map(|x| x.1)
        };
        // At rest: eleven lights a side, all off. A light is a note-on to
        // its button's own note, 127 lit and 0 off.
        let rest = e.lights(&room, &local);
        assert_eq!(rest.len(), 22);
        assert!(rest.iter().all(|&(_, v)| v == 0), "{rest:02X?}");
        // Deck A playing in sync with hot cue 3 set and its headphone cue
        // on; deck B loaded and paused.
        room.decks[0] = crate::model::DeckState {
            track: Some(crate::model::TrackId([1; 32])),
            duration: 100.0,
            playing: true,
            synced: true,
            ..Default::default()
        };
        room.decks[0].hot_cues[2] = Some(1.0);
        room.decks[1].track = Some(crate::model::TrackId([2; 32]));
        local.pfl[0] = true;
        for (k, want) in [
            ((0x91, 0x07), 127), // PLAY
            ((0x91, 0x06), 0),   // CUE
            ((0x91, 0x05), 127), // SYNC
            ((0x91, 0x0C), 127), // headphone cue
            ((0x96, 0x02), 127), // pad 3
            ((0x96, 0x00), 0),   // pad 1
            ((0x92, 0x07), 0),
            ((0x92, 0x06), 127),
            ((0x92, 0x05), 0),
            ((0x92, 0x0C), 0),
            ((0x97, 0x02), 0),
        ] {
            assert_eq!(light(&room, &local, k), Some(want), "{k:02X?}");
        }
        // A 4-beat loop: LOOP lights, and on the SHIFT layer, which the
        // unit shows while SHIFT is held, IN and OUT.
        room.decks[0].loop_in = Some(10.0);
        room.decks[0].loop_out = Some(12.0);
        room.decks[0].looping = true;
        room.decks[0].loop_beats = Some(4.0);
        for k in [(0x91, 0x03), (0x94, 0x09), (0x94, 0x0A)] {
            assert_eq!(light(&room, &local, k), Some(127), "{k:02X?}");
        }
        assert_eq!(light(&room, &local, (0x92, 0x03)), Some(0));
        // Halved to 2 beats: still looping, but LOOP is the 4-beat loop.
        room.decks[0].loop_beats = Some(2.0);
        assert_eq!(light(&room, &local, (0x91, 0x03)), Some(0));
        assert_eq!(light(&room, &local, (0x94, 0x09)), Some(127));
        // Nothing is sent to ½ and X2, the jog, LOAD, SHIFT + SYNC, CUE or
        // a pad, the pad mode buttons, the STEMS, FX and SAMPLER pads, or
        // the buttons and the ring in the middle.
        for k in [
            (0x91, 0x09),
            (0x91, 0x0A),
            (0x91, 0x08),
            (0x91, 0x0D),
            (0x94, 0x05),
            (0x94, 0x06),
            (0x96, 0x0A),
            (0x91, 0x0F),
            (0x91, 0x10),
            (0x96, 0x12),
            (0x96, 0x22),
            (0x96, 0x32),
            (0x90, 0x00),
            (0x93, 0x00),
            (0x90, 0x03),
            (0x90, 0x05),
        ] {
            assert_eq!(light(&room, &local, k), None, "{k:02X?}");
        }
    }

    #[test]
    fn inpulse_200_mk2_preset_is_whole() {
        let preset = builtin_preset("DJControl Inpulse 200 Mk2").unwrap();
        let json = serde_json::to_string(&preset).unwrap();
        assert_eq!(serde_json::from_str::<Mapping>(&json).unwrap(), preset);
        // No level meters on the unit, and plain lights.
        assert!(preset.meters.is_empty());
        assert!(preset.bindings.iter().all(|b| b.light.is_none()));
        // Each message does one thing, and SHIFT and the pad modes are the
        // unit's own business.
        for (i, a) in preset.bindings.iter().enumerate() {
            assert!(!a.shift && a.mode.is_none());
            assert!(
                preset.bindings[i + 1..]
                    .iter()
                    .all(|b| b.trigger != a.trigger),
                "{}",
                a.label()
            );
        }
        // Both decks have the same controls, and C and D have none.
        let on = |d: u8| {
            preset
                .bindings
                .iter()
                .filter(|b| b.target.deck() == Some(d))
                .count()
        };
        assert_eq!((on(0), on(1), on(2), on(3)), (30, 30, 0, 0));
        // No fine half (CC n + 32) of a fader is anything else's message.
        for b in &preset.bindings {
            if b.trigger.kind == MidiKind::Cc {
                assert!(b.trigger.number < 32, "{}", b.label());
            }
        }
    }

    #[test]
    fn learning_with_shift_and_pad_modes() {
        let mut e = sc_live_4();
        let learned = |out: Vec<MidiOut>| match &out[..] {
            [MidiOut::Learned(b)] => b.clone(),
            other => panic!("{other:?}"),
        };
        // Learning deck A's ½-beat roll: ROLL picks the pad mode and isn't
        // learned, and the pad is learned for that mode.
        e.learn = Some(Target::Roll { deck: 0, size: 3 });
        assert!(tap(&mut e, 0x94, 0x0D).is_empty());
        assert_eq!(e.learn, Some(Target::Roll { deck: 0, size: 3 }));
        let b = learned(send(&mut e, [0x94, 0x16, 0x7F]));
        assert_eq!(b.trigger, Trigger::note(4, 0x16));
        assert_eq!((b.mode, b.shift), (Some(PadMode::Rolls), false));
        assert_eq!(b.label(), "Note 0x16 · ch 5 (pad mode: rolls)");
        assert!(send(&mut e, [0x94, 0x16, 0x00]).is_empty());
        // Learning with SHIFT held: SHIFT + the pad, in HOT CUE mode.
        tap(&mut e, 0x94, 0x0B);
        e.learn = Some(Target::HotCueClear { deck: 0, slot: 0 });
        assert!(send(&mut e, [0x94, 0x1C, 0x7F]).is_empty());
        let b = learned(send(&mut e, [0x94, 0x0F, 0x7F]));
        assert_eq!((b.mode, b.shift), (Some(PadMode::HotCues), true));
        assert_eq!(b.label(), "SHIFT + Note 0x0f · ch 5 (pad mode: hot cues)");
        send(&mut e, [0x94, 0x0F, 0x00]);
        send(&mut e, [0x94, 0x1C, 0x00]);
        // Controls that aren't pads work in every pad mode.
        tap(&mut e, 0x94, 0x0D);
        e.learn = Some(Target::Reloop { deck: 0 });
        let b = learned(send(&mut e, [0x94, 0x28, 0x7F]));
        assert_eq!((b.mode, b.shift), (None, false));
        // SHIFT and the pad mode buttons can be learned themselves.
        e.learn = Some(Target::Shift);
        let b = learned(send(&mut e, [0x94, 0x1C, 0x7F]));
        assert_eq!((b.target, b.shift), (Target::Shift, false));
        e.learn = Some(Target::PadMode {
            deck: 1,
            mode: PadMode::BeatLoops,
        });
        assert_eq!(
            learned(send(&mut e, [0x95, 0x0E, 0x7F])).trigger,
            Trigger::note(5, 0x0E)
        );
        send(&mut e, [0x95, 0x0E, 0x00]);
        assert_eq!(tap(&mut e, 0x95, 0x0E), []);
        assert_eq!(e.pad_mode(1), PadMode::BeatLoops);
        // A controller without pad mode buttons learns pads as before.
        let mut e = engine();
        e.learn = Some(Target::Roll { deck: 0, size: 0 });
        let b = learned(send(&mut e, [0x97, 0x20, 0x7F]));
        assert_eq!((b.mode, b.shift), (None, false));
    }

    /// The SC Live 4 mapping written by hand on 2026-11-14 (Tandem had no
    /// preset for it), as a friend installed it in their mappings folder.
    const SC_LIVE_4_FILE: &str = include_str!("../testdata/SC_LIVE_4.json");

    #[test]
    fn hand_written_sc_live_4_mapping_becomes_the_preset() {
        let saved: Mapping = serde_json::from_str(SC_LIVE_4_FILE).unwrap();
        assert_eq!(saved.preset, None);
        assert_eq!(saved.bindings.len(), 57);
        assert!(saved
            .bindings
            .iter()
            .all(|b| !b.shift && b.mode.is_none() && b.light.is_none()));
        // As it was: hot cues in every pad mode.
        let mut e = MidiEngine::new(saved.clone(), 0.08);
        tap(&mut e, 0x94, 0x0D);
        assert_eq!(
            send(&mut e, [0x94, 0x0F, 0x7F]),
            act(Action::HotCue { deck: 0, slot: 0 })
        );
        // Untouched, it becomes the preset exactly.
        let preset = builtin_preset(&saved.device).expect("a preset for SC LIVE 4");
        let mut m = saved.clone();
        assert!(m.adopt(&preset));
        assert_eq!(m, preset);
        assert_eq!(m.upgrade_from(&preset), 0);
        assert!(!m.adopt(&preset), "only once");
        // Calibrated, with the jog inverted, and with RELOOP learned onto
        // deck B's pad 8 (where hot cue 8 stays).
        let mut m = saved.clone();
        m.jog_ticks_per_rev = Some(1012.0);
        m.scratch_sensitivity = 1.5;
        m.vinyl = false;
        let jog = Trigger::cc(4, 0x36);
        m.bindings
            .iter_mut()
            .find(|b| b.trigger == jog)
            .unwrap()
            .invert = true;
        let pad8 = Trigger::note(5, 0x16);
        m.bind(pad8, Target::Reloop { deck: 1 });
        assert!(m.adopt(&preset));
        assert_eq!(m.preset.as_deref(), Some("Denon SC Live 4"));
        assert_eq!(m.preset_version, PRESET_VERSION);
        assert_eq!(m.jog_ticks_per_rev, Some(1012.0));
        assert_eq!((m.scratch_sensitivity, m.vinyl), (1.5, false));
        let at = |m: &Mapping, t: Trigger| {
            m.bindings
                .iter()
                .filter(|b| b.trigger == t)
                .cloned()
                .collect::<Vec<_>>()
        };
        let jogs = at(&m, jog);
        assert!(jogs[0].invert && jogs[0].rel == RelMode::TwosComplement);
        let pad8_bindings: Vec<Target> = at(&m, pad8).iter().map(|b| b.target).collect();
        assert_eq!(
            pad8_bindings,
            [
                Target::HotCue { deck: 1, slot: 7 },
                Target::Reloop { deck: 1 }
            ]
        );
        assert_eq!(m.bindings.len(), preset.bindings.len() - 4 + 2);
        // Saved and loaded again, it's the same, and deck A's pads roll.
        let json = serde_json::to_vec_pretty(&m).unwrap();
        let m: Mapping = serde_json::from_slice(&json).unwrap();
        let mut e = MidiEngine::new(m, 0.08);
        tap(&mut e, 0x94, 0x0D);
        assert_eq!(send(&mut e, [0x94, 0x12, 0x7F]), roll(0, 0.5, true));
        assert_eq!(send(&mut e, [0x94, 0x12, 0x00]), roll(0, 0.5, false));
    }

    #[test]
    fn saved_sc_live_4_mappings_gain_new_pad_modes() {
        // As if a later preset added the ROLL pads: saved mappings gain
        // them on every pad, even though the pads already do hot cues and
        // beat loops in the other modes...
        let preset = builtin_preset("SC LIVE 4").unwrap();
        let rolls = |b: &Binding| b.mode == Some(PadMode::Rolls);
        let mut saved = preset.clone();
        saved.preset_version = PRESET_VERSION - 1;
        saved.bindings.retain(|b| !rolls(b));
        // ...but not on a pad the DJ learned a control onto (it works in
        // every mode), or one they learned a roll onto themselves.
        saved.bind(Trigger::note(5, 0x16), Target::Reloop { deck: 1 });
        saved
            .bind(Trigger::note(4, 0x0F), Target::Roll { deck: 0, size: 7 })
            .mode = Some(PadMode::Rolls);
        assert_eq!(saved.upgrade_from(&preset), 16 - 2);
        let mut e = MidiEngine::new(saved, 0.08);
        tap(&mut e, 0x94, 0x0D);
        assert_eq!(send(&mut e, [0x94, 0x0F, 0x7F]), roll(0, 8.0, true));
        assert_eq!(send(&mut e, [0x94, 0x10, 0x7F]), roll(0, 0.125, true));
        tap(&mut e, 0x95, 0x0D);
        assert_eq!(
            send(&mut e, [0x95, 0x16, 0x7F]),
            act(Action::Reloop { deck: 1 })
        );
    }

    #[test]
    fn only_mappings_mostly_like_the_preset_are_adopted() {
        let preset = builtin_preset("SC LIVE 4").unwrap();
        // Learned by hand for some other layout: one of three is the preset's.
        let mut m = Mapping::empty("SC LIVE 4");
        m.bind(Trigger::note(4, 0x0A), Target::Play { deck: 0 });
        m.bind(Trigger::note(4, 0x09), Target::Play { deck: 1 });
        m.bind(Trigger::note(4, 0x08), Target::Cue { deck: 1 });
        let before = m.clone();
        assert!(!m.adopt(&preset));
        assert_eq!(m, before);
        // Nothing learned yet, or already a preset's: left alone.
        assert!(!Mapping::empty("SC LIVE 4").adopt(&preset));
        let mut flx = pioneer_ddj("DDJ-FLX6", "Pioneer DDJ-FLX6", 4, 2048.0);
        assert!(!flx.adopt(&builtin_preset("DDJ-FLX6").unwrap()));
    }

    #[test]
    fn shift_pad_modes_and_lights_are_saved() {
        let preset = builtin_preset("SC LIVE 4").unwrap();
        let json = serde_json::to_string(&preset).unwrap();
        assert_eq!(serde_json::from_str::<Mapping>(&json).unwrap(), preset);
        // Bindings that need none of them are saved as before.
        let flx = serde_json::to_string(&pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0)).unwrap();
        for field in ["\"shift\"", "\"mode\"", "\"light\""] {
            assert!(!flx.contains(field), "{field}");
        }
    }

    /// Messages recorded from a DDJ-FLX6 with `--midi-monitor`, as it
    /// prints them: seconds, the bytes, what they are.
    fn recorded(lines: &str) -> Vec<(i64, [u8; 3])> {
        lines
            .lines()
            .filter_map(|l| {
                let mut w = l.split_whitespace();
                let t: f64 = w.next()?.parse().ok()?;
                let mut b = [0u8; 3];
                for x in &mut b {
                    *x = u8::from_str_radix(w.next()?, 16).ok()?;
                }
                Some(((t * 1e6) as i64, b))
            })
            .collect()
    }

    /// Play recorded messages into the engine at their times, applying the
    /// actions it sends to the room. Returns the actions.
    fn replay(e: &mut MidiEngine, room: &mut RoomState, lines: &str) -> Vec<Action> {
        let local = LocalParams::default();
        let mut acts = Vec::new();
        for (t, m) in recorded(lines) {
            for o in e.handle_at(&m, t, room, &local) {
                if let MidiOut::Action(a) = o {
                    room.apply(&crate::model::Event {
                        time: t,
                        origin: 1,
                        seq: acts.len() as u64 + 1,
                        action: a.clone(),
                    });
                    acts.push(a);
                }
            }
        }
        acts
    }

    fn flx6() -> MidiEngine {
        MidiEngine::new(builtin_preset("DDJ-FLX6").unwrap(), 0.08)
    }

    /// A room whose effect slots all have depths of their own, to catch one
    /// changing.
    fn fx_room() -> RoomState {
        let mut room = RoomState::default();
        for (d, deck) in room.decks.iter_mut().enumerate() {
            for (i, s) in deck.fx.slots.iter_mut().enumerate() {
                s.depth = 0.1 + 0.2 * i as f32 + 0.05 * d as f32;
            }
        }
        room
    }

    fn depths(room: &RoomState) -> Vec<f32> {
        room.decks
            .iter()
            .flat_map(|d| d.fx.slots.map(|s| s.depth))
            .collect()
    }

    /// Recorded right after a power cycle (the knob in the middle): FX
    /// SELECT pressed six times, once round both units. Each press sends
    /// the knob's position for the new slot, and moving onto the other unit
    /// its channel too.
    const FLX6_FX_SELECT_ROUND: &str = "
  23.153  94 71 7F                 Note 0x71 on (127) · ch 5
  23.153  94 70 00                 Note 0x70 off · ch 5
  23.153  B4 04 40                 CC 0x04 = 64 · ch 5
  23.153  B4 24 00                 CC 0x24 = 0 · ch 5
  24.171  94 72 7F                 Note 0x72 on (127) · ch 5
  24.171  94 71 00                 Note 0x71 off · ch 5
  24.171  B4 06 40                 CC 0x06 = 64 · ch 5
  24.171  B4 26 00                 CC 0x26 = 0 · ch 5
  25.191  95 70 7F                 Note 0x70 on (127) · ch 6
  25.191  94 72 00                 Note 0x72 off · ch 5
  25.191  B5 02 40                 CC 0x02 = 64 · ch 6
  25.191  B5 22 00                 CC 0x22 = 0 · ch 6
  25.191  95 1C 7F                 Note 0x1c on (127) · ch 6
  25.191  95 1E 00                 Note 0x1e off · ch 6
  25.191  95 1D 00                 Note 0x1d off · ch 6
  25.191  95 1F 00                 Note 0x1f off · ch 6
  25.191  95 14 00                 Note 0x14 off · ch 6
  26.219  95 71 7F                 Note 0x71 on (127) · ch 6
  26.219  95 70 00                 Note 0x70 off · ch 6
  26.219  B5 04 40                 CC 0x04 = 64 · ch 6
  26.219  B5 24 00                 CC 0x24 = 0 · ch 6
  27.231  95 72 7F                 Note 0x72 on (127) · ch 6
  27.231  95 71 00                 Note 0x71 off · ch 6
  27.231  B5 06 40                 CC 0x06 = 64 · ch 6
  27.231  B5 26 00                 CC 0x26 = 0 · ch 6
  28.275  94 70 7F                 Note 0x70 on (127) · ch 5
  28.275  95 72 00                 Note 0x72 off · ch 6
  28.275  B4 02 40                 CC 0x02 = 64 · ch 5
  28.275  B4 22 00                 CC 0x22 = 0 · ch 5
  28.275  94 1C 7F                 Note 0x1c on (127) · ch 5
  28.275  94 1E 00                 Note 0x1e off · ch 5
  28.275  94 1D 00                 Note 0x1d off · ch 5
  28.275  94 1F 00                 Note 0x1f off · ch 5
  28.275  94 14 00                 Note 0x14 off · ch 5
";

    #[test]
    fn flx6_fx_select_round_after_power_on_changes_no_depth() {
        let mut e = flx6();
        // Both units power on at CH1.
        assert_eq!((e.fx_unit_deck(0), e.fx_unit_deck(1)), (Some(0), Some(0)));
        for on in [false, true] {
            let mut room = fx_room();
            for d in &mut room.decks {
                for s in &mut d.fx.slots {
                    s.on = on;
                }
            }
            let before = depths(&room);
            let acts = replay(&mut e, &mut room, FLX6_FX_SELECT_ROUND);
            assert_eq!(acts, [], "slots on: {on}");
            assert_eq!(depths(&room), before);
            assert_eq!((e.fx_unit_deck(0), e.fx_unit_deck(1)), (Some(0), Some(0)));
        }
    }

    #[test]
    fn only_running_slots_ask_for_the_fx_knob_to_be_picked_up() {
        // After FX SELECT the knob sits in the middle, away from every
        // slot's depth. A slot that's off takes it as it is, so there's
        // nothing to tell the DJ; one that's on waits for it.
        let mut e = flx6();
        let mut room = fx_room();
        replay(&mut e, &mut room, FLX6_FX_SELECT_ROUND);
        assert!(!e.waiting_for_pickup().is_empty());
        assert_eq!(e.waiting_for_pickup_in(&room), []);
        room.decks[0].fx.slots[1].on = true;
        let mut waiting = e.waiting_for_pickup_in(&room);
        waiting.sort_by_key(|t| t.label());
        // Both units are on deck A, so both units' knobs for its slot 2.
        assert_eq!(
            waiting,
            [
                Target::FxDepth { unit: 0, slot: 1 },
                Target::FxDepth { unit: 1, slot: 1 }
            ]
        );
    }

    #[test]
    fn flx6_ch_select_points_a_unit_at_a_deck_and_mst_parks_it() {
        let mut e = flx6();
        let mut room = fx_room();
        // CH SELECT pressed round on FX1, then on FX2 (as recorded: CH2,
        // CH4, MST, CH3, CH1).
        let fx1 = [
            ("  76.972  94 1D 7F\n  76.972  94 1C 00", Some(1)),
            ("  77.326  94 1F 7F\n  77.326  94 1D 00", Some(3)),
            ("  77.682  94 14 7F\n  77.682  94 1F 00", None),
            ("  78.054  94 1E 7F\n  78.054  94 14 00", Some(2)),
            ("  78.454  94 1C 7F\n  78.454  94 1E 00", Some(0)),
        ];
        for (lines, deck) in fx1 {
            assert_eq!(replay(&mut e, &mut room, lines), []);
            assert_eq!(e.fx_unit_deck(0), deck, "{lines}");
            assert_eq!(e.fx_unit_deck(1), Some(0));
        }
        replay(&mut e, &mut room, "  83.032  95 1D 7F\n  83.032  95 1C 00");
        assert_eq!((e.fx_unit_deck(0), e.fx_unit_deck(1)), (Some(0), Some(1)));
        // A parked unit does nothing.
        replay(&mut e, &mut room, "  83.716  95 14 7F\n  83.716  95 1F 00");
        assert_eq!(e.fx_unit_deck(1), None);
        let before = room;
        let acts = replay(
            &mut e,
            &mut room,
            "  104.486  95 48 7F\n  104.624  95 48 00\n  108.972  95 06 7F\n  89.112  B5 02 01\n  89.112  B5 22 6F",
        );
        assert_eq!(acts, []);
        assert_eq!(room, before);
    }

    #[test]
    fn flx6_fx_knob_and_on_off() {
        let mut e = flx6();
        let mut room = fx_room();
        let knob = |coarse: u16, fine: u16| ((coarse << 7) | fine) as f32 / 16383.0;
        // FX SELECT onto FX2-1 (the knob at zero), then the knob turned up:
        // the slot is off, so it takes the knob as it is.
        replay(
            &mut e,
            &mut room,
            "
  82.140  95 70 7F                 Note 0x70 on (127) · ch 6
  82.140  94 72 00                 Note 0x72 off · ch 5
  82.140  B5 02 00                 CC 0x02 = 0 · ch 6
  82.140  B5 22 00                 CC 0x22 = 0 · ch 6
",
        );
        assert_eq!(depths(&room), depths(&fx_room()));
        let acts = replay(
            &mut e,
            &mut room,
            "
  89.112  B5 02 01                 CC 0x02 = 1 · ch 6
  89.112  B5 22 6F                 CC 0x22 = 111 · ch 6
  89.119  B5 02 04                 CC 0x02 = 4 · ch 6
  89.119  B5 22 02                 CC 0x22 = 2 · ch 6
  89.124  B5 02 06                 CC 0x02 = 6 · ch 6
  89.124  B5 22 14                 CC 0x22 = 20 · ch 6
  89.130  B5 02 07                 CC 0x02 = 7 · ch 6
  89.130  B5 22 62                 CC 0x22 = 98 · ch 6
  89.136  B5 02 08                 CC 0x02 = 8 · ch 6
  89.136  B5 22 6B                 CC 0x22 = 107 · ch 6
",
        );
        assert!(acts.iter().all(|a| matches!(
            a,
            Action::FxDepth {
                deck: 0,
                slot: 0,
                ..
            }
        )));
        assert_eq!(room.decks[0].fx.slots[0].depth, knob(8, 107));
        // FX SELECT onto FX2-2: its position comes twice, and neither
        // reading changes the slot. Then the knob, turned down a little, sets it.
        let acts = replay(
            &mut e,
            &mut room,
            "
  99.204  95 71 7F                 Note 0x71 on (127) · ch 6
  99.204  95 70 00                 Note 0x70 off · ch 6
  99.204  B5 04 40                 CC 0x04 = 64 · ch 6
  99.204  B5 24 00                 CC 0x24 = 0 · ch 6
  99.204  B5 04 40                 CC 0x04 = 64 · ch 6
  99.204  B5 24 00                 CC 0x24 = 0 · ch 6
",
        );
        assert_eq!(acts, []);
        assert_eq!(
            room.decks[0].fx.slots[1].depth,
            fx_room().decks[0].fx.slots[1].depth
        );
        let lines = "
 100.608  B5 04 3F                 CC 0x04 = 63 · ch 6
 100.608  B5 24 08                 CC 0x24 = 8 · ch 6
 100.614  B5 04 3E                 CC 0x04 = 62 · ch 6
 100.614  B5 24 32                 CC 0x24 = 50 · ch 6
";
        replay(&mut e, &mut room, lines);
        assert_eq!(room.decks[0].fx.slots[1].depth, knob(62, 50));
        // Meanwhile the partner turns it right down. ON/OFF: first the
        // depth goes to where the knob is, then the slot comes on.
        room.decks[0].fx.slots[1].depth = 0.0;
        let on = "
 104.486  95 48 7F                 Note 0x48 on (127) · ch 6
 104.624  95 48 00                 Note 0x48 off · ch 6
";
        assert_eq!(
            replay(&mut e, &mut room, on),
            [
                Action::FxDepth {
                    deck: 0,
                    slot: 1,
                    v: knob(62, 50)
                },
                Action::FxOn {
                    deck: 0,
                    slot: 1,
                    on: true
                }
            ]
        );
        // Running, and the partner moves it to 0.8: the knob has to come up
        // past it before it takes over.
        room.decks[0].fx.slots[1].depth = 0.8;
        let turn = |from: u16, to: u16| -> String {
            let mut v = String::new();
            let step = if to > from { 1 } else { -1i32 };
            let mut c = from as i32;
            while c != to as i32 {
                c += step;
                v += &format!("  101.000  B5 04 {c:02X}\n  101.000  B5 24 00\n");
            }
            v
        };
        assert_eq!(replay(&mut e, &mut room, &turn(62, 50)), []);
        // (Within 0.04 of it counts as there, as for every soft takeover.)
        assert_eq!(replay(&mut e, &mut room, &turn(50, 96)), []);
        let acts = replay(&mut e, &mut room, &turn(96, 106));
        assert!(!acts.is_empty());
        assert_eq!(room.decks[0].fx.slots[1].depth, knob(106, 0));
        // Off again (the tail rings out); the knob sets it directly again.
        assert_eq!(
            replay(
                &mut e,
                &mut room,
                "  106.092  95 48 7F\n  106.274  95 48 00"
            ),
            [Action::FxOn {
                deck: 0,
                slot: 1,
                on: false
            }]
        );
        replay(&mut e, &mut room, &turn(106, 20));
        assert_eq!(room.decks[0].fx.slots[1].depth, knob(20, 0));
        // FX SELECT away and back, the knob turned meanwhile for another
        // slot: coming back on a running slot doesn't make it jump either.
        replay(&mut e, &mut room, "  106.5  95 48 7F\n  106.6  95 48 00");
        assert!(room.decks[0].fx.slots[1].on);
        let away_and_back = "
  107.0  95 72 7F
  107.0  95 71 00
  107.0  B5 06 14
  107.0  B5 26 00
  107.2  B5 06 60
  107.2  B5 26 00
  107.4  95 71 7F
  107.4  95 72 00
  107.4  B5 04 60
  107.4  B5 24 00
  107.6  B5 04 61
  107.6  B5 24 00
";
        let before = room.decks[0].fx.slots[1].depth;
        let acts = replay(&mut e, &mut room, away_and_back);
        assert!(
            !acts
                .iter()
                .any(|a| matches!(a, Action::FxDepth { slot: 1, .. })),
            "{acts:?}"
        );
        assert_eq!(room.decks[0].fx.slots[1].depth, before);
    }

    #[test]
    fn flx6_beat_buttons_and_lights() {
        let mut e = flx6();
        let mut room = fx_room();
        let local = LocalParams::default();
        let beat_lights = |e: &MidiEngine, room: &RoomState| e.fx_beat_lights(room);
        assert_eq!(
            beat_lights(&e, &room),
            [((0xB4, 0x64), 0x05), ((0xB5, 0x64), 0x05)],
            "1 beat on deck A, for both units"
        );
        // BEAT ◀ twice, then ▶ twice, on FX2 (deck A).
        let acts = replay(
            &mut e,
            &mut room,
            "
 108.972  95 06 7F                 Note 0x06 on (127) · ch 6
 109.096  95 06 00                 Note 0x06 off · ch 6
 109.296  95 06 7F                 Note 0x06 on (127) · ch 6
 109.412  95 06 00                 Note 0x06 off · ch 6
",
        );
        assert_eq!(
            acts,
            [
                Action::FxBeats { deck: 0, beats: 4 },
                Action::FxBeats { deck: 0, beats: 3 }
            ]
        );
        assert_eq!(beat_lights(&e, &room)[1], ((0xB5, 0x64), 0x04), "½");
        replay(
            &mut e,
            &mut room,
            "
 110.476  95 07 7F                 Note 0x07 on (127) · ch 6
 110.612  95 07 00                 Note 0x07 off · ch 6
 110.812  95 07 7F                 Note 0x07 on (127) · ch 6
 110.944  95 07 00                 Note 0x07 off · ch 6
",
        );
        assert_eq!(room.decks[0].fx.beats, 5);
        // Every BEATS value's light, as measured.
        let want = [
            0x01, 0x02, 0x03, 0x04, 0x21, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A,
        ];
        for (i, &v) in want.iter().enumerate() {
            room.decks[0].fx.beats = i as u8;
            assert_eq!(beat_lights(&e, &room)[0].1, v, "{} beats", FX_BEATS[i]);
        }
        // At the ends, a step further does nothing.
        room.decks[0].fx.beats = 10;
        assert_eq!(
            replay(&mut e, &mut room, "  1.0  95 07 7F\n  1.1  95 07 00"),
            []
        );
        room.decks[0].fx.beats = 0;
        assert_eq!(
            replay(&mut e, &mut room, "  1.0  95 06 7F\n  1.1  95 06 00"),
            []
        );
        // ON/OFF lights: flashing (127) while the slot is on.
        room.decks[0].fx.slots[1].on = true;
        let lights = e.lights(&room, &local);
        let light = |key| lights.iter().find(|l| l.0 == key).map(|l| l.1);
        assert_eq!(light((0x95, 0x48)), Some(0x7F));
        assert_eq!(light((0x94, 0x48)), Some(0x7F));
        assert_eq!(light((0x95, 0x47)), Some(0x00));
        // FX1 on MST: its lights go dark (0x0B lights no BEATS light).
        replay(&mut e, &mut room, "  2.0  94 14 7F");
        let lights = e.lights(&room, &local);
        assert_eq!(lights.iter().find(|l| l.0 == (0x94, 0x48)).unwrap().1, 0);
        assert_eq!(beat_lights(&e, &room)[0], ((0xB4, 0x64), 0x0B));
        assert_eq!(
            e.fx_beat_lights_dark(),
            [((0xB4, 0x64), 0x0B), ((0xB5, 0x64), 0x0B)]
        );
        // FX SELECT onto a slot: its ON/OFF light and its unit's BEATS
        // light go out again.
        assert_eq!(e.take_resend(), []);
        replay(&mut e, &mut room, "  3.0  95 71 7F\n  3.0  95 70 00");
        assert_eq!(e.take_resend(), [(0x95, 0x48), (0xB5, 0x64)]);
        assert_eq!(e.take_resend(), []);
    }

    #[test]
    fn effects_can_be_learned_and_a_one_knob_controller_sets_all_three() {
        let all = Target::all();
        for unit in 0..FX_UNITS as u8 {
            for slot in 0..FX_SLOTS as u8 {
                for t in [
                    Target::FxOn { unit, slot },
                    Target::FxDepth { unit, slot },
                    Target::FxNext { unit, slot },
                    Target::FxFocus { unit, slot },
                ] {
                    assert_eq!(all.iter().filter(|&&x| x == t).count(), 1, "{t:?}");
                }
            }
            assert!(all.contains(&Target::FxAssignNone { unit }));
            assert!(all.contains(&Target::FxDepthAll { unit }));
        }
        assert!(Target::FxDepth { unit: 0, slot: 0 }.is_absolute());
        assert_eq!(Target::FxOn { unit: 0, slot: 0 }.deck(), None);
        // Learned onto a plain controller: unit 2 works deck B.
        let mut e = engine();
        let (mut room, local) = (fx_room(), LocalParams::default());
        e.learn = Some(Target::FxDepthAll { unit: 1 });
        e.handle(&[0xB0, 0x30, 0x10], &room, &local);
        e.learn = Some(Target::FxNext { unit: 1, slot: 2 });
        e.handle(&[0x90, 0x31, 0x7F], &room, &local);
        e.learn = Some(Target::FxOn { unit: 1, slot: 2 });
        e.handle(&[0x90, 0x32, 0x7F], &room, &local);
        room.decks[1].fx.slots[2].on = true;
        // Off slots take the knob; the one that's on waits for it.
        let outs = follow(&mut e, &mut room, &[[0xB0, 0x30, 0x40]]);
        assert_eq!(outs.len(), 2);
        let d = room.decks[1].fx.slots.map(|s| s.depth);
        assert_eq!((d[0], d[1]), (64.0 / 127.0, 64.0 / 127.0));
        assert_eq!(d[2], fx_room().decks[1].fx.slots[2].depth);
        assert_eq!(
            follow(&mut e, &mut room, &[[0x90, 0x31, 0x7F]]),
            [MidiOut::Action(Action::FxSelect {
                deck: 1,
                slot: 2,
                kind: crate::model::FxKind::Echo
            })]
        );
        assert_eq!(
            follow(&mut e, &mut room, &[[0x90, 0x32, 0x7F]]),
            [MidiOut::Action(Action::FxOn {
                deck: 1,
                slot: 2,
                on: false
            })]
        );
    }

    #[test]
    fn saved_v12_flx6_mappings_gain_the_beat_fx_strip() {
        let preset = builtin_preset("DDJ-FLX6").unwrap();
        let fx = |t: &Target| t.deck().is_none() && t.label().starts_with("FX unit");
        assert_eq!(preset.bindings.iter().filter(|b| fx(&b.target)).count(), 32);
        // As saved by preset 12: no strip, no BEATS lights, no unit decks.
        let mut json: serde_json::Value = serde_json::to_value(&preset).unwrap();
        let o = json.as_object_mut().unwrap();
        o.remove("fx_unit_decks");
        o.remove("fx_beat_lights");
        o.insert("preset_version".into(), 12.into());
        let mut saved: Mapping = serde_json::from_value(json).unwrap();
        saved.bindings.retain(|b| !fx(&b.target));
        saved.jog_ticks_per_rev = Some(6387.0);
        assert_eq!(saved.fx_unit_decks, [Some(0), Some(1), Some(2), Some(3)]);
        assert_eq!(saved.upgrade_from(&preset), 32 + 2);
        assert_eq!(saved.bindings.len(), preset.bindings.len());
        assert_eq!(saved.fx_beat_lights, preset.fx_beat_lights);
        assert_eq!(saved.fx_unit_decks, [Some(0), Some(0), Some(2), Some(3)]);
        assert_eq!(saved.jog_ticks_per_rev, Some(6387.0));
        assert_eq!(saved.upgrade_from(&preset), 0, "only once");
        // And it works.
        let mut e = MidiEngine::new(saved, 0.08);
        let mut room = fx_room();
        assert_eq!(
            replay(&mut e, &mut room, "  1.0  94 47 7F"),
            [Action::FxOn {
                deck: 0,
                slot: 0,
                on: true
            }]
        );
        // The FLX4 and DDJ-400 share the family's layout, but their strips
        // send other messages: they get none of it.
        let small = builtin_preset("DDJ-FLX4").unwrap();
        assert!(!small.bindings.iter().any(|b| fx(&b.target)));
        assert!(small.fx_beat_lights.is_empty());
    }

    /// A controller with two decks' worth of controls for four decks, whose
    /// controls send the same whichever deck a side is on: SYNC and the
    /// tempo fader of the left side, for decks A and C.
    fn layered(deck_buttons: [(u8, u8); 2]) -> MidiEngine {
        let mut m = Mapping::empty("test");
        for deck in [0u8, 2] {
            m.bind(Trigger::note(4, 0x08), Target::Sync { deck })
                .layered = true;
            m.bind(Trigger::cc(4, 0x1F), Target::Tempo { deck }).layered = true;
        }
        // The right side has decks of its own.
        m.bind(Trigger::note(5, 0x08), Target::Sync { deck: 1 });
        for (deck, note) in deck_buttons {
            m.bind(Trigger::note(4, note), Target::DeckSelect { deck });
        }
        MidiEngine::new(m, 0.08)
    }

    #[test]
    fn a_side_that_shares_its_controls_follows_its_deck_buttons() {
        // DECK 1 and DECK 3 buttons.
        let mut e = layered([(0, 0x30), (2, 0x31)]);
        let sync = |deck| act(Action::Sync { deck, on: true });
        assert_eq!(e.side_deck(0), 0);
        assert_eq!(send(&mut e, [0x94, 0x08, 0x7F]), sync(0));
        assert_eq!(tap(&mut e, 0x94, 0x31), []);
        assert_eq!((e.side_deck(0), e.side_deck(1)), (2, 1));
        assert_eq!(send(&mut e, [0x94, 0x08, 0x7F]), sync(2));
        // The other side's controls don't take turns.
        assert_eq!(send(&mut e, [0x95, 0x08, 0x7F]), sync(1));
        // The lights are deck C's, and DECK 3 is lit.
        let mut room = RoomState::default();
        room.decks[2].synced = true;
        let lit = |e: &MidiEngine, room: &RoomState, key: (u8, u8)| {
            e.leds(room, &LocalParams::default())
                .into_iter()
                .find(|(k, _)| *k == key)
                .map(|(_, on)| on)
        };
        assert_eq!(lit(&e, &room, (0x94, 0x08)), Some(true));
        assert_eq!(lit(&e, &room, (0x94, 0x31)), Some(true));
        assert_eq!(lit(&e, &room, (0x94, 0x30)), Some(false));
        tap(&mut e, 0x94, 0x30);
        assert_eq!(e.side_deck(0), 0);
        assert_eq!(lit(&e, &room, (0x94, 0x08)), Some(false));
        assert_eq!(send(&mut e, [0x94, 0x08, 0x7F]), sync(0));
        // The fader moved while it was deck C's: its first reading back on
        // deck A only says where it is.
        tap(&mut e, 0x94, 0x31);
        let (room, local) = (RoomState::default(), LocalParams::default());
        assert_eq!(e.handle(&[0xB4, 0x1F, 100], &room, &local), []);
        let out = e.handle(&[0xB4, 0x1F, 101], &room, &local);
        assert!(
            matches!(out[..], [MidiOut::Action(Action::Tempo { deck: 2, .. })]),
            "{out:?}"
        );
        // Another mapping starts on decks A and B again.
        let mapping = e.mapping.clone();
        e.load(mapping);
        assert_eq!(e.side_deck(0), 0);
    }

    #[test]
    fn one_deck_button_switches_back_and_forth() {
        let mut e = layered([(0, 0x1F), (2, 0x1F)]);
        let sync = |deck| act(Action::Sync { deck, on: true });
        let lit = |e: &MidiEngine| {
            e.leds(&RoomState::default(), &LocalParams::default())
                .into_iter()
                .find(|(k, _)| *k == (0x94, 0x1F))
                .map(|(_, on)| on)
        };
        assert_eq!(lit(&e), Some(false));
        assert_eq!(tap(&mut e, 0x94, 0x1F), []);
        assert_eq!(e.side_deck(0), 2);
        assert_eq!(lit(&e), Some(true), "lit while the side is on deck C");
        assert_eq!(send(&mut e, [0x94, 0x08, 0x7F]), sync(2));
        tap(&mut e, 0x94, 0x1F);
        assert_eq!(e.side_deck(0), 0);
        assert_eq!(lit(&e), Some(false));
        assert_eq!(send(&mut e, [0x94, 0x08, 0x7F]), sync(0));
    }

    #[test]
    fn controls_that_take_turns_are_saved_and_named_as_such() {
        let e = layered([(0, 0x1F), (2, 0x1F)]);
        let b = e.mapping.binding_for(Target::Sync { deck: 2 }).unwrap();
        assert_eq!(b.label(), "Note 0x08 · ch 5 (side on deck C)");
        let json = serde_json::to_string(&e.mapping).unwrap();
        assert_eq!(json.matches("\"layered\":true").count(), 4);
        assert!(
            !json.contains("\"layered\":false"),
            "left out when it isn't"
        );
        let back: Mapping = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e.mapping);
        // The two decks' bindings don't stand in each other's way, but
        // another control on the same note does.
        let [a, c] = [0u8, 2].map(|deck| back.binding_for(Target::Sync { deck }).unwrap().clone());
        assert!(!a.overlaps(&c));
        let mut cue = a.clone();
        cue.target = Target::Cue { deck: 2 };
        assert!(cue.overlaps(&c) && !cue.overlaps(&a));
        // A DDJ-FLX6 keeps track of its decks itself: Tandem leaves its
        // DECK lights alone.
        let flx6 = flx6();
        assert!(!flx6.mapping.bindings.iter().any(|b| b.layered));
        assert!(!flx6
            .leds(&RoomState::default(), &LocalParams::default())
            .iter()
            .any(|(k, _)| k.1 == 0x3C));
    }

    #[test]
    fn learning_a_control_that_takes_turns_keeps_it_taking_turns() {
        let mut e = layered([(0, 0x30), (2, 0x31)]);
        // Deck C's SYNC, learned again from the list.
        e.learn = Some(Target::Sync { deck: 2 });
        let out = send(&mut e, [0x94, 0x08, 0x7F]);
        assert!(
            matches!(&out[..], [MidiOut::Learned(b)] if b.layered),
            "{out:?}"
        );
        // A control of its own isn't.
        e.learn = Some(Target::Play { deck: 2 });
        let out = send(&mut e, [0x92, 0x0B, 0x7F]);
        assert!(
            matches!(&out[..], [MidiOut::Learned(b)] if !b.layered),
            "{out:?}"
        );
    }

    #[test]
    fn the_mapping_screen_lists_every_control_once_group_by_group() {
        let all = Target::all();
        let unique: HashSet<Target> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len());
        for d in 0..DECKS as u8 {
            for slot in 0..8 {
                assert!(all.contains(&Target::HotCueClear { deck: d, slot }));
            }
        }
        // A group's controls sit together.
        let mut seen = Vec::new();
        for t in &all {
            if seen.last() != Some(&t.group()) {
                assert!(!seen.contains(&t.group()), "{:?} comes back", t.group());
                seen.push(t.group());
            }
        }
        assert_eq!(seen[0], (Part::Shift, None));
        assert_eq!(seen[1], (Part::Jog, Some(0)));
        assert_eq!(Part::Jog.title(Some(2)), "Deck C · Jog wheel");
        assert_eq!(Part::Fx.title(Some(1)), "FX unit 2");
    }
}
