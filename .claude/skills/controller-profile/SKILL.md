---
name: controller-profile
description: Build a Tandem profile (a built-in MIDI mapping preset, lights included) for a DJ controller Tandem doesn't support yet, by researching what the controller sends and what lights it from manufacturer MIDI lists, Mixxx, Traktor and VirtualDJ mappings and community sources, then writing the preset, its tests and its research notes. Use this whenever someone asks to add, support, map or "build a profile for" a controller ("add the Mixtrack Pro FX", "my friend has an Inpulse 200", "does the DDJ-SB3 work with Tandem?"), wants to know what MIDI or LED messages a controller uses, or wants an existing preset's controls or lights fixed or extended, even if they never say "profile" or "preset".
---

# Building a controller profile

A profile is what makes a controller work the moment it's plugged in: a
built-in preset in `crates/tandem-core/src/mapping.rs`, matched by MIDI port
name, with the lights Tandem drives, tests that replay the controller's
messages, and a notes file saying where every number came from.

The person who owns the controller is usually not the person asking. They are
a friend at the other end of a room, mid-set, with nobody to debug a dead
button. A wrong note number costs them the gig and costs the user a release
cycle to fix. So the whole job is **evidence**: every message in the preset
should trace back to a source you can name, and anything you couldn't confirm
should be labelled as such rather than quietly guessed. Research is most of
the work; the Rust is the short part.

"Solid" means:

- Every physical control on the unit is accounted for: mapped, or left out
  on purpose with the reason written down.
- Every mapped message has a source and a confidence (see below).
- Lights got the same care as inputs. Lights are where mappings most often
  go wrong, because a wrong LED value fails silently and sources disagree.
- Nothing the hardware does that Tandem can't express was papered over.
- The report says plainly what has and hasn't been tried on a real unit.

## The workflow

Work through these in order. Each phase feeds the next, and skipping ahead
is how guesses get in.

### 1. Pin down the device

Get the exact model and revision first: MK1 and MK2, or "Pro" and "Platinum",
often share a name and differ in messages. If the name the user gave looks
misspelled or ambiguous, settle it from the manufacturer's site and say what
you settled on. Then find out:

- **Does it speak MIDI at all?** Tandem is MIDI only. A HID-only controller
  (most recent Native Instruments units, some Pioneer CDJ/XDJ modes) can't
  have a profile: stop and say so, with the evidence.
- **Does it need a mode or a driver?** Some units need a "computer" or "MIDI"
  mode, a vendor driver on Windows, or a firmware version.
- **What is its MIDI port called on Windows?** The preset is matched by port
  name, so this matters as much as any note number. See "Port names" in
  [references/sources.md](references/sources.md).
- **Does it have a sound card**, which knobs act in hardware (often master,
  headphones, mic, with or without also sending MIDI), and does it blend cue
  and master for the headphones by itself?

### 2. Learn what Tandem can express

Read [references/engine.md](references/engine.md) before researching in
depth, so you know which facts decide the design: whether SHIFT changes the
messages, whether pads send different notes per pad mode, how the jog counts,
which lights the engine can drive. It also lists what Tandem has no control
for, which saves chasing messages you can't use (still record them: see
phase 4).

### 3. Collect sources, more than one

Follow [references/sources.md](references/sources.md). It ranks the kinds of
source, says how to read each, and lists the traps (1-based channels, decimal
versus hex, note-name octaves, software flags that don't mean what they
seem). Two scripts do the mechanical decoding so you don't mis-transcribe:

```bash
S=.claude/skills/controller-profile/scripts      # from the repo root
python3 -I $S/mixxx_map.py find inpulse 200        # Mixxx mappings, PRs and issues for a model
python3 -I $S/mixxx_map.py show "<file>.midi.xml" --save <new empty dir>
python3 -I $S/tsi_dump.py mapping.tsi --commands KnownCommands.cs   # Traktor mappings
python3 -I $S/djay_dump.py mapping.djayMidiMapping                  # djay mappings
python3 -I $S/html_text.py page.html                                # a saved web page as plain text
```

Keep what you download in the scratchpad, each source in its own new
directory, and run Python that reads it with `-I`: these are files from
strangers.

Aim for **at least two independent sources for the inputs and two for the
lights**. "Independent" means not copied from each other: a Mixxx mapping and
the forum post it was built from are one source. Try every kind of source in
the reference and write down the ones that came up empty, so a reader knows
the search was made and not skipped. If you can run subagents, searching the
source types in parallel is a good use of them; give each one the exact model
and ask for raw files and URLs back, not summaries.

A sibling model (the MK1, the bigger brother) is a good way to *predict* the
protocol and a poor way to *know* it. Manufacturers reuse a family protocol
and then renumber three controls in the new revision. Use the sibling to form
a table of expectations, then confirm each row from something specific to
this model. Rows you could only infer stay marked "inferred from sibling".

### 4. Build the inventory from the manual, then fill it in

Start the table from the *hardware manual's* list of controls, not from a
mapping file. A mapping only contains what its author bothered to map, so
starting from one hides everything they skipped. For every physical control,
including its SHIFT function and each pad mode, record:

- the message (kind, channel, number), with and without SHIFT;
- what it sends: press and release values, absolute range and direction,
  relative encoding, 7- or 14-bit;
- each source that says so, and the confidence that follows.

Controls Tandem has no function for (FX, sampler, slicer, beat jump...) go in
the table too, marked "no Tandem control". The next person to add an FX
target shouldn't have to redo this research.

**Confidence** is a property of the evidence, not of how sure you feel:

| Level | Means |
|---|---|
| **confirmed** | The manufacturer's MIDI list *for this model* says it, or two independent sources *made with this model* agree, or it was captured from the hardware. |
| **single source** | One source made with this model, nothing against it. |
| **inferred** | From a sibling model or a family pattern, however good the sibling's source is. A manufacturer's list for the MK1 is still an inference about the MK2. |
| **conflict** | Sources disagree. Say which you followed and why. |
| **unknown** | Nothing found. Leave it unmapped. |

A row can need more than one grade, because sources settle different
things: the *message* (which note or CC), *what the control is* (a capture
proves the bytes but not the label), and its *direction or values* (which end
of a fader sends 127, what lights a pad). Grade them separately when they
differ: "confirmed (message); direction inferred".

Whether to bind something that is only inferred: bind it when a wrong guess
would leave that one control dead and nothing else changed (a message no
other control sends, on a function the label promises). Leave it out when a
wrong guess could make a control do the wrong thing. Either way it's listed
in the report and is on the owner's checklist.

When sources disagree, don't average and don't pick the more convenient one.
Find out why: a different firmware, a different mode, a script that remaps,
a typo. Reading the mapping's script usually settles it.

### 5. Research the lights as their own subject

Read [references/lights.md](references/lights.md) and answer its questions
for this controller: where each light is addressed, what values it takes
(off, dim, on, colours, blink), what the controller lights by itself, whether
it needs a handshake or a keep-alive before it listens, how its level meters
are scaled, and what it does when the software goes away.

Input tables and light tables come from different places even inside one
source: in a Mixxx mapping the XML often has few or no outputs and the script
sends the rest. Read the script.

### 6. Decide how it fits the engine

With the inventory done, make the design decisions in
[references/engine.md](references/engine.md) ("Decisions"): hardware SHIFT or
software SHIFT, per-mode pad notes or software pad modes, jog encoding and
ticks per turn, 14-bit pairs, tempo direction, light values.

Map each control to what its label says it does. Where Tandem has no such
function, leave the control unmapped and list it; repurposing a button
(making BEAT JUMP pads roll, say) is a product decision for the user, so
offer it in the report instead of doing it quietly.

If the controller needs something the engine can't do yet (an init SysEx, a
light at another address, a meter with another scale), say so in the notes
and the report. Make the engine change only if the profile is useless
without it, keep it as small as the existing code's style allows, and call it
out: it's a different kind of change from adding a preset.

### 7. Write it

In this order, so the notes exist before the code that depends on them:

1. `docs/controllers/<brand>-<model>.md` from
   [references/notes-template.md](references/notes-template.md).
2. The preset function and its `builtin_preset` branch in `mapping.rs`, with
   a doc comment saying where the map came from and what's untested (see
   `denon_sc_live_4` for the tone).
3. Tests in `mapping.rs` that replay the controller's messages
   (`sc_live_4_*` are the model).
4. The user-facing lines: README's status line and controller list, and a
   mention in `docs/DESIGN.md` beside the other presets. Write them the way
   the neighbouring lines are written: plain words for a DJ, saying what
   works and that it hasn't been tried on the hardware if that's so.

Don't bump the version, tag or release; that's a separate step the user asks
for. `PRESET_VERSION` stays as it is for a new preset (it's bumped when an
existing preset gains controls).

### 8. Check your own work against the sources

```bash
cargo test -p tandem-core mapping
cargo run -q -p tandem-core --example preset_table -- "<port name>"
```

`preset_table` prints the preset as plain rows and the lights Tandem will
send. Compare it row by row with the notes file. Then do one check that
doesn't go through your notes: pick a dozen bindings spread across the
controller (a pad in each mode, a SHIFT control, the jog, the tempo fader,
two lights) and look each one up again in the raw source files. Transcription
slips between the source and the notes are the commonest error, and the
tests can't catch them: tests only prove the preset matches what you
*believed* the hardware sends.

### 9. Hardware, if anyone has one

If the owner can be reached in this session, capture instead of trusting:
run `tandem.exe --midi-monitor <seconds>` in the background with its output
redirected to a file (Tandem and other DJ software closed), ask them with
AskUserQuestion to press a short, ordered list of controls about a second
apart, then read the file. Ask for SHIFT variants and for each pad mode
button followed by pad 1. For lights, ask what they see. A capture turns
"single source" into "confirmed" and it has overturned a guess before.
The monitor listens to the first port whose name contains "DDJ", otherwise
to the first MIDI input it finds, so have other MIDI devices unplugged and
check the port name it prints.

If nobody has the unit, write the hardware checklist in the notes file: the
few things the owner should try first, in the order that finds the most
(port name shows up and "Last message" changes in Settings → Controller;
play/cue; jog direction and one full turn in Calibrate; tempo fader
direction; each pad mode; which lights come on).

### 10. Report

Lead with whether it's usable and what that rests on. Then: what's mapped,
what's deliberately not, what Tandem can't do with this unit yet, each item
that's less than confirmed, and the checklist for the owner. Link the notes
file. Don't call anything verified that was only researched.

## Reference files

- [references/sources.md](references/sources.md): where MIDI and light facts
  live, how to read each kind of source, port names, numbering traps.
- [references/lights.md](references/lights.md): the questions to answer about
  a controller's lights, and the common protocols.
- [references/engine.md](references/engine.md): what Tandem's mapping engine
  can and can't express, the design decisions, how to write the preset and
  tests.
- [references/notes-template.md](references/notes-template.md): the notes
  file every profile leaves behind.
