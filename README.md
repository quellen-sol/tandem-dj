# Tandem

Back-to-back DJing with a friend, from anywhere. Two DJs in different places share up to four decks and a mixer, each hears the full mix at full quality in their own booth, and one of you, or a third PC, can stream the set.

**Personal, non-commercial use only.** See [TERMS.md](TERMS.md).

> **Status: v0.11.** Windows only. Rooms, four decks, BPM and key detection, beatgrid editing, sync, key lock, loops, level meters, crossfader curves, jog wheels that spin on like Serato, a broadcast view for streams, broadcaster-only booths and a ready-made DDJ-FLX6 mapping all work; this is still an early version for two people (and a broadcaster) to try. See [docs/ROADMAP.md](docs/ROADMAP.md) for what's next and a test plan.

## How it works

Both laptops run the whole mix locally. Only small, timestamped control messages ("deck A play at 12:04:31.208") cross the internet, and both machines agree on one shared clock. The same moves at the same timestamps give the same mix on both laptops.

- **Your own controls are instant.** Your partner's moves reach you about half a round trip late, typically 20–80 ms.
- **The broadcast is exact.** The streaming feed runs half a second behind, so both DJs' moves land in it exactly where they happened.
- **Tracks are matched by content.** If your partner loads a track you already have, Tandem finds your copy. If you don't have it, it's copied over for the session and deleted when Tandem closes.

The details are in [docs/DESIGN.md](docs/DESIGN.md).

## Install

1. Download `tandem-windows-x64.zip` from [Releases](../../releases) and unzip it anywhere.
2. Run `tandem.exe`. Windows SmartScreen will warn because the app isn't signed: click **More info → Run anyway**.
3. Enter your DJ name and accept the terms.

To check a PC without opening the app, open a terminal in the folder and run:

```
tandem.exe --self-test
```

The self-test lists your audio and MIDI devices, opens your default output for half a second of silence, and runs a full room test over the network stack, with two DJs and a broadcaster. It ends with `SELF-TEST PASSED`.

## Set up your booth

1. **Music.** In the library, click **Add music folder…**. Serato crates from `Music\_Serato_` (and `X:\_Serato_` on external drives) show up automatically. Tandem only reads them and never writes Serato files.
   - **All Music**, at the top of the library's list, shows every track in your folders, **Shared with me** and your Serato crates in one list, each track once. Search, sorting and **Analyze BPM** work on it like any other list. It's rescanned each time you pick it, so new files and crate changes show up.
   - **Click a column heading** (TITLE, ARTIST, BPM, KEY, TIME) to sort the list by it. Click again to reverse it, and a third time to go back to the list's own order. Keys sort in Camelot order (1A, 1B, 2A…). Tracks with no BPM or key yet go last. Tandem remembers the sort.
   - **Search** looks in titles and artists. Every word has to match, and `|` separates alternatives (`house | garage`). The **✖** at the end of the box, or Escape while you're typing, clears it, and the track you had selected stays selected in the whole list.
2. **Audio.** Open **Settings → Audio**.
   - **Speakers (master):** your controller's sound card, channels 1/2.
   - **Headphones (cue):** the same device, channels 3/4. Most controllers expose four channels.
   - Use the **🎧 CUE** buttons on the mixer to send a deck to your headphones, and **phones: cue ↔ master** to blend.
