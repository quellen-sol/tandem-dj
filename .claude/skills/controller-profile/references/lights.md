# Researching a controller's lights

A wrong input is noticed at once: the button does nothing. A wrong light
fails quietly. The pad stays dark, or glows the wrong colour, or the whole
unit sits in its demo show, and the DJ reads the controller wrong for a
whole set. Sources also cover lights worse than inputs: many mappings light
only half the unit, and manufacturers bury the LED table at the back. So
lights get their own pass, with their own sources and their own confidence.

## The questions to answer

Answer each for this controller, with a source. "Not found" is an answer;
a guess isn't.

1. **Who lights what?** Some controllers light a button themselves while
   it's pressed, track their own pad mode and SHIFT lights, or run every LED
   locally until software takes over. Software lights everything else. Know
   which is which, or Tandem and the controller fight over a light or both
   leave it dark.

2. **What does the unit need before it listens?** Possibilities: nothing
   (plain MIDI works from power-on); a SysEx on connect that hands the LEDs
   to the software or ends a demo mode; a keep-alive every fraction of a
   second, without which it falls back to a light show (Pioneer DDJs); a
   mode chosen on the hardware. Look for `sendSysexMsg` in Mixxx scripts'
   `init`, `<init>` / `<ping>` in VirtualDJ definitions, and "the following
   message must be sent" in manufacturer lists. Write the bytes down exactly.

3. **Where is each light addressed?** Usually a note-on to the same channel
   and note as the button it sits in. Not always: lights can be on another
   channel, on CCs, or shared across SHIFT layers. For each light record the
   message, and whether it equals its button's own.

4. **What do the values mean?** The common schemes:

   | Scheme | Looks like | How to recognise it |
   |---|---|---|
   | On / off | `0x7F` lit, `0x00` dark | Manufacturer table says "7F = lit"; mappings only ever send two values. |
   | Dim / bright | `0x01` dim, `0x7F` bright, `0x00` off | Mappings send `1` for "available but inactive". Buttons then never go fully dark in normal use. |
   | Indexed palette | velocity is a colour number from a table | A table of named colours in the script or the manufacturer list; values scattered through 1–127. |
   | Packed colour | bits of the velocity are red, green and blue (2 bits each gives 64 colours, for example) | Colour values built with shifts or multiplications in a script; a comment giving the bit layout. |
   | Blink states | certain values or a second channel mean "blink" | The manufacturer list has a "blink" column; scripts use timers when the hardware can't blink by itself. |
   | SysEx colour | full RGB sent as SysEx | Long `0xF0 …` messages per pad. |

   For a colour scheme, record the whole table or the bit layout, not just
   the values one mapping happened to use, and note the values for the eight
   colours Tandem's hot cues use on screen: red, orange, yellow, green, cyan,
   blue, purple, pink.

5. **Do lights depend on a layer?** With SHIFT held or in another pad mode, a
   pad's light may be a different message (controllers whose pads send
   different notes per mode usually have a light per mode too), and the
   unit may or may not repaint the pads itself when the mode changes.

6. **Level meters.** The message (often one CC per channel, sometimes a
   master pair), the value range, and how values become lit segments:
   proportional, thresholds, or a bit per segment. A manufacturer list
   rarely gives thresholds; a mapping's meter function shows what its author
   found to work.

7. **Displays, jog rings and motors.** Jog-wheel rings and screens, 7-segment
   digits, beat indicators. Tandem drives none of these today: record their
   messages so the work isn't lost, and move on.

8. **What happens when the software leaves?** Does the unit go dark, keep
   its last state, or return to a demo show? Tandem sends 0 to every light
   it lit when it closes; note if this controller's "off" isn't 0 (dim-at-1
   units keep a glow), or if it wants a goodbye message.

## Where light facts hide

- **Manufacturer list:** the "MIDI IN" / "received messages" / "LED" section,
  usually after all the inputs.
- **Mixxx:** `<output>` rows for simple lights; the script for everything
  else. `mixxx_map.py show` prints both and says how many lights sit on
  their button's own note.
- **Traktor:** the output rows of the `.tsi` (`tsi_dump.py --out`), with the
  value range each sends.
- **VirtualDJ:** `<led>` elements in definitions, and the hardware manual's
  description of what each LED shows in each mode.
- **Forum threads:** search the model with "LED", "lights", "sysex", "demo
  mode", "vu meter".

When two sources light the same pad with different values, both may be
right: a palette has many colours and each author picked their own. They
conflict only if they imply different *schemes*.

## What Tandem can drive today

See "Lights" in [engine.md](engine.md) for the exact rules. In short: a
note-on to a button's own channel and note, one value when lit and one when
not, for controls that have a state (play, cue, sync, hot cues, loops, rolls,
pad mode, SHIFT, headphone cue), plus Pioneer-scaled level meters on a CC.
Everything else you find goes in the notes file under "Lights Tandem doesn't
drive yet", with its message and values, so adding it later needs no new
research.
