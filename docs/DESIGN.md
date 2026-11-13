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

### Local vs shared

| Shared (in the timeline) | Local (this booth only) |
|---|---|
| decks: load, play, cue, hot cues, tempo, bend, nudge | headphone cue (PFL) per deck |
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
