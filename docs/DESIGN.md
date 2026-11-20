# Design

## The problem

Over the internet, a message takes 20–80 ms one way, plus buffering. At 128 BPM a 16th note is 117 ms. If one machine runs the mix and the other DJ steers it remotely, every press makes a round trip before it's heard. Long fades survive that; hot cues and cuts don't.

## The approach: replicated state on a shared clock

```
 DJ A (host)                   room clock = host's clock              DJ B (joined)
 controller ─▶ timeline ─▶ live lane ─▶ speakers / phones     controller ─▶ timeline ─▶ live lane ─▶ speakers / phones
                   │      └▶ broadcast lane (−0.5 s) ─▶ virtual cable ─▶ OBS
                   └──── timestamped events over TCP ────────────────┘
```

### Shared clock

The host's monotonic clock is the room clock. The joiner pings the host, NTP-style, and trusts the sample with the smallest round trip. That sample has the least room for network asymmetry. Pinging continues once a second to track drift. See `clock.rs`.

### Timeline

Every control action is an `Event { time, origin, seq, action }` in room time. `model.rs` defines the room state at time *t* as a fold of all events up to *t* over a base state. That makes it:

- **Deterministic:** the same events give bit-identical state on any machine.
- **Order-independent:** a late event is inserted at its timestamp, and every later query includes it.
- **Queryable at any time,** including the past. That's what the delayed broadcast lane needs.

A deck's playhead is analytic: `position(t) = anchor_pos + (t − anchor_time) × rate`. No audio has to run for the position to be known. (A record let go mid-spin follows a curve instead of a line; see [Spinning records](#spinning-records).)

The host sends a digest of the state from 1.5 s ago every 2 s. If the joiner disagrees twice in a row, it asks for a full resync.

### Lanes and read heads

`render.rs` turns a timeline into audio. A *lane* renders the room as heard at `room time − delay`:

- **Live lane (delay 0):** your speakers and headphones. Your own moves are applied immediately, with the usual 10–20 ms sound-card buffer. Your partner's arrive one network trip late.
- **Broadcast lane (delay 500 ms by default):** by the time it reaches a moment, events from both DJs for that moment have arrived, so the stream has every cut exactly where it was played.

Each deck in each lane has a *read head* that chases the model's playhead:

- **Discontinuities** (play, cue, hot cue, load): hard jump with a 2 ms crossfade.
- **Small differences** (jog nudges, clock corrections, sound-card crystal drift of about 50 ppm, which is roughly 180 ms an hour): absorbed by playing up to 10% faster or slower until it catches up.

`StreamClock` maps each sound-card callback to room time and smooths out callback jitter.

### Loops

A loop is part of the deck's state: an in point, an out point and whether it's looping. While looping, `position_at` wraps the analytic playhead, so it's still a pure function of the events: past the out point it carries on from the in point, at `in + (p − in) mod length`.

- **Anchor.** Anything that moves the anchor to or past the out point (a nudge, a sync phase shift, halving) wraps it. A jump (seek, CUE, hot cue) that lands outside the loop ends it.
- **Beat loops** start on the grid: on the beat the playhead is in, or for loops under a beat, on that fraction of the beat (`BeatGrid::beat_loop`). The playhead is always inside the loop, and wrapping keeps the beat phase, so a synced deck stays locked.
- **Quantized IN and OUT.** `LoopIn` and `LoopOut` carry a `quantize` flag: the pressing DJ's own setting, which every booth applies the same way, so a loop is the same everywhere whoever made it. On a deck with a grid, IN takes the beat nearest the playhead (inside the track; it may be just ahead, and the playhead walks into it). OUT sets the loop's length to the nearest of ¼ beat, ½ beat, 1 beat, ½ bar, 1 bar … 128 bars (`quantized_loop_beats`; half way between two lengths goes to the longer), timed with the tempo at the in point, as beat loops are. An out point behind the playhead wraps it round the loop, keeping the beat phase. The loop counts as a beat loop of that size (`loop_beats`), so its pad lights and ½ / ×2 keep it on the grid. While looping, IN moves only the start to the nearest beat. Without a grid, or with the flag off, IN and OUT are exactly at the playhead. The snap is done by the model, from the room state at the event's time, rather than by the booth that pressed: a partner's move that arrives late (a nudge, a grid edit) is then counted in by every booth alike.
- **Leaving a loop** shorter than a beat can leave a synced deck between beats, so the deck re-matches its phase. Whole-beat loops don't move.
- **Relocks** for tempo changes only look at markers inside the loop, plus the jump back to the in point.

Read heads go round loops by themselves. They read the track through the loop: frames past the out point come from the in point, and the last 96 frames (about 2 ms) before the out point fade into the frames just before the in point. Going round is therefore seamless and click-free, through the stretcher too, with no jump and no re-priming. The chase measures its error the short way round the loop.

