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

## Round 2 candidates

Rough priority order:

1. **Beatgrids and sync:** import Serato's BeatGrid tag, analyse otherwise, add a sync button and quantize. Beat-snapped actions get scheduled on the next beat, which hides network latency completely for cues, as long as the delay is under one beat.
2. **Key lock** (time-stretch, e.g. Signalsmith Stretch).
3. **Serato cue points and loops** from the Markers2 tag.
4. **Controller LED feedback** (MIDI out) and per-controller presets for common Pioneer, Numark and Hercules models.
5. **Lower-latency audio:** WASAPI exclusive mode or ASIO.
6. **Loops and a filter knob** per channel.
7. **Recording** the broadcast lane to WAV.
8. **Deck ownership indicators and optional locks.**
9. **Encrypted transport** (QUIC/TLS) and an optional relay server, so neither DJ needs Tailscale or port forwarding.
10. **Four decks.**
