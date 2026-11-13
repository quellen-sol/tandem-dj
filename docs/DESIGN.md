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

A deck's playhead is analytic: `position(t) = anchor_pos + (t − anchor_time) × rate`. No audio has to run for the position to be known.

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

### Key lock and key shift

With key lock on, or with a key shift, a read head feeds the track through Signalsmith Stretch, which changes speed without changing pitch (and pitch without changing speed). The stretcher's latency is part of the read head's maths: the audio heard now is `fed position − input latency − speed × output latency`. Chasing and sync therefore work exactly as in vinyl mode. When starting at a cue point, the stretcher is primed and pre-rolled, so the first sample out is the cue point itself. A test checks that key-locked kicks land within 4 ms of vinyl ones. The stretcher uses a fixed random seed, so both booths still render identical audio. Scratching always uses vinyl mode.

### BPM, beatgrid and key detection

`analysis.rs` builds a kick-weighted onset envelope and sweeps every tempo from 68 to 190 BPM over the whole track. Each tempo is scored together with its double and half, with a mild preference for dance tempos, so dotted 2-step rhythms and half-time grooves don't win. The beat phase comes from the same comb. `keys.rs` folds sustained note energy into pitch classes and matches them against Temperley's major/minor key profiles. Results are cached per file (and versioned, so better detection re-runs). They travel with the `Load` event, so both booths use the same grid and key.

### Beatgrids and editing them

A grid (`grid.rs`) is a list of up to 32 markers. Each marker sits on a downbeat and starts a section with its own BPM, which runs until the next marker; analysis gives a one-marker grid. Bars are four beats: the first marker is bar 1, the bars before it count -1, -2…, and every marker starts a new bar. A section's last beat stretches or shrinks by up to half a beat to meet the next marker.

Edits (SET, DELETE, ADJUST, SLIP, ½/×2, RESET) are worked out in the UI from the deck's current grid and sent as one `SetGrid` event carrying the whole new grid, so both booths apply exactly the same thing. ADJUST and SLIP only touch the section under the playhead. Each booth saves an edited grid to `beatgrids.json`, keyed by content hash, and uses it the next time that track loads. Analysis never overwrites it, and RESET removes it. The library cache keeps the detected grid, so RESET can go back to it.

Grids are boxed inside `Action` (which is therefore `Clone`, not `Copy`) so the common events stay small. `DeckState` keeps its grid inline, so the state stays `Copy` and the audio threads never allocate.

**Sync through tempo changes.** A synced deck's tempo is matched to the other deck's BPM *at the playhead*. When a playing deck reaches a marker where its BPM changes, the lock has to be redone at that exact moment. The host (or a booth on its own) looks ahead every maintenance tick and schedules a `Relock` event for the moment the deck reaches the marker; if it missed one, it sends a `Relock` straight away. A `Relock` recomputes the lock from the state when it's applied, so a spare one does nothing. Jumps (seek, hot cue, cue, jog) into a section with another tempo, and grid edits, relock as part of applying that event.

### Local vs shared

| Shared (in the timeline) | Local (this booth only) |
|---|---|
| decks: load, play, cue, hot cues, loops, tempo, bend, nudge, beatgrids | headphone cue (PFL) per deck |
| channel faders, EQ, crossfader | headphone mix and level |
| | master and broadcast levels, sync click |

### Tracks

A track's id is the BLAKE3 hash of its file, so identical files match whatever they're called.

When a deck loads a track, the other booth first looks in its own library. It checks known hashes first, then hashes any files of exactly the same size. Only if that finds nothing is the file transferred.

Every peer decodes with the same decoder (symphonia) and the loader's exact duration travels with the `Load` event. That keeps both booths' samples identical.

## Network

- **Topology.** The host listens on TCP 47800. The joiner opens one *control* connection, plus one *files* connection per transfer. Files have their own connection so a 60 MB track never queues ahead of a fader move.
- **Upload cap.** Transfers are capped at 2 MB/s by default (configurable) so they don't flood the uplink.
- **Direction.** Connections always go joiner to host, so only the host needs reachability. The host asks the joiner to upload with `PushRequest`.
- **Format.** Length-prefixed JSON frames, plus a binary payload for file chunks (`protocol.rs`).
- **Security.** There's no encryption at this layer, so run it over Tailscale. The room code keeps strangers out but is not a password. The host only serves files that are loaded on a deck or offered in a share.

## Sharing and consent

- **Sender side.** The sender confirms a pledge ("I own or am licensed to use these tracks…") before the offer is sent.
- **Receiver side.** The receiver sees a popup naming the sharer and the non-commercial terms. They must tick the pledge before *Accept and download* is enabled. Declining sends nothing.
- **Record.** Both sides append to `consent-log.jsonl`: time, terms version, names, share title, exact statement and track list.
- **Session copies.** A track your partner plays that you don't have lives in `session-cache` and is deleted on exit. The terms you accept on first launch cover this.

## Code map

| Crate | What |
|---|---|
| `tandem-core` | Everything platform-independent: `time`, `clock`, `model`, `render`, `audio` (decode), `net`, `files`, `session`, `library`, `serato`, `mapping` (MIDI learn and soft takeover), `terms`, `selftest`. Tests run anywhere. |
| `tandem` | The Windows app: egui UI (`ui_*.rs`), sound output via cpal/WASAPI (`output.rs`), MIDI via midir (`midi.rs`). |
