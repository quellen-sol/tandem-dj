//! Reading what a jog wheel (or an endless knob) sends, for Learn, the
//! step-by-step setup and the jog wheel check alike.
//!
//! A wheel either *counts* (each message says how far it turned since the
//! last, around 64 or as 1 and 127) or says *where it is* (7 bits on one
//! control change, or 14 on two). Nothing in a single message tells which:
//! 1 is one tick of one wheel, 63 ticks back on another, and a place on a
//! third. Read the wrong way, a wheel only ever goes one way, or races. So a
//! reading needs several messages, and is only given when they fit nothing
//! else:
//!
//! - A wheel that counts sends the same small numbers over and over when
//!   it's turned slowly. A position never repeats, and a position's coarse
//!   half, which does, climbs or falls without ever going back.
//! - A 14-bit position is two control changes, where the coarse one steps
//!   by one exactly when the fine one comes round.
//! - A fader sends positions too. Only a wheel comes round past its end
//!   ([`Wheel::round`]), or turns with a hand on its touch sensor, which is
//!   for the caller to know.
//!
//! [`read`] reads a wheel turned one way (or just turned); [`read_both`] one
//! turned clockwise and then back, which tells more.

use crate::mapping::{MidiKind, RawMidi, RelMode, Trigger};

/// Messages a wheel has to send before it's read.
pub const TURN_MESSAGES: usize = 8;
/// The two halves of a 14-bit position follow each other within this (us).
const HALVES_US: i64 = 5_000;
/// Further than this between two messages (in 14 bits) isn't one wheel
/// turned slowly.
const SMOOTH: i32 = 63;

/// A control change heard from the controller, at shared time `t` (us).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Msg {
    pub t: i64,
    pub trigger: Trigger,
    pub v: u8,
}

impl Msg {
    /// The message as one of these, if it's a control change.
    pub fn of(raw: RawMidi, t: i64) -> Option<Msg> {
        match raw {
            RawMidi::Cc { ch, cc, val } => Some(Msg {
                t,
                trigger: Trigger::cc(ch, cc),
                v: val,
            }),
            _ => None,
        }
    }
}

/// A wheel, as its messages read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wheel {
    /// What to bind: for a 14-bit position, its coarse half.
    pub trigger: Trigger,
    pub rel: RelMode,
    /// How far it turned in all, in its own ticks (not inverted).
    pub ticks: i64,
    /// Messages that moved it.
    pub n: usize,
    /// A position that came round past its end, as no fader can.
    pub round: bool,
}

impl Wheel {
    /// Whether it's a wheel whichever control this is: it counts, or came
    /// round. (A position that didn't may be a fader's.)
    pub fn sure(&self) -> bool {
        !self.rel.is_position() || self.round
    }
}

/// `d` as the shortest way round a circle of `size`.
pub fn shortest(d: i32, size: i32) -> i32 {
    let r = d.rem_euclid(size);
    if r * 2 > size {
        r - size
    } else {
        r
    }
}

/// Where a wheel that sends its position was last heard, to tell how far it
/// has turned since.
#[derive(Clone, Copy, Debug, Default)]
pub struct Position {
    coarse: Option<u8>,
    last: Option<i32>,
}

impl Position {
    /// A coarse half (on its own it moves nothing).
    pub fn coarse(&mut self, v: u8) {
        // Until it's known, where the wheel is isn't.
        if self.coarse.replace(v).is_none() {
            self.last = None;
        }
    }

    /// A fine half: how far the wheel has turned since the last.
    pub fn fine(&mut self, v: u8) -> i32 {
        let at = (self.coarse.unwrap_or(0) as i32) << 7 | v as i32;
        self.moved(at, 1 << 14)
    }

    /// A 7-bit position: how far the wheel has turned since the last.
    pub fn at(&mut self, v: u8) -> i32 {
        self.moved(v as i32, 1 << 7)
    }

    /// The first message only says where the wheel is.
    fn moved(&mut self, at: i32, size: i32) -> i32 {
        self.last
            .replace(at)
            .map_or(0, |last| shortest(at - last, size))
    }
}

