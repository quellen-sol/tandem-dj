# Roadmap

## Round 1 (v0.1): what's in

- **Sync core:** shared clock, event timeline, read heads that chase it, delayed broadcast lane, state digests with automatic resync.
- **Decks:** 2 decks with CDJ-style cue, 4 hot cues, tempo (±6–50%), pitch bend, jog nudge, overview and scrolling waveforms.
- **Mixer:** 3-band isolator EQ, channel faders, constant-power crossfader, per-booth headphone cue.
- **Rooms:** host or join over TCP, with clock sync, round-trip and in-sync display.
- **Tracks:** matched by content hash; missing tracks are transferred automatically for the session.
- **Sharing:** crate sharing with sender and receiver consent and a local consent log.
- **Library:** folders, Serato crates (read-only), Shared with me, search.
- **Controllers:** MIDI learn with 14-bit faders, relative jogs and encoders, and soft takeover.
- **Audio routing:** master, headphones and broadcast to any device or channel pair.
- **Self-test:** `--self-test`, plus `--data-dir` for a second copy on one PC.

## Test plan for you and your buddy

Work through these in order and note anything that fails. `tandem.log` (Settings → General) helps a lot.

1. **Each PC on its own.** Does `tandem.exe --self-test` end with `SELF-TEST PASSED`?
2. **Solo booth.** Add a music folder and load two tracks.
   - Do play, CUE (hold to preview), hot cues and tempo work?
   - Does the controller map with Learn?
   - Do faders pick up correctly?
   - Is headphone cue on the right output?
3. **Connect.** Host on one PC and join from the other, over Tailscale.
   - Does the top bar show the round trip and **✔ in sync**?
4. **Clock check.** Turn on **Sync click** on both and get on a call.
   - Do the clicks line up? Any audible flam is worth reporting.
5. **Same track, both libraries.** Load a track you both have.
   - Does the other booth show it ready without a transfer?
6. **Missing track.** Load a track only one of you has.
   - Does the other booth show "Receiving…" and then play it?
7. **Real B2B.** One mixes in while the other preps, then swap roles. Try touching the crossfader on both controllers.
   - Does soft takeover feel right?
8. **Stream it.** The broadcaster routes **Broadcast feed** to VB-Cable and records it in OBS.
   - Listen back: are the other DJ's cuts and hot cues tight in the recording?
9. **Share a crate.** Share a crate and accept it on the other side.
   - Is the popup clear?
   - Do the files land in `Music\Tandem Shared`?
10. **Stress.** Pull the network cable mid-set.
    - Do both booths keep playing?
    - Can you rejoin?

## Round 2 (v0.2): done

- **DDJ-FLX6 support:** built-in mapping (FLX4 and DDJ-400 too), vinyl scratching with jog calibration, ring pitch-bend nudging, filter and trim, button and pad lights, keep-alive, and saved mappings that pick up new controls.
- **BPM and beatgrid detection,** with beat markers, plus key detection in Camelot notation. Both are cached per file and run in the background with Analyze BPM.
- **SYNC:** tempo and beat match with half/double time, locked tempos, synced decks starting on the beat, and SHIFT + SYNC (or right-click) to return to the deck's own BPM.
- **Key lock** (on by default), semitone key shift and MATCH KEY, using Signalsmith Stretch.
- **Loading onto a playing deck** starts the new track on the beat.
- **Library:** analysed BPM and key columns; "Title - Artist" file names detected; `|` in search means "or".

### v0.2.1

- **Tempo fader responds straight away after SYNC.** It used to wait for pickup (the fader had to reach the synced tempo first), which could take seconds or never happen. Now, whenever the deck's tempo isn't where the fader sits, the fader moves it relatively.

## Round 3 (v0.3): done

- **Beatgrid editing**, Serato-style. Press **GRID** on a deck, then **SET** puts a marker at the playhead. **ADJUST** (±.01/.1/1 BPM, ½, ×2) stretches the grid and **SLIP** (±1/5/25 ms) slides it. Both act only on the section under the playhead.
  - Several markers handle tracks whose tempo changes, and synced decks re-lock at every tempo change.
  - Edits are saved per track (by content hash) and shared with your partner.
- **Bar numbers** on the waveforms and a bar.beat readout. The first marker is bar 1 and the bars before it count down from −1, for counting a track in.
- **SHIFT + CUE** (or right-click CUE) jumps back to the start of the track. Saved FLX6/FLX4 mappings pick it up automatically.
- **Gated hot cues:** a hot cue pressed while paused plays only while held, then snaps back to the hot cue, paused. While playing, hot cues jump as before.
- **Filter mutes at the ends.** The low-pass sweeps down to 20 Hz and the high-pass up to the top of the audio band. The last 10% of travel fades out, so a knob turned all the way is silent.
- Protocol version 5: both booths need this version.

### Test plan for beatgrid editing

1. Load a track whose detected grid is off. Press GRID, pause on the first kick of a bar, and press SET.
   - Does the grid snap there?
   - Is that spot bar 1, with −1, −2 before it?
2. Play it. Where the beats drift away from the kicks, press ADJUST until they line up, then SLIP if they're all early or late.
3. Find a track that changes tempo. Press SET at the change, then ADJUST the new section.
   - Does the first section stay put?
4. Sync another deck to it and play through the tempo change. Do the beats stay locked?
5. Reload the track.
   - Is your grid still there?
   - Does RESET bring back the detected one?
