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

## Round 7 (v0.7): level meters

- **Level meters** on the mixer: one per channel, before the fader, and a stereo master meter between them, in dBFS with a one-second peak hold. The broadcast view shows them too, as late as the feed.
- **On the controller:** the DDJ-FLX6, FLX4 and DDJ-400 channel meters show decks A and B. The FLX6's light thresholds were measured on the hardware, so its lights come on at the same levels as the screen's marks. Saved mappings pick the meters up automatically.
- **The controller's lights now go out reliably when Tandem closes.** Closing used to race the last "lights off" messages.
- No protocol change: v0.6 and v0.7 can share a room.

### Test plan for level meters

1. Play a loud track at TRIM's centre.
   - Does the channel meter bounce in the orange, with red on the loudest peaks?
   - Turn TRIM down a little. Does the red stop?
2. Pull the channel fader down. Does the channel meter keep moving while the master meter falls?
3. On the FLX6, do the CH1 and CH2 meters match the screen? Do CH3 and CH4 stay dark?
4. Open the broadcast view and cut a deck with its fader. Does the broadcast window's master meter drop when the cut is heard in the feed, not before?

### v0.7.1

- **All Music** in the library: every track in your folders, Shared with me and your Serato crates in one list, each track once. Search, sorting and Analyze BPM work on it. It's rescanned each time you pick it, while the last list stays on screen.
- Rescanning a list of more than 500 tracks (a crate, or Shared with me after a share arrives) keeps the old list on screen until the new one is ready, instead of cutting it short to 500 while it scans.

## Round 8 (v0.8): crossfader curves

- **Crossfader curve:** **CURVE**, under the crossfader, picks Linear, Constant power, Full centre or Scratch. A small picture beside it shows each deck's volume across the fader.
  - With **Full centre**, a deck playing on its own stays at full volume with the crossfader in the middle. Constant power, the only curve until now, drops it 3 dB there. Constant power is still the default.
  - **Scratch** keeps both decks at full volume until the last 3% of travel, for cuts.
  - The room has one curve, like a club mixer's curve switch: both booths and the broadcast hear the same one. Your booth remembers the curve you last picked and starts on it. Joining a room takes the host's.
- **The browse knob no longer flickers** near the end of a short list. Each tick used to draw one frame with the list scrolled too far.
- Protocol version 7: v0.8 can't share a room with v0.7 or earlier, so both booths need v0.8.

### Test plan for crossfader curves

1. Play one deck alone with the crossfader on its side, then move the crossfader to the middle.
   - With Constant power, does it get a little quieter?
   - With Full centre, does it stay at full volume?
2. Blend two tracks across on each curve. Linear dips the most in the middle; Full centre gets louder there.
3. On Scratch, does the far deck cut in within the first couple of millimetres, with no click?
4. Restart Tandem. Is your curve still picked?
5. In a room: change the curve on one PC.
   - Does the other PC's CURVE follow?
   - Does the broadcast recording change at the same moment?

### v0.8.1

- **Cancel for Analyze BPM.** A **Cancel** button beside "Analysing BPM 12/300" stops the analysis once the tracks it's working on are done; it says "Stopping analysis…" until then. Tracks already analysed keep their BPM and key, and pressing **Analyze BPM** again carries on with the rest.
- No protocol change: v0.8.0 and v0.8.1 can share a room.

## Round 9 (v0.9): records that spin on

- **Spin the record and let go, and it keeps spinning,** like in Serato. A hard backspin on a playing deck runs back for over a second, slowing down, then the track winds back up and plays on. A record flicked on a paused deck coasts to a stop, and you hear it. Touch the platter to catch it.
  - Let go with your hand still, or moving with the music, and nothing changes: drops and cue points stay exact.
  - Lifting your hand off a still FLX6 platter used to nudge a paused deck about 0.1 s, because the platter flicks on a little. It no longer moves the deck.
  - Both booths and the broadcast hear the same spin.
- Protocol version 8: v0.9 can't share a room with v0.8 or earlier, so both booths need v0.9.

### Test plan for spinning records

1. Play a track. Touch the platter, spin it back hard and let go.
   - Does the track keep going backwards, slow down gradually, then wind up and play on?
   - Does it play on from the right place, with no jump or click?