A loop that appears or changes under audio the head has already read makes the head hard-sync with the usual 2 ms crossfade. That happens with LOOP OUT pressed at the playhead, a partner's loop arriving a little late, and always with a stretched head, because the stretcher reads ahead.

### Loop rolls

A loop roll is a beat loop that plays only while its pad is held, while the track runs on underneath (slip), so letting go carries on exactly where the track would have been. On Pioneer controllers the BEAT JUMP pads roll 1/16 to 8 beats (`model::ROLLS`, top row short to long), and on the Denon SC Live 4 the pads in ROLL pad mode.

- **State.** `DeckState::roll` holds the roll's loop and the *slip position*, where the track underneath was at a moment (`slip_time`, `slip_pos`). The deck's playhead goes round the roll (`active_loop` is the roll while one is held), so read heads, waveforms and the relock schedule treat it like any loop. The track underneath moves at the deck's play speed and goes round the ordinary loop if one plays (`slip_position_at`). `reanchor` moves both, so tempo, bend and sync changes carry the track underneath too. It's all in the events, so both booths and the broadcast hear the same roll.
- **Starting.** `Roll { on: true }` on a playing deck starts on the grid at the playhead, like a beat loop (`grid_loop`), so the playhead doesn't move. Another size while one is held keeps the in point and puts the playhead at the point in the new roll that the track underneath is at, `in + (slip − in) mod length`, so the roll stays in step with the track. A paused deck doesn't roll.
- **Letting go.** `Roll { on: false }` with the size that's rolling puts the playhead on the track underneath (`end_roll`). Before the roll has gone round, the two agree to the last bit and nothing jumps; otherwise it's a jump (a `gen` bump, so a 2 ms crossfade). Letting go of another size does nothing. The controller keeps track of which roll pads are held (`MidiEngine::roll_pad`): letting go of the latest of two sends the other one's size, so its roll comes back.
- **What lets go first.** A jump (seek, CUE, a hot cue jump, the end of a preview, a load), PAUSE and every loop control end the roll, then apply, so a quantized LOOP IN or OUT snaps where the track has got to, not where the roll is. Tempo, bend, SYNC, the mixer and scratching carry on under it. Scratching and its nudges move only the playhead, so the track keeps running underneath, like slip scratching.
- **Over a loop.** The ordinary loop stays set and the track underneath keeps going round it. A roll can be longer than the loop. Letting go comes back into the loop wherever the track has got to.
- **Sync.** The beat phase is the track's underneath (`beat_phase` reads the slip position), so SYNC pressed under a roll lines up the track, and a synced deck is back on the beat after any roll, even a 1/16 stutter. A whole-beat roll also sounds on the beat while it plays.

Mapping: `Target::Roll { deck, size }` is one target per pad, pressed and let go, so controllers whose pads send the same notes in every pad mode can use it too. Preset version 10 puts the Pioneer BEAT JUMP pads (notes 0x20–0x27 on the pad channels) on it, and saved mappings pick them up unless the DJ learned something else onto those pads. The SC Live 4's pads send the same notes in every mode, so its roll pads are bindings for the ROLL pad mode (see [SHIFT and pad modes](#shift-and-pad-modes-tracked-by-tandem)).

### Spinning records

Let go of the jog platter mid-spin (vinyl mode) and the record runs on and slows down, as in Serato. On the DDJ-FLX6 a hard backspin leaves the hand at about 5–6× normal speed. After the hand leaves, the platter itself keeps turning for 0.3–0.9 s, and even a still hand flicks it about 400 ticks as it lifts off.

- **One event.** The controller measures the hand's speed over the last 20 ms and sends it with the release (`JogTouch { down: false, spin }`, an `f32` so it survives JSON exactly). The deck stores the record's speed at its anchor (`DeckState::spin`), and `spin::motion` gives where it is at any later time. Friction takes `1.6 + 1.2 × speed` off its speed per second, measured from where it settles, so a fast spin fades out gradually and a slow one stops soon. A playing deck's backspin then winds back up to the play speed in 0.2 s, like a motor catching it. Every booth and the broadcast compute the same spin, with no stream of nudges.
- **Settling.** A release within 0.5× of the resting speed (stopped, or the play speed) doesn't spin, so drops and cue points stay exact. A backspin that would run past 0:00 is slowed so it stops right there.
- **Momentum carries through.** Anything that re-anchors the deck (tempo, pause, key lock, a relock) keeps the record's current speed, so the curve is unchanged; pausing mid-spin lets it run down and stay stopped. Touching the platter catches it, and jumps (cue, hot cue, seek, load) stop it.
- **The platter's own turning is ignored.** After a release, `MidiEngine` drops jog ticks until the platter has been still for 50 ms (at most 2 s), so the free-turning platter doesn't also search or bend.
- **Sound.** While spinning, the read head plays vinyl-style (no key lock), chasing the curve like a scratch. A paused deck is audible until the record stops.

