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

The host's monotonic clock is the room clock. Each joiner pings the host, NTP-style, and trusts the sample with the smallest round trip. That sample has the least room for network asymmetry. Pinging continues once a second to track drift. See `clock.rs`.

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
- **Broadcast lane (delay 500 ms by default):** by the time it reaches a moment, events from every DJ for that moment have arrived, so the stream has every cut exactly where it was played.

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
- **It learns who's in the room from the host,** whose `Members` message lists the DJs and broadcasters whenever that changes. The DJs show the broadcasters in the top bar, and the broadcaster names every DJ on the broadcast window.

The host relays the joining DJs' events and track details to the broadcasters as they arrive, as it does to the other DJs (see [Rooms of up to four DJs](#rooms-of-up-to-four-djs)), and sends its own events, track details and digests to everyone. A broadcaster sees a joining DJ's moves after two network hops instead of one, which the broadcast delay absorbs.

Tracks come from the host over the usual files connections. The host serves every track on a deck, including ones it found in its own library and ones it downloaded from a joining DJ. A broadcaster can ask for a partner's track before the host has it, so the host holds the download for up to 20 s while it's still arriving (`Session::pending_wait`), then tells it to ask again (`FileError::later`), and the download goes to the back of its queue and asks again (`Session::fetch_later`) rather than failing. A client also asks again every 5 s for a track on a deck that failed to download.

A broadcaster that loses the room empties its decks rather than carrying on with them as a solo booth.

**Timing.** A move is exact on the stream if it reaches the broadcaster before its broadcast lane plays that moment: within the broadcast delay, less the sound card's buffer. The joining DJs' moves take two hops, so a broadcaster's delay is its own setting (`AudioSettings::broadcaster_delay_ms`, 1000 ms by default; a DJ's stays 500 ms). Each event carries the shared time it happened, so a booth measures how late every move from the room arrives (`Session::note_lateness`, moves made since it joined, a scheduled one counting as on time) and keeps the latest per second for a minute (`LinkView::late_us`). Within 100 ms of the delay the broadcaster warns, and offers `config::broadcast_delay_for`: a quarter second over the lateness, rounded up to a quarter second, at most 2000 ms. It never changes the delay by itself, since the stream repeats or skips by the difference when it does.

A move that's late anyway still lands in time with the music: the timeline puts it at the moment it happened, so a play or hot cue starts on the beat, missing its first few ms, and a fader moves late by the excess. One more than the delay plus 3 s late is clamped to the timeline's base and leaves the booth out of step until a resync.

### Rooms of up to four DJs

A room takes up to four DJs (`net::MAX_DJS`): the host and three who join. Nothing in the model knows how many there are: every event carries its DJ's id (`origin`), the timeline folds them all the same way, and every DJ can work every deck. Only the links change.

