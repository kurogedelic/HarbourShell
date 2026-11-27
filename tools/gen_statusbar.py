#!/usr/bin/env python3
"""Build the status-bar icon atlas from hand-drawn drafts.

Drafts live in assets/statusbar-src/<name>.png. Every icon is 12 x 12. An icon with several
states ("animated": Wi-Fi, speaker, antenna, battery) is one horizontal strip, 12 px per frame
in the order listed in ICONS below (e.g. wifi.png is 60 x 12: disabled, 0, 1, 2, 3).

Two colours plus transparency, read from the draft's pixels:
  alpha < 128                 -> transparent
  dark  (luminance < 64)      -> foreground  (the theme's ink)
  anything lighter, opaque    -> tint1       (the grey: the theme's muted tone)

Output:
  assets/images/statusbar/atlas_fg.png    white mask of the foreground pixels
  assets/images/statusbar/atlas_tint.png  white mask of the tint1 pixels
  src/generated/statusbar.rs              the icon table (row per icon, frames across)
The game tints the two masks with the theme's colours, so one atlas serves every theme.

usage: gen_statusbar.py [--preview out.png]
"""
# NOTE: the artwork sources (assets/icons-src/, assets/statusbar-src/) are kept private and are not in the public
# repository; this tool only works on a machine that has them.
import os, sys
from PIL import Image

SIZE = 12
# (file name, Rust name, frame names). One row of the atlas per icon, in this order.
ICONS = [
    ("anchor", "Anchor", ["static"]),
    ("vpn", "Vpn", ["static"]),
    ("5g", "FiveG", ["static"]),
    ("4g", "FourG", ["static"]),
    ("satellite", "Satellite", ["static"]),
    ("bluetooth", "Bluetooth", ["static"]),
    ("bluetooth-disabled", "BluetoothDisabled", ["static"]),
    ("lock", "Lock", ["static"]),
    ("globe", "Globe", ["static"]),
    ("globe-disabled", "GlobeDisabled", ["static"]),  # derived: the globe in grey with disabled-slash.png over it
    ("ethernet", "Ethernet", ["static"]),
    ("wifi", "Wifi", ["disabled", "0", "1", "2", "3"]),
    ("speaker", "Speaker", ["mute", "0", "1", "2"]),
    ("antenna", "Antenna", ["0", "1", "2", "3"]),
    ("battery", "Battery", ["0", "1", "2", "3", "charge", "disabled", "charge1", "charge2", "charge3"]),  # charge* are derived (see derive_battery)
]

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
src_dir = os.path.join(root, "assets", "statusbar-src")
out_dir = os.path.join(root, "assets", "images", "statusbar")
cols = max(len(f) for _, _, f in ICONS)
W, H = cols * SIZE, len(ICONS) * SIZE
fg = Image.new("RGBA", (W, H), (0, 0, 0, 0))
tint = Image.new("RGBA", (W, H), (0, 0, 0, 0))
preview = Image.new("RGBA", (W, H), (255, 182, 30, 255))
problems = []

def derive_globe_disabled():
    """The globe turned grey (tint1) with the slash (foreground) laid over it."""
    globe = Image.open(os.path.join(src_dir, "globe.png")).convert("RGBA")
    slash = Image.open(os.path.join(src_dir, "disabled-slash.png")).convert("RGBA")
    out = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    for x in range(SIZE):
        for y in range(SIZE):
            if globe.getpixel((x, y))[3] >= 128:
                out.putpixel((x, y), (128, 128, 128, 255))
            if slash.getpixel((x, y))[3] >= 128:
                out.putpixel((x, y), (0, 0, 0, 255))
    return out


BOLT = ["...#", "..##", ".##.", "###.", ".##.", "##..", "#..."]  # 4 x 7, centred on a battery frame at x = 4