3. **Controller.** Close Serato, rekordbox and any other DJ app, because only one app can use the controller at a time. Then open **Settings → Controller** and pick your controller.
   - **Pioneer DDJ-FLX6, DDJ-FLX4 and DDJ-400 map themselves.** Everything on the deck and mixer sections works straight away, including the pads, jog wheels, SYNC and the button lights. On the FLX6, decks 1–4 are Tandem's decks A–D: the **DECK** buttons pick which one each side plays, and mixer channels 3 and 4 are decks C and D. See [Four decks](#four-decks).
   - **Other controllers:** click **Learn** next to each function, then move or press the matching control.
   - **Calibrate the jog wheel** once (Settings → Controller → **Calibrate…**: turn the platter exactly one full turn). Then scratching moves the record exactly as far as your hand, like a 33⅓ record.
   - **Spin the record and let go, and it keeps spinning,** like in Serato. It carries on at the speed your hand left it and slows down gradually: a hard backspin runs on for over a second, a gentle flick stops quickly. On a playing deck the track then picks up speed and plays on; on a paused deck it comes to a stop. Touch the platter to catch it. Let go with your hand still (or moving with the music) and the record stays exactly where you left it, so drops and cue points are exact.
   - **Faders and knobs use soft takeover.** A control nobody has moved yet takes your hardware position straight away. Once the room value has changed, your fader takes over only when it reaches that value, so you never jump over your partner's mix.
   - **The tempo fader never waits.** After SYNC (or your partner) moves a deck's tempo away from where your fader sits, the fader changes the tempo by exactly as much as you move it, starting from the deck's current tempo. **SHIFT + SYNC** puts the deck back on the fader.
   - **SHIFT + tempo fader** moves the fader without changing the tempo. Out of travel? Hold SHIFT, slide the fader back, let go and keep going the same way, past the fader's range, up to ±50%. On another controller, learn **SHIFT + tempo fader** if it sends its own message for it.
   - **SHIFT + CUE** (or right-clicking CUE) jumps back to the start of the track.
   - **Hot cues held while paused are gated.** The deck plays from the hot cue only while you hold the pad. Let go and it snaps back to the hot cue, paused. Press PLAY while holding to keep it playing.
   - **Loops:** **IN**, **OUT** and **RELOOP/EXIT** work as in [Loops](#loops). **SHIFT + IN** is a 4-beat loop, and **CUE/LOOP CALL ◀ / ▶** halve and double the loop. Switch the pads to **BEAT LOOP** (SHIFT + BEAT JUMP) for ¼, ½, 1, 2, 4, 8, 16 and 32-beat loops. The pad of the loop that's playing lights up.
   - **Browsing:** the browse knob moves through the tracks. Press it to move through the lists on the left instead (All Music, This room, your folders and Serato crates): each list's tracks show as you turn. Press it again to go back into the tracks, pick one and press LOAD. **BACK** also takes the knob to the lists. A blue outline shows which side the knob is on. Clicking a list with the mouse puts the knob back on its tracks, and closing the SERATO CRATES section makes the knob skip the crates. On other controllers, learn **Library browse press** and **Library back**.
   - **Jog wheel moves the wrong way or jumps?** Tick **1/127 style** or **invert** next to it.
   - **The channel level meters** on the FLX6, FLX4 and DDJ-400 show each channel's deck, like the meters on screen: see [Level meters](#level-meters).

Mappings are saved per controller, so you only do this once. When Tandem's built-in mapping gains new controls, your saved mapping picks them up automatically. Your calibration and changes are kept.

## BPM, sync and keys

- **Analyze BPM** (library toolbar) works out the tempo, beatgrid and musical key of every track in the list from its audio, in the background. BPM and key tags in the files aren't used. Each track is only analysed once, and tracks are also analysed when you load them. A grid you've edited always wins over analysis.
  - **Cancel**, beside the progress, stops it once the tracks it's working on are done. Tracks already analysed keep their BPM and key, and **Analyze BPM** carries on with the rest.
  - When a new version of Tandem detects better, its results from older versions are dropped: press **Analyze BPM** again on your lists. Grids you've edited are kept.
  - A track that changes tempo part-way through gets a grid section for each tempo, if the change is big and holds for at least half a minute.
- **Beat markers** run along the top of each scrolling waveform.
- **SYNC** matches a deck's tempo and beats to the **master**, using half or double time when that's closer. The first time a deck syncs, the deck it syncs to becomes the master: one that's playing if there is one, the deck across the crossfader first. The master shows **MASTER** next to its SYNC button. While decks are synced, moving the tempo fader of any of them (or of the master) moves them all, and pressing play on a synced deck starts it on the beat.
  - **When the master stops or gets a new track,** a synced deck that's playing becomes the master. So you can stop a deck, load the next track on it and prepare it without dragging the deck you're hearing. Holding CUE or a hot cue on a stopped deck is only a preview: it doesn't take the master.
- **SHIFT + SYNC** on the controller, or right-clicking SYNC, turns sync off. The deck goes back to its own BPM, offset only by where your tempo fader sits.
- **KEY LOCK** is on by default, so changing tempo doesn't change pitch. Turn it off for vinyl-style pitch. Scratching always sounds like vinyl.
- **Keys** show in Camelot notation (8A, 11B…), in green when a deck is harmonically compatible with the deck it's mixed with: the master, otherwise a deck that's playing (the one across the crossfader first). Use **−/+** to shift a deck by semitones. **MATCH KEY** shifts it the smallest amount needed to sit in that deck's key, or its relative major/minor.
- **Loading onto a playing deck** swaps the track instantly. The new track starts on the beat of what's playing: the master or another deck if one's playing, otherwise the track you replaced. It starts at bar 1 (its first grid marker). A synced deck stays synced.
- **Bar numbers** run along the bottom of each scrolling waveform, and the deck shows the bar and beat you're on (e.g. `12.3`). Bar 1 is the first grid marker. The bars before it count down **−1, −2…**, so you can park a track a bar or two early, press play and wait for the 1.

## Loops

Each deck has a **LOOP** row, and the controller's loop buttons and BEAT LOOP pads do the same things.

- **IN** marks where a loop starts, on the nearest beat. **OUT** ends it, and the deck goes round the loop. Loops made with IN and OUT are quantized: the loop is **1 bar, ½ bar, ¼ bar …** (down to ¼ beat) **or 2, 4, 8 bars …**, whichever is nearest to when you press OUT. So IN, then OUT about a bar later, makes a 1-bar loop; OUT pressed a little late still gives exactly 1 bar, and the deck carries on round the loop on the beat.
- **While looping,** IN moves the start to the nearest beat, and OUT makes the loop the length nearest to the playhead (keeping its start).
- **Quantize off** (Settings › General › **Quantize loop IN and OUT**): IN and OUT go exactly where the playhead is, and while looping they move the start or the end to the playhead. Tracks without a beatgrid always loop exactly. It's each DJ's own setting: a loop your partner makes follows theirs.
- **RELOOP/EXIT** leaves the loop, and playback carries on through it. Press it again to jump back to the loop and go round it again.
- **Beat loops (¼ to 32 beats)** start on the beat the playhead is in, so they're always on the beat. A ¼-beat loop is the quarter of the beat the playhead is in. Press the same size again to leave the loop. Another size resizes the loop, keeping its start. Without a beatgrid, a beat loop starts at the playhead, at 120 BPM.
- **×½ / ×2** halve and double the loop, keeping its start.
- **Jumping outside the loop** (a hot cue, CUE, clicking the overview) leaves it. RELOOP still brings it back.
- **SYNC keeps working.** A synced deck stays on the beat while it loops. Leaving a loop shorter than a beat puts it back on the beat.
- The loop shows as a green band on the waveforms. Going round is seamless, with no click at the join, and your partner hears it exactly as you do.

### Fixing a beatgrid

Detection isn't always right. It doesn't find bars (the "1"), gradual tempo drifts or every tempo change, and a few tracks come out at half or double the BPM you'd count. Click **GRID** on a deck to edit its grid. The key and tempo rows are replaced by the grid controls.

1. **SET** puts a red marker exactly where the playhead is. Put markers on the first beat of a bar (the 1). Pause and jog onto the kick for the most precise spot.
   - On a detected grid, the first SET moves the whole grid so it lines up there, and that spot becomes bar 1.
   - After that, each SET adds a marker. Add one wherever the tempo changes. If a marker is already within half a beat, SET moves that marker instead.
2. Each marker starts a **section** with its own BPM, running up to the next marker. **ADJUST** and **SLIP** only change the section the playhead is in (its marker is the bright one), so fixing one part never moves another.
   - **ADJUST** stretches the section to the track's tempo. Its marker stays put and the beats further away move more. The buttons are **±.01**, **±.1** and **±1** BPM, plus **½** and **×2** for a grid at half or double time.
   - **SLIP** slides the section's beats earlier (◀) or later (▶) by **1**, **5** or **25 ms**, without changing the BPM.
3. **DELETE** removes the section's marker. **RESET** throws away your edits and goes back to the detected grid.

Your edits are saved for the track and used every time you load it. They're matched by the file's content, so moving or renaming it keeps them, but editing its tags (or re-encoding it) doesn't. They're shared with your partner live, and their booth saves them too. If a deck is synced, it keeps following: a synced deck re-locks its tempo the moment either track passes a tempo change.

## Four decks

Every room has four decks, A to D. Click **4 decks** in the top bar to show C and D.

- **The four-deck view** puts A over C on the left and B over D on the right, with a four-channel mixer between them in the order C, A, B, D (like a four-channel club mixer and the FLX6), and all four scrolling waveforms underneath. Each deck keeps all its controls in a more compact panel.
- **It switches on by itself** when deck C or D gets a track or starts playing, whether you, your controller or your partner did it, so nothing that's playing is ever off screen. You can switch it off again while C and D are stopped.
- **Loading:** **Load to A B C D** in the library, the right-click menu on a track, or the controller's LOAD buttons. A double-click loads onto the first deck on screen that's stopped.
- **On the DDJ-FLX6,** press a side's **DECK** button to switch it between decks 1 and 3 (or 2 and 4): decks 3 and 4 are C and D, with their own pads, loops, jog and tempo fader. Mixer channels 3 and 4 are C and D's channels, with their meters. A mapping you saved with v0.10 or earlier moves its deck 3 and 4 controls over by itself, keeping your jog calibration and any controls you changed. Soft takeover and the tempo fader work per deck, just as on A and B.
- **Sync** works across all four: every synced deck follows the master. See [BPM, sync and keys](#bpm-sync-and-keys).

## Level meters

The mixer has a level meter on the inside of each channel fader, and a stereo master meter between them. They read peaks in dBFS: **green** up to −9 dB, **orange** in the top 9 dB, and **red** over full scale (0 dB). The light just above a falling bar holds the loudest peak of the last second.

- **Channel meters** read the channel before its fader, after TRIM, EQ and the filter, like a DJ mixer. They keep moving with the fader down, so use them to match the decks with TRIM.
- **The master meter** reads the mix after the faders and crossfader, before your own master level. Both of you see the same levels, whatever your speakers are set to.
- **Red means the peaks are over full scale**, so they get soft-clipped on the way out. Loud, modern masters can do this at TRIM's centre: the EQ's crossovers shift the low end's phase against the rest, which raises an already-limited track's peaks by up to about 5 dB. Turn TRIM down a little if a channel's red light keeps coming on.
- **On the controller,** the FLX6's five lights per channel come on at −24 and −14 dB (green), −8 and −3 dB (orange), and over 0 dB (red), the same marks as on screen. Channels 3 and 4 show decks C and D.
- **The broadcast view** shows the meters too, as late as the broadcast feed.

## Crossfader curve

**CURVE**, under the crossfader, sets how the crossfader shares the volume between its two sides. The small picture next to it shows each side's volume (left orange, right blue) from one end of the fader to the other.

- **Constant power** (the default): both decks at −3 dB in the middle, so a blend of two tracks stays about as loud as one. A deck playing on its own gets a little quieter as the crossfader reaches the middle.
- **Full centre:** both decks at full volume in the middle. Each fades out only on the far half, so a deck playing on its own stays at full volume until the crossfader passes the middle. Blends get louder in the middle, so ride the channel faders.
- **Linear:** straight fades, with both decks at −6 dB in the middle, for long, gentle blends.
- **Scratch:** both decks at full volume almost all the way across. A deck cuts in within the last 3% of travel, for cuts and scratching.

**◀ • ▶**, next to each channel's 🎧 CUE, is its crossfader assign switch: the channel is on the crossfader's left side (◀), its right side (▶), or through (•), where only its channel fader sets its level. Decks A and C start on the left and B and D on the right, and the letters at the crossfader's ends show which decks are on each side. Like the curve, the switches are part of the room.

The room has one curve, like the curve switch on a club mixer, so changing it changes it for your partner and the stream too. Tandem remembers the curve you last picked and starts on it. When you join a room, you get the host's.

## Play together

1. **Get connected.** The easiest way is for both of you to install [Tailscale](https://tailscale.com) (free) and sign in. You'll each get a `100.x.y.z` address that works from anywhere. Without it, the host has to forward TCP port 47800 on their router.
2. **Host.** One of you clicks **Host a room**, then sends the other one of the listed addresses and the room code. Click **Allow** if Windows Firewall asks.
3. **Join.** The other clicks **Join a room**, enters the address and code, and joins.
4. **Check the connection.** The top bar shows your partner, the round-trip time and **✔ in sync**. If it says **⚠ no reply for N s** instead, nothing has arrived from the other PC for a while: its connection has stalled. The room carries on if it comes back within 15 seconds.

**If the connection drops,** the joining PC gets back into the room by itself. Its top bar says **Reconnecting to … attempt N** while it tries (for up to 5 minutes), and the host's says **Waiting for … to reconnect**. The decks play on meanwhile, but what the joining DJ changes doesn't reach the room: once it's back, its decks follow the room again, as on a fresh join. **Give up** stops trying. Leaving on purpose, or the host closing the room, never starts this.

**Sync click** in the top bar plays a metronome on the shared room clock. Put both laptops side by side, or on a phone call, and the clicks should line up.

### Sharing crates

1. Pick a folder or Serato crate.
2. Click **Share this list…** and confirm you have the right to share it.
3. Your partner gets a popup and must agree to use the tracks for personal, non-commercial purposes before anything downloads.
4. Tracks they already have are skipped. The rest are saved to `Music\Tandem Shared\<your name>\<crate>` and show up under **Shared with me**.

Every agreement is written to a local consent log (Settings → General → Logs and consent record).

### Streaming the set

The easiest way is a third PC that joins as a broadcaster: see [Streaming from another PC](#streaming-from-another-pc). To stream from one of the DJs' PCs instead:

1. On the broadcaster's PC, install [VB-Audio Virtual Cable](https://vb-audio.com/Cable/).
2. Set **Settings → Audio → Broadcast feed** to **CABLE Input**.
3. In OBS, add an *Audio Input Capture* of **CABLE Output**.

The broadcast feed is delayed by 500 ms by default, so both DJs' moves are on time in it. If you add a camera in OBS, delay the video by the same amount.

**Show the decks.** Click **Broadcast view** in the top bar. It opens a second window made for viewers: the decks, the mixer and the scrolling waveforms (decks C and D too while either has a track), with the name of whoever last touched each deck. It runs the same delay behind the room as the broadcast feed, so the picture matches the sound.

- **OBS:** add a *Window Capture* of "Tandem broadcast". It needs no delay filter.
- **Discord:** share the "Tandem broadcast" window with its sound **off**, and set Discord's input device to **CABLE Output**, with noise suppression, echo cancellation and automatic gain control turned off. Discord captures everything Tandem plays, not just the broadcast feed, so don't share the window's sound.
- Keep the window visible, on a second screen if you have one. A minimised window can't be captured.
- It redraws at the **Screen frame rate** (Settings → General), 60 fps by default. That's already as smooth as the stream: Discord shares at 30 fps (60 with Nitro), and OBS usually records at 30 or 60.

### Streaming from another PC

Any other PC with Tandem can join the room **as a broadcaster**: a spare laptop, or a friend who runs the stream. It plays only the broadcast feed and shows the broadcast window, so there's no audio routing to set up.

1. On the streaming PC, click **Join a room**, enter the host's address and room code, tick **Broadcaster only**, and join.
2. The broadcast window opens. The PC plays the broadcast feed on **Speakers (master)** (Settings → Audio), and its headphones output is off.
3. **Discord:** share the "Tandem broadcast" window with its sound **on**. Tandem on that PC plays nothing but the broadcast feed, so that's all Discord picks up: no virtual cable, no microphone tricks.
4. **OBS:** a *Window Capture* of "Tandem broadcast" and an *Application Audio Capture* of Tandem (or a capture of its speakers). Neither needs a delay.

What a broadcaster can and can't do:

- **It can't change anything.** Its main window has no decks, mixer or library, only the DJs' names, whether the decks' tracks have arrived, the feed's level and these instructions. A controller plugged into it does nothing, and the host ignores anything it sends.
- **It doesn't need your music.** Tracks come from the host as the DJs load them, and are deleted when Tandem closes, like the session copies a DJ gets of their partner's tracks. A track takes a few seconds to arrive (a joining DJ's goes through the host), and its deck is silent on the stream until it has.
- **It runs 1000 ms behind the room,** twice a DJ's broadcast delay, because the joining DJ's moves reach it through the host. That leaves room for slow connections, and viewers can't tell. It's its own setting: Settings → Audio → Broadcast delay changes it while you're broadcasting, and your delay as a DJ stays as it was.
- **TIMING,** in its main window, shows how long the DJs' moves take to get there (the longest in the last minute). If that gets too close to the delay, some moves land late on the stream: TIMING turns amber, the top bar warns, and one click raises the delay enough. The stream repeats that much once as it changes, so do it between tracks if you can.
- **Both DJs see it** in the top bar (📡 and its name). Up to 4 broadcasters can join a room, alongside the two DJs. Tandem remembers the **Broadcaster only** tick for next time.
- **If the connection drops,** it gets back into the room by itself, and the stream plays on meanwhile as the room last was. **If the host leaves,** or it can't get back in within 5 minutes, its decks empty and it goes quiet. Join again to carry on.

## Trying it on one PC

Run a second, completely separate copy with its own settings:

```
tandem.exe --data-dir C:\temp\tandem-bob
```

Host in one window and join `127.0.0.1` from the other.

## Known limits in v0.11

- **Decks and DJs:** 4 decks, 2 DJs per room, plus up to 4 broadcasters. There's one sync master: you can't sync two groups of decks to two different tempos.
- **Beatgrids:** detection finds big, steady tempo changes but not gradual drifts, and doesn't find bars (the "1"). Some tracks come out at half or double time (87 instead of 174, say): **½** and **×2** under GRID fix that. Set markers by hand for both: see [Fixing a beatgrid](#fixing-a-beatgrid). A grid can have up to 32 markers, and every bar is 4 beats. Hot cues and play aren't snapped to the beat, except on a synced deck (loops are: see [Loops](#loops)). Quantized loops count bars from the beat you press IN on, not from the grid's bar 1.
- **Loops:** no slip mode or loop rolls, no fine-adjusting IN/OUT with the jog wheel, and loops aren't saved with the track.
- **Keys:** detection is good but not perfect: on a test library it matched reference tools on about 3 tracks in 4, and most misses were a compatible neighbour (the relative major/minor, or one step round the Camelot wheel). A track gets one key, even if it changes key part-way through.
- **Mixer:** no effects. EQ is a 3-band isolator, plus a filter and trim per channel.
- **Controllers:** MIDI only. Controllers that only speak HID aren't supported.
- **CPU:** the screen redraws use more CPU than the audio does, and the broadcast window adds about half again. They run at 60 fps by default. **Settings → General → Screen frame rate** goes from 30 fps up to your monitor's full rate, which costs two to three times as much CPU on a 120–144 Hz monitor. Key lock costs under 2% of one core per deck.
- **Audio driver:** WASAPI shared mode, roughly 10–20 ms of output latency. No ASIO yet.
- **Serato data:** crates are read, but Serato cue points and beatgrids aren't imported yet.
- **Security:** traffic isn't encrypted by Tandem itself. Use Tailscale, which is encrypted. The room code is a simple gate, not a password.

## Troubleshooting

- **Can't connect.**
  - Check that the address and room code are right.
  - The host must click **Allow** on the Windows Firewall prompt (or allow `tandem.exe` in firewall settings).
  - Without Tailscale, the host needs TCP 47800 forwarded.
  - **"This room already has two DJs":** to join just to stream, tick **Broadcaster only**.
- **The broadcaster says moves arrive late.** Click the raise button in its TIMING. If it only happens while tracks are being sent, lower **Upload speed limit** (Settings → General) on the host. That limit covers all of a PC's uploads together, so it keeps track transfers from filling the connection that carries the DJs' moves.
- **Controller not listed.** Close other DJ software, replug it, then click **Refresh**.
- **No sound.** Pick the right devices in **Settings → Audio** and check **Status** there.
- **Logs.** `%LOCALAPPDATA%\Tandem\data\tandem.log`. Settings → General has buttons to open every folder.

## Build from source

You need Rust (stable) and the Visual Studio Build Tools (C++ workload). The C++ compiler builds the vendored time-stretcher.

```
cargo build --release -p tandem      # target\release\tandem.exe
cargo test -p tandem-core            # core tests, including a room test over TCP
```

`tandem-core` is platform-independent and its tests also run on Linux and macOS. Windows builds and releases are produced by GitHub Actions (`.github/workflows/build.yml`). Push a `v*` tag to publish a release.

## License

[PolyForm Noncommercial 1.0.0](LICENSE). The source is available for any non-commercial use. This is intentionally *not* an OSI open-source license, because those can't forbid commercial use.

Tandem includes [Signalsmith Stretch](https://github.com/Signalsmith-Audio/signalsmith-stretch) for key lock and key shift (MIT). See [THIRD_PARTY.md](THIRD_PARTY.md).
