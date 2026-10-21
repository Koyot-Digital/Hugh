#!/usr/bin/env python3
"""Download the Lucide artwork behind Hugh's icon slots.

Hugh never prints an emoji of its own. Every glyph on a case card comes from an
icon slot that an operator fills with one of their server's custom emoji, and
this script fetches the matching Lucide icon for each slot so there is
something to upload.

The slot list is read straight out of `src/config.rs`, so the assets cannot
drift from the slots Hugh actually renders.

    python scripts/icons.py                 # download SVGs and render PNGs
    python scripts/icons.py --stroke "#B5BAC1" --size 160
    python scripts/icons.py --svg-only

Rendering uses whichever rasteriser is on the machine: cairosvg, rsvg-convert,
Inkscape, ImageMagick, or Node with sharp. With none of them installed the
SVGs are still written, and any converter can finish the job.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONFIG = ROOT / "src" / "config.rs"
ICON_DIR = ROOT / "assets" / "icons"

# Pinned so a refresh is a deliberate, reviewable change rather than whatever
# Lucide published this morning.
LUCIDE_VERSION = "1.60.0"
SOURCE = "https://unpkg.com/lucide-static@{version}/icons/{icon}.svg"

ICONS_PATTERN = re.compile(
    r"pub const ICONS: \[\(&str, &str\); \d+\] = \[(.*?)\n\];", re.DOTALL
)
ENTRY_PATTERN = re.compile(r'\("([a-z_]+)",\s*"([a-z0-9-]+)"\)')


def slots() -> list[tuple[str, str]]:
    """Every (slot, lucide icon) pair Hugh declares."""
    source = CONFIG.read_text(encoding="utf-8")
    block = ICONS_PATTERN.search(source)
    if not block:
        raise SystemExit(f"could not find the ICONS table in {CONFIG}")
    found = ENTRY_PATTERN.findall(block.group(1))
    if not found:
        raise SystemExit(f"the ICONS table in {CONFIG} is empty")
    return found


def download(icon: str) -> str:
    url = SOURCE.format(version=LUCIDE_VERSION, icon=icon)
    request = urllib.request.Request(url, headers={"User-Agent": "hugh-icons"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.read().decode("utf-8")
    except urllib.error.HTTPError as error:
        if error.code == 404:
            raise SystemExit(
                f"Lucide {LUCIDE_VERSION} has no icon named {icon!r}; "
                "check the name against https://lucide.dev/icons"
            ) from error
        raise SystemExit(f"failed to download {url}: {error}") from error
    except urllib.error.URLError as error:
        raise SystemExit(f"failed to download {url}: {error.reason}") from error


def recolour(svg: str, stroke: str, width: float) -> str:
    """Bakes a colour and stroke weight in.

    Lucide ships `stroke="currentColor"`, which resolves to black outside a
    web page, and a 2px weight that thins out at emoji size.
    """
    svg = svg.replace('stroke="currentColor"', f'stroke="{stroke}"')
    svg = re.sub(r'stroke-width="[\d.]+"', f'stroke-width="{width}"', svg)
    return svg


def rasteriser() -> tuple[str, object] | None:
    """The first available way to turn an SVG into a PNG."""
    try:
        import cairosvg  # noqa: F401  (probed, imported again on use)

        return ("cairosvg", None)
    except ImportError:
        pass
    for command, kind in (
        ("rsvg-convert", "rsvg"),
        ("inkscape", "inkscape"),
        ("magick", "magick"),
    ):
        found = shutil.which(command)
        if found:
            return (kind, found)
    node = shutil.which("node")
    if node and _has_sharp(node):
        return ("sharp", node)
    return None


def _has_sharp(node: str) -> bool:
    probe = subprocess.run(
        [node, "-e", "require.resolve('sharp')"],
        capture_output=True,
        check=False,
        cwd=ROOT,
    )
    return probe.returncode == 0


def render(kind: str, tool: object, svg: Path, png: Path, size: int) -> None:
    if kind == "cairosvg":
        import cairosvg

        cairosvg.svg2png(
            url=str(svg), write_to=str(png), output_width=size, output_height=size
        )
        return
    if kind == "rsvg":
        command = [
            str(tool), "--width", str(size), "--height", str(size),
            "--output", str(png), str(svg),
        ]
    elif kind == "inkscape":
        command = [
            str(tool), str(svg), "--export-type=png",
            f"--export-filename={png}", f"--export-width={size}",
            f"--export-height={size}",
        ]
    elif kind == "magick":
        command = [
            str(tool), "-background", "none", "-density", "384",
            str(svg), "-resize", f"{size}x{size}", str(png),
        ]
    elif kind == "sharp":
        script = (
            "const sharp=require('sharp');"
            "const [i,o,s]=process.argv.slice(1);"
            "sharp(i,{density:384}).resize(Number(s),Number(s),"
            "{fit:'contain',background:{r:0,g:0,b:0,alpha:0}})"
            ".png().toFile(o).catch(e=>{console.error(e.message);process.exit(1);});"
        )
        command = [str(tool), "-e", script, str(svg), str(png), str(size)]
    else:
        raise SystemExit(f"unknown rasteriser {kind!r}")
    result = subprocess.run(command, capture_output=True, check=False, cwd=ROOT)
    if result.returncode != 0:
        raise SystemExit(
            f"{kind} failed on {svg.name}: {result.stderr.decode(errors='replace').strip()}"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--stroke",
        default="#E3E5E8",
        help="stroke colour baked into the artwork (default: Discord's light text)",
    )
    parser.add_argument(
        "--stroke-width",
        type=float,
        default=2.25,
        help="stroke weight at Lucide's 24px grid (default: 2.25)",
    )
    parser.add_argument(
        "--size", type=int, default=128, help="PNG edge in pixels (default: 128)"
    )
    parser.add_argument(
        "--svg-only", action="store_true", help="skip PNG rendering"
    )
    arguments = parser.parse_args()

    table = slots()
    svg_dir = ICON_DIR / "svg"
    png_dir = ICON_DIR / "png"
    svg_dir.mkdir(parents=True, exist_ok=True)

    converter = None if arguments.svg_only else rasteriser()
    if converter:
        png_dir.mkdir(parents=True, exist_ok=True)
    elif not arguments.svg_only:
        print(
            "No SVG rasteriser found, writing SVGs only. Install one of "
            "cairosvg, rsvg-convert, Inkscape or ImageMagick, or run "
            "`npm install sharp`, then run this again.",
            file=sys.stderr,
        )

    manifest = []
    for slot, icon in table:
        svg = recolour(download(icon), arguments.stroke, arguments.stroke_width)
        # Named after the slot, so uploading the file and naming the emoji
        # after it is all the operator has to do.
        svg_path = svg_dir / f"{slot}.svg"
        svg_path.write_text(svg, encoding="utf-8", newline="\n")
        entry = {"slot": slot, "lucide": icon, "svg": f"svg/{slot}.svg"}
        if converter:
            png_path = png_dir / f"{slot}.png"
            with tempfile.TemporaryDirectory() as scratch:
                staged = Path(scratch) / f"{slot}.svg"
                staged.write_text(svg, encoding="utf-8", newline="\n")
                render(converter[0], converter[1], staged, png_path, arguments.size)
            entry["png"] = f"png/{slot}.png"
        manifest.append(entry)
        print(f"{slot:<12} {icon}")

    (ICON_DIR / "icons.json").write_text(
        json.dumps(
            {
                "lucide_version": LUCIDE_VERSION,
                "license": "ISC, https://github.com/lucide-icons/lucide/blob/main/LICENSE",
                "stroke": arguments.stroke,
                "stroke_width": arguments.stroke_width,
                "size": arguments.size,
                "icons": manifest,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
        newline="\n",
    )
    print(f"\n{len(manifest)} icons written to {ICON_DIR}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
