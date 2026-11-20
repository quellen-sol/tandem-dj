//! A record let go while it's spinning.
//!
//! Let go of the jog platter mid-spin (vinyl mode) and the record carries on
//! at the speed your hand left it, then slows down like a record on a
//! turntable, the way Serato does it: a hard backspin runs on for over a
//! second and fades out, a gentle flick stops in a fraction of one. On a
//! playing deck the motor then brings the record back up to the play speed.
//! Let go close to its resting speed (held still, or moving with the music)
//! and the record just settles, so drops and cue points stay exact.
//!
//! The motion is analytic, like the rest of the playhead: from the speed at
//! one moment, a formula gives where the record is at any later time. So
//! every booth (and the delayed broadcast) works out exactly the same spin
//! from the one event that let it go.
//!
//! Speeds are in track seconds per second: 1.0 is normal speed, negative is
//! backwards.

/// Friction: a spinning record slows by `DRAG + DAMP × speed` per second
/// (speed counted from where it will settle). The proportional part lets a
/// fast spin run on and fade out gradually; the constant part brings it to a
/// stop in a finite time.
const DRAG: f64 = 1.6;
const DAMP: f64 = 1.2;
/// After a backspin has run down, a playing deck's motor brings the record
/// up to the play speed at this rate (stopped to normal speed in 0.2 s).
const MOTOR: f64 = 5.0;
/// Let go closer than this to its resting speed, a record just settles.
pub const MIN: f64 = 0.5;
/// Fastest spin taken (the read head follows up to 8x).
pub const MAX: f64 = 8.0;

/// Friction acting for `dt` seconds on a record going `u` (> 0) faster than
/// where it settles: how much faster it still is, and how far it went.
fn friction(u: f64, dt: f64) -> (f64, f64) {
    let w = DRAG / DAMP;
    // 1 - e^(-DAMP dt), accurate for small dt.
    let decay = -(-DAMP * dt).exp_m1();
    let speed = (u + w) * (1.0 - decay) - w;
    let dist = (u + w) * decay / DAMP - w * dt;
    (speed, dist)
}

/// How long friction takes to bring a record going `u` (> 0) to rest.
fn stop_time(u: f64) -> f64 {
    (u * DAMP / DRAG).ln_1p() / DAMP
}

/// How far a record going `u` (> 0) runs on before friction stops it.
fn run_out(u: f64) -> f64 {
    friction(u, stop_time(u)).1
}

/// A record spinning at `v` on a deck that rests at `rest` (0 when paused,
/// the play speed when playing): how far it has gone `dt` seconds later, its
/// speed then, and whether it's still spinning (false once it has settled
/// at `rest`, after which it moves at `rest`).
pub fn motion(v: f64, rest: f64, dt: f64) -> (f64, f64, bool) {
    if dt <= 0.0 {
        return (0.0, v, true);
    }
    if v > rest {
        // Faster than it rests: friction slows it to `rest`.
        let u = v - rest;
        let stop = stop_time(u);
        let (s, d) = friction(u, dt.min(stop));
        return if dt < stop {
            (rest * dt + d, rest + s, true)
        } else {
            (rest * dt + d, rest, false)
        };
    }
    let (mut dist, mut t, mut v) = (0.0, dt, v);
    if v < 0.0 {
        // Backwards: friction stops it, then (playing) the motor takes over.
        let stop = stop_time(-v);
        if dt < stop {
            let (s, d) = friction(-v, dt);
            return (-d, -s, true);
        }
        dist = -run_out(-v);
        t = dt - stop;
        v = 0.0;
    }
    // Between stopped and the play speed: the motor brings it up to speed.
    let ramp = (rest - v) / MOTOR;
    if t < ramp {
        (dist + v * t + 0.5 * MOTOR * t * t, v + MOTOR * t, true)
    } else {
        (
            dist + v * ramp + 0.5 * MOTOR * ramp * ramp + rest * (t - ramp),
            rest,
            false,
        )
    }
}

