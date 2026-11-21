#!/usr/bin/env python3
"""Find and read Mixxx controller mappings (the largest open set of MIDI maps).

    python3 -I mixxx_map.py find inpulse 200          # mappings in Mixxx, and PRs/issues that mention it
    python3 -I mixxx_map.py pr 13579                  # the controller files a Mixxx pull request adds
    python3 -I mixxx_map.py show "Pioneer-DDJ-FLX4.midi.xml"            # by name, from Mixxx's main branch
    python3 -I mixxx_map.py show ./some.midi.xml --js ./some-script.js  # local files (a PR's, a forum post's)
    python3 -I mixxx_map.py show <url-of-raw-xml> --save DIR            # keep the XML and scripts to grep later

`show` prints every input and output as one line with the MIDI message
decoded (kind, number, 1-based channel, status byte, Tandem's 0-based
channel), then what the tables imply: the channel layout, 14-bit pairs,
relative controls, which lights sit on their button's own message, and where
the mapping's script handles jogs, lights and SysEx.

Mixxx XML: <control> rows are inputs, <output> rows are lights. <status> is
the status byte (0x9n note, 0xBn CC, 0xEn pitch bend; n = channel - 1) and
<midino> the note or CC number. Standard library only; reads GitHub without
a login (60 requests an hour).
"""
import argparse
import json
import os
import re
import sys
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

RAW = "https://raw.githubusercontent.com/mixxxdj/mixxx/main/res/controllers/"
API = "https://api.github.com/"
# Shared libraries every mapping pulls in: nothing controller-specific in them.
COMMON_JS = {"common-controller-scripts.js", "midi-components-0.0.js", "lodash.mixxx.js", "common-hid-packet-parser.js"}
# What each <options> flag means for the value a control sends (from Mixxx's
# MidiController::computeValue).
OPTIONS = {
    "diff": "relative, two's complement (1 = +1, 127 = -1)",
    "selectknob": "relative, two's complement (1 = +1, 127 = -1)",
    "hercjog": "relative, two's complement (1 = +1, 127 = -1)",
    "hercjogfast": "relative, two's complement (1 = +1, 127 = -1)",
    "rot64": "relative, 64 = still (65 = +1, 63 = -1)",
    "rot64inv": "relative, 64 = still, direction inverted",
    "rot64fast": "relative, 64 = still",
    "spread64": "relative, 64 = still, accelerating",
    "invert": "absolute, inverted (127 - value)",
    "fourteen-bit-msb": "coarse half of a 14-bit value",
    "fourteen-bit-lsb": "fine half of a 14-bit value",
    "script-binding": "handled by the script function named as its key: read it",
}


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": "midi-map-reader", "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read()


def num(text):
    text = (text or "").strip()
    return int(text, 16) if text.lower().startswith("0x") else int(text or 0)


def message(status, data1):
    kind = {0x80: "note", 0x90: "note", 0xB0: "cc", 0xE0: "pitch bend"}.get(status & 0xF0, f"status {status:#04x}")
    ch = status & 0x0F
    if kind == "pitch bend":
        return f"pitch bend ch {ch + 1} (status 0x{status:02X}, Tandem channel {ch})"
    off = " note-off" if status & 0xF0 == 0x80 else ""
    return f"{kind} {data1:#04x} ch {ch + 1} (status 0x{status:02X}{off}, Tandem channel {ch})"


