# Denon SC Live 4: deck layers and jog wheels

**Status:** DECK confirmed on a unit on 2026-11-24 by a DJ's DECK button check (v0.15.1, report [6]): it leaves deck switching to the software. Researched 2026-11-23, when an owner reported that DECK "doesn't work" in Tandem and that "the controller handles the deck switching". Built-in preset "Denon SC Live 4" since v0.11.11; decks C and D since preset 14.

This covers only DECK, the jog wheel screens' deck number and mixer channels 3 and 4 (what decks C and D need), and [the jog wheels](#the-jog-wheels), whose turn the sources disagree about and nobody has recorded yet. The rest of the map is in the preset's comments in `crates/tandem-core/src/mapping.rs` (`denon_sc_live_4`). Status bytes are written as sent (`94 1F` = note 0x1F on MIDI channel 5). Tandem's `Trigger` channel is the status byte's low digit.

## Why DECK did nothing (up to preset 13)

The built-in mapping bound nothing to DECK (`94 1F` / `95 1F`) and had no deck C or D controls, and the unit leaves deck switching to the software, so pressing DECK changed nothing and the side stayed on deck A or B.

The owner's "the controller handles it" matches the unit **without a computer**: in Engine OS standalone, DECK switches the side's deck and the jog wheel screen shows it ([4] p.25).

## What the unit sent

From the report [6] (Windows, computer mode, MIDI out: yes). Buttons let go with a note on of velocity 0 (`94 0A 00`), not a note off.