### Broadcast view

A stream needs a picture that's as late as its sound. The broadcast window (`ui_broadcast.rs`) draws `Session::broadcast_view()`: the room at `now − broadcast delay`, taken from the same timeline the broadcast lane renders. Every move appears on it at the moment its sound reaches the feed, including the partner's. It's read-only, names DJs by name (never "you"), and is a second OS window (an egui immediate viewport) so it can be captured or screenshared on its own. It's drawn inside the main window's frame, so it redraws at the same rate.

### Broadcaster booths

A booth can join a room "Broadcaster only", to stream it from a PC that isn't DJing. It's an ordinary joiner with three differences:

- **It changes nothing.** `Session::act_at`, `load_path`, `reset_grid` and `offer_share` do nothing on it, and its window has no decks, mixer or library (`ui_broadcaster.rs`). The host enforces this too: a broadcaster's control connection only gets its pings and resync requests answered; events, tracks, shares and uploads from it are dropped.
- **It plays only the broadcast lane.** `output::start` routes the broadcast lane to the master output (and to the broadcast feed, if that's set to another device or pair), skips the headphones, and meters the broadcast lane. The broadcast window therefore takes its meters as they are, with no extra delay. Because the process plays nothing else, a capture of its window's sound (Discord, OBS application audio) is exactly the feed.
- **It learns who's in the room from the host,** whose `Members` message lists the DJs and broadcasters whenever that changes. The DJs show the broadcasters in the top bar, and the broadcaster names both DJs on the broadcast window.

The host relays the joining DJ's events and track details to the broadcasters as they arrive, and sends its own events, track details and digests to everyone. A broadcaster sees the joining DJ's moves after two network hops instead of one, which the broadcast delay absorbs.

Tracks come from the host over the usual files connections. The host serves every track on a deck, including ones it found in its own library and ones it downloaded from the joining DJ. A broadcaster can ask for a partner's track before the host has it, so the host holds the download for up to 20 s while it's still arriving, and a client asks again every 5 s for a track on a deck that failed to download.

A broadcaster that loses the room empties its decks rather than carrying on with them as a solo booth.

**Timing.** A move is exact on the stream if it reaches the broadcaster before its broadcast lane plays that moment: within the broadcast delay, less the sound card's buffer. The joining DJ's moves take two hops, so a broadcaster's delay is its own setting (`AudioSettings::broadcaster_delay_ms`, 1000 ms by default; a DJ's stays 500 ms). Each event carries the shared time it happened, so a booth measures how late every move from the room arrives (`Session::note_lateness`, moves made since it joined, a scheduled one counting as on time) and keeps the latest per second for a minute (`LinkView::late_us`). Within 100 ms of the delay the broadcaster warns, and offers `config::broadcast_delay_for`: a quarter second over the lateness, rounded up to a quarter second, at most 2000 ms. It never changes the delay by itself, since the stream repeats or skips by the difference when it does.

A move that's late anyway still lands in time with the music: the timeline puts it at the moment it happened, so a play or hot cue starts on the beat, missing its first few ms, and a fader moves late by the excess. One more than the delay plus 3 s late is clamped to the timeline's base and leaves the booth out of step until a resync.

### Frame rate

The UI redraws at the **Screen frame rate** setting (60 fps by default). Frames are due on a fixed beat (`TandemApp::pace_frames`), so vsync rounding a frame up to the next monitor refresh doesn't drag the average down. Input still redraws at once. Controller moves go straight from the MIDI thread to the session with their own timestamps, so they never wait for a frame. Note that egui brings a requested repaint forward by its predicted frame time (1/60 s): asking for a repaint after 16 ms means "now", which is why the UI used to run at the monitor's full rate.

### Small screens

The decks area and the library share the window's height by rule (`DeckLayout` in `ui_decks.rs`, `library_height` in `ui_library.rs`). At 1440 × 930 points and up nothing changes. On a shorter window:

1. **The library gives way first.** Until the DJ drags its edge, its height follows the window: 330 points when there's room, less on a shorter one, never under 150. After a drag it keeps the DJ's height, but can't be dragged over the waveforms' minimum.
2. **Then the decks.** Two decks swap their full panels for the four-deck view's compact panels, beside a short two-channel mixer: each channel's knobs beside its fader, and the booth levels behind a **🎧 LEVELS** button. They do this as soon as keeping the full panels would leave the library under 180 points, and on a window too narrow for full panels (under about 1190 points wide). Four decks squeeze in stages: thinner track overviews, then tighter panels, with the levels behind the button.
3. **The waveforms never go under a minimum** (46 points each for two decks, 28 for four). When the controls don't fit above them even squeezed, the controls scroll, with a scroll bar in sight.

