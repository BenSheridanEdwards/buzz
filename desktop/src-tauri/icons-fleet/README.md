# Fleet Buzz icon

Icon set for the Fleet demo build of Buzz ("Fleet Buzz"). `render.py` draws it
from `../icons/buzz-source.png`: the upstream bee glyph in FLEET cyan
(#3fe9ff / #8df6ff) on the FLEET navy canvas (#03070e).

- `icon.png` (1024): macOS master, squircle with Apple's 100/1024 margin so it
  sits with the other Dock icons. `icon.icns`, `32x32.png`, `128x128.png`,
  `128x128@2x.png` and `icon.ico` are derived from it.
- `icon-1024.png`: full-bleed square master for Android; `scripts/fleet-branding.sh`
  resizes it into the debug launcher overlay.

Regenerate (needs Pillow and numpy):

```bash
python3 render.py
mkdir -p FleetBuzz.iconset
for s in 16 32 128 256 512; do
  sips -z $s $s icon.png --out FleetBuzz.iconset/icon_${s}x${s}.png
  sips -z $((s*2)) $((s*2)) icon.png --out FleetBuzz.iconset/icon_${s}x${s}@2x.png
done
iconutil -c icns FleetBuzz.iconset -o icon.icns && rm -r FleetBuzz.iconset
sips -z 32 32 icon.png --out 32x32.png
sips -z 128 128 icon.png --out 128x128.png
sips -z 256 256 icon.png --out 128x128@2x.png
python3 -c 'from PIL import Image; Image.open("icon.png").save("icon.ico", sizes=[(16,16),(32,32),(48,48),(64,64),(128,128),(256,256)])'
```

Build the desktop app with it:

```bash
just desktop-demo-build Fleet aarch64-apple-darwin 51094c33a0d51d7c "Fleet Buzz" icons-fleet
```
