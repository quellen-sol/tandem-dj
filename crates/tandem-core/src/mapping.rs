//! MIDI controller mapping ("MIDI learn") and soft takeover.
//!
//! Each DJ maps their own controller; mappings are local files and never
//! shared. Because two people control one mixer, absolute controls (faders,
//! knobs) use *soft takeover*: your hardware fader only takes effect once it
//! reaches the value it's currently at in the room, so it never jumps over
//! what your partner set.

use crate::model::{Action, RoomState, DECKS};
use crate::render::LocalParams;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub fn label(&self) -> String {
        match self.kind {
            MidiKind::Note => format!("Note {} · ch {}", self.number, self.channel + 1),
            MidiKind::Cc => format!("CC {} · ch {}", self.number, self.channel + 1),
            MidiKind::PitchBend => format!("Pitch bend · ch {}", self.channel + 1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Target {
    Play { deck: u8 },
    Cue { deck: u8 },
    HotCue { deck: u8, slot: u8 },
    Tempo { deck: u8 },
    Volume { deck: u8 },
    Eq { deck: u8, band: u8 },
    Pfl { deck: u8 },
    Jog { deck: u8 },
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
            Target::Tempo { deck: d } => format!("Deck {} · Tempo fader", deck(d)),
            Target::Volume { deck: d } => format!("Deck {} · Channel fader", deck(d)),
            Target::Eq { deck: d, band } => format!(
                "Deck {} · EQ {}",
                deck(d),
                ["low", "mid", "high"][band.min(2) as usize]
            ),
            Target::Pfl { deck: d } => format!("Deck {} · Headphone cue", deck(d)),
            Target::Jog { deck: d } => format!("Deck {} · Jog wheel (nudge)", deck(d)),
            Target::BendDown { deck: d } => format!("Deck {} · Pitch bend −", deck(d)),
            Target::BendUp { deck: d } => format!("Deck {} · Pitch bend +", deck(d)),
            Target::Load { deck: d } => format!("Deck {} · Load selected track", deck(d)),
            Target::Crossfader => "Crossfader".into(),
            Target::CueMix => "Headphone cue/master mix".into(),
            Target::Browse => "Library browse encoder".into(),
        }
    }

    pub fn is_absolute(&self) -> bool {
        matches!(
            self,
            Target::Tempo { .. }
                | Target::Volume { .. }
                | Target::Eq { .. }
                | Target::Crossfader
                | Target::CueMix
        )
    }

    pub fn is_relative(&self) -> bool {
        matches!(self, Target::Jog { .. } | Target::Browse)
    }

    /// Every mappable target, in the order the mapping screen shows them.
    pub fn all() -> Vec<Target> {
        let mut v = Vec::new();
        for d in 0..DECKS as u8 {
            v.push(Target::Play { deck: d });
            v.push(Target::Cue { deck: d });
            for slot in 0..4 {
                v.push(Target::HotCue { deck: d, slot });
            }
            v.push(Target::Load { deck: d });
            v.push(Target::Jog { deck: d });
            v.push(Target::BendDown { deck: d });
            v.push(Target::BendUp { deck: d });
            v.push(Target::Tempo { deck: d });
            v.push(Target::Volume { deck: d });
            for band in (0..3).rev() {
                v.push(Target::Eq { deck: d, band });
            }
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
    /// 64 = still, 65 = +1, 63 = -1 (Pioneer and many others).
    #[default]
    Offset64,
    /// 1 = +1, 127 = -1 (Numark, Hercules and others).
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mapping {
    pub device: String,
    pub bindings: Vec<Binding>,
    #[serde(default = "one")]
    pub jog_sensitivity: f32,
}

impl Mapping {
    pub fn binding_for(&self, target: Target) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.target == target)
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
                format!("Note {note} on (vel {vel}) · ch {}", ch + 1)
            }
            RawMidi::NoteOff { ch, note } => format!("Note {note} off · ch {}", ch + 1),
            RawMidi::Cc { ch, cc, val } => format!("CC {cc} = {val} · ch {}", ch + 1),
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
    /// Set by the mapping screen; the next control you touch gets bound.
    pub learn: Option<Target>,
    pub tempo_range: f64,
    pub last_message: Option<String>,
    takeover: HashMap<Target, Takeover>,
    msb: HashMap<(u8, u8), u8>,
}

const BEND: f64 = 0.04;
const JOG_SECONDS_PER_TICK: f64 = 0.0008;

impl MidiEngine {
    pub fn new(mapping: Mapping, tempo_range: f64) -> Self {
        Self {
            mapping,
            learn: None,
            tempo_range,
            last_message: None,
            takeover: HashMap::new(),
            msb: HashMap::new(),
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

    pub fn handle(&mut self, bytes: &[u8], room: &RoomState, local: &LocalParams) -> Vec<MidiOut> {
        let Some(raw) = parse(bytes) else {
            return Vec::new();
        };
        self.last_message = Some(raw.label());

        if let Some(target) = self.learn {
            let trigger = match raw {
                RawMidi::NoteOn { ch, note, .. } => Some(Trigger {
                    kind: MidiKind::Note,
                    channel: ch,
                    number: note,
                }),
                // CCs 32-63 are usually the fine half of a 14-bit pair; learn the coarse one.
                RawMidi::Cc { ch, cc, .. }
                    if !(32..64).contains(&cc) || !self.msb.contains_key(&(ch, cc - 32)) =>
                {
                    Some(Trigger {
                        kind: MidiKind::Cc,
                        channel: ch,
                        number: cc,
                    })
                }
                RawMidi::PitchBend { ch, .. } => Some(Trigger {
                    kind: MidiKind::PitchBend,
                    channel: ch,
                    number: 0,
                }),
                _ => None,
            };
            if let RawMidi::Cc { ch, cc, val } = raw {
                self.msb.insert((ch, cc), val);
            }
            let Some(trigger) = trigger else {
                return Vec::new();
            };
            self.learn = None;
            let binding = Binding {
                trigger,
                target,
                invert: false,
                rel: RelMode::default(),
            };
            self.mapping.bindings.retain(|b| b.target != target);
            self.mapping.bindings.push(binding.clone());
            return vec![MidiOut::Learned(binding)];
        }

        let mut out = Vec::new();
        match raw {
            RawMidi::NoteOn { ch, note, .. } => self.button(
                Trigger {
                    kind: MidiKind::Note,
                    channel: ch,
                    number: note,
                },
                true,
                room,
                &mut out,
            ),
            RawMidi::NoteOff { ch, note } => self.button(
                Trigger {
                    kind: MidiKind::Note,
                    channel: ch,
                    number: note,
                },
                false,
                room,
                &mut out,
            ),
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
                    if let Some(&msb) = self.msb.get(&(ch, cc - 32)) {
                        let trig = Trigger {
                            kind: MidiKind::Cc,
                            channel: ch,
                            number: cc - 32,
                        };
                        if self
                            .mapping
                            .bindings
                            .iter()
                            .any(|b| b.trigger == trig && b.target.is_absolute())
                        {
                            let v = (((msb as u16) << 7) | val as u16) as f32 / 16383.0;
                            self.value(trig, v, None, room, local, &mut out);
                            return out;
                        }
                    }
                }
                self.msb.insert((ch, cc), val);
                let trig = Trigger {
                    kind: MidiKind::Cc,
                    channel: ch,
                    number: cc,
                };
                self.value(trig, val as f32 / 127.0, Some(val), room, local, &mut out);
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
                Target::Pfl { deck } if down => out.push(MidiOut::TogglePfl(deck)),
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
            if b.target.is_relative() {
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
                        let delta = ticks as f64
                            * JOG_SECONDS_PER_TICK
                            * self.mapping.jog_sensitivity as f64;
                        out.push(MidiOut::Action(Action::Nudge { deck, delta }));
                    }
                    Target::Browse => out.push(MidiOut::Browse(ticks)),
                    _ => {}
                }
                continue;
            }
            if !b.target.is_absolute() {
                // A button sent as CC (127 press / 0 release).
                self.button(trig, raw7.map(|r| r >= 64).unwrap_or(v >= 0.5), room, out);
                continue;
            }
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
                Target::Crossfader => MidiOut::Action(Action::Crossfader { v }),
                Target::CueMix => MidiOut::CueMix(v),
                _ => continue,
            });
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
            Target::Crossfader => room.crossfader,
            Target::CueMix => local.cue_mix,
            _ => 0.0,
        }
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
            if (v - current).abs() < 0.04 || crossed {
                t.engaged = true;
            }
        }
        t.last_hw = Some(v);
        if t.engaged {
            t.last_sent = Some(v);
        }
        t.engaged
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

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> MidiEngine {
        MidiEngine::new(
            Mapping {
                device: "test".into(),
                bindings: Vec::new(),
                jog_sensitivity: 1.0,
            },
            0.08,
        )
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
    fn soft_takeover_waits_for_pickup() {
        let mut e = engine();
        let local = LocalParams::default();
        let mut room = RoomState::default(); // volume 1.0
        e.mapping.bindings.push(Binding {
            trigger: Trigger {
                kind: MidiKind::Cc,
                channel: 0,
                number: 19,
            },
            target: Target::Volume { deck: 0 },
            invert: false,
            rel: RelMode::Offset64,
        });
        // Hardware fader is at the bottom: ignored.
        assert!(e.handle(&[0xb0, 19, 0], &room, &local).is_empty());
        assert!(e.handle(&[0xb0, 19, 60], &room, &local).is_empty());
        // Reaches the top: picks up.
        let out = e.handle(&[0xb0, 19, 127], &room, &local);
        assert_eq!(
            out,
            vec![MidiOut::Action(Action::Volume { deck: 0, v: 1.0 })]
        );
        room.decks[0].volume = 1.0;
        let out = e.handle(&[0xb0, 19, 120], &room, &local);
        assert_eq!(out.len(), 1);
        // Partner pulls the fader down to 0.2: we release until we cross it again.
        room.decks[0].volume = 0.2;
        assert!(e.handle(&[0xb0, 19, 110], &room, &local).is_empty());
        assert_eq!(e.handle(&[0xb0, 19, 20], &room, &local).len(), 1);
    }

    #[test]
    fn fourteen_bit_and_jog() {
        let mut e = engine();
        let local = LocalParams::default();
        let room = RoomState::default(); // tempo 1.0 = fader 0.5
        e.mapping.bindings.push(Binding {
            trigger: Trigger {
                kind: MidiKind::Cc,
                channel: 0,
                number: 0,
            },
            target: Target::Tempo { deck: 0 },
            invert: false,
            rel: RelMode::Offset64,
        });
        e.mapping.bindings.push(Binding {
            trigger: Trigger {
                kind: MidiKind::Cc,
                channel: 0,
                number: 33,
            },
            target: Target::Jog { deck: 0 },
            invert: false,
            rel: RelMode::Offset64,
        });
        let _ = e.handle(&[0xb0, 0, 64], &room, &local);
        let out = e.handle(&[0xb0, 32, 0], &room, &local);
        match &out[..] {
            [MidiOut::Action(Action::Tempo { tempo, .. })] => assert!((tempo - 1.0).abs() < 0.001),
            other => panic!("{other:?}"),
        }
        // CC 33 isn't bound as a tempo LSB (CC 1 is unbound) so it's the jog.
        let out = e.handle(&[0xb0, 33, 66], &room, &local);
        assert!(matches!(out[..], [MidiOut::Action(Action::Nudge { .. })]));
    }

    #[test]
    fn port_name_keys() {
        assert_eq!(mapping_key("2- DDJ-FLX4"), "DDJ-FLX4");
        assert_eq!(mapping_key("DDJ-400"), "DDJ-400");
        assert_eq!(mapping_key("Mixtrack Pro FX"), "Mixtrack_Pro_FX");
    }
}
