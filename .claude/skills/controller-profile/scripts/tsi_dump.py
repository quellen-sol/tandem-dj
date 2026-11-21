#!/usr/bin/env python3
"""Decode a Traktor .tsi controller mapping into a plain table.

    python3 -I tsi_dump.py mapping.tsi                      # every device in the file
    python3 -I tsi_dump.py mapping.tsi --commands KnownCommands.cs
    python3 -I tsi_dump.py mapping.tsi --device 2 --out     # only device 2's outputs (lights)

A .tsi is XML; the mapping is the base64 value of the entry
"DeviceIO.Config.Controller": nested frames of [4-char tag][u32 big-endian
size], strings as u32 length + UTF-16BE. Each mapping row ties a Traktor
command to a MIDI message name such as "Ch05.Note.A#-1" or "Ch01.CC.031".

Traktor command ids are numbers. To see their names, download the id list
from the cmdr project (don't copy it into this repo, it's GPL):
    curl -sSLO https://raw.githubusercontent.com/cmdr-editor/cmdr/master/cmdr/cmdr.TsiLib/Commands/Interpretation/KnownCommands.cs
and pass it with --commands.

Channels are printed 1-based as Traktor names them, with the 0-based status
nibble Tandem's `Trigger` uses beside it. Standard library only.
"""
import argparse
import base64
import re
import struct
import sys

NOTES = {"C": 0, "C#": 1, "D": 2, "D#": 3, "E": 4, "F": 5, "F#": 6, "G": 7, "G#": 8, "A": 9, "A#": 10, "B": 11}
CONTROL_TYPES = {0: "button", 1: "fader/knob", 2: "encoder", 65535: "LED"}
INTERACTION = {0: "trigger", 1: "toggle", 2: "hold", 3: "direct", 4: "relative", 5: "inc", 6: "dec", 7: "reset", 8: "output"}
ENCODER_MODES = {0: "3Fh/41h (64 = still: Offset64)", 1: "7Fh/01h (two's complement)", 3: "proprietary"}


class Reader:
    def __init__(self, buf, pos=0, end=None):
        self.buf, self.pos, self.end = buf, pos, len(buf) if end is None else end

    def u32(self):
        v = struct.unpack_from(">I", self.buf, self.pos)[0]
        self.pos += 4
        return v

    def i32(self):
        v = struct.unpack_from(">i", self.buf, self.pos)[0]
        self.pos += 4
        return v

    def f32(self):
        v = struct.unpack_from(">f", self.buf, self.pos)[0]
        self.pos += 4
        return v

    def wstr(self):
        n = self.u32()
        s = self.buf[self.pos:self.pos + 2 * n].decode("utf-16-be", "replace")
        self.pos += 2 * n
        return s

    def frame(self, want=None):
        """Enter the next frame: returns (tag, Reader over its body)."""
        tag = self.buf[self.pos:self.pos + 4].decode("ascii", "replace")
        size = struct.unpack_from(">I", self.buf, self.pos + 4)[0]
        body = Reader(self.buf, self.pos + 8, self.pos + 8 + size)
        self.pos += 8 + size
        if want and tag != want:
            raise ValueError(f"expected frame {want}, found {tag!r}")
        return tag, body

    def frames(self):
        while self.pos + 8 <= self.end:
            yield self.frame()


def midi_name(name):
    """'Ch05.Note.A#-1' -> ('note', 4, 0x0A); 'Ch01.CC.031' -> ('cc', 0, 31).

    Traktor counts octaves from C-1 = note 0. A 14-bit pair is named
    'Ch05.CC.075+CC.031' (order isn't reliable: check which is coarse)."""
    m = re.match(r"Ch(\d+)\.(.*)", name)
    if not m:
        return None
    ch, rest = int(m.group(1)) - 1, m.group(2)
    n = re.match(r"Note\.([A-G]#?)(-?\d+)$", rest)
    if n:
        return ("note", ch, (int(n.group(2)) + 1) * 12 + NOTES[n.group(1)])
    c = re.match(r"CC\.(\d+)(?:\+(?:Ch\d+\.)?CC\.(\d+))?$", rest)
    if c:
        return ("cc", ch, int(c.group(1)), int(c.group(2)) if c.group(2) else None)
    if rest.lower().startswith("pitch"):
        return ("pitchbend", ch, 0)
    return None


def show_midi(name):
    d = midi_name(name)
    if not d:
        return name
    kind, ch = d[0], d[1]
    status = {"note": 0x90, "cc": 0xB0, "pitchbend": 0xE0}[kind] | ch
    if kind == "pitchbend":
        return f"pitch bend ch {ch + 1} (status 0x{status:02X})"
    extra = f" + cc {d[3]:#04x} (14-bit pair)" if kind == "cc" and d[3] is not None else ""
    return f"{kind} {d[2]:#04x}{extra} ch {ch + 1} (status 0x{status:02X}, Tandem channel {ch})"


def load_commands(path):
    names = {}
    if not path:
        return names
    desc = None
    for line in open(path, encoding="utf-8-sig", errors="replace"):
        d = re.search(r'CommandDescription\(Categories\.(\w+),\s*"([^"]*)"', line)
        if d:
            desc = f"{d.group(1)} / {d.group(2)}"
            continue
        m = re.match(r"\s+(\w+)\s*=\s*(\d+),", line)
        if m:
            names[int(m.group(2))] = desc or m.group(1)
            desc = None
    return names