2. Pause a deck. Flick the platter forward and let go. Do you hear the track coast and slow to a stop? Flick it backwards too.
3. While a record is spinning on, touch the platter. Does it stop dead under your hand?
4. Scratch, then let go on a beat with your hand still. Does the track start exactly as before, with no extra slide?
5. On a paused deck, scratch to a kick and lift your hand off. Does the playhead stay on the kick?
6. Backspin hard near the very start of a track. Does it stop at 0:00 without hanging there?
7. In a room: backspin on one PC. Does the other PC hear the same spin, and does the broadcast recording have it?

## Round 10 (v0.10): broadcaster-only booths

- **Join a room as a broadcaster.** Tick **Broadcaster only** in Join a room, on a PC that isn't DJing. That PC plays only the broadcast feed and opens the broadcast window, so Discord can share the window with its sound on, or OBS can capture it, with no virtual cable.
  - A broadcaster can't change anything: no decks, mixer or library on screen, its controller does nothing, and the host ignores anything it sends.
  - It doesn't need the music: the host sends it every track on a deck, including the joining DJ's.
  - Both DJs see who's broadcasting in the top bar. Up to 4 broadcasters per room.
  - If the host leaves, the broadcaster's decks empty and it goes quiet.
- **A broadcaster runs 1000 ms behind the room** (its own setting; a DJ's broadcast delay stays 500 ms), since the joining DJ's moves reach it through the host.
- **TIMING** on the broadcaster shows how long the DJs' moves take to get there. Too close to the delay, it warns (and the top bar does) and offers a longer delay in one click.
- **The upload speed limit covers all uploads together,** not each one, so a host sending tracks to its partner and broadcasters at once can't fill its connection and delay the DJs' moves. Two transfers at once now share it, so each goes at half the speed.
- A third DJ trying to join is told to tick **Broadcaster only**.
- The self-test now runs a room with two DJs and a broadcaster.
- Protocol version 9: v0.10 can't share a room with v0.9 or earlier, so both DJs and the broadcaster need v0.10.

### Test plan for broadcasters

1. Host and join as usual, then join from a third PC with **Broadcaster only** ticked.
   - Do both DJs' top bars show 📡 and the broadcaster's name?
   - Does the broadcaster's window show "*host* b2b *partner*", and the broadcast window open by itself?
2. Load a track on each DJ's PC that the broadcaster doesn't have.
   - Does the broadcaster's DECKS list go from "receiving" to "ready" for both?
3. Play and mix. On the broadcaster's PC, do the sound and the broadcast window match the DJs' moves, 1000 ms late?
4. Discord: share the broadcaster's "Tandem broadcast" window with sound on, and join the call from a phone.
   - Is the sound clean, with no other sounds from that PC?
   - Do picture and sound line up?
5. Plug a controller into the broadcaster and move things. Does nothing in the room move?
6. Check TIMING on the broadcaster during the set, and while a DJ loads a track it doesn't have. Does it stay green? If it turns amber, does the raise button fix it?
7. Close Tandem on the host. Does the broadcaster go quiet and say it's no longer broadcasting?

## Round 11 (v0.11): four decks

- **Decks C and D.** Every room has four decks. **4 decks** in the top bar shows them: A over C on the left, B over D on the right, a four-channel mixer (C A B D) between them, and four scrolling waveforms. It switches on by itself when deck C or D gets a track or starts playing, from either booth.
- **Sync master.** Synced decks follow one master deck, shown as **MASTER**. A tempo change on any synced deck, or the master, carries them all. When the master stops or loads a new track, a synced deck that's playing takes over, so the stopped deck can be prepared without dragging the one you're hearing.
- **Crossfader assign** (◀ • ▶) on every channel: left, through or right. A and C start on the left, B and D on the right. Part of the room, like the curve.
- **DDJ-FLX6:** decks 3 and 4 drive C and D, and mixer channels 3 and 4 and their level meters are mapped. Saved FLX6 mappings move their deck 3 and 4 controls over by themselves, keeping jog calibration and custom controls.
- **Broadcast view** shows decks C and D while either has a track, and so does a broadcaster booth's DECKS list.
- Settings saved by older versions keep working (headphone cues included).
- Protocol version 10: v0.11 can't share a room with v0.10 or earlier, so every booth (broadcasters too) needs v0.11.

### Test plan for four decks

1. Load tracks onto C and D (library **Load to**, right-click, or the FLX6's DECK 3/4 and LOAD).
   - Does the four-deck view come on by itself?
   - Do C and D play, loop, hot cue and scratch like A and B?
2. Sync B to A, then C, then D.
   - Does A show MASTER, and do all three follow it?
   - Move C's tempo fader: do A, B and D all move with it?
3. Stop A while B is playing synced. Does B become the master? Load a new track on A and move its tempo: does B stay put?
4. Put D on THRU (•) and move the crossfader. Does D stay at the same level? Do the crossfader's end letters change?
5. On the FLX6: switch the left side to deck 3.
   - Do the pads, loop buttons, jog and tempo fader drive C, and do C's lights show?
   - Do mixer channels 3 and 4 move C and D, with their meters lit?
   - Switch back to deck 1: does the tempo fader pick up without a jump?
6. In a room: load and play deck D on one PC.
   - Does the other PC switch to four decks and play it too?
   - Does the broadcast window show C and D, on a DJ's PC and on a broadcaster's?
7. Switch **4 decks** off with C and D stopped. Are A and B back to the big layout?

### v0.11.1

- **A stalled connection shows in the top bar.** When nothing has arrived from your partner for 3 seconds (from the host, on a broadcaster booth), the dot turns amber and **⚠ no reply for N s** replaces the round trip, which used to sit at its last value until the room gave up. If the connection comes back within 15 seconds the room carries on; after that it drops, as before.
  - Test: in a room, unplug one PC's network for about 5 seconds. Does **⚠ no reply** appear after about 3 seconds on both PCs, and the round trip come back once it's plugged in?
- No protocol change: v0.11.0 and v0.11.1 can share a room.

### v0.11.2

- **Pressing the browse knob switches between the lists and the tracks.** Turn it on the lists (All Music, folders, Serato crates…) to see each one's tracks, and press it again to go into one and pick a track. **BACK** also goes to the lists. A blue outline shows which side the knob is on, and clicking a list puts the knob on its tracks. Saved FLX6 mappings pick up the press and BACK by themselves, and other controllers can learn them.
  - Test: on the FLX6, press the browse knob, turn to a crate, press again, turn to a track and press LOAD. Does the outline move each time, and does the track load? Does BACK go to the lists?
- No protocol change: v0.11.1 and v0.11.2 can share a room.

### v0.11.3

- **Clear the library search in one click.** While the search box has text, a **✖** sits at its right end: click it, or press Escape while typing, to empty the box. The track you had selected stays selected in the whole list, scrolled into view, and the cursor stays in the box for the next search.
  - Test: search for an artist, pick the third result, then click **✖**. Is the whole list back, with that track still highlighted and on screen? Search again and press Escape: the same?
- No protocol change: v0.11.2 and v0.11.3 can share a room.

### v0.11.4

- **Loop IN and OUT are quantized.** IN goes on the nearest beat, and OUT makes the loop 1 bar, ½ bar, ¼ bar … or 2, 4, 8 bars …, whichever is nearest to when you press it. IN, then OUT about a bar later, is exactly 1 bar, even if you're a little late, and the deck goes round on the beat. While looping, IN moves the start to the nearest beat and OUT sets the length the same way. Turn it off in Settings › General (**Quantize loop IN and OUT**) to loop exactly at the playhead; it's each DJ's own setting. Tracks without a beatgrid loop exactly, as before.
  - Test: press IN off the beat, then OUT about a bar later. Does the green band start on a beat line and end exactly a bar later, with the **4** pad lit? Does OUT about half a bar after IN give ½ bar (the **2** pad)? With quantize off, does the loop start right where you pressed?
- Protocol version 11: v0.11.4 can't share a room with earlier versions, so every booth needs v0.11.4.

### v0.11.5

- **SHIFT + tempo fader moves the fader, not the tempo.** Run out of travel? Hold SHIFT, slide the fader back, let go and keep going the same way, past the fader's range, up to ±50%. It used to jump back by however far you'd slid it. It works the same on synced decks (the synced decks follow as usual) and when your partner has moved the deck. Saved FLX6 mappings pick it up by themselves, and other controllers can learn **SHIFT + tempo fader**.
  - Test: move deck A's tempo fader to the bottom (−8%), SHIFT + slide it back to the centre (the tempo stays at −8%), then move it down again. Does the tempo go on smoothly to −16%? Then press SHIFT + SYNC: does the deck go to where the fader sits?
- No protocol change: v0.11.4 and v0.11.5 can share a room.

### v0.11.6

- **A dropped connection rejoins by itself.** When a joining DJ's or a broadcaster's connection drops, Tandem gets it back into the same room: its top bar says **Reconnecting to … attempt N** (with **Give up**) and it keeps trying for 5 minutes, while the host's says **Waiting for … to reconnect**. The decks play on meanwhile, and a broadcaster's stream doesn't go silent. Once it's back, the decks follow the room again, as after a fresh join, so anything the joining DJ changed while cut off is dropped (since v0.11.16 they come into the room instead: see below). Leaving, or the host closing the room, never starts it.
  - Test: in a room, unplug the joining PC's network for about 30 seconds, then plug it back in. Does it say **Reconnecting to …**, then get back in within a few seconds with its decks following the host's?
- No protocol change: v0.11.5 and v0.11.6 can share a room, but only a v0.11.6 booth rejoins by itself, and a v0.11.5 host takes a booth back only after its old connection times out (15 s).

### v0.11.7

- **Loop rolls on the DDJ-FLX6's BEAT JUMP pads.** Hold a pad and the deck loops 1/16, 1/8, ¼ or ½ beat (top row) or 1, 2, 4 or 8 beats (bottom row) while the track runs on underneath, shown as a dashed line on the waveform. Let go and it carries on from that line, exactly where the track would have been, still on the beat. Press another pad while holding one to switch size; the pad that's rolling lights up. Rolls work over a loop (letting go comes back into it), on synced decks and while scratching, and your partner and the stream hear them exactly as you do. Saved FLX6 mappings pick up the pads by themselves (the FLX4 and DDJ-400 too).
  - Test: switch deck A's pads to BEAT JUMP and, on a playing deck, hold pad 4 (½ beat) for a few bars, then let go. Does the track carry on in time, as if it had never rolled? Hold pad 5 (1 beat), press pad 4 and let it go again: does it go ½ beat, then back to 1 beat?
- Protocol version 12: v0.11.7 can't share a room with earlier versions, so every booth needs v0.11.7.

### v0.11.8

- **No more loading over the music.** Loading onto a deck that's playing and audible in the mix is refused, whether from the library, the controller's LOAD buttons or your partner's booth, with a note at the bottom of the window: *Deck A is playing: pause it or pull its fader down to load onto it.* A deck that's playing but silent in the mix can still be loaded over: its fader at about 10% or below, the crossfader all the way across, its filter turned all the way, or its trim or all three EQs down. If you load just as your partner brings that deck up, whichever happened first wins, on both booths.
- **A loaded track always starts paused** at its start, with sync, loops and rolls off, ready to cue. It used to start playing on the beat if the deck had been playing.
  - Test: play deck A with its fader up and press LOAD on A. Is it refused, with the note? Pull A's fader down and press LOAD again: does the new track sit paused at 0:00?
- Protocol version 13: v0.11.8 can't share a room with earlier versions, so every booth needs v0.11.8.

### v0.11.9

- **Master and headphone level knobs.** The controller's MASTER LEVEL and HEADPHONE LEVEL knobs now set your own master and headphone levels (never your partner's), and the sliders on screen follow them, so the master knob works through your PC's speakers or a headset too. They're mapped on the DDJ-FLX6; on other controllers, Learn them in Settings → Controller. Saved FLX6 mappings pick them up by themselves.
- When Tandem plays through the controller's own sound card, the controller turns that output up and down by itself, so Tandem's level stays put and the sound isn't turned down twice. Tandem knows the sound card by its name ("Line (DDJ-FLX6)"). The mixer says so under the level sliders, and Settings → Controller says what each knob does with your audio settings.
  - Test: with Speakers (master) on your PC's speakers, turn the FLX6's MASTER LEVEL knob. Does the master slider follow it (after you turn the knob past the slider, if you'd moved the slider on screen)? Then pick "Line (DDJ-FLX6)" for Speakers: does the knob change only the FLX6's own volume, with the slider staying put?
- No protocol change: v0.11.8 and v0.11.9 can share a room.

### v0.11.10

- **Small screens.** Tandem now fits laptop screens. It opens maximised when its usual window doesn't fit, and goes down to a 1080p laptop at 175% display scaling. On a short window the library gets less height first, then the two decks switch to compact panels with a shorter mixer: each channel's knobs beside its fader, and your headphone and master levels (with the level knobs' notes) under **🎧 LEVELS**. Every scrolling waveform stays on screen; with four decks on a very short window, the deck controls scroll instead.
- Drag the library's top edge to set its height yourself: Tandem keeps it from then on, but the library never covers the waveforms. On a narrow window the top bar keeps the room's address and clock spread in the hover text of the room name and round trip.
  - Test: on a laptop at 150% display scaling (or a window about 1280 × 640), are both waveforms moving, and do every deck control, the mixer and the library still work? Does **🎧 LEVELS** open your headphone and master levels? Switch on **4 decks**: are all four waveforms there?
- No protocol change: v0.11.9 and v0.11.10 can share a room.

### v0.11.11

- **The Denon SC Live 4 maps itself, and its ROLL pads roll.** Press ROLL and hold a pad: the deck loops 1/16, 1/8, ¼ or ½ beat (top row) or 1, 2, 4 or 8 beats (bottom row) while the track runs on underneath, as on the FLX6, and the pad lights up. HOT CUE gives hot cues (SHIFT + pad clears one) and LOOP gives beat loops of ¼ to 32 beats; each side keeps its own mode, and SLICER leaves the pads as they were. SHIFT + CUE, SHIFT + SYNC, SHIFT + tempo fader, the browse press, MASTER and the headphone level work too. A hand-made SC_LIVE_4.json becomes the built-in mapping by itself, keeping its jog calibration. Not yet tried on the hardware.
- Other controllers whose SHIFT and pad mode buttons don't change what they send can learn **SHIFT** and **Pad mode**: hold SHIFT or pick the mode before learning a control, and it does that only with SHIFT, or only in that pad mode.
  - Test (on the SC Live 4, in computer mode before opening Tandem): press ROLL on the left and hold pad 4 on a playing deck A for a bar, then let go. Does it stutter ½ beat and carry on in time, with ROLL and the pad lit? Press HOT CUE: are the pads hot cues again, with the right side unchanged? Does SHIFT + pad clear a hot cue?
- No protocol change: v0.11.10 and v0.11.11 can share a room.

### v0.11.12

- **A track on its way carries on after the connection drops.** If the link drops while a track is being sent (either way, or to a broadcaster), the transfer pauses instead of failing, and the deck says **paused at N% until the link is back**. Once you're back together it carries on from there (**resumed at N%**) instead of starting over, and the deck that was waiting comes in exactly where the room is. Shares carry on too.
  - Test: load a track your partner doesn't have. While their deck says **Receiving this track… N%**, unplug the joining PC's network for 20 seconds. Does it say paused, then carry on from about where it stopped once the PC is back, with the deck coming in playing in time?
- No protocol change: v0.11.11 and v0.11.12 can share a room, but a transfer with a v0.11.11 or older booth starts over.

### v0.11.13

- **Resumed transfers no longer stall on Windows after a connection goes quiet.** When the link went silent (Wi-Fi gone, rather than the connection closing) and the booth got back in, the old transfer could stay stuck on its dead connection for 30 seconds, and the resumed one waited behind it: the joining DJ's download could stall for up to half a minute, and the host could give up on the joining DJ's track until the link next dropped. Now the resumed transfer takes over at once and the stuck one can't touch the file. A stuck upload also no longer shows as failed in the transfers list while the resumed one carries on.
- v0.11.12's build was never published (its Windows tests failed on this), so v0.11.13 is the first build with resumable transfers: see the v0.11.12 notes above.
  - Test: as for v0.11.12, but turn off the joining PC's Wi-Fi for 20 seconds instead of unplugging the cable. Does the track carry on within a few seconds of it reconnecting, on both PCs?
- No protocol change: v0.11.12 and v0.11.13 can share a room.

### v0.11.14

- **Channel CUE on the FLX6 plays in your headphones.** With **Headphones (cue)** left on Off, the cue had nowhere to go: only MASTER CUE worked, because the FLX6 does that by itself. Now, when the master plays through a DDJ-FLX6, FLX4 or DDJ-400 and no headphone output is picked, the headphones go to its jack (channels 3/4; Settings shows **Auto**).
- **The HEADPHONES MIX knob blends cue and master on the controller.** Those controllers mix their headphones in hardware, so Tandem now sends their jack only the cued decks: the knob goes from your cue to the master, and MASTER CUE adds the master. Before, Tandem mixed the master in a second time from the same knob. The **phones: cue ↔ master** slider on screen is greyed out there, and Settings → Controller says what the knob does. Through any other sound card the knob and slider set Tandem's mix as before.
- Pressing CUE with no headphone output shows a reminder in the status bar.
  - Test (FLX6, master through the FLX6, Headphones on Auto): play deck 1 with its fader down and CH1 CUE on, and deck 2 with its fader up. Sweep HEADPHONES MIX from CUE to MASTER: does it go from deck 1 to deck 2? (Checked on the laptop's FLX6 with a test build.) On a four-deck FLX6, does CH3 and CH4 CUE work the same?
- No protocol change: v0.11.13 and v0.11.14 can share a room.

### v0.11.15

- **Switching decks on the FLX6 no longer makes the tempo jump.** One tempo fader drives decks 1 and 3 (or 2 and 4). Move it on deck 3, switch back to deck 1, and deck 1's first move used to jump by however far the fader had gone on deck 3. Now, after a DECK switch, the fader moves the deck you switched to by exactly as much as you move it, starting from that deck's own tempo, as it does after SYNC. The fader then usually doesn't sit where that deck's tempo is; the screen shows the real tempo, and **SHIFT + SYNC** puts the deck back on the fader. Saved FLX6 mappings pick it up by themselves.
  - Test (four-deck FLX6): set deck 1 to −4% with the tempo fader, press DECK to switch to deck 3 and push the fader up to +4%, then switch back to deck 1 and nudge the fader up a little. Does deck 1 go up a little from −4%, with no jump? Does deck 3 stay where you left it?
- No protocol change: v0.11.14 and v0.11.15 can share a room.

### v0.11.16

- **Moves made while cut off come through.** When the joining PC's connection drops and it gets back in, what you both did meanwhile is now in the room on both PCs, at the moment it was done. On a control you both touched, whichever move came later wins, whoever made it. A deck your partner started while you were apart comes in playing from where it would be by now, and a fader they moved jumps to where they left it. A load onto a deck the other DJ had playing out loud by then is refused, with the usual note. **Give up**, then joining the same room again within 5 minutes, brings your moves too.
  - Test: in a room, unplug the joining PC's network. While it says **Reconnecting…**, move the crossfader on the host and deck A's fader on the joining PC, then plug it back in. Once it's back, do both PCs show both moves?
- Protocol version 14: v0.11.16 can't share a room with earlier versions, so every booth needs v0.11.16.

### v0.11.17

- **The Hercules DJControl Inpulse 200 MK2 maps itself.** Pick it in Settings → Controller: the left side is deck A and the right side deck B. PLAY, CUE, SYNC, LOAD, the jog wheels, the tempo faders, GAIN, HIGH, LOW, FILTER, the volume faders and crossfader, the headphone cue buttons, the BROWSER knob and the MASTER and HEADPHONES knobs all work, and the lights on PLAY, CUE, SYNC, LOOP, the headphone cue buttons and the hot cue pads follow Tandem. In HOT CUE mode the four pads are hot cues 1 to 4 (SHIFT + pad clears one). LOOP starts a 4-beat loop and leaves it again, ½ and X2 halve and double it, and with SHIFT they are loop IN and OUT. SHIFT + CUE, SHIFT + SYNC and SHIFT + tempo fader work as on the FLX6, and SHIFT + pressing BROWSER takes the knob to the lists. The STEMS, FX and SAMPLER pads do nothing in Tandem.
- **It hasn't been tried on the hardware yet.** Hercules publishes no MIDI list for the MK2, so the mapping comes from mappings and programs other people made with one, read against Hercules' list for the first Inpulse 200. Which way the tempo fader runs and how many steps the jog wheel counts per turn are taken from its sibling models, so calibrate the jog wheel first (Settings → Controller → **Calibrate…**, with your hand on top of the platter). [controllers/hercules-djcontrol-inpulse-200-mk2.md](controllers/hercules-djcontrol-inpulse-200-mk2.md) says where each control's message came from, how sure it is, and what to check.
  - Test (on an Inpulse 200 MK2, with DJUCED and Serato closed): does Settings → Controller list "DJControl Inpulse 200 Mk2", and do PLAY and CUE work and light up on both sides? Move a tempo fader down, towards +: does the BPM rise? Set a hot cue with pad 1, then press SHIFT + pad 1: does it clear, and does the pad's light go out? Press LOOP on a playing deck, then ½, X2 and LOOP again: does it loop 4 beats, halve, double and leave? The notes file has the full list of 17 checks.
- **For adding the next controller:** a `controller-profile` skill for Claude Code (in `.claude/skills`) researches a controller's MIDI messages and lights from several sources, then writes the mapping, its tests and a notes file like the one above. `cargo run -p tandem-core --example preset_table -- "<port name>"` prints a built-in mapping to check, and with `--json` a mapping file to try on a Tandem that doesn't have it built in yet.
- No protocol change: v0.11.16 and v0.11.17 can share a room.

## Round 12 (v0.12): rooms of up to four DJs

- **Up to four DJs in a room:** the host and up to three who join. Everyone works every deck and the whole mixer, and everyone hears every move.
  - Everything goes through the host: each DJ's moves, tracks and shares reach the others from the host's PC, so only the host needs to be reachable, as before.
  - A track only one DJ has reaches all the others through the host, the way a joining DJ's always reached a broadcaster.
  - Moves between two DJs who both joined take both of their trips to the host, so they arrive about a round trip late instead of half of one. The broadcast still has them exactly where they were made.
  - The host's top bar shows each DJ with their own light and round trip; everyone else's shows the other DJs by name and the round trip to the host. The broadcast window names everyone: "*A* b2b *B* b2b *C*".
  - A fifth DJ is told **This room already has 4 DJs** and can tick **Broadcaster only** to stream it instead. Up to 4 broadcasters still fit alongside.
- **A share goes to every other DJ,** and each accepts or declines for themselves. Between two DJs who both joined, the host passes the tracks on without keeping them, whether or not it accepted.
- **A dropped connection, per DJ.** Each joining DJ whose connection drops gets back in by itself, as before, and the room keeps their seat while it waits. What they did meanwhile reaches every DJ, not just the host. A DJ who joined leaving doesn't end the room for anyone else; the host leaving still closes it.
- **A track that's still on its way to the host no longer shows as failed** on a broadcaster or a joining DJ: their download waits and asks again until the host has it.
- **Fixed:** a resync could apply some of your own recent moves a second time (a jog nudge from just before it could move the deck twice).
- The self-test now runs a room with four DJs and a broadcaster, and checks that a fifth DJ is turned away.
- Protocol version 15: v0.12 can't share a room with v0.11 or earlier, so everyone in the room, broadcasters included, needs v0.12.

### Test plan for four DJs

1. Host on one PC and join from three others.
   - Does the host's top bar show each DJ with a green light and a round trip?
   - Does each joining DJ's top bar show the other three by name?
   - Does a fifth PC trying to join as a DJ get "This room already has 4 DJs"? Does it get in with **Broadcaster only** ticked?
2. Each DJ loads a track only they have onto a different deck. Does every PC get every track, "receiving" then ready?
3. Mix with all four.
   - Do two DJs who both joined hear each other's moves promptly, about a round trip to the host late?
   - Is every cut on time in the broadcast?
4. Share a crate from a joining DJ.
   - Do the other three each get the popup?
   - Have another joining DJ accept and the host ignore it. Do the tracks arrive, and does the host's `Music\Tandem Shared` stay empty?
5. Unplug one joining DJ's network for 20 seconds while the others keep playing.
   - Does the host say **Waiting for *name* to reconnect…** while the rest carry on?
   - Once it's back, does every PC have what that DJ did meanwhile, and that DJ what the others did?
6. Have one joining DJ leave. Do the others carry on, and can someone else join in their place?

## Next candidates

Rough priority order:

1. **Automatic downbeats (bars)** in the detected beatgrid, plus quantize for hot cues and play (loop IN/OUT are quantized). Grids already have bars; for now you set bar 1 by hand.
2. **Beatgrid editing from the controller**, and placing markers by clicking the waveform.
3. **More loop tools:** beat jump, slip mode for scratching and hot cues (loop rolls are done), and fine-adjusting IN/OUT with the jog wheel.
4. **Lower UI CPU:** redraw waveforms only when they change (the frame rate cap is done).
5. **Serato cue points and loops** from the Markers2 tag.
6. **Lower-latency audio:** WASAPI exclusive mode or ASIO.
7. **Recording** the broadcast lane to WAV.
8. **Effects:** beat FX (echo, reverb) on the FLX6's FX section.
9. **Encrypted transport** (QUIC/TLS) and an optional relay server, so neither DJ needs Tailscale or port forwarding.
10. **Crossfader assign from the controller**, for controllers with assign switches, and a MASTER button to pick the sync master by hand.