Compact panels narrow their buttons on a narrow window rather than spill into the mixer, and below 1300 points wide the top bar moves the room's address and clock spread into hover text. The window opens at 1440 × 930; when that doesn't fit above the taskbar (most laptops at 125% display scaling or more), the app maximises it on its first frame (`TandemApp::fit_window`). It goes down to 1024 × 520, about a 1080p laptop at 175%. `--window-size 1280x641` and `--zoom 1.5` reproduce a small screen and display scaling on a big one.

### Level meters

The mixer's meters measure the live lane of the device that plays the master output, in `Lane::render` (`meter.rs`):

- **Channel meters** take each channel's peak after trim, EQ and filter, before the fader. That's what TRIM is for, and the meter keeps moving with the fader down.
- **The master meter** takes the bus after the faders and crossfader, before this booth's master level, so both booths show the same levels.

Peaks rise at once and fall at 24 dB/s, worked out per 64-frame block on the audio thread. After each callback they're published as `f32` bits in atomics, with a timestamp, so readers never lock and never block the audio. Levels that haven't been updated for 250 ms read as silence, in case the sound card stopped.

- **The screen** reads the meters every frame and holds each peak for a second (`ui_meters.rs`).
- **The broadcast window** keeps a short history keyed by the local clock (which never jumps, unlike the room clock when you join) and shows the levels from a broadcast delay ago.
- **The controller thread** sends them every 25 ms, only when a value changes, and zeroes them when it stops. `disconnect` waits for that, so the lights go out when Tandem closes.

Pioneer's channel meters are a control change per channel (CC 0x02 on the deck's channel, 0–127). Pioneer doesn't publish where each light comes on, so it was measured on a DDJ-FLX6: its five lights turn on at about 37–40, 65–69, 85–88, 101–106 and exactly 119. `pioneer_meter_value` maps dB onto those values so the lights come on at −24, −14, −8 and −3 dBFS, and red only over 0 dBFS. Those are the same marks as on screen.

### Key lock and key shift

With key lock on, or with a key shift, a read head feeds the track through Signalsmith Stretch, which changes speed without changing pitch (and pitch without changing speed). The stretcher's latency is part of the read head's maths: the audio heard now is `fed position − input latency − speed × output latency`. Chasing and sync therefore work exactly as in vinyl mode. When starting at a cue point, the stretcher is primed and pre-rolled, so the first sample out is the cue point itself. A test checks that key-locked kicks land within 4 ms of vinyl ones. The stretcher uses a fixed random seed, so both booths still render identical audio. Scratching always uses vinyl mode.

### BPM, beatgrid and key detection

Both come from the audio alone; BPM and key tags are ignored.

`analysis.rs` builds a kick-weighted onset envelope and, over the middle ~100 s, sweeps every tempo from 68 to 190 BPM with a comb of beat ticks. Each tempo is scored together with the metrical levels around it (bars, half bars, eighths and sixteenths), since a real beat is supported at all of them. The strongest tempo family (equal up to octaves) wins, except when its rival is a dotted or triplet pulse 4:3 or 3:2 away, the classic confusion in R&B, jungle and 2-step. There the fine grid decides: the slower tempo's sixteenths are the faster one's triplets, so the faster tempo wins only if its own eighths are stronger. A mild preference for dance tempos picks the octave. A comb over the whole track then refines the BPM to about 0.01 and finds the phase.

Tempo changes: 16 s windows across the track look for a long (32 s or more), confident stretch at a tempo unrelated to the main one (not half/double time, not a 4:3 or 3:2 feel), checked again over the whole stretch. Each becomes its own grid section, switching on the beat where the old grid's last beats and the new grid's first beats fit best. Gradual drifts aren't followed.

`keys.rs` takes note magnitudes from C2 to B6 in ~0.7 s frames, log-compresses them, keeps only what stands above the local spectrum (±6 semitones, which removes drums, noise and the overall tilt), folds them into pitch classes and matches them against Temperley's major/minor key profiles.

Results are cached per file, and versioned so better detection re-runs. They travel with the `Load` event, so both booths use the same grid and key.

### Beatgrids and editing them

A grid (`grid.rs`) is a list of up to 32 markers. Each marker sits on a downbeat and starts a section with its own BPM, which runs until the next marker; analysis gives a one-marker grid. Bars are four beats: the first marker is bar 1, the bars before it count -1, -2…, and every marker starts a new bar. A section's last beat stretches or shrinks by up to half a beat to meet the next marker.

Edits (SET, DELETE, ADJUST, SLIP, ½/×2, RESET) are worked out in the UI from the deck's current grid and sent as one `SetGrid` event carrying the whole new grid, so both booths apply exactly the same thing. ADJUST and SLIP only touch the section under the playhead. Each booth saves an edited grid to `beatgrids.json`, keyed by content hash, and uses it the next time that track loads. Analysis never overwrites it, and RESET removes it. The library cache keeps the detected grid, so RESET can go back to it.