| | Sent | Note |
|---|---|---|
| DECK, left and right | `94 1F 7F` / `94 1F 00`, `95 1F 7F` / `95 1F 00` | the same at both presses |
| Right PLAY | `95 0A 7F` before DECK, after one press and after two | the side doesn't move |
| Left PLAY | `94 0A 7F` before DECK and after two presses | after one press CUE was pressed instead, and sent `94 09`: the note CUE has on deck 1, so this side doesn't move either |
| LOAD | `9F 01` left, `9F 02` right, the same before and after DECK | |
| Mixer faders 3 and 4 | `B2 0E` and `B3 0E`, over 0 to 127 (which end is up wasn't recorded) | |
| Left jog wheel touched | `94 21 7F` / `94 21 00` | by accident |
| Left jog screen at DECK | didn't change, says the DJ | |
| Left jog screen asked for 3 | showed it, says the DJ | box CCs and note 0x38 only: no start-up SysEx needed |

Not seen: SHIFT + DECK, the right jog screen, whether DECK has a light, and everything else in the preset.

The check itself called the left side "switches decks itself", wrongly: it took the jog wheel for DECK and CUE for PLAY. It no longer does, and replays this capture in its tests.

## What DECK sends

| | Message | Grade | Sources |
|---|---|---|---|
| Left DECK | `94 1F 7F` press, `94 1F 00` release | **seen on a unit** | [6]; [1] [2] |
| Right DECK | `95 1F 7F` / `95 1F 00` | **seen on a unit** | [6]; [1] [2] |
| After DECK | the side sends exactly what it sent before: left always channel 5, right channel 6 | **seen on a unit** | [6]; [1] [2]; the sibling Prime 4 [3] |
| SHIFT + DECK | SHIFT (`94 1C`) and then the same `94 1F`: SHIFT is tracked in software | inferred from the mappings | [1] [2] |
| SHIFT + LOAD | SHIFT and then LOAD's own `9F 01` / `9F 02` | read in a mapping made on a unit (it ejects, as here: `shift` swaps `LoadSelectedTrack` for `eject` on the same message), not seen | [1]; the sibling Prime 4 [3] |
| DECK light | none, going by a user report passed on 2026-11-23. Engine OS's guide says that without a computer DECK flashes as the track end warning, when a track on the deck not in focus is nearly over (SC Live 4 and Prime 4) [4]. Neither mapping sends it anything | conflict | [4] |

- **Mixxx** [1] (written and "tested on the hardware with Engine OS 5.0.4"): "Both layers share the same MIDI channel, so switching is handled entirely in the mapping." Only tried on macOS, says its manual page. It builds decks 1 and 3 on channel 5 and decks 2 and 4 on channel 6, and `makeDeckToggle` binds `[0x90 + 4, 0x1F]` and `[0x90 + 5, 0x1F]`. SHIFT + DECK switches Mixxx between its two- and four-deck views. No deck control is bound on channel 7 or 8.
- **Traktor** [2] (by Vi_Ti, not Penaka): left DECK flips modifier M3 ("Select Deck C" / "Select Deck A") and right DECK M6; decks C and D stay on channels 5 and 6 and differ only by those modifiers.
- **Prime 4** [3]: its mapping's author wrote on 2022-07-14 that "the Prime 4 doesn't change its outgoing MIDI values even if you press the deck toggle or shift buttons".

So Tandem needs a software deck layer: DECK toggles which deck a side's controls drive, A↔C on the left and B↔D on the right. The unit gives no state, so the toggle has to count presses, and it starts on A and B.

## The jog wheel screen's deck number

The software sets it: on the unit, DECK didn't change the left screen, and the messages below did [6]. Grade: left screen seen on a unit, the right one read in mappings [1] [2].

- **Digit:** note `0x38` on the side's channel, the value is the digit: `94 38 03` shows 3 on the left.
- **The "DECK" box around it:** CCs on the same side, found by probing [1]: `0x35` the top of the box, `0x36` the bottom, `0x37` the word "DECK", each sent 127.
- **Mixxx sends** the box and the digit after every switch, the digit again 150 ms later "in case the unit dropped it during the burst", and both again every second as a safety net [1]. Traktor sends note 0x38 with 1/3 on the left and 2/4 on the right [2].
- Mixxx also sends Engine OS's start-up SysEx (`F0 7E 00 06 01 F7`, `F0 00 02 0B 7F 12 42 00 00 F7`, `F0 00 02 0B 7F 12 60 00 04 04 01 01 03 F7`) and, a second later, `F0 00 02 0B 7F 12 04 00 00 F7`, which asks for every control's position [1]. Tandem sends none of them. If the screen ignores the digit, try these first.
- The 7" main screen is inactive in computer mode [1].

The DECK button check asks the left screen to show 3 with Mixxx's box and digit messages (the digit again 150 ms later), without its start-up SysEx or its once-a-second repeat, and asks the DJ whether it did. A No may only mean the start-up SysEx is needed.

## Mixer channels 3 and 4

Same CCs as channels 1 and 2, on MIDI channels 3 and 4 (`B2` / `B3`): gain `0x03`, high `0x04`, mid `0x06`, low `0x08`, sweep FX `0x0B`, fader `0x0E`, cue note `0x0D` (`92` / `93`). Grade: read in mappings [1] [2]. Each strip also has a crossfader assign switch on note `0x0F` ([1] only), which Tandem has no target for. The DECK button check records what channels 3 and 4's faders send.

## The jog wheels

Researched 2026-11-24, when an owner's wheels went wrong after they learned them again ("tried to rebind [the] jogwheel but now it only goes one way, and nearly double speed" the other way).

| | Message | Grade | Sources |
|---|---|---|---|
| Touch | `94 21 7F` / `94 21 00` (left), `95 21` (right): let go with velocity 0 | left **seen on a unit** (by accident), right read in mappings | [6]; [1] [2] |
| Turn, as Traktor has it | CC `0x36` on the side's channel, relative, 1 / 127 | **conflict**: read in a mapping made on a unit | [2] |
| Turn, as Mixxx has it | the wheel's position in 14 bits: coarse `B4 11`, then fine `B4 31` (right `B5`), coming round at 16384 | **conflict**: read in a mapping "tested on the hardware with Engine OS 5.0.4" | [1] |
| Platter and edge | one message for both: both mappings tell them apart by the touch note alone | read in mappings | [1] [2] |
| Ticks in a turn | 1000 in Mixxx's mapping (a figure it shares with its Prime 4 and Prime GO mappings); nothing in Traktor's | inferred | [1] [3] |
| VINYL | note `0x23`, a button whose state the software keeps (Mixxx also lights it) | read in mappings; no Tandem control | [1] [2] |

- **Mixxx** [1]: `inputWheelMSB` on `B4 11` keeps the coarse half and `inputWheelLSB` on `B4 31` puts the two together; its comment: "the controller reports the absolute position of the wheel with 14-bit precision". Touch is note 0x21, the turn scratches while it's held and VINYL is on and nudges otherwise. Its manual page: "Turn the outer edge of the jog wheel to nudge". The comment and the arithmetic are the Prime 4 mapping's [3], but the numbers aren't (0x37 / 0x4D there), which suggests they were found on an SC Live 4.
- **Traktor** [2]: "Jog Turn Deck A" is CC 0x36 on channel 5 as an encoder in 7Fh/01h mode, and "Jog Touch Deck A" note 0x21. Traktor's jog turn takes an encoder that counts, as far as we know, so a wheel that only says where it is couldn't have been mapped there.
- **Siblings:** the Prime 4, Prime GO and Numark Mixstream Pro send 14-bit positions on 0x37 / 0x4D, touch note 0x21 [3] [7] [8]. Denon's own MIDI list for the LC6000 gives its jog wheel as *relative* data on two control changes, upper 0x37 and lower 0x36 (reverse 127–64, forward 1–63) [9]. So in this family CC 0x36 is a wheel that counts, and the SC Live 4 may well send both.
- **The owner's report** fits a wheel on CC 0x36 that counts 1 / 127, learned by Tandem 0.15.1 as counting around 64 (Learn always did): turned one way the track runs at half speed, the other at one and a half, and a hand on top throws the record 28 s the wrong way for a quarter turn. It also fits a position's coarse half learned that way: then both ways go the same way, set by where the wheel is. Neither is proven, and why they learned it again isn't known.

## What Tandem does

- **Jog wheels:** touch on note 0x21, and the turn bound both ways to one `Jog` (one message for both): CC 0x36 counting 1 / 127, and the position on 0x11 / 0x31. Whichever comes moves the deck once. A saved mapping that learned CC 0x36 or 0x11 gets the right encoding back, keeping its invert and calibration. 1000 ticks a turn until it's calibrated. **Check the jog wheels…** records all of it and says which it is.
- **DECK** (`94 1F` / `95 1F`): `Target::DeckSelect` for both of the side's decks, so each press goes to the other one. The side's controls, LOAD and SHIFT + LOAD (`Target::Eject`, a SHIFT + binding on LOAD's note) included, are `Binding::layered`. Each side starts on A or B when Tandem connects: the unit gives no state to ask for.
- **Mixer channels 3 and 4** are decks C and D's.
- **Jog wheel screens:** `Mapping::deck_displays`, sent as Mixxx sends them, without its start-up SysEx.
- **DECK's light:** sent like the other buttons' (127 on C or D, 1 on A or B), in case there is one.

## Sources

1. Mixxx PR #17201, "Denon SC Live 4: add controller mapping" (open): `Denon-SC-Live-4-scripts.js` at 5d977dd6, lines 224–261 and 400–470, and its manual page. https://github.com/mixxxdj/mixxx/pull/17201
2. Vi_Ti's Traktor mapping `denon-sc-live-4-vt-0-2.tsi`, attached to https://community.native-instruments.com/discussion/45563/denon-dj-sc-live-4-traktor-mapping
3. Mixxx PR #4027 (Denon Prime 4), review comment of 2022-07-14: https://github.com/mixxxdj/mixxx/pull/4027#discussion_r920952146
4. Engine OS user guide v5.3.0: p.25 (DECK and the jog display's active deck) and the Track End Warning setting (the flashing DECK button).
5. VirtualDJ's and Serato's SC Live 4 pages describe DECK as switching layers 1/3 and 2/4, but both drive the unit through Denon's own driver, so they say nothing about plain MIDI.
6. A DJ's `deck-check` report, Tandem v0.15.1, 2026-11-24 16:22 UTC (kept as test data in `deck_check.rs`: `CHECKED`).
7. Mixxx PR #12047 (Denon Prime GO), `Denon-Prime-GO.midi.xml` at fcf0cd50. https://github.com/mixxxdj/mixxx/pull/12047
8. Mixxx's Numark Mixstream Pro mapping (merged, PR #15732): `B2 37` / `B2 4D`, touch note 0x21.
9. Denon's LC6000 PRIME MIDI Specification v1.0, "Double Precision Control": Jog Wheel, upper byte CC 55, lower byte CC 54, relative data. https://cdn.inmusicbrands.com/denondj/lc6000/LC6000-PRIME-MIDI-Specification-v1.0.pdf