/// How far a wheel turned over `msgs` if it's bound to `trigger` and counts
/// as `rel`: (net ticks, messages that moved it one way, the other way).
pub fn decode(msgs: &[Msg], trigger: Trigger, rel: RelMode) -> (i64, usize, usize) {
    let mut out = (0, 0, 0);
    let mut position = Position::default();
    for m in msgs.iter().filter(|m| m.trigger.channel == trigger.channel) {
        let ticks = match rel {
            RelMode::Absolute14 { fine } if m.trigger == Trigger::cc(trigger.channel, fine) => {
                position.fine(m.v)
            }
            RelMode::Absolute14 { .. } if m.trigger == trigger => {
                position.coarse(m.v);
                continue;
            }
            RelMode::Absolute if m.trigger == trigger => position.at(m.v),
            _ if m.trigger == trigger => rel.ticks(m.v).unwrap_or(0),
            _ => continue,
        };
        out.0 += ticks as i64;
        match ticks {
            0 => {}
            1.. => out.1 += 1,
            _ => out.2 += 1,
        }
    }
    out
}

/// One control's messages, in order.
struct Control {
    trigger: Trigger,
    /// (Its place among all the messages, value.)
    sent: Vec<(usize, u8)>,
}

impl Control {
    fn values(&self) -> impl Iterator<Item = u8> + '_ {
        self.sent.iter().map(|&(_, v)| v)
    }

    fn distinct(&self) -> usize {
        self.values()
            .fold(0u128, |seen, v| seen | 1 << v.min(127))
            .count_ones() as usize
    }

    /// The steps from each value to the next, the shortest way round.
    fn steps(&self) -> Vec<i32> {
        self.sent
            .windows(2)
            .map(|w| shortest(w[1].1 as i32 - w[0].1 as i32, 128))
            .collect()
    }

    /// It moves like a position: through enough values, hardly ever the
    /// same twice, in small steps that go one way.
    fn climbs(&self) -> bool {
        let steps = self.steps();
        let n = steps.len();
        let up = steps.iter().filter(|&&s| s > 0).count();
        let down = steps.iter().filter(|&&s| s < 0).count();
        n + 1 >= TURN_MESSAGES
            && self.distinct() >= 4
            && (up.max(down)) * 10 >= n * 9
            && steps.iter().filter(|s| s.abs() <= 30).count() * 20 >= n * 19
    }

    /// The same, from fewer messages: it keeps moving on one way, as a
    /// position's fine half does and ticks don't.
    fn drifts(&self) -> bool {
        let steps = self.steps();
        let up = steps.iter().filter(|&&s| s > 0).count();
        let down = steps.iter().filter(|&&s| s < 0).count();
        steps.len() >= 3 && up.max(down) * 10 >= steps.len() * 7
    }

    /// Its values came round past the end.
    fn round(&self) -> bool {
        self.sent
            .windows(2)
            .any(|w| (w[1].1 as i32 - w[0].1 as i32).abs() > 64)
    }
}

/// The control changes in `msgs`, each with what it sent.
fn controls(msgs: &[Msg]) -> Vec<Control> {
    let mut out: Vec<Control> = Vec::new();
    for (i, m) in msgs.iter().enumerate() {
        match out.iter_mut().find(|c| c.trigger == m.trigger) {
            Some(c) => c.sent.push((i, m.v)),
            None => out.push(Control {
                trigger: m.trigger,
                sent: vec![(i, m.v)],
            }),
        }
    }
    out.retain(|c| c.trigger.kind == MidiKind::Cc);
    out
}

/// How many times in a row a wheel sends one of these when it's turned
/// slowly, whichever way it counts.
fn near(rel: RelMode, v: u8) -> bool {
    rel.ticks(v).is_some_and(|t| (1..=3).contains(&t.abs()))
}

/// Ticks of one kind, hardly any of them none at all (a button let go).
fn ticks_of(c: &Control, rel: RelMode) -> bool {
    let ticks = |v: u8| rel.ticks(v).unwrap_or(0).abs();
    let n = c.sent.len();
    let none = c.values().filter(|&v| ticks(v) == 0).count();
    let own = c.values().filter(|&v| (1..=40).contains(&ticks(v))).count();
    none * 4 <= n && own * 10 >= (n - none) * 9
}

