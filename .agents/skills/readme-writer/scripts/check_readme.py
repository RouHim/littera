#!/usr/bin/env python3
"""Static checks for a README written with the readme-writer skill.

Checks prose budget, section order, fences, local links and images,
placeholders, em dashes, and the GUI screenshot. It never executes the
project and never writes to the repository.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

FENCE = re.compile(r"^\s*(`{3,}|~{3,})")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*$")
IMAGE = re.compile(r"!\[([^\]]*)\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
LINK = re.compile(r"(?<!\!)\[([^\]]+)\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
INLINE_CODE = re.compile(r"`[^`\n]*`")
URL = re.compile(r"https?://[^\s)>\]]+")
HTML_COMMENT = re.compile(r"<!--.*?-->")
HTML_TAG = re.compile(r"<[^>]+>")
WORD = re.compile(r"[A-Za-z0-9][A-Za-z0-9'_-]*")
TABLE_RULE = re.compile(r"^\s*\|?[\s:|-]+\|?\s*$")

PLACEHOLDERS = ("todo", "tbd", "fixme", "placeholder", "screenshot here", "lorem ipsum")
IMAGE_SUFFIXES = {".png", ".webp", ".jpg", ".jpeg", ".gif", ".svg"}

SECTION_KEYS = {
    "requirements": re.compile(r"requirements|prerequisites|what you need|before you start", re.I),
    "install": re.compile(r"install|installation|setup|getting started", re.I),
    "usage": re.compile(r"usage|how to use|quick start|use", re.I),
    "troubleshooting": re.compile(r"troubleshooting|help|support|diagnostics|known issues", re.I),
    "license": re.compile(r"license|licence", re.I),
}


def readme_words(lines: list[str], fences: set[int]) -> list[str]:
    """Return prose words, excluding headings, code, URLs and image markup."""
    words: list[str] = []
    for number, line in enumerate(lines, 1):
        if number in fences or HEADING.match(line):
            continue
        if TABLE_RULE.match(line) and "-" in line:
            continue
        text = HTML_COMMENT.sub("", line)
        text = IMAGE.sub("", text)
        text = LINK.sub(r"\1", text)
        text = INLINE_CODE.sub("", text)
        text = HTML_TAG.sub("", text)
        text = URL.sub("", text)
        words.extend(WORD.findall(text))
    return words


def png_width(path: Path) -> int | None:
    try:
        data = path.read_bytes()[:24]
    except OSError:
        return None
    if len(data) >= 24 and data[:8] == b"\x89PNG\r\n\x1a\n" and data[12:16] == b"IHDR":
        return int.from_bytes(data[16:20], "big")
    return None


def check(readme: Path, kind: str) -> dict:
    checks: list[dict] = []

    def add(name: str, passed: bool, evidence: str) -> None:
        checks.append({"name": name, "passed": passed, "evidence": evidence})

    if not readme.is_file():
        add("readme_exists", False, f"missing: {readme}")
        return {"file": str(readme), "kind": kind, "word_count": 0, "checks": checks, "passed": False}

    lines = readme.read_text(encoding="utf-8", errors="replace").splitlines()
    fences: set[int] = set()
    in_fence = False
    fence_count = 0
    headings: list[tuple[int, int, str]] = []
    images: list[tuple[int, str, str]] = []
    links: list[tuple[int, str]] = []

    for number, line in enumerate(lines, 1):
        if FENCE.match(line):
            in_fence = not in_fence
            fence_count += 1
            fences.add(number)
            continue
        if in_fence:
            fences.add(number)
            continue
        match = HEADING.match(line)
        if match:
            headings.append((number, len(match.group(1)), match.group(2).strip()))
            continue
        for alt, target in IMAGE.findall(line):
            images.append((number, alt, target))
        for _, target in LINK.findall(line):
            links.append((number, target))

    add("fences", fence_count % 2 == 0, f"{fence_count} fence markers")

    h1 = [item for item in headings if item[1] == 1]
    add("title", bool(h1), f"H1 at line {h1[0][0]}" if h1 else "no H1 title")

    pitch_line = None
    if h1:
        title_line = h1[0][0]
        for number in range(title_line + 1, len(lines) + 1):
            if number in fences or HEADING.match(lines[number - 1]):
                continue
            stripped = IMAGE.sub("", INLINE_CODE.sub("", lines[number - 1])).strip()
            if len(WORD.findall(stripped)) >= 3:
                pitch_line = number
                break
    add("pitch", pitch_line is not None, f"pitch at line {pitch_line}" if pitch_line else "no pitch after the title")

    found: dict[str, int] = {}
    for key, pattern in SECTION_KEYS.items():
        for number, level, text in headings:
            if level >= 2 and pattern.search(text):
                found[key] = number
                break
    missing = [key for key in SECTION_KEYS if key not in found]
    add("sections", not missing, f"missing: {', '.join(missing)}" if missing else "all required headings present")

    order = [found[key] for key in ("requirements", "install", "usage", "troubleshooting", "license") if key in found]
    add("section_order", order == sorted(order), f"heading lines {order}")
    license_line = found.get("license")
    after = [number for number, level, _ in headings if level >= 2 and license_line is not None and number > license_line]
    add("license_last", license_line is not None and not after, f"headings after license: {after}")

    words = readme_words(lines, fences)
    add("word_count", len(words) < 500, f"{len(words)} words (limit 499)")

    image_failures: list[str] = []
    for number, alt, target in images:
        if not alt.strip():
            image_failures.append(f"line {number}: empty alt text")
        if re.match(r"https?://", target, re.I):
            image_failures.append(f"line {number}: remote image {target}")
            continue
        path = (readme.parent / target.split("#", 1)[0].split("?", 1)[0]).resolve()
        if not path.is_file():
            image_failures.append(f"line {number}: missing {target}")
            continue
        if path.suffix.lower() not in IMAGE_SUFFIXES:
            image_failures.append(f"line {number}: unsupported image type {path.suffix}")
        try:
            if path.stat().st_size > 500 * 1024:
                image_failures.append(f"line {number}: {path.name} over 500 KB")
        except OSError:
            image_failures.append(f"line {number}: cannot read {path.name}")
        width = png_width(path)
        if width is not None and width > 1600:
            image_failures.append(f"line {number}: {path.name} is {width}px wide")
    add("images", not image_failures, "; ".join(image_failures) if image_failures else f"{len(images)} local image(s)")

    if kind == "gui":
        requirements_line = found.get("requirements") or found.get("install") or len(lines) + 1
        screenshots = [
            (number, alt, target)
            for number, alt, target in images
            if pitch_line is not None
            and number > pitch_line
            and number < requirements_line
            and "banner" not in alt.lower()
            and "banner" not in target.lower()
        ]
        add("gui_screenshot", bool(screenshots), f"line {screenshots[0][0]}" if screenshots else "no screenshot between pitch and requirements")
    else:
        add("gui_screenshot", True, f"not required for kind={kind}")

    link_failures: list[str] = []
    for number, target in links:
        lowered = target.lower()
        if lowered.startswith(("#", "http://", "https://", "mailto:", "tel:")):
            continue
        path = (readme.parent / target.split("#", 1)[0].split("?", 1)[0]).resolve()
        if not path.exists():
            link_failures.append(f"line {number}: missing {target}")
    add("links", not link_failures, "; ".join(link_failures) if link_failures else f"{len(links)} link(s)")

    haystack = "\n".join(
        [text for _, _, text in headings]
        + [line for number, line in enumerate(lines, 1) if number not in fences and not HEADING.match(line)]
    ).lower()
    found_placeholders = [word for word in PLACEHOLDERS if word in haystack]
    add("placeholders", not found_placeholders, f"found: {', '.join(found_placeholders)}" if found_placeholders else "none")

    prose = "\n".join(
        line
        for number, line in enumerate(lines, 1)
        if number not in fences and not HEADING.match(line)
    )
    add("em_dashes", "—" not in prose, f"{prose.count('—')} em dash(es)" if "—" in prose else "none")

    return {
        "file": str(readme),
        "kind": kind,
        "word_count": len(words),
        "checks": checks,
        "passed": all(check["passed"] for check in checks),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("readme", type=Path)
    parser.add_argument("--kind", choices=("gui", "cli", "library", "none"), default="none")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    result = check(args.readme, args.kind)
    if args.json:
        print(json.dumps(result, indent=2))
    else:
        for entry in result["checks"]:
            state = "PASS" if entry["passed"] else "FAIL"
            print(f"{state} {entry['name']}: {entry['evidence']}")
        print(f"\n{result['word_count']} prose words")
        print("RESULT:", "PASS" if result["passed"] else "FAIL")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
