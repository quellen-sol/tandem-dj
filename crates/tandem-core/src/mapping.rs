//! MIDI controller mapping ("MIDI learn"), built-in presets, soft takeover
//! and LED feedback.
//!
//! Each DJ maps their own controller; mappings are local files and never
//! shared. Because two people control one mixer, absolute controls (faders,
//! knobs) use *soft takeover*: once the room value has been changed, your
//! hardware control only takes effect after it reaches that value, so it
//! never jumps over what your partner set. The tempo fader is the exception:
//! SYNC moves tempos without moving the fader, so when the two disagree the
//! fader works *relatively* (see [`MidiEngine::tempo_fader`]).

use crate::meter::{to_db, Levels};
use crate::model::{Action, RoomState, BEAT_LOOPS, DECKS};
use crate::render::LocalParams;
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
    Tempo {
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
    Browse,
}

impl Target {
    pub fn label(&self) -> String {
        let deck = |d: u8| if d == 0 { "A" } else { "B" };
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
            Target::Tempo { deck: d } => format!("Deck {} · Tempo fader", deck(d)),
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
            Target::Browse => "Library browse encoder".into(),
        }
    }

    pub fn kind(&self) -> TargetKind {
        match self {
            Target::Tempo { .. }
            | Target::Volume { .. }
            | Target::Eq { .. }
            | Target::Trim { .. }
            | Target::Filter { .. }
            | Target::Crossfader
            | Target::CueMix => TargetKind::Absolute,
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

    /// Every mappable target, in the order the mapping screen shows them.
    pub fn all() -> Vec<Target> {
        let mut v = Vec::new();
        for d in 0..DECKS as u8 {
            v.push(Target::Play { deck: d });
            v.push(Target::Cue { deck: d });
            v.push(Target::BackToStart { deck: d });
            v.push(Target::Sync { deck: d });
            v.push(Target::SyncOff { deck: d });
            for slot in 0..8 {
                v.push(Target::HotCue { deck: d, slot });
            }
            v.push(Target::LoopIn { deck: d });
            v.push(Target::LoopOut { deck: d });
            v.push(Target::Reloop { deck: d });
            v.push(Target::LoopHalve { deck: d });
            v.push(Target::LoopDouble { deck: d });
            for size in 0..BEAT_LOOPS.len() as u8 {
                v.push(Target::BeatLoop { deck: d, size });
            }
            v.push(Target::Load { deck: d });
            v.push(Target::JogTouch { deck: d });
            v.push(Target::Jog { deck: d });
            v.push(Target::BendDown { deck: d });
            v.push(Target::BendUp { deck: d });
            v.push(Target::Tempo { deck: d });
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
        v.push(Target::Browse);
        v
    }
}

/// Beats in beat loop `size` (an index into [`BEAT_LOOPS`]).
pub fn beat_loop_beats(size: u8) -> f64 {
    BEAT_LOOPS[(size as usize).min(BEAT_LOOPS.len() - 1)]
}

/// "¼", "½", "1", "16"… for a loop length in beats.
pub fn beats_label(beats: f64) -> String {
    match beats {
        b if (b - 0.25).abs() < 1e-9 => "¼".into(),
        b if (b - 0.5).abs() < 1e-9 => "½".into(),
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
}

/// Bump when built-in presets gain controls; saved mappings pick them up.
pub const PRESET_VERSION: u32 = 6;

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
    /// to something else) and its level meters, keeping calibration and
    /// customisations. Returns how many controls were added.
    pub fn upgrade_from(&mut self, preset: &Mapping) -> usize {
        if self.preset.is_none() || self.preset_version >= preset.preset_version {
            return 0;
        }
        let mut added = 0;
        for b in &preset.bindings {
            let trigger_used = self.bindings.iter().any(|x| x.trigger == b.trigger);
            if !trigger_used {
                self.bindings.push(b.clone());
                added += 1;
            }
        }
        if self.meters.is_empty() {
            self.meters = preset.meters.clone();
            added += self.meters.len();
        }
        self.preset_version = preset.preset_version;
        added
    }

    fn bind(&mut self, trigger: Trigger, target: Target) -> &mut Binding {
        self.bindings.push(Binding {
            trigger,
            target,
            invert: false,
            rel: RelMode::Offset64,
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
    Browse(i32),
    LoadSelected(u8),
    Learned(Binding),
}

#[derive(Clone, Copy, Debug)]
struct Takeover {
    engaged: bool,
    last_sent: Option<f32>,
    last_hw: Option<f32>,
}

pub struct MidiEngine {
    pub mapping: Mapping,
    /// Set by the mapping screen; the next matching control you touch gets bound.
    pub learn: Option<Target>,
    pub tempo_range: f64,
    pub last_message: Option<String>,
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
            mapping,
            learn: None,
            tempo_range,
            last_message: None,
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
        }
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

        if let Some(target) = self.learn {
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
            let binding = Binding {
                trigger,
                target,
                invert: false,
                rel,
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
        let targets: Vec<Target> = self
            .mapping
            .bindings
            .iter()
            .filter(|b| b.trigger == trig)
            .map(|b| b.target)
            .collect();
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
                Target::HotCue { deck, slot } => out.push(MidiOut::Action(if down {
                    Action::HotCue { deck, slot }
                } else {
                    Action::HotCueUp { deck, slot }
                })),
                Target::HotCueClear { deck, slot } if down => {
                    out.push(MidiOut::Action(Action::ClearHotCue { deck, slot }))
                }
                Target::LoopIn { deck } if down => {
                    out.push(MidiOut::Action(Action::LoopIn { deck }))
                }
                Target::LoopOut { deck } if down => {
                    out.push(MidiOut::Action(Action::LoopOut { deck }))
                }
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
                _ => {}
            }
        }
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
        let bindings: Vec<Binding> = self
            .mapping
            .bindings
            .iter()
            .filter(|b| b.trigger == trig)
            .cloned()
            .collect();
        for b in bindings {
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
                    // A button sent as CC (127 press / 0 release).
                    self.button(trig, raw7.map(|r| r >= 64).unwrap_or(v >= 0.5), room, out);
                }
                TargetKind::Absolute => {
                    let v = if b.invert { 1.0 - v } else { v };
                    if let Target::Tempo { deck } = b.target {
                        if let Some(tempo) = self.tempo_fader(deck, v, room) {
                            out.push(MidiOut::Action(Action::Tempo { deck, tempo }));
                        }
                        continue;
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
    ///   carried it, or your partner moved it), moving the fader changes the
    ///   tempo by exactly as much as the fader moved, from wherever the deck
    ///   is. SHIFT + SYNC puts the deck back on the fader.
    ///
    /// A deck nobody has touched (tempo 0%) takes the fader's position
    /// straight away, like the other controls.
    fn tempo_fader(&mut self, deck: u8, v: f32, room: &RoomState) -> Option<f64> {
        /// Closer than this (0.05%) counts as "on the fader".
        const MATCH: f64 = 0.0005;
        let range = self.tempo_range;
        let at = |v: f32| 1.0 + (v as f64 - 0.5) * 2.0 * range;
        let current = room.decks.get(deck as usize)?.tempo;
        let t = self
            .takeover
            .entry(Target::Tempo { deck })
            .or_insert(Takeover {
                engaged: false,
                last_sent: None,
                last_hw: None,
            });
        let prev = t.last_hw.replace(v);
        let hw = at(v);
        let next = match prev {
            _ if (hw - current).abs() < MATCH => hw,
            None if t.last_sent.is_none() && (current - 1.0).abs() < 1e-9 => hw,
            // First reading from this fader: we only learn where it is.
            None => return None,
            Some(p) => {
                let next = (current + hw - at(p)).clamp(0.5, 1.5);
                // Rejoined the fader (e.g. it hit the deck's tempo after
                // someone else moved it): snap so rounding can't build up.
                if (next - hw).abs() < MATCH {
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
            _ => 0.0,
        }
    }

    /// The value nobody has changed yet, so the hardware can take it directly.
    fn pristine(target: Target, current: f32) -> bool {
        let default = match target {
            Target::Volume { .. } => 1.0,
            Target::CueMix => 0.0,
            _ => 0.5,
        };
        (current - default).abs() < 1e-6
    }

    fn pickup(&mut self, target: Target, v: f32, current: f32) -> bool {
        let t = self.takeover.entry(target).or_insert(Takeover {
            engaged: false,
            last_sent: None,
            last_hw: None,
        });
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
        let mut out: Vec<((u8, u8), bool)> = Vec::new();
        for b in &self.mapping.bindings {
            if b.trigger.kind != MidiKind::Note {
                continue;
            }
            let on = match b.target {
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
                Target::LoopOut { deck } | Target::Reloop { deck } => {
                    room.decks[deck as usize].looping
                }
                // The pad of the beat loop that's playing.
                Target::BeatLoop { deck, size } => {
                    let d = &room.decks[deck as usize];
                    d.looping
                        && d.loop_beats
                            .is_some_and(|b| (b - beat_loop_beats(size)).abs() < 1e-9)
                }
                _ => continue,
            };
            let key = (0x90 | b.trigger.channel, b.trigger.number);
            match out.iter_mut().find(|(k, _)| *k == key) {
                Some(e) => e.1 |= on,
                None => out.push((key, on)),
            }
        }
        out
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
    if n.contains("DDJ-FLX6") {
        // Large jogs: same resolution as the DDJ-SX family until calibrated.
        Some(pioneer_ddj(port_name, "Pioneer DDJ-FLX6", 4, 2048.0))
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

/// Pioneer DDJ layout (DDJ-400 / FLX4 / FLX6 generation). Deck N uses MIDI
/// channel N; the mixer and browser use channel 7; performance pads use
/// channel 8 (deck 1), 10 (deck 2), 12 (deck 3), 14 (deck 4), with SHIFT on
/// the next channel up. On 4-deck controllers decks 1 and 3 drive Tandem's
/// deck A and decks 2 and 4 drive deck B, whichever the DECK button selects.
///
/// The pads send a different note range in each pad mode: HOT CUE 0x00,
/// PAD FX1 0x10, BEAT JUMP 0x20, SAMPLER 0x30, and with SHIFT + the mode
/// button KEYBOARD 0x40, PAD FX2 0x50, BEAT LOOP 0x60, KEY SHIFT 0x70
/// (recorded from a DDJ-FLX6).
fn pioneer_ddj(device: &str, name: &str, hw_decks: u8, ticks_per_rev: f32) -> Mapping {
    let mut m = Mapping::empty(device);
    m.preset = Some(name.to_string());
    m.preset_version = PRESET_VERSION;
    m.jog_ticks_per_rev = Some(ticks_per_rev);
    for hw in 0..hw_decks {
        let ch = hw;
        let deck = hw % 2;
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
        m.bind(Trigger::note(6, 0x46 + hw), Target::Load { deck });
        // Loop section: IN, OUT, RELOOP/EXIT; SHIFT + IN is a 4-beat loop;
        // CUE/LOOP CALL ◀ / ▶ halve and double the loop.
        m.bind(Trigger::note(ch, 0x10), Target::LoopIn { deck });
        m.bind(Trigger::note(ch, 0x11), Target::LoopOut { deck });
        m.bind(Trigger::note(ch, 0x4D), Target::Reloop { deck });
        m.bind(Trigger::note(ch, 0x4C), Target::BeatLoop { deck, size: 4 });
        m.bind(Trigger::note(ch, 0x51), Target::LoopHalve { deck });
        m.bind(Trigger::note(ch, 0x53), Target::LoopDouble { deck });
        // The mixer has its own strip per channel: only channels 1 and 2
        // are Tandem's two decks. Channels 3/4 are left alone.
        if hw < 2 {
            m.bind(Trigger::note(ch, 0x54), Target::Pfl { deck });
            m.bind(Trigger::cc(ch, 0x13), Target::Volume { deck });
            m.bind(Trigger::cc(ch, 0x04), Target::Trim { deck });
            m.bind(Trigger::cc(ch, 0x07), Target::Eq { deck, band: 2 });
            m.bind(Trigger::cc(ch, 0x0B), Target::Eq { deck, band: 1 });
            m.bind(Trigger::cc(ch, 0x0F), Target::Eq { deck, band: 0 });
            m.bind(Trigger::cc(6, 0x17 + hw), Target::Filter { deck });
        }
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
        }
    }
    // Channel level meters: channels 1 and 2 show decks A and B.
    for deck in 0..2u8 {
        m.meters.push(MeterLight {
            channel: deck,
            cc: 0x02,
            deck,
        });
    }
    m.bind(Trigger::cc(6, 0x1F), Target::Crossfader);
    m.bind(Trigger::cc(6, 0x0C), Target::CueMix);
    m.bind(Trigger::cc(6, 0x40), Target::Browse).rel = RelMode::TwosComplement;
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
        // Deck 3 on the FLX6 drives deck A too. Let go a while after the
        // hand stopped: no spin.
        let out = e.handle_at(&[0x82, 0x36, 0], 100_000, &room, &local);
        assert_eq!(
            out,
            vec![MidiOut::Action(Action::JogTouch {
                deck: 0,
                down: false,
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
    fn flx6_mixer_channels_3_and_4_are_not_decks() {
        let m = pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0);
        let bound = |t: Trigger| m.bindings.iter().any(|b| b.trigger == t);
        assert!(bound(Trigger::note(0, 0x54)));
        assert!(
            !bound(Trigger::note(2, 0x54)),
            "channel 3 headphone cue must not drive deck A"
        );
        assert!(
            !bound(Trigger::cc(2, 0x13)),
            "channel 3 fader must not drive deck A"
        );
        assert!(
            bound(Trigger::note(2, 0x0B)),
            "deck 3 play still drives deck A"
        );
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
        // SHIFT + CUE (deck 2, and deck 4 on the same Tandem deck): back to the start.
        let start = || act(Action::Seek { deck: 1, pos: 0.0 });
        assert_eq!(e.handle(&[0x91, 0x48, 127], &room, &local), start());
        assert!(e.handle(&[0x91, 0x48, 0], &room, &local).is_empty());
        assert_eq!(e.handle(&[0x93, 0x48, 127], &room, &local), start());
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
        assert_eq!(press([0x90, 0x10, 0x7F]), act(Action::LoopIn { deck: 0 }));
        assert_eq!(press([0x90, 0x11, 0x7F]), act(Action::LoopOut { deck: 0 }));
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
        assert_eq!(press([0x91, 0x10, 0x7F]), act(Action::LoopIn { deck: 1 }));
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
        // Left alone: SHIFT + OUT, SHIFT + RELOOP/EXIT, SHIFT + ◀/▶, BEAT JUMP pads.
        for msg in [
            [0x90, 0x77, 0x7F],
            [0x90, 0x50, 0x7F],
            [0x90, 0x61, 0x7F],
            [0x90, 0x62, 0x7F],
            [0x97, 0x20, 0x7F],
        ] {
            assert!(press(msg).is_empty(), "{msg:02X?}");
        }
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
        assert_eq!(saved.upgrade_from(&preset), 2);
        assert_eq!(saved.meters, preset.meters);
        assert_eq!(saved.bindings.len(), preset.bindings.len());
    }

    #[test]
    fn level_meters_follow_the_channels() {
        let e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4, 2048.0), 0.08);
        let db = |db: f32| 10f32.powf(db / 20.0);
        let levels = Levels {
            channels: [db(-6.0), 0.0],
            master: [db(-3.0); 2],
        };
        let out = e.meters(&levels);
        // Mixer channels 1 and 2 only: channels 3 and 4 aren't Tandem's.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], ((0xB0, 0x02), pioneer_meter_value(db(-6.0))));
        assert_eq!(out[1], ((0xB1, 0x02), 0));
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
        for k in [
            (0x90, 0x10),
            (0x90, 0x11),
            (0x90, 0x4D),
            (0x97, 0x64),
            (0x9B, 0x64),
        ] {
            assert_eq!(on(&room, k), Some(true), "{k:02X?}");
        }
        assert_eq!(on(&room, (0x97, 0x63)), Some(false));
        assert_eq!(on(&room, (0x99, 0x64)), Some(false), "deck B");
        // Left the loop: all off.
        room.decks[0].looping = false;
        for k in [(0x90, 0x10), (0x90, 0x11), (0x90, 0x4D), (0x97, 0x64)] {
            assert_eq!(on(&room, k), Some(false), "{k:02X?}");
        }
    }

    #[test]
    fn beat_loop_labels() {
        let labels: Vec<String> = BEAT_LOOPS.iter().map(|b| beats_label(*b)).collect();
        assert_eq!(labels, ["¼", "½", "1", "2", "4", "8", "16", "32"]);
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
    }
}