/// The speed a record let go at `v` spins on with, or `None` if it just
/// settles at `rest`. `back` is how far it is from the start of the track: a
/// backspin that would run past it is slowed so it stops right there.
pub fn let_go(v: f64, rest: f64, back: f64) -> Option<f64> {
    if !v.is_finite() || (v > -MIN && v < rest + MIN) {
        return None;
    }
    let v = v.clamp(-MAX, MAX);
    if v < 0.0 && run_out(-v) > back {
        let (mut lo, mut hi) = (0.0, -v);
        for _ in 0..60 {
            let mid = 0.5 * (lo + hi);
            if run_out(mid) > back {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        // Hardly any room (or none): it just stops there.
        return (lo > 1e-3).then_some(-lo);
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Follows `motion` in small steps and checks that the distance it gives
    /// is the integral of the speed it gives.
    fn check_consistent(v: f64, rest: f64) {
        let dt = 1e-4;
        let mut t = 0.0;
        let mut integrated = 0.0;
        while t < 4.0 {
            let (_, s0, _) = motion(v, rest, t);
            let (_, s1, _) = motion(v, rest, t + dt);
            integrated += 0.5 * (s0 + s1) * dt;
            t += dt;
            let (d, _, _) = motion(v, rest, t);
            assert!(
                (d - integrated).abs() < 1e-6,
                "v {v} rest {rest} at {t:.3} s: distance {d} but speed integrates to {integrated}"
            );
        }
    }

    #[test]
    fn distance_is_the_integral_of_speed() {
        for (v, rest) in [
            (-5.5, 1.0),
            (-5.5, 0.0),
            (3.0, 0.0),
            (4.0, 1.0),
            (0.3, 1.0),
            (-0.7, 1.04),
        ] {
            check_consistent(v, rest);
        }
    }

    #[test]
    fn a_hard_backspin_runs_on_then_stops() {
        // About what a hard backspin on a DDJ-FLX6 leaves the hand at.
        let v = -5.5;
        let (_, s, spinning) = motion(v, 0.0, 0.5);
        assert!(spinning && s < -1.5 && s > -4.0, "half a second in: {s}");
        let stop = stop_time(-v);
        assert!(stop > 1.0 && stop < 2.0, "stops after {stop} s");
        let (d, s, spinning) = motion(v, 0.0, 3.0);
        assert!(!spinning && s == 0.0);
        assert!(d < -2.0 && d > -4.0, "ran back {d} s of track");
        // A gentle flick stops much sooner and goes much less far.
        let (d, _, _) = motion(0.8, 0.0, 3.0);
        assert!(stop_time(0.8) < 0.5 && d > 0.0 && d < 0.2, "flick: {d}");
    }

    #[test]
    fn a_playing_deck_comes_back_to_speed() {
        let rest = 1.02;
        let (_, s, spinning) = motion(-4.0, rest, 0.8);
        assert!(spinning && s < 0.0, "still going back: {s}");
        // Backwards, then stopped, then up to speed within 0.2 s more.
        let back = stop_time(4.0);
        let (_, s, spinning) = motion(-4.0, rest, back + 0.1);
        assert!(spinning && s > 0.4 && s < rest);
        let (d0, s, spinning) = motion(-4.0, rest, back + 0.3);
        assert!(!spinning && s == rest);
        // From then on it plays on at the play speed.
        let (d1, _, _) = motion(-4.0, rest, back + 1.3);
        assert!((d1 - d0 - rest).abs() < 1e-12);
        // Thrown forwards, it slows down to the play speed.
        let (_, s, spinning) = motion(4.0, rest, 0.2);
        assert!(spinning && s > rest && s < 4.0);
        let (_, s, spinning) = motion(4.0, rest, 5.0);
        assert!(!spinning && s == rest);
    }

    #[test]
    fn released_near_its_resting_speed_it_just_settles() {
        assert_eq!(let_go(0.2, 0.0, 10.0), None);
        assert_eq!(let_go(-0.4, 0.0, 10.0), None);
        // Playing: held still, or moving with the music.
        assert_eq!(let_go(0.0, 1.0, 10.0), None);
        assert_eq!(let_go(1.3, 1.0, 10.0), None);
        assert_eq!(let_go(-5.0, 1.0, 10.0), Some(-5.0));
        assert_eq!(let_go(2.0, 1.0, 10.0), Some(2.0));
        assert_eq!(let_go(30.0, 0.0, 10.0), Some(MAX));
        assert_eq!(let_go(f64::NAN, 0.0, 10.0), None);
    }

    #[test]
    fn a_backspin_stops_at_the_start_of_the_track() {
        let v = let_go(-6.0, 0.0, 0.5).unwrap();
        assert!(v > -6.0);
        let (d, _, spinning) = motion(v, 0.0, 10.0);
        assert!(!spinning && (d + 0.5).abs() < 1e-9, "ran back {d}");
        // Already at the start: nowhere to go.
        assert_eq!(let_go(-6.0, 0.0, 0.0), None);
    }
}