- **The host is the hub.** Every joining booth, DJ or broadcaster, has one link, to the host (`Session::peers` holds that one link on a joining booth, and every joining DJ's link on the host). The host passes each DJ's events and track details on to everyone else, the other DJs and the broadcasters (`Session::relay`, skipping the link it came on), and sends its own to everyone. So a move between two joining DJs takes two network hops, as it always did to a broadcaster: each hears the other about one round trip to the host late instead of half of one. The broadcast lane's 500 ms covers that on any ordinary connection.
- **Tracks go through the host too.** The host remembers which DJ told the room about each track (`track_from`: the `Track` message came on their link, or with their cut-off moves, or in their share) and asks that DJ for it (`PushRequest` on their link only, `Session::source_for`); a track it doesn't know the source of goes to any linked DJ, as it always went to the one. Another joining DJ gets the track from the host, waiting if the host is still getting it (see [Broadcaster booths](#broadcaster-booths) for `FileError::later`). An upload is only taken from the DJ it was asked of (`accept_push`). Every download remembers the booth it comes from (`Fetch::from`), so when one DJ's link drops only the transfers with them pause, and they carry on when that DJ is back.
- **Shares go to every other DJ.** A joining DJ's offer goes to the host, which passes it on to the other DJs (not the broadcasters), and their answers back the same way. A DJ who accepts downloads from the host. The host doesn't take a share it didn't accept: when an accepting DJ asks for a track it hasn't got, it fetches it from the sharer then (`Session::servable_for`, `Purpose::Relay`: into the session cache, not decoded) and passes it on.
- **Lists shared live.** A DJ can share a list (a crate of theirs, a folder or a Serato crate) live: its track details go to the other DJs, and the files stay put until someone loads one (see [Crates and lists shared live](#crates-and-lists-shared-live)).
- **Getting back in works per DJ.** The host keeps a list of DJs it expects back (`awaiting`), each with their seat, and its held room reaches back for the one heard from longest ago. A DJ's cut-off moves merged into the room reach the others in a `Resync` (see [Network](#network)).
- **Leaving.** A joining DJ leaving, or giving up getting back in, doesn't change the room for anyone else, and frees their seat; the host leaving closes the room for everyone, as before.
- **On screen.** The host's top bar shows each joining DJ with their own round trip and light, and "Waiting for X to reconnect…" for each one expected back. A joining DJ's shows the other DJs by name and its round trip to the host. Decks name whoever touched them last (`SessionView::name_of`, from the room's list of DJs), and the broadcast window names every DJ, "A b2b B b2b C".

### Crates and lists shared live

A DJ's own crates are named lists of paths, in the order they were added, kept in `crates.json` beside the library cache (`library::MyCrate`). A crate is listed like any other source (`SourceKind::Mine`): files that have gone are left out, and the list's error says how many. All Music includes them. **⟳ Resync folders** (`library::resync`) rediscovers Serato crates and rescans every folder, Shared with me and each crate that's been listed, keeping the old lists on screen. It snapshots the files those lists held, and when the last of them finishes compares, so the UI can say how many tracks came and went.

Sharing a list live puts its tracks' details in front of the other DJs without sending any files:

- **On the wire.** `Msg::Crate` carries a whole `RoomCrate`: its owner, an id the owner picked, a name and up to `ROOM_CRATE_MAX` (500) `TrackInfo`s, about 200 KB at most. It's sent again whenever the list changes, and `CrateGone` takes it away. The control connection carries them, between the moves; 500 tracks keeps one to a moment's worth of a home uplink. The host passes each DJ's on to the other DJs (never the broadcasters), takes them only from their owner, and sends a DJ who joins every list shared so far, its own included. When a DJ leaves for good (a Bye, or the host stops waiting for them), the host drops their lists and tells the others.
- **Keeping it current.** The owner's app watches the source list's version (`TrackList::rev`). A change in which files it holds goes out within a second; new details alone (BPMs being analysed) at most every 10 s. `Session::update_live` works out each track's room details in the background from the library cache (`library::room_info`: hashing only files it hasn't hashed before), and sends the list only if something the others see changed. It registers every file as one this booth can serve (`servable`).
- **Through a dropped link.** The host keeps a DJ's lists while it expects them back (the others don't show them meanwhile: a list shows only while its owner is in the room's list of DJs). On getting back in, a DJ sends its lists again, and `CrateGone` for any it stopped sharing while cut off (`unshared_away`). The host passes on only the ones that changed meanwhile: a list it already has, just as it is, goes no further, so the other DJs' moves don't wait behind a list they already have.
- **Loading from one.** `Session::load_room_track` sends the track's `Track` message (if the room doesn't know it), then loads it, exactly like a DJ's own load: every booth that hasn't got the file fetches it. The host gets it from the list's owner: a track that's in a linked DJ's live list comes from them, whoever loaded it (`crate_owner`, in the `Track` handler, `source_for` and `take_cut_off`). The loader's own download can reach the host before its load does, so `servable_for` fetches a live list's track from its owner when asked for it, as it does for a share's (`Purpose::Relay`).
- **Tracks nobody analysed.** A load carries its beatgrid and key, so every booth plays the same grid. A track the owner never analysed has neither, and a later `SetGrid` would make every booth forget a grid it had edited for the same file. So the loader gets the track first (`pending_loads`): it fetches and decodes it, analyses it (`library::analysis_for`, using the loader's own edited grid if it has one), and only then sends the load, as a load from the library does after decoding. The deck shows it loading meanwhile. If another track goes on the deck first (any DJ's load, or the loader's), the waiting load doesn't land over it, and says so. If the track can't be got, it gives up with a note.

### Frame rate

The UI redraws at the **Screen frame rate** setting (60 fps by default). Frames are due on a fixed beat (`TandemApp::pace_frames`), so vsync rounding a frame up to the next monitor refresh doesn't drag the average down. Input still redraws at once. Controller moves go straight from the MIDI thread to the session with their own timestamps, so they never wait for a frame. Note that egui brings a requested repaint forward by its predicted frame time (1/60 s): asking for a repaint after 16 ms means "now", which is why the UI used to run at the monitor's full rate.

### Small screens

The decks area and the library share the window's height by rule (`DeckLayout` in `ui_decks.rs`, `library_height` in `ui_library.rs`). At 1440 × 930 points and up nothing changes. On a shorter window:

1. **The library gives way first.** Until the DJ drags its edge, its height follows the window: 330 points when there's room, less on a shorter one, never under 150. After a drag it keeps the DJ's height, but can't be dragged over the waveforms' minimum.
2. **Then the decks.** Two decks swap their full panels for the four-deck view's compact panels, beside a short two-channel mixer: each channel's knobs beside its fader, and the booth levels behind a **🎧 LEVELS** button. They do this as soon as keeping the full panels would leave the library under 180 points, and on a window too narrow for full panels (under about 1190 points wide). Four decks squeeze in stages: thinner track overviews, then tighter panels, with the levels behind the button.
3. **The waveforms never go under a minimum** (46 points each for two decks, 28 for four). When the controls don't fit above them even squeezed, the controls scroll, with a scroll bar in sight.
4. **The FX strip** sits under the waveforms: one row (19 points, 22 with its spacing) for two decks, two for four. The DJ opens and closes it with **FX** in the top bar (`Settings::fx_strip`, this booth's own). **Open, it pushes the library down by its own height, never under the library's 150 points** (`library_height`): at 1440 × 930 the library is about 310 points instead of 330, and the decks and waveforms are where they were. It doesn't change which panels the decks get: full or compact is decided as if the strip weren't there, so 1080p at 125% keeps its full panels over a library of about 167 points. Where the library is already at its least, the strip takes what the waveforms have over their minimum, then what the four-deck panels can squeeze. How it shows is worked out from the window by `FxStrip::pick`, with the library at its least and the controls at their least without scrolling (compact panels for two decks, the squeezed row for four): open if it fits there; otherwise it takes no height and each deck's effects show in its waveform's corner, where a click opens the deck's bank. So the strip never makes the controls scroll or the library go under 150. In practice: with two decks it's open on every window from 1097 × 538 up; with four from about 684 points tall (1366 × 689 fits), and on the waveforms under that (1280 × 641). Closed, nothing of it shows and the library has its height back; a deck with an effect on still says **FX** in its header. A dragged library leaves an open strip its room (`library_max_height`).

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

### Effects

Every deck has a bank of effects after its channel fader and the crossfader, like a Pioneer mixer's Beat FX: three slots in series, each with an effect, on/off and a depth, and one BEATS value per bank (Serato's model). The first effects are echo, reverb and phaser.

**Room state.** `DeckState::fx` is an `FxBank`: three `FxSlot`s (`kind`, `on`, `depth`, `since`) and `beats`, an index into `model::FX_BEATS` (1/16, 1/8, ¼, ½, ¾, 1, 2, 4, 8, 16, 32): an index rather than a float, because JSON floats without `float_roundtrip` have already broken digests. `FxOn`, `FxDepth`, `FxSelect` and `FxBeats` set explicit values (not toggles), so late and reordered moves converge. They touch nothing else: no jump, no anchor, no roll let go. `since` is the time a slot was switched on or changed effect, a starting point every booth agrees on. The bank belongs to the channel: `Load` and `Eject` leave it alone. All of it is in the digest.

**Audio** (`fx.rs`). Each lane has an `FxChain` per deck, with an instance of each effect per slot. Per sub-block, `Lane::render` works each deck to its fader into the deck's buffer, runs its chain over the block, then sums the decks in the same order as before. `FxChain::prepare` reads everything an effect needs from the room at the block's time: the bank, the deck's BPM (`effective_bpm_at`, 120 without a grid) and the phaser's sweep, from `BeatGrid::phase(slip position, 1 / beats)` (from `since` at 120 BPM without a grid). So every lane, the broadcast lane included, works out the same effect at the same moment, whenever it started: a phaser started half a second later agrees within 0.001 once settled, and two lanes rendering the same moment are bit-identical.

- **Off and silent costs nothing and changes nothing.** An effect that's off fades its input out over 10 ms and keeps running until its tail is under −120 dBFS (for the echo, everything it could still play; for the reverb, twice its longest path). Then it goes idle and forgets its buffers: the echo's delay line counts the frames written since it was cleared and reads anything older as silence, so clearing it is free. While every effect of a deck is idle, the deck's signal doesn't go near the chain, and the mix is bit-identical to Tandem's before effects: tests check it against the same room without effect moves and after an echo has rung out, and rendering a busy room with both versions gave the same bytes.
- **Audio-thread rules.** Every buffer is allocated in `Lane::new` (about 2.6 MB per deck per lane at 48 kHz, mostly echo lines). Nothing in the callback allocates, locks or does unbounded work. Values in feedback paths are flushed to zero under 1e-20, far above denormals, so a minute of silence with every effect on leaves none (tested at 44.1 and 48 kHz).
- **Switching** feeds a slot's other effect at once; the old one rings out. On/off fades the effect's input over 10 ms, and depth is smoothed over 10 ms like the faders, so turning it down silences it at once.

| Effect | Depth | BEATS | Measured |
|---|---|---|---|
| Echo | the echoes' level, 0 to 0.6 of the dry sound (which stays at unity); feedback 0.35 to 0.7 | the delay; over 2 s it halves until it fits | repeats within 1 ms of BEATS × 60 / BPM at 44.1 and 48 kHz; each a little duller (one-pole low-pass at 7 kHz and high-pass at 90 Hz in the loop). A new delay (tempo, BEATS) crossfades over 30 ms: no click, no pitch glide |
| Reverb | up to half: the tail comes up under the dry sound; above, the dry sound fades out and the tail comes up by 60% more, to reverb alone | decay: 60 dB in 2 × BEATS beats, 0.3 to 10 s | Freeverb's 8 combs and 4 all-passes per channel, lengths scaled to the sample rate, input high-passed at 200 Hz, damping 0.2, no randomness. Its Schroeder decay (300 Hz – 1.5 kHz) lands within 10% of the stated time from 0.3 to 10 s. The tail's level is evened out across decay times (within 0.45×–1.6×) so BEATS doesn't swing it |
| Phaser | notch depth: 0 is the dry sound exactly; feedback up to 0.3 | the sweep's period | six first-order all-passes, swept 200 Hz – 4 kHz evenly in pitch, coefficient updated per 64-frame block and ramped per frame. No tail |

Levels are conservative, since loud masters already reach the soft clipper: on loud noise at full depth no effect raises the level by more than 2 dB (echo +1.7, reverb at half depth +0.6, phaser −1 to −2 dB). A test holds them there.

**Cost** (`examples/fx_bench.rs`, one lane at 48 kHz in 10 ms callbacks, deck A playing, best of seven): reverb about 0.3–0.4% of one core, echo about 0.05–0.1%, phaser about 0.05–0.12%; settings moved but nothing on, nothing measurable (the lane with every effect off costs what it did before effects). All twelve slots on all four decks: about 2–3%. Measured on Linux and natively on Windows.

### Controller effects

A controller has **FX units**: sections of controls that each work one deck's bank. `MidiEngine::fx_deck` says which deck each unit works, starting from the mapping's `fx_unit_decks`; `FxAssign` and `FxAssignNone` move it. The room never sees units, only the actions they send. Targets: `FxOn`, `FxDepth`, `FxDepthAll` (one knob for all three), `FxBeatsDown` / `FxBeatsUp`, `FxNext`, `FxAssign`, `FxAssignNone` and `FxFocus`, all learnable.

**The depth knob.** The DDJ-FLX6 has one LEVEL/DEPTH knob per unit, shared by its three slots: it sends the selected slot's CC pair, and every FX SELECT press sends the knob's position for the new slot. So:

- **A slot that's off** takes the knob as it is. Nobody can hear it, so there's nothing to pick up.
- **Switching a slot on** from the controller first sets its depth to where the knob is (if the engine knows), then switches it on, and the knob is in control from there.
- **A slot that's on** waits for the knob to pick it up, without the "untouched control" shortcut, so touching the knob never makes a running effect jump.
- **After `FxFocus`** (FX SELECT), the next reading only says where the knob is, like the tempo fader after a DECK button. A 14-bit knob sends two halves, and right after connecting Tandem doesn't know the knob is 14-bit until a fine half arrives, so a coarse half read this way takes the fine half straight after it along (`Takeover::relearn_fine`). The FLX6 can send the position twice in one press (recorded), so a reading that repeats the last one isn't a move either. FX SELECT also drops the knob's hold on a running slot: it may have been turned for another slot meanwhile.
- **Pointing a unit at another deck** makes a running slot wait for pickup against that deck's depth (the usual "someone else moved it" rule).

**Lights.** `FxOn` bindings light while their slot is on (on the FLX6, 127 flashes the ON/OFF light, 0 makes it steady). A unit's BEATS indicator is `Mapping::fx_beat_lights`: a CC whose value comes from a table per BEATS value, sent like the meters, and a dark value for a unit that works no deck (and when Tandem lets go). The FLX6 shows only the selected slot's ON/OFF light and the selected unit's BEATS, and ignores messages for the others, so on every `FxFocus` the engine asks the controller thread to forget what it sent those two (`take_resend`) and they go out again.

**DDJ-FLX6** (`flx6_beat_fx`, in `ddj_flx6`, not the shared `pioneer_ddj`: the FLX4 and DDJ-400 strips send other messages). FX1 is MIDI channel 5, FX2 channel 6. Recorded and light-probed on the unit:

| Control | Message | Target |
|---|---|---|
| ON/OFF | note `0x47 + slot` | `FxOn` |
| LEVEL/DEPTH | CC `0x02 + 2 × slot`, 14-bit (coarse first, fine +0x20) | `FxDepth` |
| BEAT ◀ / ▶ | note 0x06 / 0x07 | `FxBeatsDown` / `FxBeatsUp` |
| FX SELECT | note `0x70 + slot`, 127 for the new slot, then the knob's position for it | `FxFocus` |
| CH SELECT CH1–4 / MST | note 0x1C–0x1F / 0x14, 127 for the new one | `FxAssign` / `FxAssignNone` |
| BEATS lights | CC 0x64: 01 and 02 blink ¼ (1/16, 1/8), 03 ¼, 04 ½, 21 ¾, 05 1, 06 1 and 4 (2 beats), 07–0A 4–32; 0B dark | `fx_beat_lights` |

The unit sends nothing on connect and won't say where its strip is. It powers on with FX1-1 selected and both units on CH1, so both start on deck A (`fx_unit_decks` = A, A, C, D), and Tandem follows them from there: CH SELECT reports a unit's channel, and FX SELECT moving onto the other unit reports that unit's. SHIFT + ON/OFF (0x43), SHIFT + BEAT (0x66, 0x6B), MERGE FX, the PAD FX pads and JOG CUTTER stay unbound. Preset version 13 adds the strip, and `upgrade_from` gives saved FLX6 mappings its bindings, its BEATS lights and the units' starting decks.

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

All of this is a pure function of the events, so every booth always agrees on the master, and it's part of the digest. With two decks it behaves as before, except for the handover: once the master stops, its tempo fader no longer drags the synced deck.

On the DDJ-FLX6, decks 3 and 4 send on MIDI channels 3 and 4 and drive decks C and D. Until preset version 6 they drove A and B; `Mapping::upgrade_from` moves every saved binding that still does exactly that to C or D, and adds mixer channels 3 and 4 and their meters, leaving bindings the DJ changed alone.

### Browsing the library from a controller

The browse knob moves through either the tracks of the list on show or the lists on the left (`BrowseFocus` in `app.rs`). Pressing it switches between them, BACK goes to the lists, and a blue outline marks the side it's on. Turning through the lists shows each one's tracks straight away, like Serato and rekordbox. It doesn't rescan crates or All Music on the way, since All Music walks every folder; pressing the knob to go into one rescans it, as clicking it does. Crates (yours or Serato's) are skipped while their section is closed; lists shared live in the room come after Shared with me. Clicking a list goes into it, so the knob goes back to its tracks; that keeps the old habit of picking a list with the mouse and a track with the knob. Turn, press and BACK reach the UI as `MidiOut`s, never as room events, so a booth's browsing never goes on the wire.

### Controller tempo faders

Faders and knobs use soft takeover (`mapping.rs`), but the tempo fader can't: SYNC moves a deck's tempo without moving the fader, and SHIFT + TEMPO moves the fader without moving the tempo, so a DJ who runs out of travel can slide it back and keep going. `MidiEngine::tempo_fader` keeps where the fader physically is, from either message (`Target::Tempo` and `Target::TempoShift`; on the DDJ-FLX6, CC 0x00 and CC 0x05).

- **On the fader** (it was at the deck's tempo before this move, within 0.05%): it sets the tempo directly.
- **Off the fader:** a move changes the tempo by exactly as much as the fader moved, from wherever the deck is. A quick move whose new reading happens to land on the deck's tempo doesn't put the deck on the fader, or one fast flick would undo what SHIFT gained.
- **Back on the fader** when SHIFT leaves it at the deck's tempo, someone moves the deck to where it sits (SHIFT + SYNC, the partner), or the tempo is held at the limit and the fader catches up.
- **After a DECK button switch** (`Target::DeckSelect`; on the DDJ-FLX6, note 0x3C on the deck's channel, sent when a side switches between decks 1 and 3 or 2 and 4): one fader drives both decks, and it may have moved while it drove the other one, but the controller doesn't say where it is. So `MidiEngine::deck_selected` forgets where it was for this deck: the next reading only says where it is (both halves of a 14-bit fader, and a fine half alone waits for a new coarse one), and the deck goes on from its own tempo, off the fader. Switching never makes a tempo jump, whichever deck the fader moved last. Preset version 12 adds the four messages to the FLX6 preset.
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

The Hercules DJControl Inpulse 200 MK2's built-in mapping is the Pioneer shape, not the Denon one: SHIFT moves a deck's controls three MIDI channels up, and the unit keeps the pad mode itself and sends another note range in each, so nothing in it needs `Target::Shift` or a pad mode. Hercules publishes no MIDI list for it. The mapping comes from mappings, programs and a MIDI recording made with the MK2, read against Hercules' list for the first Inpulse 200, whose messages it kept; [controllers/hercules-djcontrol-inpulse-200-mk2.md](controllers/hercules-djcontrol-inpulse-200-mk2.md) says where each message came from, how sure it is, and what to check on the unit. Two things differ from the other presets: its tempo fader's value falls as the tempo rises, so `Tempo` and `TempoShift` are inverted; and its LOOP button, a loop on/off in other software, is `BeatLoop` 4, the nearest target there is.

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
- **The booth that loads.** `Session::act_at` refuses a `Load` the model would drop, and `load_path` refuses before decoding and again once the track is ready, since a fader may have come up meantime. Both queue a notice (`Session::take_notices`) that the app shows as a toast. Every way of loading (library buttons, right-click, double-click, the controller's LOAD buttons) goes through these. A double-click picks the first stopped deck on screen, otherwise the quietest silent one. If a partner's earlier move arrives late and the room drops a load this booth already sent, `check_my_loads` notices (the deck doesn't have the track at the load's time) and gives the same notice. The same goes for loads either DJ made while the link was down, once the joining booth is back (see [Network](#network)).

### Local vs shared

| Shared (in the timeline) | Local (this booth only) |
|---|---|
| decks: load, play, cue, hot cues, loops and loop rolls, tempo, bend, nudge, beatgrids | headphone cue (PFL) per deck, which hears the deck without its effects |
| channel faders, EQ, crossfader, its curve and the assign switches | headphone mix and level |
| the sync master | master and broadcast levels, sync click |
| every deck's effects: each slot's effect, on/off and depth, and the bank's BEATS | showing decks C and D, and the FX strip open or closed |
| | which deck each of the controller's FX units works |

**Level knobs.** A controller's MASTER LEVEL and HEADPHONE LEVEL knobs (`Target::MasterLevel`, `Target::HeadphoneLevel`) set this booth's `LocalParams` levels directly from the MIDI thread, with soft takeover against the sliders on screen, and never send an action. On a controller with a sound card, the same knobs also turn its own outputs up and down in hardware. So when the master or the headphones play through it, the knob leaves that level alone; otherwise the sound would be turned down twice. `ControllerCard` decides by name: Windows names a controller's sound card after it (MIDI port "DDJ-FLX6", sound card "Line (DDJ-FLX6)"), so it's the controller's when its name contains the port's, ignoring case, punctuation, the "2- " prefix and a trailing "MIDI". The app works it out every frame from the controller's port and the devices the audio actually opened, the system default included. The FLX6 preset has both knobs from preset version 11 (14-bit, CC 0x08 and 0x0D on channel 7), so saved FLX6 mappings pick them up.

### Tracks

A track's id is the BLAKE3 hash of its file, so identical files match whatever they're called.

When a deck loads a track, the other booth first looks in its own library. It checks known hashes first, then hashes any files of exactly the same size. Only if that finds nothing is the file transferred.

Every peer decodes with the same decoder (symphonia) and the loader's exact duration travels with the `Load` event. That keeps both booths' samples identical.

## Network

- **Topology.** The host listens on TCP 47800. Each joiner opens one *control* connection, plus one *files* connection per transfer. Files have their own connection so a 60 MB track never queues ahead of a fader move. Up to three joining DJs (`net::MAX_DJS`, four with the host) and 4 broadcasters connect the same way, in any order; the host is the hub, so no joining booth needs to be reachable. See [Rooms of up to four DJs](#rooms-of-up-to-four-djs).
- **Upload cap.** All of a booth's uploads together are capped at 2 MB/s by default (configurable), so they don't flood the uplink and delay the control link's messages behind them in the router. One `files::UploadPacer` per session books each 128 KB chunk the next free slot, so a host sending to three DJs and two broadcasters at once takes turns at one rate.
- **Direction.** Connections always go joiner to host, so only the host needs reachability. The host asks the joiner to upload with `PushRequest`.
- **Format.** Length-prefixed JSON frames, plus a binary payload for file chunks (`protocol.rs`).
- **Getting back in.** A link that hears nothing for 15 s (`LINK_TIMEOUT`) is dropped; after 3 s (`LINK_QUIET`) the top bar already says so. A link that ends without a `Bye` (a timeout, or the connection closing) was lost, not left: a joining DJ or broadcaster booth then rejoins the same address and room by itself (`net::rejoin_loop`), trying after 1, 2 and 4 s and then every 5 s for 5 minutes (`REJOIN_FOR`), with "Reconnecting to X… attempt N" and **Give up** in the top bar. Getting back in is a fresh join, the same handshake and clock sync (`Session::on_joined` with `rejoin`), except that a DJ's booth brings the moves it made while cut off (next point). Its local settings, library, decoded tracks and event sequence numbers carry on, and tracks on the decks it hasn't got are fetched again. Meanwhile it plays on from its own timeline: a DJ's decks keep going (their moves kept for the room), and a broadcaster's stream plays the room as it last was instead of going silent. It stops at a `Reject` (another version, a new room code), after giving up, or after 5 minutes, and is then on its own as before (a broadcaster's decks empty). A `Bye` ends a link on purpose and never starts a rejoin, so `leave` waits up to 300 ms for its `Bye`s to go out before closing, and the writer closes the connection behind a `Bye`.
- **Moves made while cut off come through.** Once the booth is back, the room is every DJ's moves together, each at the shared time it was made, whichever side's network failed. So on a control more than one touched the later move wins, whoever made it, and the transport folds in order: a PLAY pressed while cut off leaves the deck playing from then, unless another DJ's later PAUSE stops it. The moves land when the booth gets back in: the host and the other DJs hear the joiner's arrive at once (a fader jumps, a deck started meanwhile comes in where it would be by now), and the joiner hears theirs. Every timeline folds its history into the base after about 3.5 s, so two things keep it:
  - *The joining DJ keeps its own moves* (`Session::own_moves`): every one from 10 s (`HELD_MARGIN_US`) before it last heard from the host, so those sent into a dead link before the drop was noticed are among them, and every one while cut off, for as long as the host holds its place. The `Hello` that gets it back in carries them, with the tracks they load (`protocol::CutOff`).
  - *The host keeps the room's history further back* (`Session::held`): a second timeline that gets every event the live one does, but compacts only to 10 s before the joining DJ heard from longest ago was last heard from, while any is linked or expected back (`REJOIN_FOR` + `LINK_TIMEOUT` after the drop). The live timeline is always the held one compacted to its own base. Only the live one is published, so the audio threads' work doesn't change, and the booth's own events skip the duplicate check in the held one (`Timeline::insert_new`), which can hold minutes of moves.
  - *Merging* (`Session::take_cut_off`, before the `Welcome`): from the booth whose place the host holds (the same id and booth key, whether the host noticed the drop or still holds the old link), the host puts its moves into the held timeline at their own times (`Timeline::merge`). It skips any it already has (by origin and sequence number) and any older than the held history: it had those, since a move that never arrived was sent after the last message that did. The live timeline is made again from the held one, the broadcasters and the other DJs get it in a `Resync`, and the `Welcome` says `merged` and carries the held timeline, reaching back far enough for the joiner to check its loads. The joiner then adds the moves it made after sending its `Hello` (during the clock sync), sends them on, and compacts as usual (`took_merged_room`). Any move from a joining DJ that arrives later than the live timeline reaches back, but within the held one, goes in at its own time the same way (`insert_event`; the live timeline is remade at the next maintenance tick, and goes to everyone else in a `Resync`) instead of being clamped. A booth taking a `Resync` keeps its own moves from after its base that the host hasn't got yet, and drops older ones, which the host's base already holds, so none is applied twice (`Session::take_resync`).
  - *Loads* made while cut off go through the load guard at their own time. One the merged room refuses (another DJ's deck was playing out loud by then) is reported to the DJ who made it, on whichever booth, as for a load that lost a race (`check_my_loads`), and the deck carries on as it was.
  - Giving up and joining the same room again by hand while the host still holds the place brings the moves the same way, including those made after giving up. Nobody else's are taken: a booth with another id or key, or one the host stopped waiting for, gets the room as it is (one getting back in is told what it did didn't reach the room). Leaving on purpose, or the host closing the room, ends it.
- **Transfers carry on.** A track transfer cut off part-way picks up where it got to, in either direction. The receiver keeps what arrived in a `.part` file in `session-cache` named after the track's hash, so any later download of that track this session continues it, whatever started it: the booth getting back in, a rejoin by hand, a share. It asks for the rest by offset (`FileGet`'s `offset`; for an upload, the host answers the joiner's `FilePut` with the offset in `FileOk`), re-hashes what it has, and checks the whole file's hash at the end as always. If that fails, the partial file was bad: it's cleared and the track sent again from the start. When a link drops, every transfer with that booth stops at once, and its downloads are paused rather than failed (`TransferState::Paused`, with the track still "receiving"). They carry on when the same booth is back (`Session::on_joined` on a joiner, `add_peer` on the host), and fail once it isn't coming back (the rejoin gave up, the host stopped waiting, or someone left). A download whose connection breaks while the link stays up waits a second, so a link that's dropping too is noticed first, then carries on. A finished file goes to a fresh name in the share folder, or into the session cache, only after it checks out.
- **One writer per `.part` file.** Every transfer registers in a `files::Running` with the booth at the other end and the `.part` file it writes. A transfer that wants a file another is still writing (one whose link dropped, blocked on its dead connection, or one that stalled) stops it and takes over at once. Every change to a `.part` file goes through its transfer's gate, and stopping closes the gate after any write in progress, so the stopped one never touches the file again, and two transfers never write one file. It doesn't wait for the stopped one to end: on Windows, closing a connection from another thread doesn't always end a read or write blocked on it (after a silent drop it can stay stuck until the files connection's 30 s timeout). A stopped transfer isn't reported as failed, and every try has a serial, so an older one ending, even 30 s later, can't settle a newer one or mark its upload failed. The wire change is three fields with defaults (`FileGet.offset`, `FileHeader.offset`, `FilePut.resume`) and `FileOk.offset`: a booth from before them asks for and sends whole files, and is answered with whole files, so they share a room.
- **Same booth, same seat.** Every `Hello` carries the booth's id and a booth key, a secret from the OS's randomness that the booth keeps for its lifetime (the id is no secret: every event carries it). The host gives a booth with a matching id and key its seat back even while it still holds the old, dead link, closing that one (`Session::seat_for`, `evict`), so there's no second DJ or broadcaster of the same name and no "room full". A DJ's seat is also kept for them while they're expected back, so the room can't fill up behind them. Every booth in a room has its own id: one that comes with the id of a booth already there (or expected back) but without its key is turned away, since two booths with one id would make their moves look like one DJ's. Every link has a serial, so the old link ending later can't drop the new one. A host that was frozen finds the connections the booth left behind at each timed-out try waiting all at once; it skips those the booth has already closed (`net::closed_already`), so they can't throw out its live try. While a DJ is expected back, the host shows "Waiting for X to reconnect…", one per DJ. A host from before booth keys turns a returning booth away as room full until the old link times out, so a rejoin keeps trying through that for 25 s.
- **Security.** There's no encryption at this layer, so run it over Tailscale. The room code keeps strangers out but is not a password, and anyone with it can join as a broadcaster (or as a DJ, while there's a seat). A booth only serves files that are loaded on a deck, offered in a share or in a list it shares live, and the host only accepts uploads it asked a joining DJ for, from that DJ. It takes room state (`Resync`, `Members`, digests) from nobody: those only go from the host.

## Sharing and consent

- **Sender side.** The sender confirms a pledge ("I own or am licensed to use these tracks…") before the offer is sent, or before a list is shared live.
- **Receiver side.** The receiver sees a popup naming the sharer and the non-commercial terms. They must tick the pledge before *Accept and download* is enabled. Declining sends nothing.
- **Record.** Both sides append to `consent-log.jsonl`: time, terms version, names, share title, exact statement and track list. Sharing a list live is recorded too (`share-live`, with the tracks it held then); it needs no answer from the others, since nothing reaches them but its track details until a track is loaded, which makes a session copy like any other.
- **Session copies.** A track your partner plays that you don't have lives in `session-cache` and is deleted on exit, as are the `.part` files of transfers that never finished (shares included). The terms you accept on first launch cover this.
- **Passing a share on.** A share goes to every other DJ in the room, and each answers for themselves. Between two joining DJs, it goes through the host. The host only passes the tracks on: it fetches one from the sharer when a DJ who accepted asks for it (`Purpose::Relay`), into its `session-cache`, deleted on exit, and never into its library or share folder unless the host accepts the share too. The sharer's consent record names every DJ the offer went to.

## Code map

| Crate | What |
|---|---|
| `tandem-core` | Everything platform-independent: `time`, `clock`, `model`, `spin` (records let go mid-spin), `render`, `fx` (effects), `meter` (level meters), `audio` (decode), `net`, `files`, `session`, `library`, `serato`, `mapping` (MIDI learn and soft takeover), `terms`, `selftest`. Tests run anywhere. |
| `tandem` | The Windows app: egui UI (`ui_*.rs`, including the broadcast window in `ui_broadcast.rs` and a broadcaster booth's main window in `ui_broadcaster.rs`), sound output via cpal/WASAPI (`output.rs`), MIDI via midir (`midi.rs`). |
