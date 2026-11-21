#!/usr/bin/env python3
"""Print a saved web page as plain text, without scripts, styles or menus.

    python3 -I html_text.py page.html            # the whole page
    python3 -I html_text.py page.html | grep -n -i -B2 -A6 "shift"

For manuals, forum threads and archived support pages, so they can be
searched instead of read whole. Table cells are joined with " | " and each
row, heading, paragraph and list item starts a new line. Links keep their
address in brackets, since a support page's value is often the PDF it links.
Standard library only.
"""
import html.parser
import re
import sys

SKIP = {"script", "style", "noscript", "svg", "nav", "footer", "head", "form", "iframe"}
BREAK = {"p", "div", "br", "tr", "li", "h1", "h2", "h3", "h4", "h5", "h6", "section", "article", "table", "ul", "ol", "dt", "dd", "pre"}


class Text(html.parser.HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.out, self.skip, self.href = [], 0, None

    def handle_starttag(self, tag, attrs):
        if tag in SKIP:
            self.skip += 1
        elif tag in BREAK:
            self.out.append("\n")
        elif tag in ("td", "th"):
            self.out.append(" | ")
        elif tag == "a":
            self.href = dict(attrs).get("href")

    def handle_endtag(self, tag):
        if tag in SKIP:
            self.skip = max(0, self.skip - 1)
        elif tag == "a" and self.href and not self.skip:
            if not self.href.startswith(("#", "javascript:")):
                self.out.append(f" [{self.href}]")
            self.href = None
        elif tag in BREAK:
            self.out.append("\n")

    def handle_data(self, data):
        if not self.skip:
            self.out.append(data)


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    with open(sys.argv[1], encoding="utf-8", errors="replace") as f:
        p = Text()
        p.feed(f.read())
    try:
        for line in "".join(p.out).split("\n"):
            line = re.sub(r"[ \t\r\f\v]+", " ", line).strip(" |")
            if line:
                print(line)
    except BrokenPipeError:  # piped into head or grep -m
        sys.stderr.close()


if __name__ == "__main__":
    main()
