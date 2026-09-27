"""Draw the tray icons, one per status: python3 generate.py (needs rsvg-convert).

Two screens side by side, the filled one holding the pointer; a slash for
paused, an incoming arrow for controlled, a padlock for locked. Shapes, not
colors, tell the statuses apart: macOS tints its monochrome templates.
"""

import pathlib
import subprocess

HERE = pathlib.Path(__file__).parent

# The accent color (Windows); templates are black
ACCENT = "#3ccfbe"

# Status -> (pointer away from this device, extra marks)
STATUSES = {
    "idle": (False, ""),
    "controlling": (True, ""),
    "controlled": (
        False,
        '<path d="M11.8 2.4H6.4M8 .8 6.4 2.4 8 4" stroke="{c}" stroke-width="1.3" '
        'fill="none" stroke-linecap="round" stroke-linejoin="round"/>',
    ),
    "paused": (False, '<path d="M2 14.5 16 1.5" stroke="{c}" stroke-width="1.6" stroke-linecap="round"/>'),
    "locked": (
        True,
        '<rect x="12" y="1" width="4" height="3" rx=".7" fill="{c}"/>'
        '<path d="M12.9 1.2v-.3a1.1 1.1 0 0 1 2.2 0v.3" stroke="{c}" stroke-width=".9" fill="none"/>',
    ),
}


def svg(away: bool, extra: str, color: str) -> str:
    """One icon, square (the glyph is 18 x 16)"""
    fill = lambda filled: color if filled else "none"  # noqa: E731
    # The slash cuts a gap through the screens so it reads over a filled one
    mask = ""
    screens_mask = ""
    if "M2 14.5" in extra:
        mask = (
            '<mask id="gap"><rect x="0" y="-1" width="18" height="18" fill="#fff"/>'
            '<path d="M2 14.5 16 1.5" stroke="#000" stroke-width="3.6" stroke-linecap="round"/></mask>'
        )
        screens_mask = ' mask="url(#gap)"'
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 -1 18 18">'
        f"<defs>{mask}</defs>"
        f"<g{screens_mask}>"
        f'<rect x="1" y="5" width="7" height="6" rx="1.4" stroke="{color}" stroke-width="1.4" fill="{fill(not away)}"/>'
        f'<rect x="10" y="5" width="7" height="6" rx="1.4" stroke="{color}" stroke-width="1.4" fill="{fill(away)}"/>'
        "</g>"
        f"{extra.format(c=color)}"
        "</svg>"
    )


def render(markup: str, out: pathlib.Path, size: int) -> None:
    """Rasterize with rsvg-convert"""
    subprocess.run(
        ["rsvg-convert", "-w", str(size), "-h", str(size), "-o", str(out)],
        input=markup.encode(),
        check=True,
    )


def main() -> None:
    """Every status, template (macOS, 18 pt at 4x) and colored (Windows)"""
    for name, (away, extra) in STATUSES.items():
        render(svg(away, extra, "#000"), HERE / f"{name}-template.png", 72)
        render(svg(away, extra, ACCENT), HERE / f"{name}.png", 64)


if __name__ == "__main__":
    main()
