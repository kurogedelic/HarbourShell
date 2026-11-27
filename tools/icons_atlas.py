#!/usr/bin/env python3
"""The built-in app icons as one atlas, so they can be edited in a single image.

  assets/icons-src/builtin-icons-black.png   the atlas (the master copy; builtin-icons.png is the old white
                                       version): 8 icons per row, 32 x 32 px each, in the order of
  assets/icons-src/builtin-icons.txt   one icon per line (reading order: left to right, top to bottom):
                                       `Name` or `Name path/to/file.png` (relative to the repository), or
                                       `-` for a cell that is not used (it is skipped; the cells after it keep
                                       their places)

Where each icon is written: the path on its line; else a built-in app's icon goes into its package
(DEFAULT_DEST below, e.g. `Paint apps/paint/icon.png`); else a shared icon,
assets/images/icons/<Name>.png (NoIcon, Folder, File, Trash, Controlpanel, and the icons for
official apps Harbour has no app of its own for: Notes, Dictionary, Mail).

  icons_atlas.py unpack    atlas -> each icon's file   (what the launcher loads)
  icons_atlas.py pack      each icon's file -> atlas   (the first time, or to refresh it)
  icons_atlas.py names     prints the name list with every destination written out (to update the .txt)
  icons_atlas.py guide [out.png]   a 4x copy with a grid and the names, to find your way around
  icons_atlas.py overlay   assets/icons-src/guide-overlay.png: same size as the atlas, transparent, with the
                           32 px cell borders (cyan) and the 30 px safe area (magenta): put it on a layer above
                           your drawing in the pixel editor, hide it before you save the atlas
  icons_atlas.py blacken [out.png]   a copy of the old white atlas with the drawing in black (default out:
                           assets/icons-src/builtin-icons-black.new.png, so the black master is never overwritten)
  (every command accepts `--atlas FILE` to read another atlas, e.g. the black one)
  icons_atlas.py extend    grows the atlas files (builtin-icons.png and builtin-icons-black.png) to the rows the name list
                           needs, keeping everything already drawn; new cells start as a copy of the NoIcon cell
  icons_atlas.py check     lists every icon's drawn size and flags the ones that leave the safe area
  icons_atlas.py parts     assets/icons-src/parts.png: the reusable bases (folder, document, blank app,
                           control panel) side by side, to copy from when drawing a new icon

Rules of thumb: docs/ICON-GUIDELINE.md.

Draw with the colours from assets/images/icons/README.md (one colour is the standard: the drawing
and its dither in white or black on transparent). Icons smaller than 32 px are centred when packed.
"""
# System icons (`--system` before the command): the dialog pictures (caution, stop, note, bomb) are a set of
# their own, also 32 x 32, in
#   assets/icons-src/system-icons.png   the atlas (8 cells per row like the app atlas; empty cells are fine)
#   assets/icons-src/system-icons.txt   one name per line, same rules as builtin-icons.txt
# and every icon is written to assets/images/system/<name>.png (src/launcher/sysicon.rs loads them; a dialog
# whose icon is missing or blank lays out without it). `--system overlay` writes system-icons-overlay.png.
#   icons_atlas.py --system unpack | names | guide | overlay | check
# `--src DIR` reads the private sources from another folder (e.g. the main checkout from a git worktree).
#
# NOTE: the artwork sources (assets/icons-src/, assets/statusbar-src/) are kept private and are not in the public
# repository; this tool only works on a machine that has them.
import os, sys
from PIL import Image, ImageDraw

SIZE, COLS = 32, 8
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
src = os.path.join(root, "assets", "icons-src")
icons_dir = os.path.join(root, "assets", "images", "icons")
atlas_path = os.path.join(src, "builtin-icons-black.png")
SKIP = "-"
if "--src" in sys.argv:  # the private sources live elsewhere (a git worktree has none of its own)
    k = sys.argv.index("--src")
    src = os.path.abspath(sys.argv[k + 1])
    atlas_path = os.path.join(src, "builtin-icons-black.png")
    del sys.argv[k:k + 2]
SYSTEM = "--system" in sys.argv
list_name, overlay_name = "builtin-icons.txt", "guide-overlay.png"
if SYSTEM:
    sys.argv.remove("--system")
    atlas_path = os.path.join(src, "system-icons.png")
    list_name, overlay_name = "system-icons.txt", "system-icons-overlay.png"
atlas_given = "--atlas" in sys.argv
if atlas_given:  # work on another atlas file (e.g. the black one)
    k = sys.argv.index("--atlas")
    atlas_path = sys.argv[k + 1]
    del sys.argv[k:k + 2]
