# Hercules DJControl Inpulse 200 MK2

**Status:** researched, not yet tried on the unit. Researched 2026-11-20. Built-in preset "Hercules DJControl Inpulse 200 MK2" (not released yet).

A two-deck beginner controller from 2023: per deck a touch-sensitive jog wheel, PLAY, CUE, SYNC, SHIFT, a LOOP button with ½ and X2, a tempo fader, four pads with two mode buttons (HOT CUE and STEMS, and with SHIFT: FX and SAMPLER), and a mixer channel with GAIN, a two-band EQ (HIGH, LOW), FILTER, a headphone-cue button and a volume fader. In the middle: the BROWSER knob, ASSIST./PREP., MASTER and HEADPHONES volume knobs, a MASTER monitoring button, BEATMATCH GUIDE and the crossfader. It has a built-in four-channel sound card.

This covers the **MK2** (USB 06f8:b130). The original Inpulse 200 ("MK1") has the same body with other labels (VINYL where the MK2 has LOOP, LOOP IN/OUT where the MK2 has ½/X2, a loop/roll pad mode where the MK2 has STEMS); the Inpulse 200 MK3 has a three-band EQ and other buttons. Hercules publishes a MIDI list for the MK1 and none for the MK2. Everything found says the MK2 kept the MK1's messages and only changed the labels, but each row below says what that rests on. The unit needs no mode: it speaks MIDI as soon as it's plugged in.

Channels are written 1-based the way documents write them in the table under "Layout", and as status bytes (`91 07` = note 0x07 on MIDI channel 2) everywhere else. Tandem's `Trigger` channel is the status byte's low digit.

## Connecting

