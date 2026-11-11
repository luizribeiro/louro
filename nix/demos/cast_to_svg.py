"""Renders an asciicast as an animated SVG, which GitHub plays inline. Copied
from snowglobe's nix/demos/cast_to_svg.py.

Usage: cast-to-svg CAST SVG

Uses the theme in the cast's header. Like the asciinema player, pads the
terminal and keeps pauses to at most 3s. Holds the last frame for 5s before
looping. Prefers the fonts of the terminal look file at $TERMINAL_LOOK for
viewers who have them. Runs svg-term from $SVG_TERM, or from PATH.
"""

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

MAX_PAUSE = 3.0
# The SVGs loop, so the result needs time on screen before the next run
END_HOLD = 5.0
# The asciinema player's 0.75em border, at svg-term's 16.7px font
PADDING = 12


def capped(lines: list[str]) -> list[str]:
    out, previous, skipped = [lines[0]], 0.0, 0.0
    for line in lines[1:]:
        time, kind, data = json.loads(line)
        skipped += max(0.0, time - previous - MAX_PAUSE)
        previous = time
        out.append(json.dumps([round(time - skipped, 6), kind, data]))
    return out


def held(lines: list[str]) -> list[str]:
    end = json.loads(lines[-1])[0]
    return lines + [json.dumps([round(end + END_HOLD, 6), "o", ""])]


def xresources(theme: dict) -> str:
    colors = {"foreground": theme["fg"], "background": theme["bg"],
              "colorBD": theme["fg"], "colorUL": theme["fg"],
              "cursorColor": theme["fg"]}
    for i, color in enumerate(theme["palette"].split(":")):
        colors[f"color{i}"] = color
    return "".join(f"*.{name}: {color}\n" for name, color in colors.items())


def main() -> None:
    cast, svg = Path(sys.argv[1]), Path(sys.argv[2])
    lines = cast.read_text(encoding="utf-8").splitlines()
    if len(lines) < 2:
        sys.exit(f"cast-to-svg: {cast} has no events")
    theme = json.loads(lines[0])["theme"]
    with tempfile.TemporaryDirectory() as tmp:
        short, profile = Path(tmp) / "cast", Path(tmp) / "theme"
        rendered = Path(tmp) / "svg"
        short.write_text("\n".join(held(capped(lines))) + "\n",
                         encoding="utf-8")
        profile.write_text(xresources(theme), encoding="utf-8")
        subprocess.run([os.environ.get("SVG_TERM", "svg-term"),
                        "--in", str(short), "--out", str(rendered),
                        "--padding", str(PADDING), "--term", "xresources",
                        "--profile", str(profile)], check=True)
        text = rendered.read_text(encoding="utf-8")
    attribute = 'font-family="'
    if attribute not in text:
        sys.exit(f"cast-to-svg: no {attribute} in svg-term's output")
    look = json.loads(Path(os.environ["TERMINAL_LOOK"]).read_text())
    # Before svg-term's own list, so viewers without these still get those
    fonts = "".join(f"'{font}'," for font in look["fonts"])
    svg.write_text(text.replace(attribute, attribute + fonts),
                   encoding="utf-8")


if __name__ == "__main__":
    main()