# Built-in app icons live in the app's package (apps/<id>/icon.png, see src/launcher/packages.rs).
DEFAULT_DEST = {
    "Preferences": "apps/settings/icon.png",
    "Terminal": "apps/terminal/icon.png",
    "TextEditor": "apps/texteditor/icon.png",
    "Calculator": "apps/calculator/icon.png",
    "Clock": "apps/clock/icon.png",
    "Graph": "apps/monitor/icon.png",
    "Snake": "apps/snake/icon.png",
    "Files": "apps/files/icon.png",
    "Timer": "apps/timer/icon.png",
    "Music": "apps/music/icon.png",
    "Radio": "apps/radio/icon.png",
    "Recorder": "apps/voicememo/icon.png",
    "Images": "apps/images/icon.png",
    "Camera": "apps/camera/icon.png",
    "Paint": "apps/paint/icon.png",
    "Sheet": "apps/sheet/icon.png",
    "Browser": "apps/browser/icon.png",
    "Calendar": "apps/calendar/icon.png",
    "Interceptor": "apps/interceptor/icon.png",
    "Devices": "apps/hardware/icon.png",
    "Infoscope": "apps/infoscope/icon.png",
}
DEFAULT_DIR = "assets/images/icons"
if SYSTEM:
    DEFAULT_DEST, DEFAULT_DIR = {}, "assets/images/system"
names, dests = [], {}
for line in open(os.path.join(src, list_name)):
    if not line.strip() or line.startswith("#"):
        continue
    parts = line.split(None, 1)
    n = parts[0]
    names.append(n)
    if n == SKIP:
        continue
    rel = parts[1].strip() if len(parts) > 1 else DEFAULT_DEST.get(n, f"{DEFAULT_DIR}/{n}.png")
    dests[n] = os.path.join(root, rel)
rows = (len(names) + COLS - 1) // COLS


def dest(n):
    """The file icon `n` is written to / read from."""
    return dests.get(n) or os.path.join(root, DEFAULT_DEST.get(n, f"{DEFAULT_DIR}/{n}.png"))