/// How one control counts, if it can't be anything else: nearly all its
/// values are ticks of one kind, a fair share are the few a slow turn sends,
/// and they repeat. (A position's coarse half repeats too, so a climb
/// without a step back isn't believed until it has one, and nor is the same
/// value over and over unless it's a single tick.)
fn counts(c: &Control) -> Option<RelMode> {
    let n = c.sent.len();
    if n < TURN_MESSAGES {
        return None;
    }
    let steps = c.steps();
    let up = steps.iter().filter(|&&s| s > 0).count();
    let down = steps.iter().filter(|&&s| s < 0).count();
    if up.max(down) >= 1 && up.min(down) == 0 {
        return None;
    }
    let single = |rel: RelMode| {
        c.values()
            .all(|v| rel.ticks(v).is_some_and(|t| t.abs() == 1))
    };
    [RelMode::TwosComplement, RelMode::Offset64]
        .into_iter()
        .filter(|&rel| up > 0 || single(rel))
        .find(|&rel| {
            let slow = c.values().filter(|&v| near(rel, v)).count();
            // The same again, as a slow turn sends (and no position does).
            let again = c
                .sent
                .windows(2)
                .filter(|w| w[0].1 == w[1].1 && near(rel, w[0].1))
                .count();
            ticks_of(c, rel) && slow * 4 >= n && (c.distinct() * 2 <= n || again >= 4)
        })
}

/// Whether `coarse` and `fine` are the two halves of one position: nearly
/// every move is a small one, and the coarse half stepped as the fine one
/// came round, at least once. Returns the wheel they are.
fn pair(msgs: &[Msg], coarse: Trigger, fine: Trigger) -> Option<Wheel> {
    let mut position = Position::default();
    let (mut high, mut last) = (None, None);
    let (mut n, mut smooth, mut come_round, mut stepped) = (0usize, 0usize, 0usize, 0usize);
    let mut net = 0i64;
    let mut round = false;
    for m in msgs {
        if m.trigger == coarse {
            if let Some(h) = high {
                round |= (m.v as i32 - h as i32).abs() > 64;
            }
            high = Some(m.v);
            position.coarse(m.v);
        } else if m.trigger == fine {
            let Some(h) = high else { continue };
            let moved = position.fine(m.v);
            if let Some((lh, lv)) = last {
                n += 1;
                smooth += usize::from(moved.abs() <= SMOOTH);
                net += moved as i64;
                // The fine half came round: the coarse one has to step.
                if (m.v as i32 - lv as i32).abs() > 64 {
                    come_round += 1;
                    stepped += usize::from(shortest(h as i32 - lh as i32, 128).abs() == 1);
                }
            }
            last = Some((h, m.v));
        }
    }
    (n + 1 >= TURN_MESSAGES && smooth * 20 >= n * 19 && stepped >= 1 && stepped == come_round)
        .then_some(Wheel {
            trigger: coarse,
            rel: RelMode::Absolute14 { fine: fine.number },
            ticks: net,
            n,
            round,
        })
}

/// The 14-bit positions in `msgs`.
fn pairs(msgs: &[Msg], controls: &[Control]) -> Vec<Wheel> {
    let mut out: Vec<Wheel> = Vec::new();
    for fine in controls {
        // (The usual pair, 32 apart, before any other.)
        let mut coarse: Vec<&Control> = controls
            .iter()
            .filter(|c| {
                c.trigger.channel == fine.trigger.channel
                    && c.trigger != fine.trigger
                    && c.distinct() >= 2
            })
            .collect();
        coarse.sort_by_key(|c| c.trigger.number as i32 + 32 != fine.trigger.number as i32);
        if let Some(w) = coarse
            .into_iter()
            .find_map(|c| pair(msgs, c.trigger, fine.trigger))
        {
            out.push(w);
        }
    }
    out
}

/// Whether nearly each of `c`'s messages came beside one of a position's
/// that nothing has explained yet: `c` may be its coarse half, whatever it
/// reads as on its own.
fn beside_a_position(msgs: &[Msg], controls: &[Control], pairs: &[Wheel], c: &Control) -> bool {
    controls.iter().any(|other| {
        let half = |w: &Wheel| {
            w.trigger.channel == other.trigger.channel
                && matches!(w.rel, RelMode::Absolute14 { fine } if fine == other.trigger.number)
        };
        other.trigger != c.trigger
            && other.trigger.channel == c.trigger.channel
            && !pairs.iter().any(half)
            && other.drifts()
            && c.sent
                .iter()
                .filter(|&&(i, _)| {
                    let t = msgs[i].t;
                    [i.wrapping_sub(1), i + 1].into_iter().any(|j| {
                        msgs.get(j).is_some_and(|m| {
                            m.trigger == other.trigger && (m.t - t).abs() <= HALVES_US
                        })
                    })
                })
                .count()
                * 10
                >= c.sent.len() * 8
    })
}

