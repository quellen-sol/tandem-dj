# Denon SC Live 4: deck layers

**Status:** desk research, not yet seen on a unit. Researched 2026-11-23, when an owner reported that DECK "doesn't work" in Tandem and that "the controller handles the deck switching". Built-in preset "Denon SC Live 4" since v0.11.11, with decks A and B only. Tandem's **DECK button check** (Settings → Controller) records what a unit really sends; this file is what to read its report against.

This covers only DECK, the jog wheel screens' deck number and mixer channels 3 and 4: what decks C and D need. The rest of the map is in the preset's comments in `crates/tandem-core/src/mapping.rs` (`denon_sc_live_4`). Status bytes are written as sent (`94 1F` = note 0x1F on MIDI channel 5). Tandem's `Trigger` channel is the status byte's low digit.

## Why DECK does nothing in Tandem today

The built-in mapping binds nothing to DECK (`94 1F` / `95 1F`), and has no deck C or D controls. So if the unit leaves deck switching to the software, as every mapping found says, pressing DECK changes nothing and the side stays on deck A or B. If it switched channels itself, the side would go quiet while it's on deck 3 or 4. Either way, it can't work with the built-in mapping (as of v0.15).

The owner's "the controller handles it" matches the unit **without a computer**: in Engine OS standalone, DECK switches the side's deck and the jog wheel screen shows it ([4] p.25). Nothing found says it does that in computer mode.

## What DECK sends

| | Message | Grade | Sources |
|---|---|---|---|
| Left DECK | `94 1F 7F` press, `84 1F` release | read in mappings ([1] was tested on a unit) | [1] [2] |
| Right DECK | `95 1F 7F` / `85 1F` | same | [1] [2] |
| After DECK | the side sends exactly what it sent before: left always channel 5, right channel 6 | same | [1] [2]; the sibling Prime 4 seen on hardware [3] |
| SHIFT + DECK | SHIFT (`94 1C`) and then the same `94 1F`: SHIFT is tracked in software | inferred from the mappings | [1] [2] |
| DECK light | none, going by a user report passed on 2026-11-23. Engine OS's guide says that without a computer DECK flashes as the track end warning, when a track on the deck not in focus is nearly over (SC Live 4 and Prime 4) [4]. Neither mapping sends it anything | conflict | [4] |

- **Mixxx** [1] (written and "tested on the hardware with Engine OS 5.0.4"): "Both layers share the same MIDI channel, so switching is handled entirely in the mapping." Only tried on macOS, says its manual page. It builds decks 1 and 3 on channel 5 and decks 2 and 4 on channel 6, and `makeDeckToggle` binds `[0x90 + 4, 0x1F]` and `[0x90 + 5, 0x1F]`. SHIFT + DECK switches Mixxx between its two- and four-deck views. No deck control is bound on channel 7 or 8.
- **Traktor** [2] (by Vi_Ti, not Penaka): left DECK flips modifier M3 ("Select Deck C" / "Select Deck A") and right DECK M6; decks C and D stay on channels 5 and 6 and differ only by those modifiers.
- **Prime 4** [3]: its mapping's author wrote on 2022-07-14 that "the Prime 4 doesn't change its outgoing MIDI values even if you press the deck toggle or shift buttons".

So Tandem needs a software deck layer: DECK toggles which deck a side's controls drive, A↔C on the left and B↔D on the right. The unit gives no state, so the toggle has to count presses, and it starts on A and B.

## The jog wheel screen's deck number

The software sets it; nothing says the unit changes it by itself in computer mode. Grade: read in mappings [1] [2].

- **Digit:** note `0x38` on the side's channel, the value is the digit: `94 38 03` shows 3 on the left.
- **The "DECK" box around it:** CCs on the same side, found by probing [1]: `0x35` the top of the box, `0x36` the bottom, `0x37` the word "DECK", each sent 127.
- **Mixxx sends** the box and the digit after every switch, the digit again 150 ms later "in case the unit dropped it during the burst", and both again every second as a safety net [1]. Traktor sends note 0x38 with 1/3 on the left and 2/4 on the right [2].
- Mixxx also sends Engine OS's start-up SysEx (`F0 7E 00 06 01 F7`, `F0 00 02 0B 7F 12 42 00 00 F7`, `F0 00 02 0B 7F 12 60 00 04 04 01 01 03 F7`) and, a second later, `F0 00 02 0B 7F 12 04 00 00 F7`, which asks for every control's position [1]. Tandem sends none of them. If the screen ignores the digit, try these first.
- The 7" main screen is inactive in computer mode [1].

The DECK button check asks the left screen to show 3 with Mixxx's box and digit messages (the digit again 150 ms later), without its start-up SysEx or its once-a-second repeat, and asks the DJ whether it did. A No may only mean the start-up SysEx is needed.

## Mixer channels 3 and 4

Same CCs as channels 1 and 2, on MIDI channels 3 and 4 (`B2` / `B3`): gain `0x03`, high `0x04`, mid `0x06`, low `0x08`, sweep FX `0x0B`, fader `0x0E`, cue note `0x0D` (`92` / `93`). Grade: read in mappings [1] [2]. Each strip also has a crossfader assign switch on note `0x0F` ([1] only), which Tandem has no target for. The DECK button check records what channels 3 and 4's faders send.

## What to build once a unit confirms it

- **The check says "doesn't switch decks itself"** (expected): bind `94 1F` / `95 1F` to a software deck layer for each side, add mixer channels 3 and 4 for decks C and D, and send the jog screen the deck number (box and digit) after each switch, again 150 ms later, and when Tandem connects. v0.15.0's engine has the layer ("decks that take turns", `Binding::layered`), and its **Set up step by step…** can map DECK on the unit without any of this: the test `the_sc_live_4s_built_in_mapping_gains_decks_c_and_d` does it from the built-in mapping.
- **The check says "switches decks itself":** its **Map decks C and D** button copies the side's controls onto the channel it heard. Then put that in the preset.

## Sources

1. Mixxx PR #17201, "Denon SC Live 4: add controller mapping" (open): `Denon-SC-Live-4-scripts.js` at 5d977dd6, lines 224–261 and 400–470, and its manual page. https://github.com/mixxxdj/mixxx/pull/17201
2. Vi_Ti's Traktor mapping `denon-sc-live-4-vt-0-2.tsi`, attached to https://community.native-instruments.com/discussion/45563/denon-dj-sc-live-4-traktor-mapping
3. Mixxx PR #4027 (Denon Prime 4), review comment of 2022-07-14: https://github.com/mixxxdj/mixxx/pull/4027#discussion_r920952146
4. Engine OS user guide v5.3.0: p.25 (DECK and the jog display's active deck) and the Track End Warning setting (the flashing DECK button).
5. VirtualDJ's and Serato's SC Live 4 pages describe DECK as switching layers 1/3 and 2/4, but both drive the unit through Denon's own driver, so they say nothing about plain MIDI.