def cell(i):
    return ((i % COLS) * SIZE, (i // COLS) * SIZE)


cmd = sys.argv[1] if len(sys.argv) > 1 else ""
if cmd == "pack":
    atlas = Image.new("RGBA", (COLS * SIZE, rows * SIZE), (0, 0, 0, 0))
    for i, n in enumerate(names):
        if n == SKIP or (SYSTEM and not os.path.exists(dest(n))):  # a system icon not drawn yet: empty cell
            continue
        im = Image.open(dest(n)).convert("RGBA")
        x, y = cell(i)
        atlas.paste(im, (x + (SIZE - im.width) // 2, y + (SIZE - im.height) // 2))
    atlas.save(atlas_path)
    print(f"packed {len(names)} icons into {atlas_path} ({atlas.width}x{atlas.height})")
elif cmd == "unpack":
    atlas = Image.open(atlas_path).convert("RGBA")
    if atlas.width != COLS * SIZE or atlas.height < rows * SIZE:
        sys.exit(f"the atlas is {atlas.size[0]}x{atlas.size[1]}, want {COLS * SIZE}x{rows * SIZE} (or taller) for {len(names)} cells")
    done = 0
    for i, n in enumerate(names):
        if n == SKIP:
            continue
        x, y = cell(i)
        out = dest(n)
        icon = atlas.crop((x, y, x + SIZE, y + SIZE))
        if SYSTEM and not icon.split()[3].point(lambda v: 255 if v >= 128 else 0).getbbox():
            # Not drawn yet: no file, so dialogs lay out without it (an old file is left alone).
            print(f"  {n}: empty cell, skipped")
            continue
        done += 1
        os.makedirs(os.path.dirname(out), exist_ok=True)
        icon.save(out)
    print(f"unpacked {done} icons" + (f" into {DEFAULT_DIR}" if SYSTEM else f" (apps/<id>/icon.png and {os.path.relpath(icons_dir, root)})"))
elif cmd == "names":
    for n in names:
        print(n if n == SKIP else f"{n} {os.path.relpath(dest(n), root)}")
elif cmd == "guide":
    out = sys.argv[2] if len(sys.argv) > 2 else "/tmp/icons-guide.png"
    k = 4
    atlas = Image.open(atlas_path).convert("RGBA")
    g = Image.new("RGBA", (atlas.width * k, atlas.height * k + rows * 12), (200, 200, 200, 255))
    d = ImageDraw.Draw(g)
    for i, n in enumerate(names):
        x, y = cell(i)
        tile = Image.new("RGBA", (SIZE, SIZE), (230, 182, 111, 255))
        t = atlas.crop((x, y, x + SIZE, y + SIZE))
        for px in range(SIZE):
            for py in range(SIZE):
                r, gg, b, a = t.getpixel((px, py))
                if a >= 128:
                    tile.putpixel((px, py), (38, 22, 6, 255) if (r + gg + b) // 3 < 128 and (r, gg, b) != (255, 255, 255) else (38, 22, 6, 255))
        gx, gy = x * k, y * k + (y // SIZE) * 12
        g.paste(tile.resize((SIZE * k, SIZE * k), Image.NEAREST), (gx, gy + 12))
        d.rectangle([gx, gy + 12, gx + SIZE * k - 1, gy + 12 + SIZE * k - 1], outline=(120, 120, 120, 255))
        d.text((gx + 2, gy + 1), n, fill=(0, 0, 0, 255))
    g.save(out)
    print("guide", out)
elif cmd == "overlay":
    # 3 x 5 pixel capitals for the names (a cell is only 32 px wide: 7 letters per line, two lines).
    F = {
        "A": "010101111101101", "B": "110101110101110", "C": "011100100100011", "D": "110101101101110",
        "E": "111100110100111", "F": "111100110100100", "G": "011100101101011", "H": "101101111101101",
        "I": "111010010010111", "J": "001001001101010", "K": "101101110101101", "L": "100100100100111",
        "M": "101111111101101", "N": "110101101101101", "O": "010101101101010", "P": "110101110100100",
        "Q": "010101101111011", "R": "110101110101101", "S": "011100010001110", "T": "111010010010010",
        "U": "101101101101111", "V": "101101101101010", "W": "101101111111101", "X": "101101010101101",
        "Y": "101101010010010", "Z": "111001010100111",
    }
    ov = Image.new("RGBA", (COLS * SIZE, rows * SIZE), (0, 0, 0, 0))
    d = ImageDraw.Draw(ov)
    label = (0, 90, 200, 255)

    def word(x, y, text):
        for ch in text.upper():
            for k, bit in enumerate(F.get(ch, "0" * 15)):
                if bit == "1":
                    ov.putpixel((x + k % 3, y + k // 3), label)
            x += 4

    for i in range(COLS * rows):
        x, y = cell(i)
        d.rectangle([x, y, x + SIZE - 1, y + SIZE - 1], outline=(0, 200, 255, 255))
        d.rectangle([x + 1, y + 1, x + SIZE - 2, y + SIZE - 2], outline=(255, 0, 200, 255))
        if i < len(names) and names[i] != SKIP:
            n = names[i]
            lines = [n] if len(n) <= 7 else [n[: (len(n) + 1) // 2], n[(len(n) + 1) // 2:]]
            for k, ln in enumerate(lines):
                word(x + 2, y + 2 + k * 6, ln)
    ov.save(os.path.join(src, overlay_name))
    print(f"wrote {overlay_name} (with the icon names)")
elif cmd == "check":
    atlas = Image.open(atlas_path).convert("RGBA")
    bad = 0
    for i, n in enumerate(names):
        if n == SKIP:
            continue
        x, y = cell(i)
        a = atlas.crop((x, y, x + SIZE, y + SIZE)).split()[3].point(lambda v: 255 if v >= 128 else 0)
        bb = a.getbbox()
        if not bb:
            print(f"{n:14s} (empty)")
            continue
        w, h = bb[2] - bb[0], bb[3] - bb[1]
        out = bb[0] < 1 or bb[1] < 1 or bb[2] > SIZE - 1 or bb[3] > SIZE - 1
        bad += out
        print(f"{n:14s} {w:2d} x {h:2d} at ({bb[0]:2d},{bb[1]:2d})" + ("   <- touches the 1 px margin" if out else ""))
    print(f"{bad} icon(s) leave the 30 x 30 safe area")
elif cmd == "extend":
    want = (COLS * SIZE, rows * SIZE)
    for f in (os.path.join(src, "builtin-icons.png"), os.path.join(src, "builtin-icons-black.png")):
        if not os.path.exists(f):
            continue
        old = Image.open(f).convert("RGBA")
        if old.size == want:
            print("ok", os.path.basename(f), old.size)
            continue
        new = Image.new("RGBA", want, (0, 0, 0, 0))
        new.paste(old, (0, 0))
        base = names.index("NoIcon") if "NoIcon" in names else 0
        bx, by = cell(base)
        template = old.crop((bx, by, bx + SIZE, by + SIZE))
        first_new = (old.height // SIZE) * COLS
        for i in range(first_new, len(names)):
            x, y = cell(i)
            new.paste(template, (x, y))
        new.save(f)
        print("extended", os.path.basename(f), old.size, "->", want)
elif cmd == "blacken":
    # Reads the old white atlas (unless --atlas) and never overwrites the black master by default.
    out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(src, "builtin-icons-black.new.png")
    atlas = Image.open(atlas_path if atlas_given else os.path.join(src, "builtin-icons.png")).convert("RGBA")
    for x in range(atlas.width):
        for y in range(atlas.height):
            r, g, b, a = atlas.getpixel((x, y))
            if a >= 128 and (r + g + b) // 3 > 192:
                atlas.putpixel((x, y), (0, 0, 0, 255))
    atlas.save(out)
    print("wrote", out)
elif cmd == "parts":
    base = ["Folder", "File", "NoIcon", "Controlpanel"]
    labels = ["folder", "document", "blank app", "control panel"]
    parts = Image.new("RGBA", (len(base) * SIZE, SIZE), (0, 0, 0, 0))
    for i, n in enumerate(base):
        parts.paste(Image.open(dest(n)).convert("RGBA"), (i * SIZE, 0))
    parts.save(os.path.join(src, "parts.png"))
    print("wrote parts.png:", ", ".join(labels))
else:
    sys.exit(__doc__)
