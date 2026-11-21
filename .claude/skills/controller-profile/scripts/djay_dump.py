#!/usr/bin/env python3
"""Print an Algoriddim djay .djayMidiMapping (an XML property list) as one
line per control.

    python3 -I djay_dump.py mapping.djayMidiMapping

A djay mapping is learned by pressing each control on the real unit, so its
messages are hardware evidence. The header gives the MIDI port name
(`endpointName`) and the USB id. In each row: `modifier` is what the author
put on the SHIFT layer, `output` means djay lights that control on the same
message, and `flipped` is djay's own direction flag. The function names say
what the author made a control do, not what it's labelled.

Channels are 0-based in the file, as in Tandem's `Trigger`. Standard library
only.
"""
import plistlib
import sys

# midiMessageType as the files use it.
KINDS = {1: ("note", 0x90), 3: ("cc", 0xB0)}


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    with open(sys.argv[1], "rb") as f:
        d = plistlib.load(f)
    head = {k: (f"{v:#x}" if k == "USBID" and isinstance(v, int) else v) for k, v in d.items() if k != "controls"}
    print(head)
    rows = []
    for c in d.get("controls", []):
        ch = c.get("midiChannel") or 0
        kind = c.get("midiMessageType")
        number = c.get("midiData") or 0
        extra = {k: v for k, v in c.items() if k not in ("keyPath", "midiChannel", "midiData", "midiMessageType")}
        rows.append((ch, kind or 0, number, c.get("keyPath"), extra))
    for ch, kind, number, path, extra in sorted(rows, key=lambda r: r[:3]):
        name, status = KINDS.get(kind, (f"type {kind}", 0))
        flags = "  " + ", ".join(f"{k}={v}" if v not in (True, {}) else k for k, v in sorted(extra.items())) if extra else ""
        print(f"{status | ch:02X} {number:02X}  {name:<5} {number:#04x} ch {ch + 1:<2} (Tandem channel {ch})  {path}{flags}")
    print(f"{len(rows)} controls")


if __name__ == "__main__":
    main()