def find(words):
    want = [re.sub(r"[^a-z0-9]", "", w.lower()) for w in words]
    tree = json.loads(get(API + "repos/mixxxdj/mixxx/git/trees/main:res/controllers"))
    hits = [t["path"] for t in tree.get("tree", [])
            if all(w in re.sub(r"[^a-z0-9]", "", t["path"].lower()) for w in want)]
    print(f"Mappings on Mixxx's main branch matching {' '.join(words)!r}: {len(hits)}")
    for h in hits:
        print(f"  {h}")
    if not hits:
        print("  (none: look at the family's other models below and at the pull requests)")
        loose = [t["path"] for t in tree.get("tree", [])
                 if want and want[0] in re.sub(r"[^a-z0-9]", "", t["path"].lower()) and t["path"].endswith(".xml")]
        for h in loose[:40]:
            print(f"  family? {h}")
    q = urllib.parse.quote(f"repo:mixxxdj/mixxx {' '.join(words)}")
    res = json.loads(get(API + f"search/issues?q={q}&per_page=30"))
    print(f"\nMixxx pull requests and issues mentioning it: {res.get('total_count', 0)}")
    for it in res.get("items", []):
        kind = "PR   " if "pull_request" in it else "issue"
        merged = ""
        if "pull_request" in it:
            merged = " merged" if it["pull_request"].get("merged_at") else " not merged"
        print(f"  {kind} #{it['number']} [{it['state']}{merged}] {it['title']}\n        {it['html_url']}")
    print("\nAn open or closed-unmerged PR often carries a complete mapping: `mixxx_map.py pr <number>`.")
    print("Also search https://mixxx.discourse.group and the Mixxx manual's hardware pages for the model.")


def pr(number):
    files = json.loads(get(API + f"repos/mixxxdj/mixxx/pulls/{number}/files?per_page=100"))
    info = json.loads(get(API + f"repos/mixxxdj/mixxx/pulls/{number}"))
    print(f"PR #{number}: {info.get('title')} [{info.get('state')}, merged: {bool(info.get('merged_at'))}]")
    for f in files:
        if "controllers" in f["filename"] or f["filename"].endswith((".xml", ".js")):
            print(f"  {f['status']:<9} {f['filename']}\n            {f['raw_url']}")


def load(ref, save):
    """A local path, a URL, or a file name in Mixxx's res/controllers."""
    if os.path.exists(ref):
        return open(ref, "rb").read(), os.path.dirname(os.path.abspath(ref)) + os.sep, False
    url = ref if re.match(r"https?://", ref) else RAW + urllib.parse.quote(ref)
    data = get(url)
    if save:
        os.makedirs(save, exist_ok=True)
        with open(os.path.join(save, os.path.basename(urllib.parse.unquote(url))), "wb") as f:
            f.write(data)
    return data, url.rsplit("/", 1)[0] + "/", True


def option_names(el):
    opts = el.find("options")
    return [o.tag for o in opts] if opts is not None else []