/// Whether nearly each of `c`'s messages came straight after one of another
/// control's: `c` may be the fine half of a position whose coarse half
/// hasn't stepped yet.
fn after_another(msgs: &[Msg], c: &Control) -> bool {
    let follows = |&&(i, _): &&(usize, u8)| {
        i.checked_sub(1).is_some_and(|j| {
            let (m, before) = (msgs[i], msgs[j]);
            before.trigger != m.trigger
                && before.trigger.channel == m.trigger.channel
                && m.t - before.t <= HALVES_US
        })
    };
    c.sent.iter().filter(follows).count() * 10 >= c.sent.len() * 8
}

/// Whether `t` is one of the halves of these positions.
fn half_of(pairs: &[Wheel], t: Trigger) -> bool {
    pairs.iter().any(|w| {
        w.trigger == t
            || (w.trigger.channel == t.channel
                && matches!(w.rel, RelMode::Absolute14 { fine } if fine == t.number))
    })
}

/// The wheels in `msgs`, however they were turned. A wheel that counts comes
/// before one that sends its position; nothing for a control that isn't
/// clearly a wheel yet.
pub fn read(msgs: &[Msg]) -> Vec<Wheel> {
    let controls = controls(msgs);
    let pairs = pairs(msgs, &controls);
    let mut counting = Vec::new();
    let mut places = Vec::new();
    for c in &controls {
        if half_of(&pairs, c.trigger) || beside_a_position(msgs, &controls, &pairs, c) {
            continue;
        }
        if let Some(rel) = counts(c) {
            let (ticks, a, b) = decode(msgs, c.trigger, rel);
            counting.push(Wheel {
                trigger: c.trigger,
                rel,
                ticks,
                n: a + b,
                round: false,
            });
        } else if c.climbs() && !after_another(msgs, c) {
            let (ticks, a, b) = decode(msgs, c.trigger, RelMode::Absolute);
            places.push(Wheel {
                trigger: c.trigger,
                rel: RelMode::Absolute,
                ticks,
                n: a + b,
                round: c.round(),
            });
        }
    }
    counting.extend(pairs);
    counting.extend(places);
    counting
}

/// A wheel bound to `trigger` and telling how far it turned as `rel`, if
/// that's what was turned clockwise (`cw`) and then back (`ccw`): nearly all
/// one way in the first, and the other way in the second.
pub fn turned_back(cw: &[Msg], ccw: &[Msg], trigger: Trigger, rel: RelMode) -> Option<Wheel> {
    let way = |(net, a, b): (i64, usize, usize)| -> Option<i64> {
        (a + b >= TURN_MESSAGES && a.max(b) * 20 >= (a + b) * 17).then_some(net)
    };
    let (there, back) = (decode(cw, trigger, rel), decode(ccw, trigger, rel));
    let (x, y) = (way(there)?, way(back)?);
    (x.signum() * y.signum() < 0).then_some(Wheel {
        trigger,
        rel,
        ticks: x,
        n: there.1 + there.2,
        round: false,
    })
}

/// How far a control's ticks are from none, on average, if it counts as
/// `rel`.
fn mean(c: &Control, rel: RelMode) -> f64 {
    let sum: i32 = c.values().map(|v| rel.ticks(v).unwrap_or(0).abs()).sum();
    sum as f64 / c.sent.len().max(1) as f64
}

