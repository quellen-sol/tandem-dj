//! MIDI controller mapping ("MIDI learn"), built-in presets, soft takeover
//! and LED feedback.
//!
//! Each DJ maps their own controller; mappings are local files and never
//! shared. Because two people control one mixer, absolute controls (faders,
//! knobs) use *soft takeover*: once the room value has been changed, your
//! hardware control only takes effect after it reaches that value, so it
//! never jumps over what your partner set.

use crate::model::{Action, RoomState, DECKS};
use crate::render::LocalParams;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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
    Play { deck: u8 },
    Cue { deck: u8 },
    HotCue { deck: u8, slot: u8 },
    HotCueClear { deck: u8, slot: u8 },
    Tempo { deck: u8 },
    Volume { deck: u8 },
    Eq { deck: u8, band: u8 },
    Trim { deck: u8 },
    Filter { deck: u8 },
    Pfl { deck: u8 },
    Jog { deck: u8 },
    JogTouch { deck: u8 },
    BendDown { deck: u8 },
    BendUp { deck: u8 },
    Load { deck: u8 },
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
            Target::HotCue { deck: d, slot } => format!("Deck {} · Hot cue {}", deck(d), slot + 1),
            Target::HotCueClear { deck: d, slot } => {
                format!("Deck {} · Clear hot cue {}", deck(d), slot + 1)
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
            for slot in 0..8 {
                v.push(Target::HotCue { deck: d, slot });
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
    /// Nudge strength when turning the jog without touching the platter.
    #[serde(default = "one")]
    pub jog_sensitivity: f32,
    /// Scratch distance per jog tick, relative to 720 ticks per 33⅓ rpm turn.
    #[serde(default = "one")]
    pub scratch_sensitivity: f32,
    /// Touching the jog platter holds the record (vinyl mode).
    #[serde(default = "yes")]
    pub vinyl: bool,
    /// Name of the built-in preset this mapping started from, if any.
    #[serde(default)]
    pub preset: Option<String>,
}

impl Mapping {
    pub fn empty(device: &str) -> Self {
        Self {
            device: device.to_string(),
            bindings: Vec::new(),
            jog_sensitivity: 1.0,
            scratch_sensitivity: 1.0,
            vinyl: true,
            preset: None,
        }
    }

    pub fn binding_for(&self, target: Target) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.target == target)
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
}

const BEND: f64 = 0.04;
/// Nudge per jog tick when not scratching (seconds of audio).
const JOG_SECONDS_PER_TICK: f64 = 0.0008;
/// Scratch per tick: one 33⅓ rpm turn (1.8 s) over 720 ticks.
const SCRATCH_SECONDS_PER_TICK: f64 = 60.0 / (100.0 / 3.0) / 720.0;

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
        }
    }

    /// Targets whose hardware control hasn't "picked up" the room value yet.
    pub fn waiting_for_pickup(&self) -> Vec<Target> {
        self.takeover
            .iter()
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
                Target::HotCue { deck, slot } if down => {
                    out.push(MidiOut::Action(Action::HotCue { deck, slot }))
                }
                Target::HotCueClear { deck, slot } if down => {
                    out.push(MidiOut::Action(Action::ClearHotCue { deck, slot }))
                }
                Target::Pfl { deck } if down => out.push(MidiOut::TogglePfl(deck)),
                Target::JogTouch { deck } => {
                    let scratching = room.decks[deck as usize].scratching;
                    if (down && self.mapping.vinyl && !scratching) || (!down && scratching) {
                        out.push(MidiOut::Action(Action::JogTouch { deck, down }));
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
                            let delta = if room.decks[deck as usize].scratching {
                                ticks as f64
                                    * SCRATCH_SECONDS_PER_TICK
                                    * self.mapping.scratch_sensitivity as f64
                            } else {
                                ticks as f64
                                    * JOG_SECONDS_PER_TICK
                                    * self.mapping.jog_sensitivity as f64
                            };
                            out.push(MidiOut::Action(Action::Nudge { deck, delta }));
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
                Target::Pfl { deck } => local.pfl[deck as usize],
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
}

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
        Some(pioneer_ddj(port_name, "Pioneer DDJ-FLX6", 4))
    } else if n.contains("DDJ-FLX4") || n.contains("DDJ-400") {
        Some(pioneer_ddj(port_name, "Pioneer DDJ-FLX4 / DDJ-400", 2))
    } else {
        None
    }
}