def show(ref, js_refs, save):
    data, base, remote = load(ref, save)
    root = ET.fromstring(data)
    info = root.find("info")
    if info is not None:
        for tag in ("name", "author", "description", "manual", "forums", "wiki"):
            v = info.findtext(tag)
            if v:
                print(f"{tag}: {' '.join(v.split())}")
    ctl = root.find("controller")
    if ctl is not None:
        print(f"controller id (close to the USB product name Mixxx matches): {ctl.get('id')!r}")
    scripts = [(f.get("filename"), f.get("functionprefix")) for f in root.iter("file")]
    print("scripts: " + ", ".join(f"{n} (prefix {p})" for n, p in scripts))

    ins, outs = [], []
    for c in root.iter("control"):
        ins.append({
            "status": num(c.findtext("status")), "no": num(c.findtext("midino")),
            "group": (c.findtext("group") or "").strip(), "key": (c.findtext("key") or "").strip(),
            "opts": option_names(c), "desc": " ".join((c.findtext("description") or "").split()),
        })
    for o in root.iter("output"):
        outs.append({
            "status": num(o.findtext("status")), "no": num(o.findtext("midino")),
            "group": (o.findtext("group") or "").strip(), "key": (o.findtext("key") or "").strip(),
            "on": o.findtext("on"), "off": o.findtext("off"),
            "min": o.findtext("minimum"), "max": o.findtext("maximum"),
        })
    order = lambda r: (r["status"] & 0x0F, r["status"] & 0xF0, r["no"])

    print(f"\n== INPUTS ({len(ins)}) ==")
    for r in sorted(ins, key=order):
        opts = ", ".join(r["opts"]) or "Normal"
        desc = f"  # {r['desc']}" if r["desc"] else ""
        print(f"IN  {message(r['status'], r['no']):<52} {r['group']} {r['key']} ({opts}){desc}")

    print(f"\n== OUTPUTS / LIGHTS declared in the XML ({len(outs)}) ==")
    for r in sorted(outs, key=order):
        vals = ", ".join(f"{k} {r[k]}" for k in ("on", "off", "min", "max") if r[k])
        print(f"OUT {message(r['status'], r['no']):<52} {r['group']} {r['key']} ({vals})")
    if not outs:
        print("(none: the script sends the lights, see the script section below)")

    print("\n== WHAT THE TABLES IMPLY (check each against the script and a second source) ==")
    by_status = {}
    for r in ins:
        by_status.setdefault(r["status"], []).append(r)
    print("Channel layout (which controls live on which status byte):")
    for st in sorted(by_status, key=lambda s: (s & 0x0F, s & 0xF0)):
        groups = {}
        for r in by_status[st]:
            groups[r["group"]] = groups.get(r["group"], 0) + 1
        top = ", ".join(f"{g} x{n}" for g, n in sorted(groups.items(), key=lambda kv: -kv[1])[:5])
        print(f"  {message(st, 0).split(' ch ')[0].rsplit(' ', 1)[0]:<10} ch {(st & 0x0F) + 1:<2} status 0x{st:02X}: {len(by_status[st]):>3} controls  {top}")

    cc = {(r["status"], r["no"]): r for r in ins if r["status"] & 0xF0 == 0xB0}
    pairs = [(k, cc[(k[0], k[1] + 32)]) for k in cc if k[1] < 32 and (k[0], k[1] + 32) in cc]
    flagged = [r for r in ins if any(o.lower().startswith("fourteen-bit") for o in r["opts"])]
    if pairs or flagged:
        print("14-bit faders (coarse CC n + fine CC n+32 is the only pair layout Tandem joins):")
        for k, fine in sorted(pairs):
            same = "" if (cc[k]["group"], cc[k]["key"]) == (fine["group"], fine["key"]) else "  [different keys: may not be a pair]"
            print(f"  {message(k[0], k[1])}  + fine cc {k[1] + 32:#04x}: {cc[k]['group']} {cc[k]['key']}{same}")
        for r in flagged:
            if (r["status"], r["no"]) not in dict(pairs) and (r["status"], r["no"] - 32) not in dict(pairs):
                print(f"  {message(r['status'], r['no'])}: {', '.join(r['opts'])}  [not an n / n+32 pair]")
    if not pairs and not flagged:
        print("14-bit faders: no fine-half rows here. That doesn't show the hardware is 7-bit: "
              "authors often bind only the coarse half.")
    if any(r["status"] & 0xF0 == 0xE0 for r in ins):
        print("Pitch-bend messages (14-bit in one message; Tandem binds them as MidiKind::PitchBend):")
        for r in ins:
            if r["status"] & 0xF0 == 0xE0:
                print(f"  {message(r['status'], r['no'])}: {r['group']} {r['key']}")

    rel = [r for r in ins if any(o.lower() in OPTIONS and "relative" in OPTIONS[o.lower()] for o in r["opts"])]
    if rel:
        print("Relative controls with their encoding stated in the XML:")
        for r in rel:
            how = "; ".join(OPTIONS[o.lower()] for o in r["opts"] if o.lower() in OPTIONS)
            print(f"  {message(r['status'], r['no'])}: {r['group']} {r['key']} -> {how}")

    in_keys = {(r["status"] & 0x0F, r["no"]) for r in ins if r["status"] & 0xF0 in (0x80, 0x90)}
    note_outs = [r for r in outs if r["status"] & 0xF0 == 0x90]
    apart = [r for r in note_outs if (r["status"] & 0x0F, r["no"]) not in in_keys]
    if outs:
        print(f"Lights: {len(note_outs) - len(apart)} of {len(note_outs)} note lights sit on a button's own note "
              f"(Tandem lights a button by sending to its own note); {len(outs) - len(note_outs)} are CC or other.")
        for r in apart[:40]:
            print(f"  light with no button on its note: {message(r['status'], r['no'])} {r['group']} {r['key']}")
        vals = sorted({(r["on"], r["off"]) for r in outs})
        print("  on/off values used: " + ", ".join(f"{a}/{b}" for a, b in vals[:20]))

    script_keys = {}
    for r in ins:
        if any(o.lower() == "script-binding" for o in r["opts"]):
            script_keys.setdefault(r["key"], []).append(r)
    refs = list(js_refs)
    if not refs:
        refs = [(base + urllib.parse.quote(n) if remote else os.path.join(base, n))
                for n, _ in scripts if n and n not in COMMON_JS]
    for ref_js in refs:
        try:
            text = load(ref_js, save)[0].decode("utf-8", "replace")
        except Exception as e:  # a script that isn't next to the XML
            print(f"\n(couldn't read script {ref_js}: {e}; pass it with --js)")
            continue
        lines = text.split("\n")
        print(f"\n== SCRIPT {os.path.basename(urllib.parse.unquote(ref_js))} ({len(lines)} lines): where to read ==")
        wanted = [
            ("Jog handling (scratchEnable's 2nd argument is the ticks per platter turn the author measured)",
             r"scratchEnable|scratchTick|scratchDisable|intervalsPerRev|jog.?(resolution|sensitivity)|ticksPerRev|wheel.*(resolution|rpm)"),
            ("SysEx sent to the controller (init, 'send me your knob positions', keep-alive)",
             r"sendSysexMsg|0xF0\b|keepAlive|keep_alive|beginTimer"),
            ("Lights and colours sent from the script",
             r"sendShortMsg|ColorMapper|\bLED|[Ll]ed[A-Z_]|[Cc]olou?r\w*\s*[:=]"),
            ("Level meters", r"[Vv]u.?[Mm]eter|vu_meter|\bmeter|[Ll]evel[Mm]eter|peak_indicator"),
            ("SHIFT and layers kept in software", r"\bshift\w*\s*[:=]|isShift|shiftPressed|padMode|PadMode|deckToggle|toggleDeck"),
        ]
        for title, pat in wanted:
            found = [(i + 1, l.strip()) for i, l in enumerate(lines) if re.search(pat, l)]
            if not found:
                continue
            print(f"{title}: {len(found)} lines")
            for i, l in found[:14]:
                print(f"  {i:>5}: {l[:150]}")
            if len(found) > 14:
                print(f"         ... {len(found) - 14} more (grep the saved file)")
        if script_keys:
            print("Script-bound controls and where their function starts:")
            for key, rows in sorted(script_keys.items()):
                fn = key.split(".")[-1]
                at = next((i + 1 for i, l in enumerate(lines)
                           if re.search(rf"(\.|\b){re.escape(fn)}\s*[:=]\s*(async\s+)?function|\b{re.escape(fn)}\s*\(.*\)\s*\{{", l)), None)
                where = f"line {at}" if at else "not found here (a component object or another file?)"
                msgs = ", ".join(sorted({message(r['status'], r['no']).split(' (')[0] for r in rows})[:6])
                print(f"  {key}: {where}  <- {msgs}{' ...' if len(rows) > 6 else ''}")
    if not refs and script_keys:
        print("\n(the mapping has script-bound controls but no script was found: pass it with --js)")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("find", help="search Mixxx's mappings, pull requests and issues")
    f.add_argument("words", nargs="+")
    p = sub.add_parser("pr", help="list the controller files in a Mixxx pull request")
    p.add_argument("number", type=int)
    s = sub.add_parser("show", help="print a mapping's inputs, outputs and what they imply")
    s.add_argument("xml", help="local path, URL, or file name in Mixxx's res/controllers")
    s.add_argument("--js", action="append", default=[], help="script(s) to read instead of the ones the XML names")
    s.add_argument("--save", help="directory to keep downloaded files in (use a new, empty one)")
    a = ap.parse_args()
    if a.cmd == "find":
        find(a.words)
    elif a.cmd == "pr":
        pr(a.number)
    else:
        show(a.xml, a.js, a.save)


if __name__ == "__main__":
    try:
        main()
    except urllib.error.HTTPError as e:
        sys.exit(f"HTTP {e.code} from {e.url} (GitHub allows 60 unauthenticated requests an hour; `gh api` is the fallback)")
