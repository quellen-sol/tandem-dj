# The notes file

Every profile leaves `docs/controllers/<brand>-<model>.md` behind (lower
case, hyphens: `hercules-djcontrol-inpulse-200-mk2.md`). It's the profile's
evidence: when the owner says "pad 3 stays dark", the next person opens it
to see where that number came from and how sure anyone was, instead of
researching the controller again.

Write it for that reader: someone who knows Tandem's code but has never
seen this controller and doesn't have your session. Full sentences where a
table can't carry the point; no shorthand you made up while researching.
Keep channels in both forms once at the top and then use one consistently.

Use this structure. Drop a section only when it truly doesn't apply, and
say so in one line instead of leaving it out silently.

```markdown
# <Brand Model>

**Status:** <built-in preset since vX / researched, not yet tried on the
unit / captured from the unit on YYYY-MM-DD>. Researched YYYY-MM-DD.

One paragraph: what the unit is (decks, pads per deck, sound card), which
revision this covers and how it differs from its siblings, and the mode it
has to be in.

## Connecting

- MIDI port name(s) on Windows, every spelling found, and the source of each.
- Driver or mode needed. Whether it's MIDI, and how that was established.
- Sound card: channels for master and headphones; knobs that act in
  hardware and send no MIDI.
- Init or keep-alive it needs, bytes in full, and whether Tandem sends it.

## Layout

MIDI channels in one table: what each channel carries (1-based as documents
write it, and Tandem's 0-based number beside it). Then the answers to the
design questions, each with its source:

- SHIFT: changes the messages / sends its own note and nothing else.
- Pad modes: per-mode notes / same notes in every mode. Which modes exist.
- Deck layers, if any.
- Jog: messages, encoding, ticks per turn, touch sensor.
- Faders: 7-bit or 14-bit and the pair layout; tempo fader direction.

## Controls

One row per physical control, from the hardware manual's list. Group by
section (deck, pads per mode, mixer, browse, global).

| Control | Message | Sends | Tandem | Confidence | Sources |
|---|---|---|---|---|---|
| PLAY (deck 1) | `91 07` | 7F press, 00 release | `Play` A | confirmed | [1] p.3, [2] |
| SHIFT + PLAY | `94 07` | same | no Tandem control (stutter) | single source | [2] |

Write messages as status byte and number in hex (`91 07`: note 0x07 on
channel 2), the form `preset_table` prints, so the two can be compared row
by row. "Tandem" is the target it's bound to, or "no Tandem control (what
it is)", or "left unmapped: <reason>". Confidence is one of: confirmed,
single source, inferred, conflict, unknown, as SKILL.md defines them, graded
separately for the message, what the control is and its direction when
those differ.

## Lights

| Light | Message | Values | Lit by | Tandem | Confidence | Sources |
|---|---|---|---|---|---|---|

"Lit by" says whether the unit lights it by itself or software must.
"Tandem" says whether the preset drives it and with which values.
Then, as prose or small tables: the colour scheme in full, the level
meters, what the unit does at connect and when the software leaves.

### Lights Tandem doesn't drive yet

Messages and values for everything found but not expressible today (see
engine.md), so adding it later needs no new research.

## Conflicts and open questions

Each disagreement between sources: what each said, which was followed, why.
Each thing nobody documents. Each inference from a sibling model.

## What Tandem can't do with this unit yet

Controls with no Tandem function, and engine limits this unit runs into.
Product choices offered to the user and not taken (for instance "the
SAMPLER pads could roll").

## Check on the hardware

A numbered list for the owner, most revealing first, each step saying what
to do and what should happen. Mark the ones that would change the preset if
they fail.

## Sources

Numbered, so the tables can cite them.

1. What it is, author or publisher, version or date, URL. Which model,
   firmware and mode it covers. What it settled.

### Looked for and not found

One line per kind of source that came up empty, with what was searched.
```

Rules for filling it in:

- Cite a source for every row. A row with no source has no place in the
  preset.
- When a value was converted (1-based channel to status byte, decimal to
  hex, a note name to a number), the table holds the converted form and the
  Sources entry says which convention the source used.
- Don't paste long copyrighted tables from a manufacturer's PDF; cite the
  page and record the facts.
- Dates are absolute. Version numbers are the ones you verified, not the
  ones you remember.
