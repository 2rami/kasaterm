#!/usr/bin/env bash
# App icon outputs from the Icon Composer source (assets/AppIcon.icon). Needs Xcode 26 (actool, ictool).
#
#   assets/AppIcon.car      macOS 26 adaptive icon (light, dark, clear, tinted); build-app.sh ships it as Resources/Assets.car
#   assets/AppIcon.icns     fallback for macOS before 26 and Finder's small slots
#   assets/AppIcon.iconset  the icns slots, kept for reference
#   assets/AppIcon.png      1024 light rendition: splash and installer art
#   assets/app.ico          Windows exe icon (light rendition)
#
# The source layer has no background on purpose: the system fills the tile per appearance.
# Rerun after editing assets/AppIcon.icon, then rerun scripts/installer-art.py.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/assets/AppIcon.icon"
ICTOOL="$(xcode-select -p)/../Applications/Icon Composer.app/Contents/Executables/ictool"
[[ -d "$SRC" ]] || { echo "error: $SRC missing" >&2; exit 1; }
[[ -x "$ICTOOL" ]] || { echo "error: ictool not found (Xcode 26 required)" >&2; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP:?}"' EXIT

xcrun actool --compile "$TMP" --platform macosx --minimum-deployment-target 11.0 \
  --app-icon AppIcon --output-partial-info-plist "$TMP/partial.plist" "$SRC" >/dev/null
[[ -f "$TMP/Assets.car" && -f "$TMP/AppIcon.icns" ]] || { echo "error: actool produced no icon" >&2; exit 1; }

"$ICTOOL" "$SRC" --export-image --output-file "$TMP/light.png" --platform macOS \
  --rendition Default --width 1024 --height 1024 --scale 1 >/dev/null

python3 - "$TMP" "$ROOT/assets" <<'PY'
import subprocess, sys
from pathlib import Path
from PIL import Image

tmp, assets = Path(sys.argv[1]), Path(sys.argv[2])
iconset = tmp / "AppIcon.iconset"
iconset.mkdir()

# actool's icns carries only the small slots (Assets.car holds the rest), so macOS before 26 would get a
# blurry Dock icon from it. The fallback icns is built from the full light rendition instead. The rendition
# keeps the tile's safe-area margin; list views draw the 16 and 32 point slots directly, where that margin
# leaves a speck, so those slots use the tile trimmed to its edge.
light = Image.open(tmp / "light.png").convert("RGBA")
tile = light.crop(light.getchannel("A").point(lambda a: 255 if a > 8 else 0).getbbox())
for point in (16, 32, 128, 256, 512):
    for scale in (1, 2):
        side = point * scale
        src = tile if point <= 32 else light
        name = f"icon_{point}x{point}{'@2x' if scale == 2 else ''}.png"
        src.resize((side, side), Image.LANCZOS).save(iconset / name)
subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(tmp / "AppIcon.icns")], check=True)

out = assets / "AppIcon.iconset"
out.mkdir(exist_ok=True)
for png in iconset.glob("*.png"):
    (out / png.name).write_bytes(png.read_bytes())

light = Image.open(tmp / "light.png").convert("RGBA")
light.save(assets / "AppIcon.png")
light.save(assets / "app.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
PY

cp "$TMP/Assets.car" "$ROOT/assets/AppIcon.car"
cp "$TMP/AppIcon.icns" "$ROOT/assets/AppIcon.icns"
echo "icon outputs written to assets/"
