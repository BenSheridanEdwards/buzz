# Fleet Buzz icon

Icon set for the "Fleet Buzz" demo build (Ben's fleet build of Buzz). Generated
by `render.py` from `../buzz-source.png`: the upstream bee glyph in FLEET cyan
(#3fe9ff / #8df6ff) on the FLEET navy canvas (#03070e), masked to the macOS
squircle with the standard 100/1024 margin so it sits with the other Dock icons.

Regenerate:

```bash
python3 render.py icon.png                         # needs Pillow + numpy
mkdir -p FleetBuzz.iconset
for s in 16 32 128 256 512; do
  sips -z $s $s icon.png --out FleetBuzz.iconset/icon_${s}x${s}.png
  sips -z $((s*2)) $((s*2)) icon.png --out FleetBuzz.iconset/icon_${s}x${s}@2x.png
done
iconutil -c icns FleetBuzz.iconset -o icon.icns && rm -r FleetBuzz.iconset
sips -z 32 32 icon.png --out 32x32.png
sips -z 128 128 icon.png --out 128x128.png
sips -z 256 256 icon.png --out 128x128@2x.png
```

Used by: `just desktop-demo-build Fleet aarch64-apple-darwin <build-id> "Fleet Buzz" icons/fleet-buzz`.
