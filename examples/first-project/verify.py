#!/usr/bin/env python3
"""Check the actual standalone page, optionally against the requested heading."""
import argparse
import re
from html.parser import HTMLParser
from pathlib import Path


class Page(HTMLParser):
    def __init__(self):
        super().__init__()
        self.headings = []
        self.in_heading = False
        self.has_viewport = False
        self.has_main = False
        self.unlabelled_images = 0

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if tag == "h1":
            self.in_heading = True
            self.headings.append("")
        if tag == "meta" and attributes.get("name") == "viewport":
            self.has_viewport = "width=device-width" in (attributes.get("content") or "")
        if tag == "main":
            self.has_main = True
        if tag == "img" and not (attributes.get("alt") or "").strip():
            self.unlabelled_images += 1
        if tag == "svg" and attributes.get("role") == "img" and not (attributes.get("aria-label") or "").strip():
            self.unlabelled_images += 1

    def handle_endtag(self, tag):
        if tag == "h1":
            self.in_heading = False

    def handle_data(self, data):
        if self.in_heading:
            self.headings[-1] += data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--title", help="Expected h1 text, with HTML whitespace normalized")
    args = parser.parse_args()
    page = Page()
    page.feed(Path(__file__).with_name("index.html").read_text(encoding="utf-8"))
    headings = [re.sub(r"[ \t\n\r\f]+", " ", text).strip(" \t\n\r\f") for text in page.headings]
    failures = []
    if len(headings) != 1 or not headings[0]:
        failures.append("the page must have exactly one nonempty h1")
    if args.title is not None and headings != [args.title]:
        failures.append(f"expected h1 {args.title!r}; found {headings!r}")
    if not page.has_viewport or not page.has_main:
        failures.append("the viewport declaration and main landmark are required")
    if page.unlabelled_images:
        failures.append("all meaningful images must have accessible labels")
    if failures:
        raise SystemExit("\n".join(failures))
    print("Page structure and image-label checks passed.")
    print("These checks do not inspect CSS, scripts or network requests, or establish visual layout; inspect the page in a browser.")


if __name__ == "__main__":
    main()