/// Pioneer DDJ layout (DDJ-400 / FLX4 / FLX6 generation). Deck N uses MIDI
/// channel N; the mixer and browser use channel 7; performance pads use
/// channel 8 (deck 1), 10 (deck 2), 12 (deck 3), 14 (deck 4), with SHIFT on
/// the next channel up. On 4-deck controllers decks 1 and 3 drive Tandem's
/// deck A and decks 2 and 4 drive deck B, whichever the DECK button selects.
fn pioneer_ddj(device: &str, name: &str, hw_decks: u8) -> Mapping {
    let mut m = Mapping::empty(device);
    m.preset = Some(name.to_string());
    for hw in 0..hw_decks {
        let ch = hw;
        let deck = hw % 2;
        m.bind(Trigger::note(ch, 0x0B), Target::Play { deck });
        m.bind(Trigger::note(ch, 0x0C), Target::Cue { deck });
        m.bind(Trigger::note(ch, 0x54), Target::Pfl { deck });
        m.bind(Trigger::note(ch, 0x36), Target::JogTouch { deck });
        for cc in [0x21, 0x22, 0x23] {
            m.bind(Trigger::cc(ch, cc), Target::Jog { deck });
        }
        m.bind(Trigger::cc(ch, 0x00), Target::Tempo { deck });
        m.bind(Trigger::cc(ch, 0x13), Target::Volume { deck });
        m.bind(Trigger::cc(ch, 0x04), Target::Trim { deck });
        m.bind(Trigger::cc(ch, 0x07), Target::Eq { deck, band: 2 });
        m.bind(Trigger::cc(ch, 0x0B), Target::Eq { deck, band: 1 });
        m.bind(Trigger::cc(ch, 0x0F), Target::Eq { deck, band: 0 });
        m.bind(Trigger::cc(6, 0x17 + hw), Target::Filter { deck });
        m.bind(Trigger::note(6, 0x46 + hw), Target::Load { deck });
        let pads = 7 + 2 * hw;
        for slot in 0..8u8 {
            m.bind(Trigger::note(pads, slot), Target::HotCue { deck, slot });
            m.bind(
                Trigger::note(pads + 1, slot),
                Target::HotCueClear { deck, slot },
            );
        }
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
        let out = e.handle(&[0xb0, 0x21, 66], &room, &local);
        assert!(matches!(out[..], [MidiOut::Action(Action::Nudge { .. })]));
    }

    #[test]
    fn jog_scratches_while_touched() {
        let mut e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4), 0.08);
        let local = LocalParams::default();
        let mut room = RoomState::default();
        room.decks[0].track = Some(crate::model::TrackId([1; 32]));
        let out = e.handle(&[0x90, 0x36, 127], &room, &local);
        assert_eq!(
            out,
            vec![MidiOut::Action(Action::JogTouch {
                deck: 0,
                down: true
            })]
        );
        room.decks[0].scratching = true;
        let out = e.handle(&[0xb0, 0x22, 64 + 10], &room, &local);
        match &out[..] {
            [MidiOut::Action(Action::Nudge { delta, .. })] => {
                assert!((delta - 10.0 * SCRATCH_SECONDS_PER_TICK).abs() < 1e-12)
            }
            other => panic!("{other:?}"),
        }
        // Deck 3 on the FLX6 drives deck A too.
        let out = e.handle(&[0x82, 0x36, 0], &room, &local);
        assert_eq!(
            out,
            vec![MidiOut::Action(Action::JogTouch {
                deck: 0,
                down: false
            })]
        );
    }

    #[test]
    fn leds_follow_the_room() {
        let e = MidiEngine::new(pioneer_ddj("DDJ-FLX6", "test", 4), 0.08);
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
    fn port_name_keys() {
        assert_eq!(mapping_key("2- DDJ-FLX4"), "DDJ-FLX4");
        assert_eq!(mapping_key("DDJ-400"), "DDJ-400");
        assert_eq!(mapping_key("Mixtrack Pro FX"), "Mixtrack_Pro_FX");
        assert!(builtin_preset("DDJ-FLX6").is_some());
        assert!(builtin_preset("Mixtrack Pro FX").is_none());
    }
}