/// The wheels in messages heard while a wheel was turned clockwise (`cw`),
/// and then back (`ccw`): those that go one way in the first and the other
/// way in the second. [`Wheel::ticks`] is how far it turned clockwise.
pub fn read_both(cw: &[Msg], ccw: &[Msg]) -> Vec<Wheel> {
    let (there, back) = (controls(cw), controls(ccw));
    let (pairs_there, pairs_back) = (pairs(cw, &there), pairs(ccw, &back));
    let opposite = |trigger, rel| turned_back(cw, ccw, trigger, rel);
    // Found in either direction, as long as the other agrees.
    let mut halves: Vec<Wheel> = Vec::new();
    for w in pairs_there.iter().chain(&pairs_back) {
        if halves.iter().any(|h| h.trigger == w.trigger) {
            continue;
        }
        if let Some(mut o) = opposite(w.trigger, w.rel) {
            o.round = pairs_there
                .iter()
                .chain(&pairs_back)
                .any(|p| p.trigger == w.trigger && p.round);
            halves.push(o);
        }
    }
    let mut counting = Vec::new();
    let mut places = Vec::new();
    for c in &there {
        let Some(b) = back.iter().find(|b| b.trigger == c.trigger) else {
            continue;
        };
        if half_of(&halves, c.trigger)
            || half_of(&pairs_there, c.trigger)
            || half_of(&pairs_back, c.trigger)
            || beside_a_position(cw, &there, &halves, c)
            || beside_a_position(ccw, &back, &halves, b)
        {
            continue;
        }
        // Turned both ways, what its ticks are closest to is how it counts,
        // if that's clear and they repeat.
        let repeats = |c: &Control| c.distinct() * 2 <= c.sent.len();
        let [twos, offs] = [RelMode::TwosComplement, RelMode::Offset64]
            .map(|rel| (mean(c, rel) + mean(b, rel), rel));
        let (near, far) = if twos.0 <= offs.0 {
            (twos, offs)
        } else {
            (offs, twos)
        };
        let rel = (near.0 <= far.0 * 0.6
            && repeats(c)
            && repeats(b)
            && ticks_of(c, near.1)
            && ticks_of(b, near.1))
        .then_some(near.1);
        match rel.map(|rel| opposite(c.trigger, rel)) {
            Some(Some(w)) => counting.push(w),
            // It repeats itself, and doesn't go one way and then the other.
            Some(None) => {}
            None if c.climbs() && b.climbs() && !after_another(cw, c) => {
                if let Some(mut w) = opposite(c.trigger, RelMode::Absolute) {
                    w.round = c.round() || b.round();
                    places.push(w);
                }
            }
            None => {}
        }
    }
    counting.extend(halves);
    counting.extend(places);
    counting
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Messages from one control, 8 ms apart.
    pub(crate) fn sent(cc: u8, values: impl IntoIterator<Item = u8>) -> Vec<Msg> {
        values
            .into_iter()
            .enumerate()
            .map(|(i, v)| Msg {
                t: i as i64 * 8_000,
                trigger: Trigger::cc(4, cc),
                v,
            })
            .collect()
    }

    /// A 14-bit position moving by `step` from `from`, `n` times: the coarse
    /// half of each, and its fine half straight after.
    pub(crate) fn fourteen(coarse: u8, fine: u8, from: i32, step: i32, n: i32) -> Vec<Msg> {
        (0..n)
            .flat_map(|i| {
                let at = (from + step * i).rem_euclid(1 << 14);
                [(coarse, (at >> 7) as u8, 0), (fine, (at & 0x7F) as u8, 300)].map(|(cc, v, dt)| {
                    Msg {
                        t: i as i64 * 4_000 + dt,
                        trigger: Trigger::cc(4, cc),
                        v,
                    }
                })
            })
            .collect()
    }

    fn only(msgs: &[Msg]) -> Option<(u8, RelMode)> {
        match read(msgs)[..] {
            [] => None,
            [w] => Some((w.trigger.number, w.rel)),
            ref more => panic!("{more:?}"),
        }
    }

    const TWOS: RelMode = RelMode::TwosComplement;
    const OFFS: RelMode = RelMode::Offset64;

    #[test]
    fn a_slow_turn_reads_as_it_counts() {
        for (v, rel) in [(1, TWOS), (127, TWOS), (65, OFFS), (63, OFFS)] {
            assert_eq!(only(&sent(0x36, [v; 8])), Some((0x36, rel)), "{v}");
            // Not from one message, nor a few.
            for n in 1..8 {
                assert_eq!(only(&sent(0x36, vec![v; n])), None, "{v} × {n}");
            }
        }
        // There and back, and stopping in between.
        assert_eq!(
            only(&sent(0x36, [1, 1, 2, 1, 127, 126, 127, 127, 1])),
            Some((0x36, TWOS))
        );
        let w = read(&sent(0x36, [1, 2, 2, 1, 1, 2, 1, 1]))[0];
        assert_eq!((w.ticks, w.n, w.sure()), (11, 8, true));
    }

    #[test]
    fn a_flick_is_read_once_it_has_slowed_down() {
        let there = [5, 9, 14, 18, 20, 20, 19, 17];
        let back = there.map(|v| 128 - v);
        for (start, slow, rel) in [
            (there, [12, 8, 5, 3, 2, 2, 1, 1, 1, 1, 1, 1], TWOS),
            (
                back,
                [116, 120, 123, 125, 126, 126, 127, 127, 127, 127, 127, 127],
                TWOS,
            ),
        ] {
            // Its first messages could be a fader's, or a position's.
            assert_eq!(only(&sent(0x36, start)), None);
            let all: Vec<u8> = start.into_iter().chain(slow).collect();
            assert_eq!(only(&sent(0x36, all)), Some((0x36, rel)));
        }
        let around: Vec<u8> = [
            69, 73, 78, 82, 84, 84, 80, 74, 69, 67, 66, 66, 65, 65, 65, 65,
        ]
        .into_iter()
        .chain([65; 4])
        .collect();
        assert_eq!(only(&sent(0x22, around)), Some((0x22, OFFS)));
    }

    #[test]
    fn a_position_is_never_read_as_counting() {
        // Seven bits, from anywhere, either way, one step a message or more.
        for from in 0..128i32 {
            for step in [1, 2, 3, -1, -2, -3] {
                let m = sent(
                    0x37,
                    (0..40).map(|i| (from + step * i).rem_euclid(128) as u8),
                );
                for w in read(&m) {
                    assert_eq!(w.rel, RelMode::Absolute, "from {from} by {step}");
                    assert_eq!(w.ticks.signum() as i32, step.signum());
                }
            }
        }
        // The coarse half of a 14-bit one, with the fine half beside it:
        // however many messages have come.
        for from in (0..1 << 14).step_by(61) {
            for step in [1, 3, -1, -3] {
                let m = fourteen(0x11, 0x31, from, step, 120);
                for upto in [16, 40, 90, m.len()] {
                    for w in read(&m[..upto]) {
                        assert_eq!(
                            (w.trigger.number, w.rel),
                            (0x11, RelMode::Absolute14 { fine: 0x31 }),
                            "from {from} by {step}, {upto} messages"
                        );
                    }
                }
                // A coarse step or two on, it's read as what it is.
                if step.abs() == 3 {
                    assert_eq!(read(&m).len(), 1, "from {from} by {step}");
                }
            }
        }
        // The coarse half alone, eight messages a step, as a wheel whose
        // fine half isn't heard would send.
        for from in (4..124i32).filter(|f| ![63, 65].contains(f)) {
            for step in [1, -1] {
                let m = sent(
                    0x37,
                    (0..250).map(|i| (from + step * (i / 8)).rem_euclid(128) as u8),
                );
                assert_eq!(
                    (8..250)
                        .flat_map(|n| read(&m[..n]))
                        .filter(|w| !w.rel.is_position())
                        .count(),
                    0,
                    "from {from} by {step}"
                );
            }
        }
    }

    #[test]
    fn a_14_bit_position_is_found_whatever_its_numbers() {
        // The Prime 4's fine half isn't 32 up from its coarse one.
        for (coarse, fine) in [(0x11, 0x31), (0x37, 0x4D)] {
            let w = read(&fourteen(coarse, fine, 3000, 4, 80));
            assert_eq!(w.len(), 1, "{w:?}");
            assert_eq!(w[0].trigger, Trigger::cc(4, coarse));
            assert_eq!(w[0].rel, RelMode::Absolute14 { fine });
            assert_eq!(w[0].ticks, 79 * 4);
            // It may be a fader, until it comes round.
            assert!(!w[0].sure());
            let w = read(&fourteen(coarse, fine, (1 << 14) - 100, 4, 80));
            assert!(w[0].sure(), "{w:?}");
        }
        // A wheel that counts beside it is read too, and first.
        let mut both = Vec::new();
        for (i, p) in fourteen(0x11, 0x31, 3000, 4, 80).chunks(2).enumerate() {
            both.extend_from_slice(p);
            both.push(Msg {
                t: i as i64 * 4_000 + 600,
                trigger: Trigger::cc(4, 0x36),
                v: if i % 5 == 0 { 2 } else { 1 },
            });
        }
        let w = read(&both);
        assert_eq!(
            w.iter().map(|w| w.trigger.number).collect::<Vec<_>>(),
            [0x36, 0x11],
            "{w:?}"
        );
        assert_eq!(w[0].rel, TWOS);
    }

    #[test]
    fn faders_and_buttons_arent_wheels() {
        // A 7-bit fader moved up, then back: not one wheel that counts.
        let up: Vec<u8> = (20..90).collect();
        for w in read(&sent(0x1F, up.clone())) {
            assert!(w.rel.is_position() && !w.sure());
        }
        let both = read_both(&sent(0x1F, up.clone()), &sent(0x1F, up.into_iter().rev()));
        assert!(both.iter().all(|w| w.rel.is_position() && !w.sure()));
        // A button that sends control changes.
        assert_eq!(read(&sent(0x40, [127, 0, 127, 0, 127, 0, 127, 0])), []);
        assert_eq!(read(&sent(0x40, [127, 0])), []);
        // Nothing at all.
        assert_eq!(read(&[]), []);
        assert_eq!(read_both(&[], &[]), []);
    }

    #[test]
    fn turned_there_and_back() {
        let slow = |v: u8, w: u8| sent(0x36, (0..40).map(move |i| if i % 4 == 0 { w } else { v }));
        for (cw, ccw, rel, sign) in [
            (slow(1, 2), slow(127, 126), TWOS, 1),
            (slow(127, 126), slow(1, 2), TWOS, -1),
            (slow(65, 66), slow(63, 62), OFFS, 1),
            (slow(63, 62), slow(65, 66), OFFS, -1),
        ] {
            let w = read_both(&cw, &ccw);
            assert_eq!(w.len(), 1, "{w:?}");
            assert_eq!((w[0].rel, w[0].ticks.signum()), (rel, sign));
        }
        // A fine wheel that sends seldom: never a single tick.
        let fine = |sign: i32| {
            let v = |i: i32| (sign * (9 + i % 7 + i % 4)) as u8 & 0x7F;
            sent(0x36, (0..60).map(v))
        };
        let w = read_both(&fine(1), &fine(-1));
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!((w[0].rel, w[0].ticks.signum()), (TWOS, 1));
        // The same both ways isn't a wheel that turned back.
        assert_eq!(read_both(&slow(1, 2), &slow(1, 2)), []);
        // A 14-bit position, its coarse half sitting where it would pass
        // for a tick (65, then 1).
        for high in [65, 1, 127, 63] {
            let from = high << 7 | 10;
            let w = read_both(
                &fourteen(0x11, 0x31, from, 3, 90),
                &fourteen(0x11, 0x31, from + 270, -3, 90),
            );
            assert_eq!(w.len(), 1, "{w:?}");
            assert_eq!(w[0].rel, RelMode::Absolute14 { fine: 0x31 });
            assert_eq!(w[0].ticks, 89 * 3);
        }
        // Seven bits, backwards.
        let w = read_both(
            &sent(0x37, (0..60).map(|i| (100 - i) as u8)),
            &sent(0x37, (0..60).map(|i| (40 + i) as u8)),
        );
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!((w[0].rel, w[0].ticks), (RelMode::Absolute, -59));
    }

    #[test]
    fn positions_tell_how_far_the_wheel_turned() {
        let mut p = Position::default();
        assert_eq!(p.at(120), 0, "the first only says where it is");
        assert_eq!(p.at(126), 6);
        assert_eq!(p.at(2), 4, "round past the end");
        assert_eq!(p.at(127), -3);
        let mut p = Position::default();
        assert_eq!(p.fine(5), 0);
        p.coarse(127);
        assert_eq!(p.fine(120), 0, "now it's known where the wheel is");
        p.coarse(0);
        assert_eq!(p.fine(4), 12, "round past the end");
        p.coarse(127);
        assert_eq!(p.fine(127), -5);
    }
}
