# Tandem

Back-to-back DJing with friends, from anywhere. Up to four DJs in different places share up to four decks and a mixer, each hears the full mix at full quality in their own booth, and one of you, or another PC, can stream the set.

**Personal, non-commercial use only.** See [TERMS.md](TERMS.md).

> **Status: v0.13.** Windows only. Rooms of up to four DJs, your own crates shared live with the room, four decks, BPM and key detection, beatgrid editing, sync, key lock, loops, level meters, crossfader curves, jog wheels that spin on like Serato, a broadcast view for streams, broadcaster-only booths and a ready-made DDJ-FLX6 mapping all work, and there are ready-made Denon SC Live 4 and Hercules DJControl Inpulse 200 MK2 mappings, not yet tried on the hardware; this is still an early version for a few friends (and a broadcaster) to try. See [docs/ROADMAP.md](docs/ROADMAP.md) for what's next and a test plan.

## How it works

Every DJ's laptop runs the whole mix locally. Only small, timestamped control messages ("deck A play at 12:04:31.208") cross the internet, and all the machines agree on one shared clock. The same moves at the same timestamps give the same mix on every laptop.

- **Your own controls are instant.** Your partner's moves reach you about half a round trip late, typically 20–80 ms. With more than two of you, everything goes through the host's PC, so a move between two DJs who both joined takes both of their trips to the host.
- **The broadcast is exact.** The streaming feed runs half a second behind, so every DJ's moves land in it exactly where they happened.
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

The self-test lists your audio and MIDI devices, opens your default output for half a second of silence, and runs a full room test over the network stack, with four DJs and a broadcaster. It ends with `SELF-TEST PASSED`.

## Set up your booth

1. **Music.** In the library, click **Add music folder…**. Serato crates from `Music\_Serato_` (and `X:\_Serato_` on external drives) show up automatically. Tandem only reads them and never writes Serato files.
   - **All Music**, at the top of the library's list, shows every track in your folders, **Shared with me**, your crates and your Serato crates in one list, each track once. Search, sorting and **Analyze BPM** work on it like any other list. It's rescanned each time you pick it, so new files and crate changes show up.
   - **⟳ Resync folders** (under your folders) looks again for songs you've added, changed or removed since Tandem started: in your folders, **Shared with me**, your crates and Serato's. The lists stay on screen while it looks, and the status bar then says how many tracks came and went. **Rescan crates** under the Serato crates does the same.
   - **Your own crates.** Under **MY CRATES**, click **➕ New crate** and name it. Then drag tracks from any list onto the crate, or right-click a track → **Add to crate** (dropping a track on **➕ New crate** starts a crate with it). Tracks stay in the order you added them; sort by a column as in any list. Right-click a track in a crate → **Remove from this crate**, and right-click the crate to rename, share or delete it. A crate only lists your files: removing a track or deleting the crate never touches the file. Crates are kept in `%LOCALAPPDATA%\Tandem\data\crates.json`. A file you move or delete afterwards drops out of the crate's list, which then says how many are missing.
   - **Click a column heading** (TITLE, ARTIST, BPM, KEY, TIME) to sort the list by it. Click again to reverse it, and a third time to go back to the list's own order. Keys sort in Camelot order (1A, 1B, 2A…). Tracks with no BPM or key yet go last. Tandem remembers the sort.
   - **Search** looks in titles and artists. Every word has to match, and `|` separates alternatives (`house | garage`). The **✖** at the end of the box, or Escape while you're typing, clears it, and the track you had selected stays selected in the whole list.
2. **Audio.** Open **Settings → Audio**.
   - **Speakers (master):** your controller's sound card, channels 1/2.
   - **Headphones (cue):** the same device, channels 3/4. Most controllers expose four channels. If the master plays through a Pioneer DDJ-FLX6, DDJ-FLX4 or DDJ-400, leaving this on **Auto** sends the headphones to its jack.
   - Use the **🎧 CUE** buttons on the mixer to send a deck to your headphones, and **phones: cue ↔ master** to blend. Those Pioneer controllers blend cue and master themselves: their **HEADPHONES MIX** knob and **MASTER CUE** button do it, and the slider on screen is greyed out.
   - Pressing CUE with no headphone output set up shows a reminder in the status bar.
