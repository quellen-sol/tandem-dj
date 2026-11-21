# Where a controller's MIDI and light facts live

Contents: [Kinds of source](#kinds-of-source) ·
[Reading each kind](#reading-each-kind) · [Port names](#port-names) ·
[Numbering traps](#numbering-traps) · [By manufacturer](#by-manufacturer) ·
[The source log](#the-source-log)

## Kinds of source

Try all of them, best first. Log each one you tried, found or not.

| # | Source | Gives | Trust |
|---|---|---|---|
| 1 | **Manufacturer's MIDI list** ("List of MIDI messages", "MIDI commands", "MIDI implementation chart") | Every control and every light, SHIFT variants, value ranges | Highest. Still has typos, and says nothing of how a jog feels or where an LED threshold sits. |
| 2 | **Mixxx mapping** (`.midi.xml` + script), merged or in a pull request | Inputs, lights, init SysEx, jog resolution, colour tables, and the author's comments about quirks | High: written against real hardware and reviewed. |
| 3 | **Traktor `.tsi`** from the NI forum, DJ TechTools maps, the maker's site | Inputs, outputs with value ranges, encoder modes, the exact OS port names | Good; one author, quality varies. |
| 4 | **Manufacturer's own software mapping** (files shipped inside the maker's DJ app, rekordbox's `.midi.csv` for AlphaTheta gear) | The map the hardware was designed against | High when readable. |
| 5 | **VirtualDJ**: hardware manual pages, user-made definition XMLs | The manual is the best *control inventory* (every button, its SHIFT function, what each LED shows). Definitions give messages. | Manual: high for behaviour, no MIDI numbers. Definitions: one author. |
| 6 | **Hardware manual** | The physical controls, pad modes, SHIFT legends, modes, firmware, sound card channels | Authoritative for what exists. |
| 7 | **Community**: mappings for other programs (djay's `.djayMidiMapping`, learned on the unit), bridge and driver scripts on GitHub, forum threads with MIDI logs, blog reverse-engineering | Fills holes; often the only source for a new model, and for lights and SysEx | Varies. A pasted MIDI monitor log is hardware evidence; a recollection isn't. |
| 8 | **Sibling models** | A prediction of the protocol | A hypothesis only. Mark anything resting on it "inferred". |
| 9 | **The hardware itself** (`tandem.exe --midi-monitor`) | The truth, for inputs | Beats everything above. |

Search widely for 2, 3 and 7: the exact model name with "midi", "mapping",
"tsi", "mixxx", "djay", "sysex", "LED", "midi commands".

On GitHub, `gh search code "<exact product string>"` is the search that pays:
people hard-code the port name in bridge scripts, launch scripts and
drivers, so it finds programs written against the unit and the port's real
spelling in one go. Try the spellings the OS might use ("Inpulse 200 Mk2",
"Inpulse200MK2"). `gh search issues` and `mixxx_map.py find` match loose
words, so a model with a number in its name returns issue #200 and the like:
read titles before opening anything.

Before reading a community mapping in full, look for "based on", "converted
from" or "ported from" in its README: a conversion of Mixxx's mapping or of
another file you already have is the same source again.

The Internet Archive (`https://web.archive.org/web/2024*/<url>`) has vanished
PDFs, and also reads support sites that refuse scripts (a 403 or a
Cloudflare page from `curl`): go there straight away instead of retrying.
Compare a sibling's support page with this model's. If the older model has a
"MIDI mapping" or "MIDI commands" section and this one doesn't, that's the
finding, and the sibling's list is your best prediction.

## Reading each kind

### Manufacturer PDFs

`gs -q -dNOPAUSE -dBATCH -sDEVICE=txtwrite -sOutputFile=out.txt in.pdf`
turns one into text you can grep (there's no `pdftotext` here). Tables lose
their columns in the conversion, so when a row matters, look at the page
itself to see which column a value sat in. The Read tool can't open PDF
pages here either (it needs the same missing package), so render the page
and read the picture:

```bash
gs -q -dNOPAUSE -dBATCH -sDEVICE=png16m -r110 -dFirstPage=7 -dLastPage=7 \
   -sOutputFile=/absolute/path/page7.png in.pdf
```

Note the document's revision and the firmware it describes.

These lists usually give a status byte pattern like `9n` with a table saying
what `n` is per deck, and a separate "MIDI IN" section for the lights. Read
the notes at the bottom of each table: that's where "sent only in vinyl
mode" and "value depends on speed" live.

### Mixxx mappings

```bash
S=.claude/skills/controller-profile/scripts      # from the repo root
python3 -I $S/mixxx_map.py find <model words>     # merged mappings + PRs/issues
python3 -I $S/mixxx_map.py pr <number>            # files a PR adds, with raw URLs
python3 -I $S/mixxx_map.py show <xml> [--js <script>] [--save <dir>]
```

A mapping whose file ends `.hid.xml` or `.bulk.xml` isn't MIDI. If that's
the only mapping Mixxx has for a model, the unit is probably HID-only and
Tandem can't use it: confirm before going further.

`show` decodes every row and prints what the tables imply. Then read the
script itself; the XML is only half the mapping:

- **`Script-Binding` rows** go to a function that may do something else per
  mode. `show` lists where each starts.
- **`engine.scratchEnable(deck, N, rpm, alpha, beta)`**: `N` is the jog
  ticks per turn the author measured. The wheel handler's arithmetic shows
  the encoding: `value - 64` is offset 64; `value > 64 ? value - 128 : value`
  (or `value - 128`) is two's complement.
- **`midi.sendSysexMsg`** in `init` is the handshake (leave demo mode, hand
  over the LEDs, "send me every knob position"). One on a timer is a
  keep-alive.
- **`midi.sendShortMsg(status, number, value)`** is every light the XML
  doesn't declare. Colour constants and tables are usually at the top.
- **Comments** hold the hard-won facts: "the controller lights this itself",
  "sends 0x7F then 0x00 on one press", "firmware 1.2 changed...".

The XML's `<options>` say how Mixxx reads a value: `Diff`, `SelectKnob`,
`Hercjog` are two's complement (1 = +1, 127 = −1); `Rot64`, `Spread64` are
centred on 64; `Invert` flips an absolute control; `fourteen-bit-msb` /
`-lsb` mark the halves of a 14-bit value. A mapping with no fine-half rows
doesn't show the hardware is 7-bit: authors often bind only the coarse half.

The tempo fader shows why a flag can't be copied. Mixxx's `rate` runs −1 to
+1, and by default +1 is the *slow* end (its "down increases speed" setting,
`rate_dir` −1). So a mapping that binds the fader straight to `rate` says the
hardware sends its high value at the "−" end, and one that computes
`1 - value` (Mixxx's Pioneer mappings) says the high value is at "+". Follow
the arithmetic to the hardware fact, then apply Tandem's own rule.

Mixxx's manual has a page per supported controller
(`https://manual.mixxx.org/latest/en/hardware/controllers/`) with setup
notes: drivers, modes, port quirks, sound card channels. Its forum
(`https://mixxx.discourse.group`) has the threads where mappings were
developed, with MIDI logs. A mapping PR that was never merged is still a
complete mapping by someone with the hardware: read the review comments too,
they often contain corrections.

### Traktor `.tsi`

```bash
curl -sSLO https://raw.githubusercontent.com/cmdr-editor/cmdr/master/cmdr/cmdr.TsiLib/Commands/Interpretation/KnownCommands.cs
python3 -I $S/tsi_dump.py mapping.tsi --commands KnownCommands.cs
```

(`KnownCommands.cs` names Traktor's command ids. Download it to the
scratchpad when needed; it's GPL, so don't add it to this repo.)

The dump gives, per device in the file: the **in and out port names** as the
author's OS showed them, every input with its Traktor command, deck and
conditions, and every output with the value range it sends. What to take
from it:

- Conditions `M1`–`M8` are Traktor modifiers: how a mapping does SHIFT, deck
  layers and pad modes *in software*. If SHIFT + a button appears as the same
  message with `if M1 = 1`, the hardware sends the same message with SHIFT
  held. If it appears as a different message, the hardware changes it.
- `encoder mode 7Fh/01h` is two's complement, `3Fh/41h` is offset 64.
- Output rows are the lights: the note or CC, and `sends a..b` is the value
  range (for a pad, the colour number the author chose).
- `invert` is relative to Traktor's own idea of the control's direction. It
  doesn't tell you which end of a fader sends 127.
- A 14-bit pair is named with both CCs; which is coarse isn't reliable from
  the name. The coarse one is the one that changes slowly.

### rekordbox `.midi.csv`

rekordbox ships a CSV per supported AlphaTheta/Pioneer controller in its
install folder (`MidiMappings`), and copies are in GitHub repos. Rows are
function, control type, then MIDI in and out as four hex digits (status +
number, e.g. `900B`) per deck, with comments for SHIFT variants. Only for
controllers rekordbox supports.

### VirtualDJ

The hardware manual pages (`https://www.virtualdj.com/manuals/hardware/`)
walk through every control and LED, per layer and pad mode, and say which
controls are hardware-only. Use them to build the inventory and to learn
what each light is meant to show. User-made definitions are XML with
elements such as `<button note=…>`, `<slider cc=…>`, `<encoder>`, `<jog>`,
`<led>`, `<init midi=…>` and `<ping midi=…>` (a keep-alive). VirtualDJ's wiki
page on MIDI controller definitions explains the attributes. Definitions for
natively supported controllers are built into the app and aren't published.

### djay `.djayMidiMapping`

Algoriddim's djay saves a learned mapping as an XML property list, and users
share them on its forum (`community.algoriddim.com`) and on DJ TechTools'
maps site. Because it's made by pressing each control on the real unit, it's
often the first source to exist for a new model.

```bash
python3 -I $S/djay_dump.py mapping.djayMidiMapping
```

The dump gives the port name (`endpointName`), the USB id, and one row per
control with its message. `modifier` marks what the author assigned to the
SHIFT layer: when those rows sit on other channels or notes than the plain
ones, the hardware changes its messages with SHIFT. `output` marks controls
whose light djay drives on the same message. `flipped` is djay's own
direction flag, with the same caveat as any other program's. The function
names are what the author chose to do with a control, not its label: check
labels against the manual.

### Web pages

`python3 -I $S/html_text.py page.html` prints a saved page (a manual, a
forum thread, an archived support page) as plain text without its scripts,
styles and navigation, so it can be grepped instead of read whole.

### Community repos and forum posts

Prefer raw captures (a MIDI monitor log, a Wireshark USB trace, a table
someone measured) over prose. Check the date and the model revision. When a
repo's mapping is derived from Mixxx's or from the manufacturer's list, it's
the same source again, not a second one.

## Port names

Tandem picks a preset by the MIDI **input port's name** on Windows
(`builtin_preset` in `mapping.rs`), and finds the output port for the lights
by the same name. Get this wrong and nothing else in the profile runs.

- For a class-compliant device Windows shows the USB product string, often
  with a number in front ("2- DDJ-FLX6"); `mapping_key` strips that prefix. A
  device with a vendor driver may show another name, or several ports
  ("MIDIIN2 (…)").
- Where to find the real string: a `.tsi`'s port names (`tsi_dump.py` prints
  them), the `controller id` in a Mixxx XML, Mixxx forum logs, VirtualDJ
  definition headers, USB descriptor dumps (`lsusb -v` pastes,
  linux-hardware.org), screenshots of other software's MIDI settings.
- Record every spelling you find. Match loosely (the existing code compares
  upper-cased letters and digits with `contains`) so "2- " prefixes and
  " MIDI" suffixes don't matter, but not so loosely that a sibling with a
  different map matches too. If the MK1 and MK2 differ, the MK2's branch has
  to come first and the MK1 must not fall into it.
- If a controller exposes more than one MIDI port, find out which one carries
  the controls.

## Numbering traps

Most wrong presets are right research written down wrong. Before copying a
number, know which convention the source uses.

- **Channels.** Documents count channels 1–16; the status byte's low nibble
  is 0–15; **Tandem's `Trigger` channel is 0-based**. "Channel 5" is status
  `0x94` for a note and `Trigger::note(4, …)`. Convert once, in the notes
  file, and write both.
- **Hex and decimal.** Manufacturer lists mix them, sometimes in one table.
  Traktor names are decimal ("CC.031"), Mixxx XML is hex. `0x10` and `10` are
  six apart.
- **Note names.** Traktor and most tables call note 0 "C-1"; some call it
  "C-2", which shifts everything by 12. Anchor on a control you know from a
  numeric source before trusting a named one.
- **Release.** Some controllers release with a note-off (`0x8n`), some with a
  note-on of velocity 0. Tandem treats both as letting go. A few buttons send
  a press only, or are physical latches that send on at one press and off at
  the next: a mapping file won't tell you, a manufacturer list or a capture
  will. Tandem's Play, Sync and headphone-cue buttons act on the press, so a
  latching button would work every other press: note it.
- **"Toggle", "hold", "invert"** in a mapping file describe what that
  software does with the message, not what the hardware sends.
- **Fader direction.** Which end sends 127 is a hardware fact; whether a
  mapping marks it inverted depends on that software's convention. For the
  tempo fader, find a source that says which physical end sends the high
  value (manufacturer list, a capture, or script arithmetic you've followed
  through), or mark the direction unconfirmed.
- **14-bit values.** Coarse and fine halves are two CCs. Tandem joins a pair
  only when the fine half is the coarse CC + 32 (the MIDI standard layout),
  or when it arrives as a pitch-bend message. Sources name the pair in either
  order.
- **Deck layers.** A four-deck layout on a two-deck body either changes the
  MIDI channel when the DECK button is pressed (the hardware keeps the layer:
  decks 3 and 4 are separate messages) or sends the same messages plus a
  note for the DECK button (the software keeps the layer).
- **Modes and firmware.** A unit can send different messages in "Serato
  mode", "MIDI mode" or after another app's handshake. Note which mode each
  source was made in. Tandem sends no handshake unless told to
  (see engine.md), so the mode that matters is the one the unit is in when
  plugged in and left alone.
- **The same number twice.** If two unrelated controls share a message in
  your table, one of them is a transcription error or a per-mode message.

## By manufacturer

Starting points, not facts about any one model: check each.

- **Pioneer DJ / AlphaTheta (DDJ-…).** Publishes a "List of MIDI Messages"
  PDF for most DDJ controllers on the product's support page; copies are in
  the GitHub repo `pestrela/ddj` ("1 MIDI codes") and on the Internet
  Archive. One family protocol across the range: deck N on MIDI channel N,
  pads on their own channels with SHIFT one channel up, 14-bit faders as
  CC n / n+32, and a keep-alive SysEx or the unit drops into a demo light
  show. Tandem's `pioneer_ddj` already covers this family; a new DDJ model
  may need only a `builtin_preset` branch plus its differences.
- **Hercules (DJControl …).** Some models have a "MIDI commands" PDF linked
  from a "MIDI mapping" section of their support page
  (`ts.hercules.com/download/sound/manuals/…`); newer revisions may have
  none, in which case the older sibling's is the prediction. The live
  support site refuses scripts: use the Internet Archive. Mixxx has
  mappings for the first Inpulse 200, the 300 and the 500. What the
  Inpulse 200's list shows, and the Inpulse 200 MK2 kept (see
  `docs/controllers/`): global controls on channel 1, the decks on 2 and 3,
  and a deck's controls three channels up while its SHIFT is held; pads on
  channels 7 and 8, a note range per pad mode kept by the unit, SHIFT + pad
  eight notes up on the same channel; every fader and knob 14-bit as
  CC n / n + 32; lights a note-on to the button's own message, `7F` on and
  `00` off, with a separate set for the SHIFT layer; `B0 7F 7F` asks the unit
  to report every control's position.
- **Denon DJ, Numark, Akai, Rane (inMusic).** Rarely publish MIDI lists.
  Sources are Mixxx (many Numark Mixtracks and Denon MC units; a Denon
  Prime 4 mapping sits in a pull request), Traktor `.tsi` files and
  VirtualDJ definitions. Several need a SysEx on
  connect to leave a demo mode or to report their knob positions; lights are
  often dim at value 1 and pads take a colour number.
- **Roland (DJ-202, -505, -707M, -808).** Look for a MIDI implementation
  document on the support page; Mixxx has a DJ-505 mapping.
- **Reloop, Behringer, Allen & Heath, Vestax, Gemini.** Usually a MIDI chart
  in the manual or on the support page; many have Mixxx mappings.
- **Native Instruments (Traktor Kontrol …).** Recent units speak HID, not
  MIDI: no profile is possible. Some older ones have a MIDI mode entered
  with a button combination.

## The source log

Keep a list as you go; it becomes the "Sources" section of the notes file.
For each: what it is, the URL, its date or version, the model and firmware
it covers, the mode it was made in if known, where the raw copy is in the
scratchpad, and what it settled. For each kind of source you tried without
luck, one line saying what you searched for. A reader deciding whether to
trust the profile needs to see both lists.