Grids are boxed inside `Action` (which is therefore `Clone`, not `Copy`) so the common events stay small. `DeckState` keeps its grid inline, so the state stays `Copy` and the audio threads never allocate.

**Sync through tempo changes.** A synced deck's tempo is matched to the master's BPM *at the playhead* (see [Four decks and the sync master](#four-decks-and-the-sync-master)). When a playing deck reaches a marker where its BPM changes, the lock has to be redone at that exact moment. The host (or a booth on its own) looks ahead every maintenance tick and schedules a `Relock` event for the moment the deck reaches the marker; if it missed one, it sends a `Relock` straight away. A `Relock` recomputes the lock from the state when it's applied, so a spare one does nothing. Jumps (seek, hot cue, cue, jog) into a section with another tempo, and grid edits, relock as part of applying that event.

### Crossfader curve

The curve is part of the room, like the curve switch on a club mixer. A `CrossfaderCurve` event sets it for both booths, and the broadcast lane switches at the moment it changed. `render::xfader_gain` gives a channel's gain from its side of the crossfader. Side B's curve is side A's, mirrored.

Each channel also has an assign switch (`RoomState::xf_assign`, set by `XfAssign`): side A (left), side B (right), or THRU, where the crossfader leaves it alone. Decks A and C start on side A, B and D on side B.

| Curve | Each deck in the middle | Shape |
|---|---|---|
| Linear | −6 dB | straight fades across the whole travel |
| Constant power (default) | −3 dB | quarter-cosine fades across the whole travel; the decks' powers add up to one |
| Full centre | 0 dB | full up to the middle, then a quarter-cosine fade over the far half |
| Scratch | 0 dB | full until the last 3% of travel, then a quarter-cosine cut |

Each booth remembers the last curve it picked (`Settings::crossfader_curve`) and starts a session on it. Joining a room takes the host's.

### Four decks and the sync master

The room always has four decks (`model::DECKS`); whether C and D are on screen is each booth's own view. With more than two decks, "sync to the other deck" needs a reference, so the room has a **sync master** (`RoomState::master`), as in rekordbox and Serato:

- **Joining.** `Sync` on deck x syncs it to `sync_target(x)`: the master, or when there's none (or x is the master) another deck with a beatgrid, ranked synced and playing, synced, playing, any, the deck across the crossfader (A and B, C and D) first on ties. If there was no master, that deck becomes it.
- **The sync group** is the synced decks plus the master. A `Tempo` change on any deck in the group locks every other member to it (`follow_tempo`). When a deck's own BPM changes (a tempo section, a grid edit, a jump), the synced decks follow it if it's the master, and a synced deck re-locks to the master (`relock`).
- **Handover.** After every event, `settle_master` clears the master when no deck is synced. A master that isn't playing (or has lost its track or grid) hands over to a synced deck that is. Holding CUE or a hot cue from a pause is only a preview: it neither keeps nor takes the master. Loading a new track on the master, or ejecting it, gives up the master, so a stopped deck can be prepared without dragging the one you're hearing.
- **Keys** are compared with `mix_partner(x)`: the master, otherwise a deck that's playing, otherwise any deck with a track, across the crossfader first.

All of this is a pure function of the events, so both booths always agree on the master, and it's part of the digest. With two decks it behaves as before, except for the handover: once the master stops, its tempo fader no longer drags the synced deck.

On the DDJ-FLX6, decks 3 and 4 send on MIDI channels 3 and 4 and drive decks C and D. Until preset version 6 they drove A and B; `Mapping::upgrade_from` moves every saved binding that still does exactly that to C or D, and adds mixer channels 3 and 4 and their meters, leaving bindings the DJ changed alone.

### Browsing the library from a controller

The browse knob moves through either the tracks of the list on show or the lists on the left (`BrowseFocus` in `app.rs`). Pressing it switches between them, BACK goes to the lists, and a blue outline marks the side it's on. Turning through the lists shows each one's tracks straight away, like Serato and rekordbox. It doesn't rescan crates or All Music on the way, since All Music walks every folder; pressing the knob to go into one rescans it, as clicking it does. Crates are skipped while their section is closed. Clicking a list goes into it, so the knob goes back to its tracks; that keeps the old habit of picking a list with the mouse and a track with the knob. Turn, press and BACK reach the UI as `MidiOut`s, never as room events, so a booth's browsing never goes on the wire.

### Controller tempo faders

Faders and knobs use soft takeover (`mapping.rs`), but the tempo fader can't: SYNC moves a deck's tempo without moving the fader, and SHIFT + TEMPO moves the fader without moving the tempo, so a DJ who runs out of travel can slide it back and keep going. `MidiEngine::tempo_fader` keeps where the fader physically is, from either message (`Target::Tempo` and `Target::TempoShift`; on the DDJ-FLX6, CC 0x00 and CC 0x05).

- **On the fader** (it was at the deck's tempo before this move, within 0.05%): it sets the tempo directly.
- **Off the fader:** a move changes the tempo by exactly as much as the fader moved, from wherever the deck is. A quick move whose new reading happens to land on the deck's tempo doesn't put the deck on the fader, or one fast flick would undo what SHIFT gained.
- **Back on the fader** when SHIFT leaves it at the deck's tempo, someone moves the deck to where it sits (SHIFT + SYNC, the partner), or the tempo is held at the limit and the fader catches up.
- **The limit** is the room's ±50% (the model's clamp), not the fader's range: going past the range is what SHIFT is for. Held at the limit, the fader may run on by one coarse step (1/128 of its travel) before the rest is dropped. A 14-bit fader's coarse half arrives before its fine half, and that dip and return mustn't creep the tempo off the limit while the DJ pushes against it.

All of this runs in the booth whose controller it is; the room only sees the `Tempo` actions it sends. So it works the same on synced decks (the sync group follows them like any tempo change) and when the partner moves the same deck: the next move starts from wherever the deck is.

On a controller that sends the same message with SHIFT held (the Denon SC Live 4), `TempoShift` is a SHIFT + binding on the fader's own message (see below).

### SHIFT and pad modes tracked by Tandem

Pioneer controllers send other messages with SHIFT held and in each pad mode, so a binding is just a message and what it does. The Denon SC Live 4 doesn't: its pads send notes 0x0F–0x16 in every pad mode, and SHIFT changes nothing anything sends. So a binding can also need SHIFT, or a pad mode (`mapping.rs`):

- **SHIFT.** The `MidiEngine` keeps track of the buttons bound to `Target::Shift` (either side's shifts the whole controller, as in Engine OS). A binding marked `shift` works only while one is held, and then replaces the control's other bindings; a control with none does the same with SHIFT held. On the SC Live 4 that's SHIFT + pad (clear the hot cue), SHIFT + CUE, SHIFT + SYNC and SHIFT + tempo fader.
- **Pad modes.** Buttons bound to `Target::PadMode { deck, mode }` set the deck's `PadMode`: hot cues (the start), beat loops or rolls. A binding with a `mode` works only while its target's deck is in that mode, so each pad has a binding per mode and each side keeps its own. A mode button that isn't bound (the SC Live 4's SLICER) leaves the pads as they were.
- **Letting go** goes to whatever pressing did (`MidiEngine::held`), so a roll ends when its pad is let go even if the mode or SHIFT changed in between, and SHIFT let go before the pad doesn't turn a cleared hot cue into a hot cue release.
- **Lights** show what a control does without SHIFT, and a pad shows its binding for the deck's mode (off if it has none). A binding's `light` gives the values sent lit and unlit, instead of 127 and 0: Denon buttons are dim at 1 and lit at 127, and their pads take a colour number, two bits each of red, green and blue. The SC Live 4's hot cue pads light in the hot cues' on-screen colours, a held roll pad white.
- **Learning** with SHIFT held learns SHIFT + the control. A pad target (hot cue, clear, beat loop or roll) learned on a deck that has pad mode buttons works only in the mode the deck is in. SHIFT and the mode buttons keep working while learning, so the DJ sets them first.

All of it happens in the booth whose controller it is: the room only sees the actions it sends.

The SC Live 4's built-in mapping comes from two Traktor mappings for it and Mixxx's Denon Prime 4 mapping (Denon publishes no MIDI list), and is matched by its port name. Its first mapping was a file written by hand, with no preset. `Mapping::adopt` turns a mapping like that, made before its controller had a preset, into the preset when at least half its bindings are the preset's: it keeps the jog calibration and settings, invert and encoder style, and a button or knob bound to something the preset doesn't keeps its bindings as they were. When a preset gains controls, `Mapping::upgrade_from` adds each one unless the saved mapping already answers that message: the same button or knob, with SHIFT or without as in the preset, in a pad mode they share. So a later pad mode reaches saved SC Live 4 mappings too.

### Loading onto a deck

A `Load` puts the track on the deck paused at its cue point, whatever the deck was doing: it's never playing afterwards, and the deck's loop, sync, bend, CUE or hot cue preview, scratch and spin all go. Loading onto the sync master gives up the master.

Nobody loads over music the room can hear. `RoomState::audible_in_master(x, t)` is true when deck x is making sound at t (`DeckState::sounding_at`: playing, previewing a cue, under the DJ's hand or still spinning, and not run off either end) and `render::master_gain` puts it above `SILENT_GAIN`, −40 dB. `master_gain` multiplies the mixer's own gain laws: trim, the loudest EQ band, the filter, the channel fader, and the crossfader for the channel's side and the room's curve. The headphone cue doesn't count. Each of these makes a deck silent on its own:

| Control | Silent at | Why there |
|---|---|---|
| Channel fader | about 10% and below | its gain is position², so 10% is −40 dB |
| Crossfader | the far end from the channel's side; never on THRU | every curve cuts the far end completely. Short of it, only Linear gets under −40 dB, in its last 1% |
| Filter | the last 1% of travel, either way | the sweep alone still lets the bass (low-pass) or the hats (high-pass) through, and only the fade over the last 10% silences it. Measured on pink noise, 1% from the end lets through −40 dB on the low-pass side and −45 dB on the high-pass side; further in, the rule counts only the fade, which errs towards "audible" |
| Trim, EQ | trim all the way down, or all three bands killed | both are zero gain there |

They multiply, so smaller cuts add up (the fader at 30%, −21 dB, with trim down 21 dB is −42 dB). Boosts count too: trim at +12 dB needs the fader lower.

Two places enforce it:

- **The shared model.** `RoomState::apply` drops a `Load` onto a deck that's audible at the event's own time, before changing anything (not even who touched the deck). Faders, crossfader, curve, assign switches, filter, trim and EQ are all room state, so every booth and the broadcast lane decide the same way whatever order events arrive in. If one DJ loads just as the other brings that deck up, the earlier event wins on both booths. A partner's load arriving over the network gets the same treatment.
- **The booth that loads.** `Session::act_at` refuses a `Load` the model would drop, and `load_path` refuses before decoding and again once the track is ready, since a fader may have come up meantime. Both queue a notice (`Session::take_notices`) that the app shows as a toast. Every way of loading (library buttons, right-click, double-click, the controller's LOAD buttons) goes through these. A double-click picks the first stopped deck on screen, otherwise the quietest silent one. If a partner's earlier move arrives late and the room drops a load this booth already sent, `check_my_loads` notices (the deck doesn't have the track at the load's time) and gives the same notice.

### Local vs shared

| Shared (in the timeline) | Local (this booth only) |
|---|---|
| decks: load, play, cue, hot cues, loops and loop rolls, tempo, bend, nudge, beatgrids | headphone cue (PFL) per deck |
| channel faders, EQ, crossfader, its curve and the assign switches | headphone mix and level |
| the sync master | master and broadcast levels, sync click |
| | showing decks C and D |

**Level knobs.** A controller's MASTER LEVEL and HEADPHONE LEVEL knobs (`Target::MasterLevel`, `Target::HeadphoneLevel`) set this booth's `LocalParams` levels directly from the MIDI thread, with soft takeover against the sliders on screen, and never send an action. On a controller with a sound card, the same knobs also turn its own outputs up and down in hardware. So when the master or the headphones play through it, the knob leaves that level alone; otherwise the sound would be turned down twice. `ControllerCard` decides by name: Windows names a controller's sound card after it (MIDI port "DDJ-FLX6", sound card "Line (DDJ-FLX6)"), so it's the controller's when its name contains the port's, ignoring case, punctuation, the "2- " prefix and a trailing "MIDI". The app works it out every frame from the controller's port and the devices the audio actually opened, the system default included. The FLX6 preset has both knobs from preset version 11 (14-bit, CC 0x08 and 0x0D on channel 7), so saved FLX6 mappings pick them up.

### Tracks

A track's id is the BLAKE3 hash of its file, so identical files match whatever they're called.

When a deck loads a track, the other booth first looks in its own library. It checks known hashes first, then hashes any files of exactly the same size. Only if that finds nothing is the file transferred.

Every peer decodes with the same decoder (symphonia) and the loader's exact duration travels with the `Load` event. That keeps both booths' samples identical.

## Network

- **Topology.** The host listens on TCP 47800. The joiner opens one *control* connection, plus one *files* connection per transfer. Files have their own connection so a 60 MB track never queues ahead of a fader move. Up to 4 broadcasters connect the same way, after or alongside the joining DJ; the host is the hub, so neither the joining DJ nor a broadcaster needs to be reachable.
- **Upload cap.** All of a booth's uploads together are capped at 2 MB/s by default (configurable), so they don't flood the uplink and delay the control link's messages behind them in the router. One `files::UploadPacer` per session books each 128 KB chunk the next free slot, so a host sending to its partner and two broadcasters at once takes turns at one rate.
- **Direction.** Connections always go joiner to host, so only the host needs reachability. The host asks the joiner to upload with `PushRequest`.
- **Format.** Length-prefixed JSON frames, plus a binary payload for file chunks (`protocol.rs`).
- **Getting back in.** A link that hears nothing for 15 s (`LINK_TIMEOUT`) is dropped; after 3 s (`LINK_QUIET`) the top bar already says so. A link that ends without a `Bye` (a timeout, or the connection closing) was lost, not left: a joining DJ or broadcaster booth then rejoins the same address and room by itself (`net::rejoin_loop`), trying after 1, 2 and 4 s and then every 5 s for 5 minutes (`REJOIN_FOR`), with "Reconnecting to X… attempt N" and **Give up** in the top bar. Getting back in is a fresh join: the same handshake and clock sync, and the room exactly as the host has it now (`Session::on_joined` with `rejoin`). Moves the booth made while cut off, and any still in flight when the link went, never reached the room, so they're dropped with the rest of its timeline; its local settings, library, decoded tracks and event sequence numbers carry on, and tracks on the decks it hasn't got are fetched again. Meanwhile it plays on from its own timeline: a DJ's decks keep going (their moves stay local), and a broadcaster's stream plays the room as it last was instead of going silent. It stops at a `Reject` (another version, a new room code), after giving up, or after 5 minutes, and is then on its own as before (a broadcaster's decks empty). A `Bye` ends a link on purpose and never starts a rejoin, so `leave` waits up to 300 ms for its `Bye`s to go out before closing, and the writer closes the connection behind a `Bye`.
- **Transfers carry on.** A track transfer cut off part-way picks up where it got to, in either direction. The receiver keeps what arrived in a `.part` file in `session-cache` named after the track's hash, so any later download of that track this session continues it, whatever started it: the booth getting back in, a rejoin by hand, a share. It asks for the rest by offset (`FileGet`'s `offset`; for an upload, the host answers the joiner's `FilePut` with the offset in `FileOk`), re-hashes what it has, and checks the whole file's hash at the end as always. If that fails, the partial file was bad: it's cleared and the track sent again from the start. When a link drops, every transfer with that booth stops at once, and its downloads are paused rather than failed (`TransferState::Paused`, with the track still "receiving"). They carry on when the same booth is back (`Session::on_joined` on a joiner, `add_peer` on the host), and fail once it isn't coming back (the rejoin gave up, the host stopped waiting, or someone left). A download whose connection breaks while the link stays up waits a second, so a link that's dropping too is noticed first, then carries on. A finished file goes to a fresh name in the share folder, or into the session cache, only after it checks out.
- **One writer per `.part` file.** Every transfer registers in a `files::Running` with the booth at the other end and the `.part` file it writes. A transfer that wants a file another is still writing (one whose link dropped, blocked on its dead connection, or one that stalled) stops it by closing its connection and waits for it to let go, so two transfers never write one file. A stopped transfer isn't reported as failed, and every download try has a serial, so an older try ending can't settle a newer one. The wire change is three fields with defaults (`FileGet.offset`, `FileHeader.offset`, `FilePut.resume`) and `FileOk.offset`: a booth from before them asks for and sends whole files, and is answered with whole files, so they share a room.
- **Same booth, same seat.** Every `Hello` carries the booth's id and a booth key, a secret from the OS's randomness that the booth keeps for its lifetime (the id is no secret: every event carries it). The host gives a booth with a matching id and key its seat back even while it still holds the old, dead link, closing that one (`Session::seat_for`, `evict`), so there's no second DJ or broadcaster of the same name and no "room full". Every link has a serial, so the old link ending later can't drop the new one. A host that was frozen finds the connections the booth left behind at each timed-out try waiting all at once; it skips those the booth has already closed (`net::closed_already`), so they can't throw out its live try. While a DJ is expected back, the host shows "Waiting for X to reconnect…". A host from before booth keys turns a returning booth away as room full until the old link times out, so a rejoin keeps trying through that for 25 s.
- **Security.** There's no encryption at this layer, so run it over Tailscale. The room code keeps strangers out but is not a password, and anyone with it can join as a broadcaster. The host only serves files that are loaded on a deck or offered in a share, and only accepts uploads from the joining DJ.

## Sharing and consent

- **Sender side.** The sender confirms a pledge ("I own or am licensed to use these tracks…") before the offer is sent.
- **Receiver side.** The receiver sees a popup naming the sharer and the non-commercial terms. They must tick the pledge before *Accept and download* is enabled. Declining sends nothing.
- **Record.** Both sides append to `consent-log.jsonl`: time, terms version, names, share title, exact statement and track list.
- **Session copies.** A track your partner plays that you don't have lives in `session-cache` and is deleted on exit, as are the `.part` files of transfers that never finished (shares included). The terms you accept on first launch cover this.

## Code map

| Crate | What |
|---|---|
| `tandem-core` | Everything platform-independent: `time`, `clock`, `model`, `spin` (records let go mid-spin), `render`, `meter` (level meters), `audio` (decode), `net`, `files`, `session`, `library`, `serato`, `mapping` (MIDI learn and soft takeover), `terms`, `selftest`. Tests run anywhere. |
| `tandem` | The Windows app: egui UI (`ui_*.rs`, including the broadcast window in `ui_broadcast.rs` and a broadcaster booth's main window in `ui_broadcaster.rs`), sound output via cpal/WASAPI (`output.rs`), MIDI via midir (`midi.rs`). |
