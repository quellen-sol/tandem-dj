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
- **Leaving a loop** shorter than a beat can leave a synced deck between beats, so the deck re-matches its phase. Whole-beat loops don't move.
- **Relocks** for tempo changes only look at markers inside the loop, plus the jump back to the in point.

Read heads go round loops by themselves. They read the track through the loop: frames past the out point come from the in point, and the last 96 frames (about 2 ms) before the out point fade into the frames just before the in point. Going round is therefore seamless and click-free, through the stretcher too, with no jump and no re-priming. The chase measures its error the short way round the loop.

A loop that appears or changes under audio the head has already read makes the head hard-sync with the usual 2 ms crossfade. That happens with LOOP OUT pressed at the playhead, a partner's loop arriving a little late, and always with a stretched head, because the stretcher reads ahead.

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

**Sync through tempo changes.** A synced deck's tempo is matched to the other deck's BPM *at the playhead*. When a playing deck reaches a marker where its BPM changes, the lock has to be redone at that exact moment. The host (or a booth on its own) looks ahead every maintenance tick and schedules a `Relock` event for the moment the deck reaches the marker; if it missed one, it sends a `Relock` straight away. A `Relock` recomputes the lock from the state when it's applied, so a spare one does nothing. Jumps (seek, hot cue, cue, jog) into a section with another tempo, and grid edits, relock as part of applying that event.

### Crossfader curve

The curve is part of the room, like the curve switch on a club mixer. A `CrossfaderCurve` event sets it for both booths, and the broadcast lane switches at the moment it changed. `render::xfader_gain` gives each deck's gain. Deck B's curve is deck A's, mirrored.

| Curve | Each deck in the middle | Shape |
|---|---|---|
| Linear | −6 dB | straight fades across the whole travel |
| Constant power (default) | −3 dB | quarter-cosine fades across the whole travel; the decks' powers add up to one |
| Full centre | 0 dB | full up to the middle, then a quarter-cosine fade over the far half |
| Scratch | 0 dB | full until the last 3% of travel, then a quarter-cosine cut |

Each booth remembers the last curve it picked (`Settings::crossfader_curve`) and starts a session on it. Joining a room takes the host's.

### Local vs shared

| Shared (in the timeline) | Local (this booth only) |
|---|---|
| decks: load, play, cue, hot cues, loops, tempo, bend, nudge, beatgrids | headphone cue (PFL) per deck |
| channel faders, EQ, crossfader and its curve | headphone mix and level |
| | master and broadcast levels, sync click |

### Tracks

A track's id is the BLAKE3 hash of its file, so identical files match whatever they're called.

When a deck loads a track, the other booth first looks in its own library. It checks known hashes first, then hashes any files of exactly the same size. Only if that finds nothing is the file transferred.

Every peer decodes with the same decoder (symphonia) and the loader's exact duration travels with the `Load` event. That keeps both booths' samples identical.

## Network

- **Topology.** The host listens on TCP 47800. The joiner opens one *control* connection, plus one *files* connection per transfer. Files have their own connection so a 60 MB track never queues ahead of a fader move. Up to 4 broadcasters connect the same way, after or alongside the joining DJ; the host is the hub, so neither the joining DJ nor a broadcaster needs to be reachable.
- **Upload cap.** All of a booth's uploads together are capped at 2 MB/s by default (configurable), so they don't flood the uplink and delay the control link's messages behind them in the router. One `files::UploadPacer` per session books each 128 KB chunk the next free slot, so a host sending to its partner and two broadcasters at once takes turns at one rate.
- **Direction.** Connections always go joiner to host, so only the host needs reachability. The host asks the joiner to upload with `PushRequest`.
- **Format.** Length-prefixed JSON frames, plus a binary payload for file chunks (`protocol.rs`).
- **Security.** There's no encryption at this layer, so run it over Tailscale. The room code keeps strangers out but is not a password, and anyone with it can join as a broadcaster. The host only serves files that are loaded on a deck or offered in a share, and only accepts uploads from the joining DJ.

## Sharing and consent

- **Sender side.** The sender confirms a pledge ("I own or am licensed to use these tracks…") before the offer is sent.
- **Receiver side.** The receiver sees a popup naming the sharer and the non-commercial terms. They must tick the pledge before *Accept and download* is enabled. Declining sends nothing.
- **Record.** Both sides append to `consent-log.jsonl`: time, terms version, names, share title, exact statement and track list.
- **Session copies.** A track your partner plays that you don't have lives in `session-cache` and is deleted on exit. The terms you accept on first launch cover this.

## Code map

| Crate | What |
|---|---|
| `tandem-core` | Everything platform-independent: `time`, `clock`, `model`, `spin` (records let go mid-spin), `render`, `meter` (level meters), `audio` (decode), `net`, `files`, `session`, `library`, `serato`, `mapping` (MIDI learn and soft takeover), `terms`, `selftest`. Tests run anywhere. |
| `tandem` | The Windows app: egui UI (`ui_*.rs`, including the broadcast window in `ui_broadcast.rs` and a broadcaster booth's main window in `ui_broadcaster.rs`), sound output via cpal/WASAPI (`output.rs`), MIDI via midir (`midi.rs`). |
