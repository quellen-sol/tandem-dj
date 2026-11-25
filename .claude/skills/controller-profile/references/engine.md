# What Tandem's mapping engine can express

Contents: [The model](#the-model) · [Controls Tandem has](#controls-tandem-has) ·
[Decisions](#decisions) · [Lights](#lights) · [What the engine can't do yet](#what-the-engine-cant-do-yet) ·
[Writing the preset](#writing-the-preset) · [Tests](#tests) · [Files a profile touches](#files-a-profile-touches)

The code is the authority: `crates/tandem-core/src/mapping.rs` (the engine,
the presets, their tests) and `crates/tandem/src/midi.rs` (ports, the
controller thread, lights out, the keep-alive, `--midi-monitor`). Read
`pioneer_ddj` and `denon_sc_live_4` and their doc comments before writing a
preset: they are the two shapes a controller can take. This file says what
isn't obvious from a skim. If it and the code disagree, the code wins: fix
this file.

## The model

A `Mapping` is a list of `Binding`s: a `Trigger` (note, CC or pitch bend, on
a **0-based** channel, with a number) and the `Target` it drives. One trigger
may have several bindings and several triggers may drive one target.

```rust
m.bind(Trigger::note(ch, 0x0B), Target::Play { deck });          // a button
m.bind(Trigger::cc(ch, 0x13), Target::Volume { deck });          // a fader
m.bind(Trigger::cc(6, 0x40), Target::Browse).rel = RelMode::TwosComplement;
let b = m.bind(pad, Target::Roll { deck, size: i });              // returns &mut Binding
b.mode = Some(PadMode::Rolls);
b.light = Some((63, 17));
```

Binding fields beyond trigger and target:

| Field | Meaning |
|---|---|
| `invert` | Absolute control: value becomes 1 − v. Relative control: direction flips. |
| `rel` | Relative encoding: `Offset64` (64 still, 65 = +1, 63 = −1; the default) or `TwosComplement` (1 = +1, 127 = −1). Faster turns send bigger steps in both. |
| `shift` | Software SHIFT: works only while a `Target::Shift` button is held, and then replaces the trigger's other bindings. |
| `mode` | Software pad mode: works only while its deck is in that `PadMode`. |
| `light` | `(lit, unlit)` values sent to the button's light instead of 127 and 0. |

How messages are read:

- **Note:** note-on with velocity > 0 is a press; note-off, or note-on with
  velocity 0, is a release.
- **CC on a button target:** value ≥ 64 is a press, below is a release.
- **CC on an absolute target:** value / 127, or 14-bit when the fine half
  arrives on CC + 32 (see Decisions).
- **CC on a relative target:** ticks, per `rel`.
- **Pitch bend:** a 14-bit absolute value. Absolute targets only.

Decks are 0–3 (A–D). A and C sit on the crossfader's left.

## Controls Tandem has

Per deck: `Play` (toggles on press), `Cue` (hold), `BackToStart`, `Sync`
(toggles), `SyncOff`, `SyncMaster` (makes the deck the sync master; not
`MasterLevel`), `HotCue { slot 0–7 }` (hold: sets when empty, else
jumps), `HotCueClear { slot }`, `LoopIn`, `LoopOut`, `Reloop`,
`LoopHalve`, `LoopDouble`, `BeatLoop { size }` (index into ¼, ½, 1, 2, 4, 8,
16, 32 beats), `Roll { size }` (hold; index into 1/16, 1/8, ¼, ½, 1, 2, 4, 8
beats), `Tempo`, `TempoShift`, `Volume`, `Eq { band }` (0 low, 1 mid,
2 high), `Trim`, `Filter` (0.5 is off), `Pfl` (headphone cue, toggles),
`JogPlatter`, `JogEdge`, `Jog`, `JogTouch`, `BendDown`, `BendUp` (hold), `Load`,
`Eject`,
`PadMode { mode }` (`HotCues`, `BeatLoops`, `Rolls`), `DeckSelect` (a
message saying a side now plays this deck, see "Deck layers").

What the loop buttons do when pressed (`model.rs` is the authority):

- `BeatLoop { size }`: with no loop playing, starts one of that length on
  the beat. With a loop playing, the *same* size leaves it and another size
  resizes it. Its light is lit while a loop of exactly that size plays.
- `Reloop`: leaves the loop that's playing, or goes back into the last one.
  Before the deck has had a loop it does nothing. Lit while looping.
- `LoopIn` / `LoopOut`: a manual loop. `LoopHalve` / `LoopDouble` resize
  the loop that's playing.

Global: `Crossfader` (0 is the A side), `CueMix` (0 is cue only, 1 master
only), `HeadphoneLevel`, `MasterLevel`, `Browse` (relative), `BrowsePress`,
`BrowseBack`, `Shift`.

Effects, per **FX unit** (0–3): a controller's FX section works one deck's
bank of three slots, and `Mapping::fx_unit_decks` says which deck each unit
works when the controller connects (unit 1 → A … unit 4 → D unless the
preset sets it; the FLX6 preset starts both units on A, as the unit powers
on). `FxOn { unit, slot }` (toggles, lit while on), `FxDepth { unit, slot }`
(absolute: an off slot takes the knob directly, an on slot waits for pickup),
`FxDepthAll { unit }` (one knob for all three), `FxBeatsDown { unit }`,
`FxBeatsUp { unit }`, `FxNext { unit, slot }` (the slot's next effect),
`FxAssign { unit, deck }` and `FxAssignNone { unit }` (a channel select, or
its MST), `FxFocus { unit, slot }` (an FX SELECT whose knob and ON/OFF are
shared between slots: the knob's next reading only says where it is, and the
slot's lights are re-sent). The FLX6 preset (`flx6_beat_fx`) is the worked
example; DESIGN.md's "Controller effects" has the rules.

Tandem has **no** control for: pad FX and Merge FX, a master FX bank, an
absolute BEATS knob (SB2's SHIFT + LEVEL), sampler, slicer, beat jump, key
lock and key shift, slip, reverse/censor, tempo range, quantize, a
vinyl-mode button (vinyl is a setting in the mapping), loop-size or
beat-jump encoders, track skip, mic, booth, crossfader curve or assign
switches, stems, waveform zoom, view/panel buttons. Leave these unmapped and
list them in the notes.

Two things that look like gaps but aren't:

- **Deck layers.** If the DECK button changes the MIDI channel, bind the
  second layer's messages to decks C and D (the FLX6 preset does). If the
  unit also says which deck a side has switched to (the FLX6 sends note
  0x3C on that deck's channel), bind that to `DeckSelect { deck }`: one
  tempo fader serves both layers, and this is how Tandem knows to re-read
  where it is instead of jumping the tempo. If the hardware sends the same
  messages on both layers, only A and B are reachable (the SC Live 4).
- **Master and headphone knobs on a controller with a sound card.** Bind
  them if they send MIDI. When Tandem plays through the controller's own
  card, it notices by the card's name and lets the hardware do the level, so
  the sound isn't turned down twice.
- **Controllers that blend their own headphones.** Some units mix cue and
  master for the headphone jack in hardware (a HEADPHONES MIX knob or a
  MASTER CUE button that works with no software running). `mixes_own_phones`
  in `mapping.rs` lists them: for those, Tandem sends the jack only the cued
  decks, greys out its own cue ↔ master slider, and uses the jack by itself
  when no headphone output is picked. Find out whether this unit does it and
  record the answer. Adding a unit to that list changes what the DJ hears,
  so do it only on a manufacturer's statement or a check on the hardware;
  otherwise leave the list alone and put the question on the owner's
  checklist.

## Decisions

Each comes straight from the research. Write the answer and its source in
the notes file.

**SHIFT.** Does holding SHIFT change what the other controls send?

- *Yes* (Pioneer, typically Hercules): each SHIFT + control is its own
  message. Bind it as an ordinary trigger: `SyncOff` on the SHIFT + SYNC
  note, `HotCueClear` on the shifted pad notes, `TempoShift` on the shifted
  fader's CC. No `Target::Shift` needed.
- *No* (Denon SC Live 4): bind the SHIFT button's own note to
  `Target::Shift` and mark the SHIFT + bindings `shift = true` on the same
  trigger as the plain one.
- *Some controls change and some don't:* both styles can live in one preset.
  A control with no `shift = true` binding does its normal thing while SHIFT
  is held.

The SHIFT + functions Tandem has: SHIFT + CUE → `BackToStart`, SHIFT + SYNC
→ `SyncOff`, SHIFT + LOAD → `Eject`, SHIFT + hot cue pad → `HotCueClear`,
SHIFT + tempo fader → `TempoShift` (moves the fader without the tempo, so a
DJ out of travel can re-centre). Map these when the hardware labels agree or
have no competing function; they are how the existing presets behave.

**Pad modes.** Do the pads send different notes in each pad mode?

- *Yes:* bind each mode's notes directly. No `Target::PadMode`.
- *No:* bind the mode buttons to `Target::PadMode { deck, mode }` and give
  each pad one binding per mode with `mode = Some(…)`.

Which hardware modes become which Tandem pads: hot cue → `HotCue`; a loop
or beat-loop mode → `BeatLoop`; a roll / loop-roll mode → `Roll`. Modes
Tandem has nothing for (sampler, slicer, pad FX, stems, beat jump) stay
unmapped. With fewer than eight pads, use the sizes the pads are labelled
with; if unlabelled, follow the manufacturer's own software.

**A LOOP or AUTO LOOP button.** Tandem has no "loop on/off at the current
size". The nearest are `BeatLoop { size: 4 }` (a 4-beat loop; the same
button leaves it, but only while the loop is still 4 beats long: after a
halve or double, the first press goes back to 4 beats and the second
leaves) and `Reloop` (leaves and re-enters, but does nothing until a loop
has been made another way). The existing presets use `BeatLoop { size: 4 }`
for an auto-loop press. Say which you chose and what the DJ will notice.

**Jog.** Bind every message the wheel sends when turned: those it sends with
a hand on top to `Target::JogPlatter`, those it sends by its edge to
`Target::JogEdge`, and one it sends for both to `Target::Jog`, where the
touch sensor decides. The sensor's note goes to `Target::JogTouch`. The
platter scratches while touched (vinyl mode); the edge, and an untouched
wheel, bends a playing deck and searches a paused one. Don't bind a sensor
that its edge also sets off when the edge has a message of its own, or one
that never lets go: a platter without one holds the record by turning. A
SHIFT + jog message (fast search) has no target: leave it. Then settle:

- the encoding (`rel`), from a script's arithmetic, a Traktor encoder mode
  or a manufacturer list;
- the direction (clockwise must count up; `invert` if not);
- `jog_ticks_per_rev`: from a Mixxx `scratchEnable`, a manufacturer figure,
  or a capture of one full turn. If unknown, give your best-sourced figure,
  say so, and put "calibrate" first on the owner's checklist. Settings →
  Controller → Calibrate fixes a wrong figure per user, so this is the one
  number that may be approximate.
- A jog that reports an absolute position instead of ticks: `RelMode::Absolute`
  (7 bits) or `RelMode::Absolute14 { fine }` (the binding's CC is the coarse
  half, sent first; `fine` any CC on the same channel).

**Tempo fader.** Tandem wants the value to rise as the tempo rises (the +
end). Find which physical end sends the high value; set `invert` if the +
end sends the low one. Don't copy an "invert" flag from another program's
mapping: each has its own convention. If no source settles it, say so in
the notes and the report: it's a ten-second check for the owner.

**14-bit faders.** Bind the coarse CC only. The engine joins the fine half by
itself when it's on coarse + 32, and reads a pitch-bend message as 14-bit.
A pair laid out any other way works as 7-bit from its coarse half (the SC
Live 4's pitch fader). Check your table for a trap: an unrelated 7-bit
control on CC n + 32 of a bound absolute CC n (n < 32, same channel) would
be mistaken for that control's fine half.

**Absolute direction and range.** Knobs and faders are 0–1 with the centre
at 0.5 for EQ, trim and filter. If a control runs backwards, `invert`. If it
has a non-linear or partial range, note it; the engine has no scaling.

**Encoders and buttons that don't fit.** A browse knob that sends one note
for clockwise and another for anticlockwise can't drive `Browse`. A latching
button would act every other press. Note them; don't bend a target to fit.

## Lights

The controller thread asks the engine every 25 ms for the state of every
light and sends what changed. A light exists for a binding when all of
these hold:

- its trigger is a **note** (a CC button gets no light);
- it isn't a software SHIFT binding;
- its target has a state: `Play`, `Cue`, `Sync`, `SyncMaster`, `Pfl`, `HotCue`, `LoopIn`,
  `LoopOut`, `Reloop`, `BeatLoop`, `Roll`, `PadMode`, `Shift`, `FxOn`.

The message is a note-on to **the binding's own channel and note**, with the
binding's `light` values, or 127 lit and 0 unlit. A pad with bindings in
several software pad modes shows the one for its deck's mode. When Tandem
closes it sends 0 to everything it lit.

So, per light you found: if it sits on its button's own note, set `light`
when the values aren't 127/0 (dim-at-1 buttons, pad colours). Hot cue pads
on colour-capable units take the eight on-screen hot cue colours (see
`DENON_HOT_CUES`). If the light is anywhere else, the engine can't reach it.

On a controller whose SHIFT changes the messages, the SHIFT layer is its own
set of notes and usually its own set of lights, which the unit shows while
SHIFT is held. A binding on a SHIFT-layer note gets its light there like any
other (loop IN on SHIFT + ½, say). But a plain button's light has no twin on
the SHIFT layer, so PLAY, CUE and the pads go dark while SHIFT is held. Say
so in the notes; it's expected, not a fault.

**Level meters:** `m.meters.push(MeterLight { channel, cc, deck })` sends the
deck's level on a CC using `pioneer_meter_value`, a 0–127 scale shaped to the
DDJ-FLX6's measured thresholds. Use it only where the controller's meter
takes a similar proportional value; a bitmask or a different scale would
show garbage, so leave `meters` empty and note the protocol.

**FX BEATS indicators:** `m.fx_beat_lights.push(FxBeatLight { channel,
cc, unit, values, dark })` sends a CC whose value is `values[i]` for the
bank's BEATS (an index into `model::FX_BEATS`) of the deck the unit works,
and `dark` when it works none and when Tandem closes. Use a value measured
or listed as dark. A controller that shows only the selected slot's or
unit's lights (the FLX6) gets them re-sent on `FxFocus`.

**Keep-alive and init:** `midi.rs` sends Pioneer's keep-alive SysEx every
200 ms to any port whose name contains "DDJ", and nothing to anyone else.

## What the engine can't do yet

If the research turns up one of these, record it in the notes under the
matching heading and tell the user. Each is a small, separate engine change;
make it only when the profile doesn't work without it, and say you did.

- An init or keep-alive SysEx for a non-Pioneer controller, or a "report
  every control's position" request on connect (soft takeover covers the
  lack of one).
- A loop on/off button at the current loop size (see Decisions).
- Different jog resolutions for the platter's top and its ring.
- A light at an address that isn't its button's own note, or on a CC
  (apart from level meters and FX BEATS indicators, above).
- Blinking, jog rings, displays.
- A level meter that isn't Pioneer-scaled.
- A relative encoding other than the two above; browse on a pair of notes.
- A 14-bit pair that isn't n / n + 32 (works, at 7-bit).
- Scaling or curves on an absolute control.
- More than one MIDI port per controller.

## Writing the preset

```rust
/// Brand Model. Where the map comes from (manufacturer's list rev X, Mixxx's
/// mapping, …) and what hasn't been tried on the unit. Then the layout in a
/// few sentences: which channels carry what, how SHIFT and the pad modes
/// reach Tandem, anything surprising.
fn brand_model(device: &str) -> Mapping {
    let mut m = Mapping::empty(device);
    m.preset = Some("Brand Model".to_string());
    m.preset_version = PRESET_VERSION;
    m.jog_ticks_per_rev = Some(/* sourced figure */);
    for side in 0..2u8 { /* per-deck bindings */ }
    /* global bindings */
    m
}
```

- Add the branch to `builtin_preset`. It compares the upper-cased port name;
  the SC Live 4 branch strips everything but letters and digits first, which
  is the safer pattern for names with spaces. Put more specific names before
  less specific ones.
- Write channels and numbers the way the neighbouring presets do (hex
  numbers, 0-based channels) and say in a comment what a number is when the
  code doesn't (`// SHIFT + CUE`).
- Comments state hardware facts and their reason, as the existing ones do.
  The provenance detail lives in the notes file; the doc comment summarises
  it in a sentence or two and names the notes file's path, so a maintainer
  reading the preset knows the evidence exists.
- No new `Target`s, fields or engine behaviour for a profile unless you hit
  "What the engine can't do yet" and the user needs it.
- `PRESET_VERSION` isn't bumped for a new preset. Someone who had already
  learned this controller by hand keeps their file; `Mapping::adopt` turns it
  into the preset when at least half its bindings match.

## Tests

Put them beside the `sc_live_4_*` and `inpulse_200_mk2_*` tests and reuse
the helpers in that module. Replay raw messages through
`MidiEngine::new(builtin_preset("<port>").unwrap(), 0.08)` and assert the
`MidiOut`s.

- `send`, `tap`, `act`, `roll`: one message against an untouched room. Right
  for buttons and pads.
- `follow`: plays messages and lets the room apply each action, as a session
  does. Use it for anything with state: a loop made and then resized, a
  fader moved twice. Against a room that never changes, soft takeover
  swallows a fader's second message, because the room didn't move to where
  the first one put it.
- `drive`, `fader`, `tempo_after_pairs`, `two_decks`, and the `TOP` /
  `CENTRE` / `BOTTOM` constants: 14-bit tempo fader moves.
- `knob`, `play_levels`: the master and headphone level knobs.

Cover:

- the port names: every spelling you found matches, with a "2- " prefix
  too, and sibling models that need another map don't;
- transport and each SHIFT function, on both decks;
- pads in every mode, including that modes Tandem doesn't have do nothing;
- the jog: a touched turn scratches in the right direction, an untouched
  turn on a playing deck bends (see `jog_scratches_while_touched`);
- the tempo fader's direction, and SHIFT + fader if mapped;
- a 14-bit fader's two halves if it has one;
- the mixer on each channel, crossfader, browse (turn direction, press);
- lights: `e.lights(&room, &local)` gives the right addresses and values at
  rest and for a set hot cue, a playing deck, a held roll;
- `serde_json` round-trip of the preset.

Tests prove the preset says what you meant, and that the engine does the
right thing with it. They can't prove the hardware sends those messages;
only sources and captures do. Write test comments as the hardware's
behaviour ("SHIFT + pad arrives on channel 4"), so a reader can check them
against the notes.

```bash
cargo test -p tandem-core mapping          # the mapping tests
cargo test -p tandem-core                  # everything CI runs on Linux
cargo run -q -p tandem-core --example preset_table -- "<port name>"
cargo run -q -p tandem-core --example preset_table -- "<port name>" --json > <key>.json
```

The `--json` file, placed in the owner's mappings folder under the name
`preset_table` prints, lets them try the preset on the Tandem they already
have, before any release.

## Files a profile touches

- `crates/tandem-core/src/mapping.rs`: the preset, its `builtin_preset`
  branch, its tests.
- `docs/controllers/<brand>-<model>.md`: the notes (new).
- `README.md`: the status line near the top and the controller list under
  "Controller" (one bullet, like the SC Live 4's: what the sides and pads do,
  what's special, "not yet tried on the unit" if so). Then search the README
  for the other presets' names: sentences such as "They're mapped on the
  FLX6…" (the level knobs) or "…on the FLX6, FLX4 and DDJ-400" (the meters)
  list controllers by feature and need this one added where it applies.
- `docs/DESIGN.md`: a sentence where the other presets' origins are
  described, if this one adds something a maintainer should know.
- `docs/ROADMAP.md` gets its entry when the change is released, with the
  version's notes: leave it unless asked to release.
- `crates/tandem/src/midi.rs`: only for an init or keep-alive the controller
  can't work without.