def derive_battery(im):
    """Frames 6..8 = charge1..3 and frame 4 = charge0: the level frames 0..3 with a lightning bolt over the body.
    The bolt is solid ink with a one-pixel clear halo (it cuts through the outline top and bottom), so it reads
    at 12 px and the fill still shows on both sides. The draft's own `charge` frame (4) is replaced."""
    out = Image.new("RGBA", (SIZE * 9, SIZE), (0, 0, 0, 0))
    out.paste(im.crop((0, 0, SIZE * 6, SIZE)), (0, 0))
    y0 = (SIZE - len(BOLT)) // 2
    cells = {(4 + dx, y0 + dy) for dy, r in enumerate(BOLT) for dx, c in enumerate(r) if c == "#"}
    for level in range(4):
        dst = 4 if level == 0 else 5 + level
        fr = im.crop((level * SIZE, 0, level * SIZE + SIZE, SIZE))
        for (x, y) in cells:
            for ox in (-1, 0, 1):
                for oy in (-1, 0, 1):
                    X, Y = x + ox, y + oy
                    if (X, Y) not in cells and 0 <= X < SIZE and 0 <= Y < SIZE:
                        fr.putpixel((X, Y), (0, 0, 0, 0))
        for (x, y) in cells:
            fr.putpixel((x, y), (0, 0, 0, 255))
        out.paste(fr, (dst * SIZE, 0))
    return out


for row, (name, _, frames) in enumerate(ICONS):
    path = os.path.join(src_dir, name + ".png")
    if name == "globe-disabled":
        im = derive_globe_disabled()
    elif not os.path.exists(path):
        problems.append(f"missing draft: {path}")
        continue
    else:
        im = Image.open(path).convert("RGBA")
        if name == "battery" and im.size == (SIZE * 6, SIZE):
            im = derive_battery(im)
    if im.size != (SIZE * len(frames), SIZE):
        problems.append(f"{name}.png is {im.size[0]}x{im.size[1]}, want {SIZE * len(frames)}x{SIZE} ({len(frames)} frame(s) of {SIZE} px)")
        continue
    for x in range(im.width):
        for y in range(im.height):
            r, g, b, a = im.getpixel((x, y))
            if a < 128:
                continue
            lum = (r * 299 + g * 587 + b * 114) // 1000
            (fg if lum < 64 else tint).putpixel((x, row * SIZE + y), (255, 255, 255, 255))
            preview.putpixel((x, row * SIZE + y), (46, 29, 0, 255) if lum < 64 else (140, 90, 0, 255))

if problems:
    print("\n".join(problems), file=sys.stderr)
    sys.exit(1)

os.makedirs(out_dir, exist_ok=True)
fg.save(os.path.join(out_dir, "atlas_fg.png"))
tint.save(os.path.join(out_dir, "atlas_tint.png"))

rs = ["// Generated by tools/gen_statusbar.py from assets/statusbar-src/*.png: do not edit by hand.",
      "",
      f"pub const SIZE: i32 = {SIZE};",
      "",
      "/// One row of the atlas per icon; the frames run left to right.",
      "#[derive(Clone, Copy, Debug, PartialEq, Eq)]",
      "pub enum Icon {"]
rs += [f"    {rn} = {i}," for i, (_, rn, _) in enumerate(ICONS)]
rs += ["}", "", "impl Icon {", "    /// Number of frames (states) of the icon.", "    pub const fn frames(self) -> i32 {", "        match self {"]
rs += [f"            Icon::{rn} => {len(f)}," for _, rn, f in ICONS]
rs += ["        }", "    }", "}", ""]
for name, rn, frames in ICONS:
    if len(frames) > 1:
        rs.append(f"/// {name}.png frames: " + ", ".join(f"{i} = {n}" for i, n in enumerate(frames)))
        rs.append(f"pub mod {name.replace('-', '_')}_frame {{")
        for i, n in enumerate(frames):
            cn = ("F" + n if n[0].isdigit() else n).upper()
            rs.append(f"    pub const {cn}: i32 = {i};")
        rs += ["}", ""]
os.makedirs(os.path.join(root, "src", "generated"), exist_ok=True)
open(os.path.join(root, "src", "generated", "statusbar.rs"), "w").write("\n".join(rs))
print(f"wrote atlas {W}x{H} and src/generated/statusbar.rs")

if "--preview" in sys.argv:
    out = sys.argv[sys.argv.index("--preview") + 1]
    preview.resize((W * 6, H * 6), Image.NEAREST).save(out)
    print("preview", out)