def parse_device(body):
    dev = {"name": body.wstr(), "in": [], "out": [], "maps": [], "names": {}}
    _, ddat = body.frame("DDAT")
    for tag, f in ddat.frames():
        if tag == "DDIC":
            dev["comment"] = f.wstr()
        elif tag == "DDPT":
            dev["in_port"], dev["out_port"] = f.wstr(), f.wstr()
        elif tag == "DDDC":
            for t, defs in f.frames():
                if t not in ("DDCI", "DDCO"):
                    continue
                for _ in range(defs.u32()):
                    _, d = defs.frame("DCDT")
                    dev["in" if t == "DDCI" else "out"].append({
                        "name": d.wstr(), "type": d.u32(), "min": d.f32(), "max": d.f32(),
                        "encoder": d.u32(), "id": d.u32(),
                    })
        elif tag == "DDCB":
            for t, sub in f.frames():
                if t == "CMAS":
                    for _ in range(sub.u32()):
                        _, c = sub.frame("CMAI")
                        row = {"binding": c.u32(), "out": c.u32() == 1, "command": c.u32()}
                        _, d = c.frame("CMAD")
                        d.u32()  # device type
                        row["control"] = d.u32()
                        row["interaction"] = d.u32()
                        row["deck"] = d.i32()
                        row["auto_repeat"], row["invert"], row["soft_takeover"] = d.u32(), d.u32(), d.u32()
                        row["sensitivity"], row["acceleration"] = d.f32(), d.f32()
                        d.u32()
                        d.u32()
                        raw = d.buf[d.pos:d.pos + 4]
                        row["set_i"], row["set_f"] = struct.unpack(">i", raw)[0], struct.unpack(">f", raw)[0]
                        d.pos += 4
                        row["comment"] = d.wstr()
                        row["cond"] = []
                        for _ in range(2):
                            cid, _target, val = d.u32(), d.i32(), d.i32()
                            if cid:
                                row["cond"].append((cid, val))
                        d.u32()
                        d.u32()
                        d.u32()
                        d.u32()
                        if d.pos + 8 <= d.end:
                            row["led_min"], row["led_max"] = d.u32(), d.u32()
                        dev["maps"].append(row)
                elif t == "DCBM":
                    for _ in range(sub.u32()):
                        _, b = sub.frame()  # the list and its entries share the tag DCBM
                        bid = b.u32()
                        dev["names"][bid] = b.wstr()
    return dev


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("tsi")
    ap.add_argument("--commands", help="cmdr's KnownCommands.cs, to name Traktor command ids")
    ap.add_argument("--device", type=int, help="only this device (1-based, as listed)")
    ap.add_argument("--in", dest="only_in", action="store_true", help="inputs only")
    ap.add_argument("--out", dest="only_out", action="store_true", help="outputs (lights) only")
    a = ap.parse_args()

    text = open(a.tsi, encoding="utf-8", errors="replace").read()
    m = re.search(r'<Entry Name="DeviceIO\.Config\.Controller"[^>]*Value="([^"]*)"', text)
    if not m:
        sys.exit("no DeviceIO.Config.Controller entry: not a controller mapping .tsi")
    blob = base64.b64decode(m.group(1))
    names = load_commands(a.commands)

    _, diom = Reader(blob).frame("DIOM")
    devices = []
    for tag, f in diom.frames():
        if tag == "DEVS":
            for _ in range(f.u32()):
                _, d = f.frame("DEVI")
                devices.append(parse_device(d))

    for i, dev in enumerate(devices, 1):
        if a.device and a.device != i:
            continue
        ins = sum(1 for r in dev["maps"] if not r["out"])
        print(f"== device {i}: {dev['name']!r}  in port {dev.get('in_port')!r}  out port {dev.get('out_port')!r}"
              f"  ({ins} in, {len(dev['maps']) - ins} out)")
        if dev.get("comment"):
            print(f"   comment: {dev['comment']}")
        defs = {d["name"]: d for d in dev["in"]}
        rows = sorted(dev["maps"], key=lambda r: (r["out"], dev["names"].get(r["binding"], "~")))
        for r in rows:
            if (a.only_in and r["out"]) or (a.only_out and not r["out"]):
                continue
            midi = dev["names"].get(r["binding"])
            if not midi:
                continue  # a mapping row with no MIDI message assigned
            cmd = names.get(r["command"], f"command {r['command']}")
            deck = "device target" if r["deck"] < 0 else "ABCD"[r["deck"]] if r["deck"] < 4 else f"target {r['deck']}"
            bits = [CONTROL_TYPES.get(r["control"], str(r["control"])), INTERACTION.get(r["interaction"], str(r["interaction"]))]
            if r["invert"]:
                bits.append("invert")
            if r["control"] == 2 and midi in defs:
                # How the mapping's author told Traktor this encoder counts.
                bits.append("encoder mode " + ENCODER_MODES.get(defs[midi]["encoder"], str(defs[midi]["encoder"])))
            if r["interaction"] in (1, 2, 3) and r["set_i"]:
                # The value is an int or a float depending on the command.
                small = abs(r["set_i"]) < 100000
                bits.append(f"value {r['set_i']}" if small else f"value {r['set_f']:.3g}")
            for cid, val in r["cond"]:
                # Conditions 2548-2555 are Traktor's modifiers M1-M8 (how a
                # mapping does SHIFT, deck layers and pad modes in software).
                name = f"M{cid - 2547}" if 2548 <= cid <= 2555 else f"condition {cid}"
                bits.append(f"if {name} = {val}")
            if r["out"] and "led_min" in r:
                bits.append(f"sends {r['led_min']}..{r['led_max']}")
            note = f"  # {r['comment']}" if r["comment"] else ""
            print(f"  {'OUT' if r['out'] else 'IN '} {show_midi(midi):<58} {cmd} [{deck}] ({', '.join(bits)}){note}")
        print()


if __name__ == "__main__":
    main()