- **MIDI port name: `DJControl Inpulse 200 Mk2`.** Found in three places: the `endpointName` of the djay mapping [4] (made on iOS), the Windows scripts of [5] and [14] (`startswith('DJControl Inpulse 200 Mk2')`, in a `.bat`), and the Linux capture tool of [6]. Windows may put "2- " in front. The preset matches any port whose letters and digits contain `INPULSE200MK2`, so the MK1 ("DJControl Inpulse 200", from Hercules' own Traktor mapping [12]) and the MK3 don't match. Nobody was found describing a second MIDI port, or the name with Hercules' driver pack installed.
- **Driver or mode:** none needed. "Plug & Play ... complies with the USB Audio and USB MIDI standards" ([1] p.44; [3]). The Hercules driver pack only adds ASIO and a control panel.
- **Sound card:** outputs 1-2 are the master (RCA), 3-4 the headphones (3.5 mm jack), 24-bit, 44.1 kHz ([1] p.6, p.42-43). The **MASTER and HEADPHONES volume knobs act in hardware** on those outputs ([3]: "Hardware driven"; [2]; [13] for the MK1) and also send MIDI. The **MASTER button** routes outputs 1-2 to the headphones in hardware ([1] p.43, [2]) and also sends MIDI.
- **Init or keep-alive:** none needed for the controls or the lights: [5] and [7] light the unit with plain note-ons and send nothing first. Two optional messages exist in the family and Tandem sends neither: `B0 7F 7F` asks the unit to send every knob and fader position ([8] p.27, sent by [9] and [10] at start), and [9]/[10] send `B0 7F 7E` when they close (its meaning isn't documented).
- At each connection the unit plays a light sequence ("Vegas Mode", four phases, [1] p.28). On a USB power supply with no computer it loops a demo ([1] p.48).

## Layout

| MIDI channel | Status bytes | Tandem channel | Carries |
|---|---|---|---|
| 1 | `90` / `B0` | 0 | Global: browser, crossfader, MASTER and HEADPHONES knobs, ASSIST./PREP., BEATMATCH GUIDE, MASTER button |
| 2 | `91` / `B1` | 1 | Deck 1 (left): buttons, knobs, faders, jog |
| 3 | `92` / `B2` | 2 | Deck 2 (right) |
| 4 | `93` / `B3` | 3 | Global controls with SHIFT held |
| 5 | `94` / `B4` | 4 | Deck 1 with SHIFT held |
| 6 | `95` / `B5` | 5 | Deck 2 with SHIFT held |
| 7 | `96` | 6 | Deck 1 pads |
| 8 | `97` | 7 | Deck 2 pads |

The design questions:

- **SHIFT changes the messages.** A deck's buttons, knobs, fader and jog move three channels up while its SHIFT is held (`91` → `94`), and its pads send their note + 8 on the same channel. SHIFT also sends a note of its own (`91 04`), which Tandem doesn't need. Sources: [4] (shifted loop buttons, jog and pads on the MK2), [7] ("1(4) = global (shifted), 2(5) = left deck (shifted), 3(6) = right deck (shifted) ... the shift key changes the note numbers of the pads", tested on an MK2), [5] (the lights' SHIFT layer), and the MK1 list [8]. So every SHIFT function is its own binding and nothing is bound to `Target::Shift`.
- **Pad modes: per-mode notes.** The unit keeps the pad mode itself and the pads send a different note range in each: HOT CUE 0x00-0x03, STEMS 0x10-0x13, FX (SHIFT + HOT CUE) 0x20-0x23, SAMPLER (SHIFT + STEMS) 0x30-0x33, each + 8 with SHIFT ([4], [5], [7]; [8] for the MK1's four modes). No `Target::PadMode`. Only HOT CUE has a Tandem function.
- **Deck layers:** none. Two decks, A and B.
- **Jog:** turning the platter with a hand on top sends CC 0x0A, turning it by the ring sends CC 0x09, both relative two's complement: 1 = one tick clockwise, 127 = one tick anticlockwise. The MK2 sources only ever show 1 and 127 ([6], [7]); the MK1 list ([8] p.14) allows bigger steps on fast turns (up to 0x3F, down to 0x40), which Tandem reads the same way. The top's touch sensor is note 0x08. Both CCs are bound to `Target::Jog`. Ticks per turn: **248, inferred** from the MK1 (Mixxx's `scratchEnable(deck, 248, ...)` in [9], credited to Hercules' technical support; [10] reuses it for the MK3). Nothing gives the MK2's figure, and [7] reports that on the MK2 the ring sends ticks more slowly than the top does, so the figure can only fit one of them: calibrate with a hand on top.
- **Faders and knobs: all 14-bit**, coarse on CC n and fine on CC n + 32, the layout Tandem joins by itself ([6]: recorded from an MK2 for the crossfader, both volume faders, filter, low, high, gain and tempo; [8] p.12-17). No bound CC has an unrelated control 32 above it.
- **Tempo fader direction: the value falls as the tempo rises, so the preset inverts it. Inferred.** The fader is marked "−" at the top and "+" at the bottom, and "Move up: slows down the track" ([1] p.10 and the panel photo in [3]). Which end sends the high value:
  - Siblings, followed through: Mixxx's MK1 mapping [9] binds the 14-bit value straight to `rate` with no invert, and the MK3 script [10] computes `rate = position / 8192 - 1`. In Mixxx, `rate` + 1 is *slower* by default (`kDefaultRateDirectionInverted = true` and `rateRatio = 1 + rate_dir * range * rate`, [11]). So on the MK1 and the MK3 the high value is the slow end, the top.
  - Two MK2 mappings flip it too, each resting on a convention that wasn't checked: the bridge [5] sends `127 - value` on to rekordbox's TempoSlider (if rekordbox reads that as Pioneer's own faders send it, high at "+", the MK2's high value is at "−"), and the djay mapping [4] marks `turntable.speed` `flipped` on both decks and nothing else but the browser.
  
  Nobody states it in words and [6] says its capture doesn't settle it. It's one move for the owner to check (step 4 of the checklist).

## Controls

"Confirmed" here means two independent sources **for the MK2** agree (or MK2 capture bytes plus an MK2 source that names them). "Single source" is one MK2 source; where the MK1's manufacturer list [8] says the same, that is noted but doesn't raise the level. "Inferred" rests on the MK1 or MK3 only. Deck 1 is shown; deck 2 is the same one channel up (`92`/`B2`, SHIFT `95`/`B5`, pads `97`).

### Deck

| Control | Message | Sends | Tandem | Confidence | Sources |
|---|---|---|---|---|---|
| PLAY | `91 07` | 7F press, 00 release | `Play` | confirmed | [4] [5] [6] [7] |
| SHIFT + PLAY | `94 07` | same | no Tandem control (stutter / play from cue) | inferred | [8] p.5, [9] |
| CUE | `91 06` | 7F / 00 | `Cue` | confirmed | [4] [6] [7] |
| SHIFT + CUE | `94 06` | 7F / 00 | `BackToStart` | single source | [7]; [8] p.5 and [9] agree |
| SYNC | `91 05` | 7F / 00 | `Sync` | confirmed | [4] [5] [7] |
| SHIFT + SYNC | `94 05` | 7F / 00 | `SyncOff` (what Serato does with it, [1] p.8) | single source | [7]; [8] p.5 and [9] agree |
| SHIFT | `91 04` | 7F / 00 | left unmapped: SHIFT changes the messages by itself | single source | [4]; [8] p.3 and [10] agree |
| LOOP | `91 03` | 7F / 00 | `BeatLoop` 4 beats (see "What Tandem can't do") | confirmed | [4] [5] [7] |
| SHIFT + LOOP (VINYL) | `94 03` | 7F / 00 | no Tandem control (vinyl mode is a setting in the mapping) | confirmed | [4] [5] [7] |
| ½ | `91 09` | 7F / 00 | `LoopHalve` | confirmed (message); the function follows the panel label, see Conflicts | [4] [7] |
| X2 | `91 0A` | 7F / 00 | `LoopDouble` | confirmed (message) | [4] [7] |
| SHIFT + ½ (IN) | `94 09` | 7F / 00 | `LoopIn` | single source | [4]; [8] p.5 and [9] agree |
| SHIFT + X2 (OUT) | `94 0A` | 7F / 00 | `LoopOut` | single source | [4]; [8] p.5 and [9] agree |
| ½ held about a second | `91 0B` | 7F / 00 | left unmapped: no separate function in Tandem | single source | [4]; [8] p.3 ("LOOP_IN long press") |
| Jog top, touched | `91 08` | 7F touch, 00 let go | `JogTouch` | confirmed | [4] [6] [7] |
| Jog turned, hand on top | `B1 0A` | relative, 1 = +1, 127 = −1 | `Jog` | confirmed | [4] [5] [6] [7] |
| Jog turned by the ring | `B1 09` | relative, 1 = +1, 127 = −1 | `Jog` | confirmed | [4] [5] [6] [7] |
| SHIFT + jog touch | `94 08` | 7F / 00 | no Tandem control | single source | [4]; [8] p.5 |
| SHIFT + jog turn | `B4 0A` (top), `B4 09` (ring) | relative | no Tandem control (fast search) | single source | [4]; [8] p.15 |
| TEMPO fader | `B1 08` + fine `B1 28` | 14-bit; high value at the "−" (top) end | `Tempo`, inverted | confirmed (message, 14-bit); direction inferred, see Layout | [4] [6] [7] |
| SHIFT + TEMPO fader | `B4 08` + fine `B4 28` | 14-bit | `TempoShift`, inverted | **inferred** | [8] p.15; [1] p.11 says Serato gives SHIFT + fader its own function |
| LOAD | `91 0D` | 7F / 00 | `Load` | confirmed | [4] [5] [7] |
| SHIFT + LOAD | `94 0D` | 7F / 00 | no Tandem control | single source | [7]; [8] p.10 |
| LOAD held | `91 0E` | 7F / 00 | no Tandem control | inferred | [8] p.3 |

### Pads

| Control | Message | Sends | Tandem | Confidence | Sources |
|---|---|---|---|---|---|
| HOT CUE mode button | `91 0F` | 7F / 00 | left unmapped: the unit switches the pads itself | confirmed | [4] [5] |
| STEMS mode button | `91 10` | 7F / 00 | left unmapped | confirmed | [4] [5] |
| SHIFT + HOT CUE (FX mode) | `91 11` | 7F / 00 | left unmapped | single source | [4]; [8] p.3 |
| SHIFT + STEMS (SAMPLER mode) | `91 12` | 7F / 00 | left unmapped | single source | [4]; [8] p.4 |
| Pads 1-4, HOT CUE | `96 00`-`96 03` | 7F / 00 | `HotCue` 1-4 | confirmed | [4] [6] [7] |
| SHIFT + pads, HOT CUE | `96 08`-`96 0B` | 7F / 00 | `HotCueClear` 1-4 | confirmed | [4] [7] |
| Pads 1-4, STEMS | `96 10`-`96 13` | 7F / 00 | no Tandem control (stems) | confirmed | [4] [5] [7] |
| SHIFT + pads, STEMS | `96 18`-`96 1B` | 7F / 00 | no Tandem control | single source | [5]; [8] p.6 |
| Pads 1-4, FX | `96 20`-`96 23` | 7F / 00 | no Tandem control (FX) | single source | [4]; [8] p.4 |
| SHIFT + pads, FX | `96 28`-`96 2B` | 7F / 00 | no Tandem control | single source | [4]; [8] p.6 |
| Pads 1-4, SAMPLER | `96 30`-`96 33` | 7F / 00 | no Tandem control (sampler) | confirmed | [4] [7] |
| SHIFT + pads, SAMPLER | `96 38`-`96 3B` | 7F / 00 | no Tandem control | single source | [4]; [8] p.7 |

### Mixer

| Control | Message | Sends | Tandem | Confidence | Sources |
|---|---|---|---|---|---|
| Volume fader | `B1 00` + fine `B1 20` | 14-bit | `Volume` | confirmed | [4] [5] [6] [7] |
| GAIN | `B1 05` + fine `B1 25` | 14-bit | `Trim` | conflict, see below | [4] [6]; [5] differs; [8] p.14 |
| HIGH | `B1 04` + fine `B1 24` | 14-bit | `Eq` high | conflict, see below | [4] [6]; [5] differs; [8] p.14 |
| LOW | `B1 02` + fine `B1 22` | 14-bit | `Eq` low | confirmed | [4] [5] [6] |
| FILTER | `B1 01` + fine `B1 21` | 14-bit, centre = off | `Filter` | confirmed | [4] [5] [6] [7] |
| Headphone cue button | `91 0C` | 7F / 00 | `Pfl` | confirmed | [4] [5] [7] |
| SHIFT + headphone cue | `94 0C` | 7F / 00 | no Tandem control | inferred | [8] p.10 and p.20 (p.5 prints `94 0D` for deck A, one of its typos); [3] "secret features" |
| SHIFT + volume, knobs | `B4 00`, `B4 01`, `B4 02`, `B4 04`, `B4 05` (+ fine) | 14-bit | no Tandem control (DJUCED: decks 3/4) | inferred | [8] p.15; [3]; [7] for the volume |
| Crossfader | `B0 00` + fine `B0 20` | 14-bit, 0 = full left | `Crossfader` | confirmed | [4] [6] [7] |

There is no MID knob and no cue/master mix knob on this unit.

### Browse and global

| Control | Message | Sends | Tandem | Confidence | Sources |
|---|---|---|---|---|---|
| BROWSER turned | `B0 01` | relative, 1 = +1 clockwise (down the list), 127 = −1 | `Browse` | confirmed | [4] [5] [7]; [6] says it recorded it |
| BROWSER pressed | `90 00` | 7F / 00 | `BrowsePress` | confirmed | [4] [5] [7] |
| SHIFT + BROWSER pressed | `93 00` | 7F / 00 | `BrowseBack` (DJUCED: back to the top of the library, [1] p.18) | **inferred** | [8] p.2, [9]; [7] says shifted global notes come on channel 4 |
| SHIFT + BROWSER turned | `B3 01` | relative | no Tandem control | inferred | [8] p.13, [9] |
| BROWSER turned with ASSIST./PREP. held | `B0 02` | relative | no Tandem control | inferred | [8] p.12 |
| ASSIST./PREP. | `90 03` | 7F / 00 | no Tandem control | confirmed | [4] [5] |
| BEATMATCH GUIDE | `90 01` | 7F / 00 | no Tandem control | confirmed | [4] [5] |
| MASTER (monitoring) | `90 02` | 7F / 00 | left unmapped: it routes the audio in hardware | confirmed | [4] [5] [7] |
| MASTER volume | `B0 03` + fine `B0 23` | 14-bit; also acts in hardware | `MasterLevel` | single source | [4]; [6] says it recorded CC 3/35 and 4/36 on channel 1 without telling them apart (not in its published excerpt); [8] p.12 and [10] agree |
| HEADPHONES volume | `B0 04` + fine `B0 24` | 14-bit; also acts in hardware | `HeadphoneLevel` | single source | [4]; as above |

Bound MASTER and HEADPHONES knobs are safe with the unit's own sound card: when Tandem's master or headphones play through a device whose name contains the port's name, Tandem leaves that level to the hardware (`ControllerCard`). The sound card's name on Windows wasn't found; it's on the checklist.

## Lights

Two sources drive an MK2's lights ([5] on Windows, [7] tested on an MK2 and an Inpulse 500), and the MK1's manufacturer list [8] p.18-26 describes the same scheme. The djay mapping [4] also declares a light output on many notes, without values:

- A light is a **note-on to its button's own channel and note, 7F lit and 00 off**. No dim state, no colours on buttons or pads.
- **Each SHIFT layer has its own lights.** While SHIFT is held the unit shows what was last sent to the SHIFT messages (`94 xx`, pads + 8). [5] sends every light to both layers "to ensure the light state is preserved when shift is pressed"; [10] says the same of the MK3.
- **A pad has a light per pad mode**, at that mode's note; the unit shows the ones for the mode it's in.

| Light | Message | Values | Lit by | Tandem | Confidence | Sources |
|---|---|---|---|---|---|---|
| PLAY | `91 07` | 7F / 00 | software | driven: lit while playing | confirmed | [5] [7]; [4] declares it; [8] p.18 |
| CUE | `91 06` | 7F / 00 | software | driven: lit while paused on a track or cueing | single source | [7]; [4] declares it; [8] p.18 |
| SYNC | `91 05` | 7F / 00 | software | driven: lit while synced | confirmed | [5] [7]; [4] declares it; [8] p.18 |
| LOOP | `91 03` | 7F / 00 | software | driven: lit while a 4-beat loop plays | confirmed | [5] [7]; [4] declares it |
| SHIFT + LOOP (shown while SHIFT is held; other software shows vinyl mode there) | `94 03` | 7F / 00 | software | not driven | confirmed | [5] [7]; [1] p.14 |
| ½ | `91 09` | 7F / 00 | software | not driven (halving has no state) | single source | [7]; [4] declares it; [8] p.18 |
| X2 | `91 0A` | 7F / 00 | software | not driven | single source | [7]; [4] declares it (deck 1); [8] p.18 |
| SHIFT + ½ / X2 (IN, OUT), shown while SHIFT is held | `94 09`, `94 0A` | 7F / 00 | software | driven: lit while looping (IN also while waiting for OUT) | inferred | [8] p.20 |
| Headphone cue | `91 0C` | 7F / 00 | software | driven | confirmed | [5] [7]; [4] declares it; [8] p.18 |
| Hot cue pads | `96 00`-`96 03` | 7F / 00 | software | driven: lit when the hot cue is set | single source for the address ([4] declares a light on these notes); the values are the scheme's, confirmed on the STEMS pads | [4]; [8] p.18 |
| SHIFT + hot cue pads, shown while SHIFT is held | `96 08`-`96 0B` | 7F / 00 | software | not driven | inferred | [8] p.20; [9] lights them |
| STEMS pads | `96 10`-`96 13` (SHIFT `96 18`-`96 1B`) | 7F / 00 | software | not driven | confirmed | [5] [7] |
| FX and SAMPLER pads | `96 20`-`96 23`, `96 30`-`96 33` | 7F / 00 | software | not driven | single source for the address | [4] declares them; [8] p.19 |
| HOT CUE / STEMS mode buttons (solid; flashing for FX / SAMPLER) | `91 0F`, `91 10` | 7F / 00 | **unclear**, see Conflicts | not driven | conflict | [5] vs [8], [10] |
| ASSIST./PREP. | `90 03` | 7F / 00 | software | not driven | inferred | [8] p.18 |
| MASTER and BEATMATCH GUIDE buttons | none | | the unit | | inferred | [8] p.18: "hardware controlled" |
| Browser ring (RGB "energy" light) | `90 05` | a colour number, 00 off (table in [8] p.25-26; e.g. 05 blue, 3C green, 60 red, 7C yellow, 7F white) | software | not driven | single source | [7] (sends a value to note 5, channel 1); [8] |
| BEAT ALIGN arrows (two per deck) | MK1: `91 1C`/`91 1D`; MK3: `91 21`/`91 22` | 7F / 00 | software | not driven | **unknown** for the MK2 | [8] p.24, [10] |
| TEMPO arrows (two per deck) | MK1: `91 1E`/`91 1F`; MK3: `91 23`/`91 24` | 7F / 00 | software | not driven | **unknown** for the MK2 | [8] p.23, [10] |
| Tempo fader centre light (green) | none found | | the unit, presumably | | unknown | [1] p.10 |

**Level meters:** the unit has none. The "VU meter" its manual mentions ([1] p.5) is the browser ring: the MK1 list calls that the "Energy Level vu-meter". `meters` is empty.

**On connect:** the Vegas Mode sequence plays ([1] p.28); what the lights show after it and before any software talks to the unit wasn't found. **When the software leaves:** not found. Tandem sends 00, which is this unit's "off", to every light it lit.

**Colours:** none found for buttons or pads on the MK2, and the pads look single-colour (red) in every photo. Mixxx's MK1 mapping [9] lights set hot cue pads with 7E instead of 7F without saying why; the MK1 list says 7F.

### Lights Tandem doesn't drive yet

- The SHIFT layer of PLAY, CUE, SYNC, the headphone cue and the hot cue pads (`94 05`-`94 07`, `94 0C`, `96 08`-`96 0B`): these go dark while SHIFT is held, because Tandem lights a button only at its own note and the SHIFT + functions on those notes (`SyncOff`, `BackToStart`, `HotCueClear`) have no state to show.
- The browser ring, `90 05` with a colour number: no target for it.
- The BEAT ALIGN and TEMPO arrows: Tandem has no beatmatch guide, and the MK2's addresses are unknown (the MK1 and the MK3 differ).
- ASSIST./PREP. (`90 03`), the STEMS / FX / SAMPLER pads, ½ and X2: no function, so no state.

## Conflicts and open questions

- **½ / X2 or IN / OUT without SHIFT?** The panel prints ½ and X2 on the buttons and IN and OUT underneath, and Hercules' manual [1] p.12 and VirtualDJ's [2] say ½ / X2 plain and IN / OUT with SHIFT. DJUCED's page [3] describes them the other way round (its text looks carried over from the MK1's page), and the djay mapping [4] also puts loop in / out on the plain messages. All agree on the *messages* (`91 09` / `91 0A` plain, `94 09` / `94 0A` with SHIFT); they differ on what each should do. The preset follows the panel and the manufacturer's manual.
- **GAIN and HIGH.** [5] names CC 0x05 "EQ_HIGH" and CC 0x04 "EQ_MID". It drives rekordbox's three-band EQ from a unit that has GAIN, HIGH and LOW knobs in that order from the top, so it uses the top knob as HIGH and the middle one as MID. [4] (learned on the unit) has 0x04 = high EQ and 0x05 = gain, as do the MK1 list [8] and [9]. Followed [4] and [8]. If the owner finds GAIN and HIGH swapped, this is why.
- **Who lights the pad mode buttons?** The MK1 list [8] has no light message for them, and [10] says of the MK3 "handled by the controller"; but [5] sends note-ons to `91 0F` / `91 10` on the MK2 to light them. Tandem doesn't touch them. If they stay dark, the DJ can't see the pad mode (and only HOT CUE does anything in Tandem).
- **Tempo fader direction** rests on arithmetic followed through three mappings, not on a statement or a capture (see Layout).
- **SHIFT + TEMPO fader and SHIFT + BROWSER press** are bound from the MK1's list only. If the MK2 doesn't send them, those two functions do nothing; nothing else is affected.
- **Jog ticks per turn** (248) is the MK1's figure. Ring and top may differ on the MK2 [7].
- **MASTER and HEADPHONES knobs:** one MK2 source [4] says which is CC 0x03 and which 0x04 (the MK1 list and the MK3 mapping say the same). If they turn out swapped, swap the two bindings.
- **Letting go with SHIFT changed.** Because SHIFT moves a button to another channel, a button pressed without SHIFT and let go with SHIFT held may send its release on the SHIFT channel. Tandem would then not see CUE or the jog touch let go until it's pressed again. Nothing found says what the unit does; the Pioneer presets have the same exposure.
- **Firmware:** no source names the firmware it was made with. VirtualDJ's page [2] (2023) says no update existed then.
- **FX pad mode in "hardware mode".** VirtualDJ [2] describes a mode where the firmware itself runs effects from the FX pads. On the MK1 those pads then send extra messages (notes 0x25-0x2B, CC 0x07, 0x0C, 0x0D on the deck's channel, per [12] and [9]). None of them is bound here.

## What Tandem can't do with this unit yet

- **A LOOP button that exits any loop.** The unit's LOOP turns a loop of the current size on and off. Tandem has `BeatLoop` (a 4-beat loop; pressed again at 4 beats it exits, at another size it goes back to 4 beats) and `Reloop` (exit, or re-enter the last loop; does nothing before the first loop). LOOP is bound to `BeatLoop` 4, like the auto-loop on the other presets. So after ½ or X2 the first press of LOOP returns to 4 beats and the second exits, and the LOOP light is lit only at 4 beats. A "loop on/off" target would fix both.
- **STEMS, FX and SAMPLER pads, SHIFT + PLAY, ASSIST./PREP., BEATMATCH GUIDE, vinyl-mode button, SHIFT + jog search, the SHIFT layer of the mixer:** no Tandem function.
- **Only four hot cues per deck, and no beat-loop or roll pads.** Offered, not done: the STEMS pads (`96 10`-`96 13`) could be beat loops of 1, 2, 4 and 8 beats (what the MK1's second pad mode did) and the SAMPLER pads (`96 30`-`96 33`) loop rolls. That's repurposing labelled pads, so it's the user's call.
- **One jog resolution for ring and top** (see above); no per-binding scale.
- **Lights on the SHIFT layer, the browser ring and the guide arrows** (see "Lights Tandem doesn't drive yet").
- **Asking for knob positions on connect** (`B0 7F 7F`): Tandem doesn't send it, so each knob and fader takes effect when first moved, with the usual soft takeover.

## Check on the hardware

Close DJUCED, Serato and any other DJ software first. Steps marked **(preset)** would change the preset if they fail.

1. Settings → Controller: the port list shows "DJControl Inpulse 200 Mk2" (perhaps with "2- " in front), and picking it says the built-in mapping is used. Press PLAY: "Last message" changes. **(preset: the port name)**
2. Load a track on each side and press PLAY, then CUE, on both decks. The deck plays and stops, and the PLAY and CUE lights follow. **(preset)**
3. Settings → Controller → Calibrate: with a hand flat on top of the left platter, turn it exactly one full turn clockwise. The count should be near 248 and positive. Save it whatever it is. **(preset: the figure, and the direction if it's negative)**
4. Play a track and move the left TEMPO fader down, towards "+". The BPM should rise. If it falls, the direction is wrong. **(preset)**
5. With a track paused, turn the platter by its ring: the track should move slowly forwards clockwise. While playing, the ring should bend the speed. Say if it feels much weaker than on other software (ring and top may count differently).
6. Touch the top while playing: the deck stops under the hand and scratches; let go and it plays on.
7. HOT CUE mode: press pads 1-4 on a playing deck (sets hot cues, pads light), press them again (jumps), then SHIFT + each pad (clears it, light goes out). **(preset)**
8. Press LOOP while playing: a 4-beat loop starts and LOOP lights. Press ½, then X2, then LOOP to leave. Then hold SHIFT and press ½ (IN), wait a bar, SHIFT + X2 (OUT): a manual loop. **(preset: swap the two pairs if the unit sends them the other way round)**
9. SHIFT + SYNC turns sync off; SHIFT + CUE goes to the start of the track. **(preset)**
10. Turn GAIN, HIGH, LOW and FILTER on each side and watch the knobs on screen: each should move its own. **(preset: GAIN and HIGH are the pair that might be swapped)**
11. Move both volume faders and the crossfader end to end; crossfader left should be deck A.
12. Turn BROWSER (clockwise moves down), press it (moves to the lists), press LOAD on each side. Then SHIFT + press BROWSER: it should go to the lists too. Say if it does nothing.
13. Hold SHIFT and move a TEMPO fader: the tempo should stay put. Say if it changes instead (then the unit doesn't send a separate message and that binding should go).
14. Turn MASTER and HEADPHONES with Tandem playing through the controller: the level should change once, not twice as fast, and Settings → Controller should say the controller sets those levels itself. Tell us the sound card's name as Windows shows it in Tandem's audio settings.
15. Lights: which of these come on by themselves and which stay dark: the HOT CUE / STEMS mode buttons (press each, and SHIFT + each), MASTER, BEATMATCH GUIDE, the green tempo-centre light? Do PLAY, CUE, SYNC and the hot cue pads go dark while SHIFT is held (expected)? Does anything stay lit after Tandem closes?
16. If anything is dead: run `tandem.exe --midi-monitor 30` with Tandem closed, press the dead control with and without SHIFT, and send the output.

## Sources

Fetched 2026-11-20. Raw copies were kept in the session's scratch directory, not in the repo.

1. **Hercules, DJControl Inpulse 200 MK2 User Manual** (21 languages; PDF created 2023-07-03, modified 2023-09-05). <https://ts.hercules.com/download/sound/manuals/DJC_Inpulse_200_MK2/DJControl_Inpulse_200_MK2_User_Manual_-_21L.pdf>. The hardware manual: every control, SHIFT functions, pad modes, sound card, class-compliant USB, Vegas and demo modes. No MIDI numbers. Page numbers above are the manual's own (the PDF's page is one more).
2. **VirtualDJ hardware manual, Inpulse 200 MK2** (Atomix, 2023): <https://virtualdj.com/manuals/hardware/hercules/inpulse200mk2/setup.html>, `controls.html`, `pads.html`. Control inventory with SHIFT actions; says MASTER is a hardware operation and describes the firmware's FX "hardware mode". No MIDI numbers.
3. **DJUCED knowledge base, "Hercules DJControl Inpulse 200 MK2 (2023) – Mapping and Manual"** (updated 2023-06-06): <https://www.djuced.com/kb/hercules-djcontrol-inpulse-200-mk2-2023-mapping-and-manual/>. Panel photos with the printed labels (½ / IN, X2 / OUT, LOOP / VINYL, tempo "−" top and "+" bottom), "hardware driven" volume knobs, SHIFT + volume fader for decks 3/4. Its loop-button text contradicts the labels (see Conflicts). No MIDI numbers.
4. **djay MIDI mapping "DJControl Inpulse 200 Mk2"** by th3StonedApe, made with djay 5.1's MIDI learn on the unit, posted 2024-02-09: <https://community.algoriddim.com/t/hercules-djcontrol-inpulse-200-mk2-midi-map-download/24481> (the same file is at <https://maps.djtechtools.com/mappings/13973>). An XML property list: `midiChannel` is 0-based, `midiMessageType` 1 = note and 3 = CC, `midiData` decimal. Gives the USB id (0x06F8B130), the port name, and 119 controls including the SHIFT layer and all four pad modes. Its author no longer has the unit. What djay *does* with each message is the author's choice, not evidence of the label.
5. **CyrilGendarme/DJControl-Inpulse-200-Mk2-to-Rekordbox**, commit bc57e55 (2026-07-03): <https://github.com/CyrilGendarme/DJControl-Inpulse-200-Mk2-to-Rekordbox>. A Python bridge that reads the MK2 on Windows and lights it. `djcontrol_specific/controller_notes.py` (hex numbers; mido's 0-based channels) and `state_machine.py` (which lights it sets, 127 / 0, on the channel and on channel + 3). A fork of a bridge written for the MK1; its commit message calls it work in progress.
6. **g-dameg78/hercules-inpulse-200-mk2-linux**, commit 02cb3c7 (2026-11-08): <https://github.com/g-dameg78/hercules-inpulse-200-mk2-linux>. 191 raw MIDI messages (`tests/fixtures/MK2_CAPTURE_EXCERPTS.jsonl`, decimal bytes) picked from four recordings of an MK2 made on 2026-10-24, the main one 20,110 messages long. It is the hardware evidence for the 14-bit pairs, the jog encoding and the message bytes. Its *names* for the controls come from [5] and [9], not from the capture (its prompts ran a step out of sync), so it counts as evidence that the bytes exist, and for a name only together with [4], [5] or [7]. Its documents say more was recorded (the browser knob, the two global knobs, STEMS pad notes) than the published excerpt contains: the excerpt has PLAY, CUE, the jog touch, the hot cue pads, the jog CCs and every 14-bit pair named in the tables.
7. **agraef/raptor7**, commit e1a2a8e (2024-05-07): <https://github.com/agraef/raptor7>, `lib/raptor.pd_lua` (`djcontrol_*` functions) and `data/djcontrol.map`. A Pure Data instrument with a Hercules driver "tested with the Inpulse 200 MK2 and the Inpulse 500", with comments about the MK2. Decimal numbers; its channel 17 + n is channel 1 + n of its second port; map keys are note + 128. Gives the SHIFT channel scheme, the lights it drives and the remark about the ring's tick rate.
8. **Hercules, "MIDI Commands of DJControl Inpulse 200", V1.0** (PDF created 2021-10-11), for the **MK1**: <https://ts.hercules.com/download/sound/manuals/DJC_Inpulse200/DJControlInpulse200_MIDI_Commands_V_1_0.pdf>. The manufacturer's list for the sibling: hex status bytes, every input with its SHIFT variant, the light messages, the guide arrows, the ring's colour table, `B0 7F 7F`. It has typos (deck B's SHIFT rows are printed with deck A's status byte). Used as the prediction for every row and as the only source for rows marked inferred.
9. **Mixxx mapping for the MK1**, `Hercules_DJControl_Inpulse_200.midi.xml` (commit fb0e21b) and `Hercules-DJControl-Inpulse-200-script.js` v1.2 (commit 43883c6), by DJ Phatso for Hercules Technical Support: <https://github.com/mixxxdj/mixxx/tree/main/res/controllers>. Hex. Jog arithmetic, 248 ticks, `rate` without invert, the lights it declares.
10. **dusan-ivanco/Mixxx_Hercules_DJControl_Inpulse_200_MK3**, commit 117bf00 (2026-10-24): <https://github.com/dusan-ivanco/Mixxx_Hercules_DJControl_Inpulse_200_MK3>. A Mixxx mapping for the **MK3**, with a message overview in the XML's header. Sibling: shows the family layout carried on, and where it changed (guide arrows).
11. **Mixxx source**, main at 825e451 (2026-11-20): `src/preferences/dialog/dlgprefdeck.cpp` and `src/engine/controls/ratecontrol.cpp`. What `rate` means, for the tempo direction.
12. **Hercules, Traktor Pro 2 & 3 mapping for the MK1** (2019-03-04): <https://ts.hercules.com/download/sound/MIDI_Mapping/DJC_Inpulse_200/DJCInpulse200_TraktorPro2_&_3.zip>. Port names in and out are both "DJControl Inpulse 200"; the FX pads' extra messages.
13. **Mixxx manual, Hercules DJControl Inpulse 200** (MK1): <https://manual.mixxx.org/latest/en/hardware/controllers/hercules_djcontrol_inpulse_200>. The volume knobs and MASTER button act in hardware and also send MIDI.
14. **CyrilGendarme/travel-dj-setup-launch-script**, commit 0bd666d (2026-06-07), `travel_setup_launch_all.bat`: the port name as Windows shows it to python-rtmidi.

### Looked for and not found

- **A manufacturer's MIDI list for the MK2.** The MK2's support page (<https://support.hercules.com/en/product/djcontrolinpulse200mk2-en/>, read through the Internet Archive's copies of 2023-09-30, 2025-04-05 and 2026-05-18 because the live site refuses scripts) has drivers, manuals and the DJUCED and Serato guides, and no "MIDI mapping" section; the MK1's page has one. Six guesses at a file name beside the MK1's PDF returned 404. The repo SuperXtremeMapper/super-xtreme-mapper searched the same pages in 2026-09 and also found none.
- **A Mixxx mapping for the MK2:** none on Mixxx's main branch, none in its pull requests or issues (`mixxx_map.py find inpulse 200 mk2`), no forum thread found by web search.
- **A Traktor `.tsi` for the MK2:** none on Hercules' site, none found on DJ TechTools or by web search (the MK1 has Hercules' own, [12]).
- **DJUCED's own mapping file for the MK2:** DJUCED's installer wasn't downloaded and unpacked. Hercules publishes one DJUCED mapping file for the MK1 (`.djm`, decks 3/4), not read.
- **VirtualDJ and Serato definitions:** both support the unit natively and publish no definition. Serato's quick-start guide wasn't read (the Hercules manual covers Serato's functions).
- **rekordbox CSV:** not an AlphaTheta controller. [5] ships a four-row CSV of its own.
- **A USB descriptor dump** (linux-hardware.org, `lsusb -v`): not searched for; the port name came from three other places.
- **willyd248/onset** (a Web MIDI app "hardcoded for the Inpulse 200 MK2" with light output, seen in GitHub code search): the repository returned 404.
- **chrisle/autopilot-mappings** has an MK2 file, but its control descriptions are Mixxx's MK1 ones and its README says nothing in it was tested: not counted.
- **OdineOsborne/karaokedj** has an MK2 preset converted from [4]: the same source again.
- **Anything about the MK2's lights at rest, after the software leaves, or its firmware versions.**