3. **Controller.** Close Serato, rekordbox and any other DJ app, because only one app can use the controller at a time. Then open **Settings → Controller** and pick your controller.
   - **Pioneer DDJ-FLX6, DDJ-FLX4 and DDJ-400 map themselves.** Everything on the deck and mixer sections works straight away, including the pads, jog wheels, SYNC and the button lights. On the FLX6, decks 1–4 are Tandem's decks A–D: the **DECK** buttons pick which one each side plays, and mixer channels 3 and 4 are decks C and D. See [Four decks](#four-decks).
   - **Denon SC Live 4 maps itself** too, in computer mode (not yet tried on the unit). The left side is deck A and the right side deck B, with mixer channels 1 and 2. Its pad mode buttons work: **HOT CUE** for hot cues (SHIFT + pad clears one), **LOOP** for beat loops of ¼ to 32 beats and **ROLL** for loop rolls of 1/16 to 8 beats; each side keeps its own mode, and **SLICER** leaves the pads as they were. SHIFT works as on the FLX6 for CUE, SYNC and the tempo fader. An SC_LIVE_4.json mapping made by hand for an earlier version becomes the built-in mapping by itself, keeping your jog calibration and any controls you changed.
   - **Hercules DJControl Inpulse 200 MK2 maps itself** too (not yet tried on the unit). The left side is deck A and the right side deck B. In **HOT CUE** mode its four pads are hot cues 1 to 4 (SHIFT + pad clears one); the STEMS, FX and SAMPLER pads do nothing in Tandem. **LOOP** starts a 4-beat loop and leaves it again, **½** and **X2** halve and double it, and with SHIFT they are loop **IN** and **OUT**. After ½ or X2, the first press of LOOP goes back to 4 beats and the second leaves the loop. SHIFT works as on the FLX6 for CUE, SYNC and the tempo fader, and SHIFT + pressing **BROWSER** takes the knob to the lists. Calibrate the jog wheel with your hand on top of the platter before you scratch: the built-in figure is the first Inpulse 200's. What still needs checking on the unit is in [docs/controllers](docs/controllers/hercules-djcontrol-inpulse-200-mk2.md).
   - **Other controllers:** click **Learn** next to each function, then move or press the matching control. If the controller's SHIFT and pad mode buttons don't change what it sends, learn them too (**SHIFT** and **Pad mode**), then hold SHIFT or pick the pad mode before pressing a control to learn: it then does that only with SHIFT, or only in that pad mode.
   - **Calibrate the jog wheel** once (Settings → Controller → **Calibrate…**: turn the platter exactly one full turn). Then scratching moves the record exactly as far as your hand, like a 33⅓ record.
   - **Spin the record and let go, and it keeps spinning,** like in Serato. It carries on at the speed your hand left it and slows down gradually: a hard backspin runs on for over a second, a gentle flick stops quickly. On a playing deck the track then picks up speed and plays on; on a paused deck it comes to a stop. Touch the platter to catch it. Let go with your hand still (or moving with the music) and the record stays exactly where you left it, so drops and cue points are exact.
   - **Faders and knobs use soft takeover.** A control nobody has moved yet takes your hardware position straight away. Once the room value has changed, your fader takes over only when it reaches that value, so you never jump over your partner's mix.
   - **The tempo fader never waits.** After SYNC (or your partner) moves a deck's tempo away from where your fader sits, the fader changes the tempo by exactly as much as you move it, starting from the deck's current tempo. **SHIFT + SYNC** puts the deck back on the fader.
   - **SHIFT + tempo fader** moves the fader without changing the tempo. Out of travel? Hold SHIFT, slide the fader back, let go and keep going the same way, past the fader's range, up to ±50%. On another controller, learn **SHIFT + tempo fader** (moving the fader with SHIFT held, if SHIFT is learned too).
   - **SHIFT + CUE** (or right-clicking CUE) jumps back to the start of the track.
   - **Hot cues held while paused are gated.** The deck plays from the hot cue only while you hold the pad. Let go and it snaps back to the hot cue, paused. Press PLAY while holding to keep it playing.
   - **Loops:** **IN**, **OUT** and **RELOOP/EXIT** work as in [Loops](#loops). **SHIFT + IN** is a 4-beat loop, and **CUE/LOOP CALL ◀ / ▶** halve and double the loop. Switch the pads to **BEAT LOOP** (SHIFT + BEAT JUMP) for ¼, ½, 1, 2, 4, 8, 16 and 32-beat loops. The pad of the loop that's playing lights up.
   - **Loop rolls:** switch the pads to **BEAT JUMP** (**ROLL** on the SC Live 4). Hold a pad and the deck loops 1/16, 1/8, ¼ or ½ beat (top row) or 1, 2, 4 or 8 beats (bottom row) while the track runs on underneath. Let go and it carries on where the track has got to, as if it had never rolled. See [Loops](#loops).
   - **Browsing:** the browse knob moves through the tracks. Press it to move through the lists on the left instead (All Music, This room, Shared with me, the lists shared live in the room, your folders, your crates and Serato crates): each list's tracks show as you turn. Press it again to go back into the tracks, pick one and press LOAD. **BACK** also takes the knob to the lists. A blue outline shows which side the knob is on. Clicking a list with the mouse puts the knob back on its tracks, and closing the MY CRATES or SERATO CRATES section makes the knob skip those crates. On other controllers, learn **Library browse press** and **Library back**.
   - **Jog wheel moves the wrong way or jumps?** Tick **1/127 style** or **invert** next to it.
   - **The channel level meters** on the FLX6, FLX4 and DDJ-400 show each channel's deck, like the meters on screen: see [Level meters](#level-meters).
   - **The FLX6's Beat FX strip works** the deck banks of [Effects](#effects): FX SELECT, CH SELECT, BEAT ◀ ▶, LEVEL/DEPTH and ON/OFF. On other controllers, **Learn** the **FX unit** functions.
   - **MASTER LEVEL and HEADPHONE LEVEL** set your own master and headphone levels, never your partner's, and the sliders on screen follow them. So the master knob works whatever Tandem plays through, your PC's speakers included. They're mapped on the FLX6 and the Inpulse 200 MK2; on other controllers, **Learn** them. When the master (or the headphones) plays through the controller's own sound card, the controller turns it up and down there by itself, so the knob leaves Tandem's level alone rather than turning the sound down twice. Tandem knows the controller's sound card by its name: Windows names it after the controller, e.g. "Line (DDJ-FLX6)". Settings → Controller says what each knob does with your audio settings.

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
- **A loaded track always starts paused** at its start, with sync and any loop off, ready to cue.
- **You can't load over music the room can hear.** If a deck is playing and audible in the mix, loading onto it is refused, from the library, the controller's LOAD buttons or your partner's booth, with a note at the bottom of the window: *Deck A is playing: pause it or pull its fader down to load onto it.* A playing deck that's as good as silent can be loaded over (it stops and takes the new track):
  - its **channel fader** at about 10% or below (−40 dB),
  - the **crossfader** all the way to the other side (a channel on THRU never counts),
  - its **filter** turned all the way, either way (the last 1% of travel),
  - or its **trim** all the way down, or all three **EQ** bands killed.

  These add up: the fader at 30% with TRIM well down counts too. Your headphone cue doesn't matter: it's what the room hears. If you load just as your partner brings that deck up, whichever happened first wins, on both booths.
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
- **Loop rolls** (the BEAT JUMP pads on the FLX6) loop only while you hold the pad, and the track runs on underneath, shown as a dashed line on the waveform. Let go and playback carries on from that line, exactly where the track would have been, still on the beat. A roll starts on the beat the playhead is in, like a beat loop. Press another roll pad while holding one to switch size: the roll keeps its start and stays in step with the track. Let go of it and the first pad's roll comes back.
  - Rolls only work on a playing deck. PAUSE, a jump (CUE, a hot cue, a seek) or any loop button lets go of the roll first. Tempo, SYNC and the mixer carry on under it, and scratching moves what you hear while the track runs on.
  - **Over a loop,** the loop keeps going round underneath, and letting go of the roll comes back into it.
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
- **Loading:** **Load to A B C D** in the library, the right-click menu on a track, or the controller's LOAD buttons. A double-click loads onto the first deck on screen that's stopped, otherwise onto one that's playing but silent in the mix (see [BPM, sync and keys](#bpm-sync-and-keys)).
- **On the DDJ-FLX6,** press a side's **DECK** button to switch it between decks 1 and 3 (or 2 and 4): decks 3 and 4 are C and D, with their own pads, loops, jog and tempo fader. Mixer channels 3 and 4 are C and D's channels, with their meters. A mapping you saved with v0.10 or earlier moves its deck 3 and 4 controls over by itself, keeping your jog calibration and any controls you changed. Soft takeover and the tempo fader work per deck, just as on A and B. Switching never makes a deck's tempo jump: after a switch, the tempo fader moves the deck you switched to by as much as you move it, from that deck's own tempo, as it does after SYNC. **SHIFT + SYNC** puts the deck back on the fader.
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

## Effects

Every deck has a bank of three effect slots, after its channel fader and the crossfader. Each slot holds an effect, is on or off, and has a depth; the bank has one **BEATS** value, from 1/16 to 32 beats of that deck. The slots run in series, 1 → 2 → 3. A new bank has ECHO, REVERB and PHASER, all off at half depth, BEATS 1.

- **ECHO** repeats the sound every BEATS, each repeat a little quieter and duller. Depth sets how loud the echoes are and how long they go on.
- **REVERB** puts a room round the sound. Up to half depth it adds reverb under it; past half the sound itself fades, to reverb alone at full depth. BEATS sets how long it rings: 60 dB in twice BEATS (between 0.3 and 10 seconds).
- **PHASER** sweeps notches up and back once every BEATS, locked to the beatgrid. Depth sets how deep they cut.

How they behave:

- **After the fader.** Close the channel fader or cut the deck with the crossfader and an echo or reverb rings on: echo out of a track, then bring the next one in. Your headphone cue hears the deck without its effects.
- **Switching an effect off lets its tail ring out.** Turning its depth down silences it at once. Picking another effect in a slot that's on switches at once, and the old one rings out.
- **They're part of the room,** like the EQ: your partners and the broadcast hear exactly what you hear. A track loaded on a deck keeps the deck's effects, and you can load over a deck whose echo is still ringing once its fader is down.
- **Timed by the deck,** from its BPM and tempo, or 120 BPM on a track without a beatgrid. Echoes longer than 2 seconds are halved until they fit.

**On screen,** the FX strip under the scrolling waveforms has every deck's bank (A and B, then C and D in the four-deck view). Click an effect's name to switch it on and off: it lights in the deck's colour. The menu beside it picks the effect, the slider sets its depth (double-click: 50%), and **◀ ▶** step BEATS. Hover over anything to see what it does for that effect. A deck with an effect on shows **FX** in its header, and the broadcast view shows the effects that are on under each channel. The triangle at the strip's left folds it to one line naming the effects that are on; Tandem remembers.

**On the DDJ-FLX6** the Beat FX strip works the banks. FX1 and FX2 are two FX units, each working one deck's bank:

- **CH SELECT** picks the deck: CH1–4 are decks A–D. On **MST** the unit does nothing (there's no master bank yet).
- **FX SELECT** picks the slot, 1 to 3 on either unit. **LEVEL/DEPTH** and **ON/OFF** work the slot it's on, and ON/OFF flashes while that slot is on. **BEAT ◀ ▶** step the deck's BEATS, and the BEAT lights show it.
- **The knob never makes a running effect jump.** On a slot that's off it sets the depth straight away. Switching a slot on sets its depth to where the knob is, so with the knob down an effect comes in from nothing as you turn up. On a slot that's already on (your partner switched it on, or you moved the knob for another slot), the knob takes over once it reaches the slot's depth, like the faders. Pressing FX SELECT never changes a depth.
- **Both units start on deck A,** as the FLX6 does when it's switched on, and Tandem follows them from there: CH SELECT tells it, and so does FX SELECT moving onto the other unit. If the strip works the wrong deck (the channels were changed before Tandem started), press CH SELECT once round, or move FX SELECT across to the other unit and back.

**On other controllers,** learn the **FX unit** functions in Settings → Controller: each slot's on/off, depth and next effect, a depth knob for all three slots, BEATS ◀ and ▶, and which deck the unit works.

## Small screens

Tandem fits laptop screens, down to a 1080p laptop at 175% display scaling.

- **It opens maximised** when its usual window doesn't fit the screen.
- **On a short window the library gets less height first.** Then the two decks switch to compact panels with a shorter mixer: each channel's knobs sit beside its fader, and **🎧 LEVELS** opens your headphone and master levels.
- **Every scrolling waveform stays on screen.** With four decks on a very short window, the deck panels and mixer scroll instead (a scroll bar shows beside them).
- **The FX strip takes its height from the waveforms first, then the library,** which keeps its 150 points. Where its rows don't fit without making the deck controls scroll, it folds to one line naming the effects that are on; on the smallest windows (two decks under about 535 points tall, four decks under about 660) the line goes too, and the effects that are on show in the corner of each waveform: click one to work that deck's effects.
- **Drag the top edge of the library** to change its height. Tandem keeps your height from then on, but the library never covers the waveforms.

## Play together

1. **Get connected.** The easiest way is for both of you to install [Tailscale](https://tailscale.com) (free) and sign in. You'll each get a `100.x.y.z` address that works from anywhere. Without it, the host has to forward TCP port 47800 on their router.
2. **Host.** One of you clicks **Host a room**, then sends the others one of the listed addresses and the room code. Click **Allow** if Windows Firewall asks. Only the host needs to be reachable: everyone else connects to the host.
3. **Join.** The others click **Join a room**, enter the address and code, and join. Up to three can join as DJs, so a room holds four.
4. **Check the connection.** The top bar shows who you're playing with, the round-trip time and **✔ in sync**. On the host's PC each DJ has their own round trip; on everyone else's, it's the trip to the host. If it says **⚠ no reply for N s** instead, nothing has arrived from that PC for a while: its connection has stalled. The room carries on if it comes back within 15 seconds.

Every DJ can work every deck and the whole mixer: there are no deck assignments, so agree among yourselves who takes which.

**If the connection drops,** the joining PC gets back into the room by itself. Its top bar says **Reconnecting to … attempt N** while it tries (for up to 5 minutes), and the host's says **Waiting for … to reconnect**. The decks play on meanwhile, and everyone can keep going: once it's back, what each of you did meanwhile comes into the room on every PC, at the moment it was done. On a control more than one of you touched, whichever move came later wins. The others' moves land when you're back together, so a fader they moved meanwhile jumps to where they left it, and a deck they started plays on from where it would be by now. A load onto a deck another DJ had playing out loud by then is refused, with the usual note. A track that was being sent either way pauses and carries on from where it got to once you're back together, and a deck waiting for it comes in where the room is. The room keeps your place while it waits, so nobody else can take your seat as a DJ. **Give up** stops trying; joining the same room again within the 5 minutes still brings your moves. Leaving on purpose, or the host closing the room, never starts this. When a DJ who joined leaves, the room carries on for everyone else; when the host leaves, the room closes.

**Sync click** in the top bar plays a metronome on the shared room clock. Put both laptops side by side, or on a phone call, and the clicks should line up.

### Sharing crates

Pick one of your crates, a folder or a Serato crate, click **Share…** above its tracks (or right-click the crate → **Share live with the room…**), confirm you have the right to share it, and pick how:

**📡 Share it live** (for building a set together):

- Every other DJ in the room sees the list in their library under **SHARED LIVE IN THIS ROOM**, as *list · your name*, with titles, artists, BPMs, keys and lengths. Nothing is copied to them.
- Any of them can load a track from it, from the screen or the controller, onto any deck. Only then does that one track come over, from your PC (through the host's when you both joined), exactly as when a DJ loads a track the others don't have: a session copy that goes when Tandem closes. The deck says **Receiving this track… N%** meanwhile.
- A track you haven't analysed (no BPM in your list) comes over to the DJ who loaded it first and is analysed there, so it lands on the deck with its beatgrid and key, as a track from their own library would. That takes as long as the download.
- Changes you make reach the others within a second or so: tracks you add to the crate or take out, a rename, a resync that finds new songs. BPMs you analyse follow every 10 seconds.
- It stays shared until you click **Stop sharing** (or right-click the crate), delete it, or leave the room, and it goes with you if you leave. Through a dropped connection it carries on, and a DJ who joins later gets it too. Broadcasters don't see it.
- Up to 500 tracks can be shared live (the whole list goes out again on each change, on the connection your moves use): put the tracks you want in a crate. Your list shows **📡 Shared live (N)** while it's shared, and the crate a 📡 on the left.

**⬇ Send copies to keep:**

1. Every other DJ in the room gets a popup, and each must agree to use the tracks for personal, non-commercial purposes before anything downloads to them.
2. Tracks they already have are skipped. The rest are saved to `Music\Tandem Shared\<your name>\<crate>` and show up under **Shared with me**.

A share between two DJs who both joined goes through the host's PC. The host passes the tracks on without keeping them: they sit in its session folder, which is emptied when Tandem closes, unless the host accepts the share too. **All Music** and **Shared with me** can only be sent as copies.

Every agreement is written to a local consent log (Settings → General → Logs and consent record), live shares included.

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
- **It runs 1000 ms behind the room,** twice a DJ's broadcast delay, because the joining DJs' moves reach it through the host. That leaves room for slow connections, and viewers can't tell. It's its own setting: Settings → Audio → Broadcast delay changes it while you're broadcasting, and your delay as a DJ stays as it was.
- **TIMING,** in its main window, shows how long the DJs' moves take to get there (the longest in the last minute). If that gets too close to the delay, some moves land late on the stream: TIMING turns amber, the top bar warns, and one click raises the delay enough. The stream repeats that much once as it changes, so do it between tracks if you can.
- **Every DJ sees it** in the top bar (📡 and its name). Up to 4 broadcasters can join a room, alongside up to four DJs. Tandem remembers the **Broadcaster only** tick for next time.
- **If the connection drops,** it gets back into the room by itself, and the stream plays on meanwhile as the room last was. **If the host leaves,** or it can't get back in within 5 minutes, its decks empty and it goes quiet. Join again to carry on.

## Trying it on one PC

Run a second, completely separate copy with its own settings:

```
tandem.exe --data-dir C:\temp\tandem-bob
```

Host in one window and join `127.0.0.1` from the other.

## Known limits in v0.13

- **Decks and DJs:** 4 decks and up to 4 DJs per room, plus up to 4 broadcasters. Every DJ can touch every deck: there are no deck assignments. There's one sync master: you can't sync two groups of decks to two different tempos. The host's PC carries everyone's moves and tracks, so its connection matters most.
- **Beatgrids:** detection finds big, steady tempo changes but not gradual drifts, and doesn't find bars (the "1"). Some tracks come out at half or double time (87 instead of 174, say): **½** and **×2** under GRID fix that. Set markers by hand for both: see [Fixing a beatgrid](#fixing-a-beatgrid). A grid can have up to 32 markers, and every bar is 4 beats. Hot cues and play aren't snapped to the beat, except on a synced deck (loops are: see [Loops](#loops)). Quantized loops count bars from the beat you press IN on, not from the grid's bar 1.
- **Loops:** loop rolls are the only slip feature (no slip mode for scratching, hot cues or loops), there's no beat jump (the FLX6's BEAT JUMP pads roll) and no slicer, no fine-adjusting IN/OUT with the jog wheel, and loops aren't saved with the track.
- **Keys:** detection is good but not perfect: on a test library it matched reference tools on about 3 tracks in 4, and most misses were a compatible neighbour (the relative major/minor, or one step round the Camelot wheel). A track gets one key, even if it changes key part-way through.
- **Mixer:** EQ is a 3-band isolator, plus a filter and trim per channel.
- **Effects:** echo, reverb and phaser only, a bank per deck. There's no master bank (the FLX6's CH SELECT on MST does nothing), no effects in the headphone cue, no repeater, no pad FX or Merge FX, and the DDJ-FLX4 and DDJ-400 Beat FX strips aren't mapped (they send other messages than the FLX6's). Effects add level to a mix that may already reach full scale, so watch the master meter.
- **Controllers:** MIDI only. Controllers that only speak HID aren't supported.
- **CPU:** the screen redraws use more CPU than the audio does, and the broadcast window adds about half again. They run at 60 fps by default. **Settings → General → Screen frame rate** goes from 30 fps up to your monitor's full rate, which costs two to three times as much CPU on a 120–144 Hz monitor. Key lock costs under 2% of one core per deck. A reverb costs about 0.3% of one core in each place it's heard (your speakers, the broadcast feed), an echo or phaser about 0.1%, and an effect that's off and has rung out nothing.
- **Audio driver:** WASAPI shared mode, roughly 10–20 ms of output latency. No ASIO yet.
- **Serato data:** crates are read, but Serato cue points and beatgrids aren't imported yet.
- **Crates:** a crate remembers each track by where the file is, so a file you move drops out of it. There's no picking several tracks at once: drag them in one at a time (a whole folder can be shared live as it is). You can't reorder a crate by hand.
- **Sharing live:** up to 500 tracks a list, and only the DJ who shares a list can change it. Lists aren't shared again by themselves in your next room.
- **Security:** traffic isn't encrypted by Tandem itself. Use Tailscale, which is encrypted. The room code is a simple gate, not a password.

## Troubleshooting

- **Can't connect.**
  - Check that the address and room code are right.
  - The host must click **Allow** on the Windows Firewall prompt (or allow `tandem.exe` in firewall settings).
  - Without Tailscale, the host needs TCP 47800 forwarded.
  - **"This room already has 4 DJs":** to join just to stream, tick **Broadcaster only**. A DJ whose connection dropped keeps their seat for 5 minutes while they reconnect.
- **The broadcaster says moves arrive late.** Click the raise button in its TIMING. If it only happens while tracks are being sent, lower **Upload speed limit** (Settings → General) on the host. That limit covers all of a PC's uploads together, so it keeps track transfers from filling the connection that carries the DJs' moves.
- **Controller not listed.** Close other DJ software, replug it, then click **Refresh**.
- **A control does nothing, or the wrong thing.** Record what the controller sends: close Tandem, double-click **record-midi.bat** (next to `tandem.exe`), type a name for the recording, then press the controls you were asked to, about a second apart. Each message shows up as it arrives. Press Enter when you're done. The file is saved next to `tandem.exe`, and a window opens showing it, ready to send to whoever is fixing the mapping.
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
