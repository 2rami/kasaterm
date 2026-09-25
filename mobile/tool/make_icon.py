#!/usr/bin/env python3
"""카사모바일 앱 아이콘을 굽는다 — 모바일 원본(흰 바탕 소녀)에 폰 배지를 얹고 iOS·웹 크기를 전부 뽑는다.

라이트·뷰어와 같은 계열로 맞춘다: 그림은 새로 그리지 않고, 뷰어 문서 배지와 **같은 자리·같은 색**
(1024 기준 x 705–845 · y 707–858, 몸통 #D8F0F8 · 강조 #50B8F8 · 남색 테두리)에 폰 모양 배지만 더한다.
원본 `tool/appicon/base-1024.png` 는 배지 전 모바일 아이콘 그대로다(데스크톱 `assets/icon_src.png` 를
모바일용으로 다시 뽑은 판 — 픽셀이 같지 않아 따로 둔다).

돌리는 법(Pillow 필요):  python3 tool/make_icon.py        # mobile/ 에서
iOS 아이콘은 알파가 없어야 해서 RGB 로 쓴다. 파일 이름·크기는 AppIcon.appiconset/Contents.json 을 따른다.
"""
import json
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
MOBILE = HERE.parent
BASE = HERE / "appicon" / "base-1024.png"
APPICON = MOBILE / "ios/Runner/Assets.xcassets/AppIcon.appiconset"
WEB = MOBILE / "web"

K = 4
FILL, ACCENT, INK = (216, 240, 248), (80, 184, 248), (0, 18, 36)
X0, Y0, X1, Y1 = 705, 707, 845, 858
STROKE = 10


def badge(base: Image.Image) -> Image.Image:
    layer = Image.new("RGBA", (1024 * K, 1024 * K), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)

    def s(*v):
        return [int(round(x * K)) for x in v]

    # 흰 후광 — 소녀 선 그림 위에서도 배지가 한 덩어리로 읽히게.
    d.rounded_rectangle(s(X0 - 9, Y0 - 9, X1 + 9, Y1 + 9), radius=34 * K, fill=(255, 255, 255, 255))
    d.rounded_rectangle(s(X0, Y0, X1, Y1), radius=26 * K, fill=FILL + (255,), outline=INK + (255,), width=STROKE * K)
    d.rounded_rectangle(s(758, 724, 792, 732), radius=4 * K, fill=INK + (255,))          # 스피커
    # 화면 안 말풍선 — 나쵸와 대화하는 앱. 강조색은 뷰어 배지의 접힌 모서리 색.
    d.rounded_rectangle(s(729, 752, 821, 810), radius=16 * K, fill=ACCENT + (255,), outline=INK + (255,), width=6 * K)
    d.polygon(s(748, 806, 770, 806, 744, 826), fill=ACCENT + (255,), outline=INK + (255,))
    d.line(s(748, 806, 770, 806), fill=ACCENT + (255,), width=6 * K)
    for y, right in ((772, 805), (790, 788)):
        d.rounded_rectangle(s(745, y - 3, right, y + 3), radius=3 * K, fill=INK + (255,))
    d.rounded_rectangle(s(758, 838, 792, 845), radius=4 * K, fill=INK + (255,))          # 홈 표시줄
    out = base.convert("RGBA")
    out.alpha_composite(layer.resize((1024, 1024), Image.LANCZOS))
    return out.convert("RGB")


def main() -> None:
    base = Image.open(BASE).convert("RGB")
    assert base.size == (1024, 1024), base.size
    icon = badge(base)
    spec = json.loads((APPICON / "Contents.json").read_text())
    written = {}
    for img in spec["images"]:
        name = img.get("filename")
        if not name:
            continue
        px = int(round(float(img["size"].split("x")[0]) * int(img["scale"].rstrip("x"))))
        if written.get(name, px) != px:
            raise SystemExit(f"{name}: Contents.json 에 크기가 둘이다")
        (icon if px == 1024 else icon.resize((px, px), Image.LANCZOS)).save(APPICON / name, optimize=True)
        written[name] = px
    for name, px in (("icons/Icon-192.png", 192), ("icons/Icon-512.png", 512)):
        icon.resize((px, px), Image.LANCZOS).save(WEB / name, optimize=True)
    # maskable 은 둥글게 잘려도 그림이 남도록 안쪽 80% 안전 구역에 앉힌다.
    for name, px in (("icons/Icon-maskable-192.png", 192), ("icons/Icon-maskable-512.png", 512)):
        c = Image.new("RGBA", (px, px), (255, 255, 255, 255))
        inner = int(px * 0.8)
        c.paste(icon.resize((inner, inner), Image.LANCZOS), ((px - inner) // 2, (px - inner) // 2))
        c.save(WEB / name, optimize=True)
    icon.resize((16, 16), Image.LANCZOS).convert("RGBA").save(WEB / "favicon.png")
    print(f"iOS {len(written)}장 · 웹 5장")


if __name__ == "__main__":
    main()
