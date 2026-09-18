"""Render the Fleet Buzz icon set from the upstream bee (../icons/buzz-source.png).

Writes two 1024px masters next to this script:
  icon.png       macOS: squircle with Apple's margin (the .icns and Tauri PNGs come from this)
  icon-1024.png  full-bleed square: the Android source scripts/fleet-branding.sh resizes
Needs Pillow and numpy.
"""
import sys, math
import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageChops

import os
HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(HERE, "..", "icons", "buzz-source.png")
OUT = os.path.join(HERE, "icon.png")
OUT_FULL = os.path.join(HERE, "icon-1024.png")
N = 1024                     # master size
# Apple's icon grid: the squircle fills 824/1024, leaving ~100px margin each side
ICON = 824
MARGIN = (N - ICON) // 2

# ---- 1. glyph mask from the upstream bee (yellow on black) ----
src = Image.open(SRC).convert("RGBA")
a = np.asarray(src).astype(np.float32)
lum = (a[..., 0] + a[..., 1] + a[..., 2]) / 3.0
mask = np.clip((lum - 40) / 80.0, 0, 1) * (a[..., 3] / 255.0)   # yellow -> 1, black -> 0
glyph = Image.fromarray((mask * 255).astype(np.uint8), "L")
bbox = glyph.getbbox()
glyph = glyph.crop(bbox)

# ---- 2. squircle (superellipse) mask ----
def squircle(size, n=5.0):
    y, x = np.mgrid[0:size, 0:size].astype(np.float32)
    cx = cy = (size - 1) / 2.0
    r = size / 2.0
    v = (np.abs((x - cx) / r) ** n + np.abs((y - cy) / r) ** n)
    # anti-alias with a soft edge
    edge = np.clip((1.0 - v) * r * 0.9 + 0.5, 0, 1)
    return Image.fromarray((edge * 255).astype(np.uint8), "L")

sq = squircle(ICON)

# ---- 3. canvas: FLEET navy with vertical gradient + faint graph-paper grid + vignette ----
top = np.array([0x0a, 0x18, 0x2c], np.float32)     # a touch lighter than #07101e
bot = np.array([0x03, 0x07, 0x0e], np.float32)     # FLEET canvas
g = np.linspace(0, 1, ICON, dtype=np.float32)[:, None, None]
canvas = (top * (1 - g) + bot * g)
canvas = np.broadcast_to(canvas, (ICON, ICON, 3)).copy()
# grid lines every 64px, very faint cyan
grid = np.zeros((ICON, ICON), np.float32)
grid[::64, :] = 1; grid[:, ::64] = 1
canvas += grid[..., None] * np.array([0x1c, 0x6b, 0x82], np.float32) * 0.28
# radial glow behind the glyph
yy, xx = np.mgrid[0:ICON, 0:ICON].astype(np.float32)
d = np.sqrt((xx - ICON/2)**2 + (yy - ICON/2)**2) / (ICON/2)
glow = np.clip(1 - d / 0.75, 0, 1) ** 2
canvas += glow[..., None] * np.array([0x1c, 0x6b, 0x82], np.float32) * 0.45
canvas = np.clip(canvas, 0, 255).astype(np.uint8)
bg = Image.fromarray(canvas, "RGB").convert("RGBA")

# ---- 4. glyph: scale to ~62% of the squircle, cyan fill with a bright inner core and soft outer glow ----
gw = int(ICON * 0.66)
gh = int(gw * glyph.height / glyph.width)
gm = glyph.resize((gw, gh), Image.LANCZOS)
gx, gy = (ICON - gw) // 2, (ICON - gh) // 2
layer = Image.new("RGBA", (ICON, ICON), (0, 0, 0, 0))
# outer glow
glow_l = Image.new("RGBA", (ICON, ICON), (0x3f, 0xe9, 0xff, 0))
glow_l.putalpha(Image.new("L", (ICON, ICON), 0))
gl = Image.new("L", (ICON, ICON), 0); gl.paste(gm, (gx, gy))
gl = gl.filter(ImageFilter.GaussianBlur(28)).point(lambda p: int(p * 0.55))
glow_l.putalpha(gl)
layer = Image.alpha_composite(layer, glow_l)
# solid fill
fill = Image.new("RGBA", (ICON, ICON), (0x3f, 0xe9, 0xff, 255))
fm = Image.new("L", (ICON, ICON), 0); fm.paste(gm, (gx, gy)); fill.putalpha(fm)
layer = Image.alpha_composite(layer, fill)
# bright core (eroded mask)
core = Image.new("RGBA", (ICON, ICON), (0x8d, 0xf6, 0xff, 255))
cm = fm.filter(ImageFilter.MinFilter(19)).filter(ImageFilter.GaussianBlur(6))
core.putalpha(cm)
layer = Image.alpha_composite(layer, core)

icon = Image.alpha_composite(bg, layer)
# full-bleed master for Android (no mask, no margin; the launcher applies its own shape)
icon.resize((N, N), Image.LANCZOS).save(OUT_FULL)
icon.putalpha(ImageChops.multiply(icon.getchannel("A"), sq))

# ---- 5. drop into the 1024 canvas with Apple's margin, subtle shadow ----
out = Image.new("RGBA", (N, N), (0, 0, 0, 0))
sh = Image.new("RGBA", (N, N), (0, 0, 0, 0))
shm = Image.new("L", (N, N), 0); shm.paste(sq, (MARGIN, MARGIN + 10))
sh.putalpha(shm.filter(ImageFilter.GaussianBlur(14)).point(lambda p: int(p * 0.35)))
out = Image.alpha_composite(out, sh)
out.paste(icon, (MARGIN, MARGIN), icon)
out.save(OUT)
print("wrote", OUT, "and", OUT_FULL)
