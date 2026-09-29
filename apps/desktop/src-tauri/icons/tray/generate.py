"""Draw the tray icons, one per status: python3 generate.py (needs rsvg-convert).

The brand's capsule mouse under two signal waves, on its 44-unit grid. Shapes,
not colors, tell the statuses apart, so the macOS monochrome templates read
too: hollow is here, filled is the pointer on another device, a slash is
paused, an arrow pointing in is being driven, a padlock is locked (on a
hollow mouse here, a filled one on another device). Idle, controlling and
paused are the brand's own drawings; the others cut a badge into the mouse's
lower right.
"""

import pathlib
import subprocess

HERE = pathlib.Path(__file__).parent

# Signal waves, mouse body and scroll wheel (the mouse is centered on x = 22)
WAVES = '<path d="M18.45 11.23A4.5 4.5 0 0 1 25.55 11.23"/><path d="M14.51 8.15A9.5 9.5 0 0 1 29.49 8.15"/>'
MOUSE = '<rect x="15.5" y="18" width="13" height="21" rx="6.5"/>'
WHEEL = "M22 23.4v3.2"
# The pause slash, corner to corner
SLASH = "M8 6.5L36 38.5"

# Badges as (drawing, knockout): {c} is the ink; the knockout is the badge
# grown by a gap, cleared out of the mouse so the badge reads over it
ARROW_PATH = "M37 35H30.5M33.5 32 30.5 35 33.5 38"
ARROW = (
    f'<path d="{ARROW_PATH}" fill="none" stroke="{{c}}" stroke-width="2.4" '
    'stroke-linecap="round" stroke-linejoin="round"/>',
    f'<path d="{ARROW_PATH}" fill="none" stroke="#000" stroke-width="5.6" '
    'stroke-linecap="round" stroke-linejoin="round"/>',
)
LOCK_BODY = 'x="28" y="31.5" width="9" height="8" rx="1.6"'
LOCK_SHACKLE = "M30.2 31.7v-2.3a2.3 2.3 0 0 1 4.6 0v2.3"
LOCK = (
    f'<rect {LOCK_BODY} fill="{{c}}"/>'
    f'<path d="{LOCK_SHACKLE}" fill="none" stroke="{{c}}" stroke-width="2.2"/>',
    f'<rect {LOCK_BODY} fill="#000" stroke="#000" stroke-width="3.2"/>'
    f'<path d="{LOCK_SHACKLE}" fill="none" stroke="#000" stroke-width="5.4"/>',
)
NO_BADGE = ("", "")

# The Windows backdrops: the brand's Signal, and a gray for paused
SIGNAL = "#12B5A2"
GRAY = "#7C919A"

# Status -> (filled mouse, scroll wheel, slash, badge, Windows backdrop)
STATUSES = {
    "idle": (False, True, False, NO_BADGE, SIGNAL),
    "controlling": (True, True, False, NO_BADGE, SIGNAL),
    "controlled": (False, True, False, ARROW, SIGNAL),
    "paused": (False, False, True, NO_BADGE, GRAY),
    "locked": (True, True, False, LOCK, SIGNAL),
    "locked-home": (False, True, False, LOCK, SIGNAL),
}

# macOS: tray-icon draws the template 18 pt tall whatever its size, so the
# template is cropped to the glyphs (waves top to mouse bottom) and drawn at
# 2x; one width for every status, centered on the mouse, so the menu bar item
# keeps its width as the status changes
TEMPLATE_PX = (32, 36)
TEMPLATE_TOP, TEMPLATE_BOTTOM = 2.75, 40.75

# Windows: the whole 44-unit tile
COLORED_PX = (64, 64)


def template_box() -> tuple[float, float, float, float]:
    """The template's view box, in grid units, for its pixel size"""
    height = TEMPLATE_BOTTOM - TEMPLATE_TOP
    width = height * TEMPLATE_PX[0] / TEMPLATE_PX[1]
    return (22 - width / 2, TEMPLATE_TOP, width, height)


def svg(status: tuple, ink: str, backdrop: str | None, box: tuple[float, float, float, float]) -> str:
    """One icon for `status`, cropped to `box`; `backdrop` is a tile behind the glyph"""
    filled, wheel, slash, (badge, knockout), _ = status
    # Shapes cleared out of the mouse: the wheel of a filled one, a gap
    # around the slash so it reads over the mouse, and the badge's gap
    cuts = knockout
    if filled and wheel:
        cuts += f'<path d="{WHEEL}" stroke="#000" stroke-width="3" stroke-linecap="round"/>'
    if slash:
        cuts += f'<path d="{SLASH}" stroke="#000" stroke-width="6.4" stroke-linecap="round"/>'
    mask = (
        '<mask id="cut" maskUnits="userSpaceOnUse" x="0" y="0" width="44" height="44">'
        f'<rect width="44" height="44" fill="#fff"/>{cuts}</mask>'
    )
    body = MOUSE.replace("/>", f' fill="{ink}"/>') if filled else MOUSE
    if wheel and not filled:
        body += f'<path d="{WHEEL}"/>'
    glyph = (
        f'<g mask="url(#cut)" fill="none" stroke="{ink}" stroke-width="3" '
        f'stroke-linecap="round" stroke-linejoin="round">{WAVES}{body}</g>'
    )
    if slash:
        glyph += f'<path d="{SLASH}" stroke="{ink}" stroke-width="3" stroke-linecap="round" fill="none"/>'
    ground = f'<rect width="44" height="44" rx="10" fill="{backdrop}"/>' if backdrop else ""
    view = " ".join(f"{v:g}" for v in box)
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view}">'
        f"{ground}<defs>{mask}</defs>{glyph}{badge.format(c=ink)}</svg>"
    )


def render(markup: str, out: pathlib.Path, size: tuple[int, int]) -> None:
    """Rasterize with rsvg-convert"""
    subprocess.run(
        ["rsvg-convert", "-w", str(size[0]), "-h", str(size[1]), "-o", str(out)],
        input=markup.encode(),
        check=True,
    )


def main() -> None:
    """Every status, template (macOS) and colored (Windows)"""
    for name, status in STATUSES.items():
        render(svg(status, "#000", None, template_box()), HERE / f"{name}-template.png", TEMPLATE_PX)
        render(svg(status, "#FFF", status[4], (0, 0, 44, 44)), HERE / f"{name}.png", COLORED_PX)


if __name__ == "__main__":
    main()