6. In a room: edit a grid on one PC. Does the other PC's waveform follow straight away?

## Round 4 (v0.4): done

- **Loops.** IN, OUT and RELOOP/EXIT, beat loops of ¼ to 32 beats on the beat, and halve/double. They're on every deck's LOOP row and on the FLX6/FLX4/DDJ-400.
  - FLX6 controls: SHIFT + IN for a 4-beat loop, CUE/LOOP CALL ◀ / ▶ to halve and double, and BEAT LOOP pad mode (SHIFT + BEAT JUMP).
  - Loop, pad and button lights follow.
  - Going round is seamless and click-free, with key lock too. Synced decks stay on the beat.
  - Saved FLX6/FLX4 mappings pick up the loop controls automatically.
- **Library sorting.** Click a column heading to sort by title, artist, BPM, key (Camelot order) or time. Click again to reverse, and a third time for the list's own order. The sort is remembered, and the selected track stays selected.
- Protocol version 6: both booths need this version.

### Test plan for loops

1. Play a track and press a beat loop (deck LOOP row, or the FLX6 pads in BEAT LOOP mode).
   - Does it loop on the beat with no click at the join?
   - Is the band on the waveform, and is the pad lit?
2. Press the same pad again: the loop ends and the track plays on. Try ¼ and ½ loops, and switch between sizes while looping.
3. IN, then OUT a few beats later.
   - Does it loop between them?
   - Does RELOOP/EXIT leave the loop, and pressing it again go back?
   - Do ◀ / ▶ (CUE/LOOP CALL) halve and double it?
4. Turn key lock off and on while looping. Any clicks or drift?
5. Sync deck B to deck A, loop deck B, then leave the loop. Are they still on the beat?
6. In a room: loop on one PC.
   - Does the other PC loop at the same moment?
   - Is the broadcast recording clean?

## Round 5 (v0.5): BPM and key detection

- **BPM and key come only from the audio.** A key tag no longer overrides detection, a BPM tag no longer steers it, and tag BPMs aren't shown in place of a detected one.
- **Better BPM.** Dotted and triplet rhythms no longer win over the beat: R&B and jungle that came out at 116 instead of 87/174 are fixed, and 2-step garage stays at 135. Fewer tracks end up with no grid.
  - On a 253-track test library, checked against madmom, Essentia and Spotify's data, the tempo is right (up to half/double time) on 214 of the 229 tracks where the references agree, up from 201.
- **Better keys.** Exactly right on 72% of the tracks where the references agree, up from 55%. Most misses are a compatible neighbour.
- **Tempo changes are detected.** A big tempo change that holds for half a minute or more starts a new grid section.
- Detection results from older versions are redone on the next **Analyze BPM** (edited grids are kept).

### Test plan for detection

1. Press **Analyze BPM** on a list analysed by an older version. Do the BPMs and keys fill in again?
2. Load tracks you know: R&B or hip-hop around 85–95, jungle or DnB, and UK garage.
   - Is the BPM right, or at most half/double?
   - Does the key match what you hear (or a Camelot neighbour)?
3. Load a track that changes tempo part-way through. Is there a second marker at the change, and do synced decks follow it?
4. Load a track you've edited the grid of. Is your edit still there?

## Round 6 (v0.6): broadcast view and frame rate

- **Broadcast view.** A second window for the stream, opened from the top bar. It shows both decks, the mixer and the scrolling waveforms, exactly as far behind the room as the broadcast feed, so a Discord or OBS capture of it matches the sound. Each deck, and the crossfader, shows the name of whoever touched it last. Nothing in it is clickable.
- **Screen frame rate** (Settings → General): 30, 60, 75, 120 or 144 fps, or match the monitor. The default is 60. Until now the screen redrew at the monitor's full rate (144 fps on a 144 Hz monitor), so 60 fps cuts the UI's CPU use by more than half there. Controller moves never wait for the screen.
- No protocol change: v0.5 and v0.6 can share a room.

### Test plan for the broadcast view

1. Host a room and click **Broadcast view**.
   - Does the header read "*you* b2b *partner*"?
   - Does each deck's name pill change when the other DJ touches that deck?
2. Have your partner do something sharp: a hot cue, a cut on the crossfader. Watch the broadcast window, and listen to the broadcast feed (CABLE Output).
   - Does the move appear in the window at the moment it's heard?
3. Discord: share the window (sound off) with CABLE Output as your microphone, and join from a phone or second account.
   - Do picture and sound line up? If not, by roughly how much?
4. Close the broadcast window with its ✕. Does the main window carry on?

## Next candidates

Rough priority order:

1. **Automatic downbeats (bars)** in the detected beatgrid, plus quantize for hot cues, play and loop IN/OUT. Grids already have bars; for now you set bar 1 by hand.
2. **Beatgrid editing from the controller**, and placing markers by clicking the waveform.
3. **More loop tools:** BEAT JUMP pads, slip mode and loop rolls, and fine-adjusting IN/OUT with the jog wheel.
4. **Lower UI CPU:** redraw waveforms only when they change (the frame rate cap is done).
5. **Serato cue points and loops** from the Markers2 tag.
6. **Lower-latency audio:** WASAPI exclusive mode or ASIO.
7. **Recording** the broadcast lane to WAV.
8. **Effects:** beat FX (echo, reverb) on the FLX6's FX section.
9. **Encrypted transport** (QUIC/TLS) and an optional relay server, so neither DJ needs Tailscale or port forwarding.
10. **Four decks.**
