#!/usr/bin/env python3
"""설치 화면 그림(Windows MSI 의 Banner·Dialog, 맥 DMG 배경)을 앱 겉모습으로 다시 그린다.

결과는 레포에 넣는다 — 빌드는 그림을 읽기만 한다. 아이콘(assets/AppIcon.png)이나 색을 바꾸면 이 맥에서 다시 돌린다:

    python3 scripts/installer-art.py

색은 DESIGN.md 클린 블루(바탕 #F5FAFE→#EAF3FC, 글자 #15294A, 강조 #2F6FD4). 글꼴은 macOS 의 Apple SD Gothic Neo.
"""
from pathlib import Path
import subprocess
import tempfile

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ICON = ROOT / "assets" / "AppIcon.png"
WIX = ROOT / "app" / "kasaterm" / "wix"
DMG = ROOT / "assets" / "dmg"
FONT = "/System/Library/Fonts/AppleSDGothicNeo.ttc"

INK = (21, 41, 74)
MUTE = (74, 99, 143)
ACCENT = (47, 111, 212)
ACCENT_2 = (74, 144, 226)
WASH = (234, 243, 252)
PAPER = (245, 250, 254)
LINE = (214, 230, 245)


def font(size, weight="Bold"):
    index = {"Regular": 0, "Medium": 2, "SemiBold": 4, "Bold": 6}[weight]
    return ImageFont.truetype(FONT, size, index=index)


def vertical(size, top, bottom):
    w, h = size
    img = Image.new("RGB", size, top)
    px = img.load()
    for y in range(h):
        t = y / max(h - 1, 1)
        c = tuple(round(a + (b - a) * t) for a, b in zip(top, bottom))
        for x in range(w):
            px[x, y] = c
    return img


def icon(side):
    """앱 아이콘(흰 둥근 판 위 우산 든 아이) — 투명 테두리를 걷어 판이 칸을 채우게."""
    src = Image.open(ICON).convert("RGBA")
    return src.crop(src.getbbox()).resize((side, side), Image.LANCZOS)


def shadow_under(canvas, box, radius, blur, alpha):
    layer = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    ImageDraw.Draw(layer).rounded_rectangle(box, radius, fill=(21, 41, 74, alpha))
    canvas.alpha_composite(layer.filter(ImageFilter.GaussianBlur(blur)))


def spaced(draw, xy, text, fnt, fill, tracking):
    x, y = xy
    for ch in text:
        draw.text((x, y), ch, font=fnt, fill=fill)
        x += draw.textlength(ch, font=fnt) + tracking
    return x


def spaced_width(draw, text, fnt, tracking):
    return sum(draw.textlength(ch, font=fnt) for ch in text) + tracking * (len(text) - 1)


def umbrella(draw, cx, cy, r, color, width):
    """하단바·웹 QR 과 같은 우산(Lucide 모양)을 선으로."""
    draw.arc((cx - r, cy - r, cx + r, cy + r), 180, 360, fill=color, width=width)
    draw.line((cx - r, cy, cx + r, cy), fill=color, width=width)
    draw.line((cx, cy - r - width, cx, cy - r + width), fill=color, width=width)
    draw.line((cx, cy, cx, cy + r * 0.62), fill=color, width=width)
    hook = r * 0.32
    draw.arc((cx, cy + r * 0.62 - hook, cx + hook * 2, cy + r * 0.62 + hook), 0, 180, fill=color, width=width)


def dialog():
    """WixUI 환영·끝 화면 바탕(493×312). 왼쪽 164px 만 그림 자리 — 오른쪽에는 설치기가 검은 글자와 약관 칸을 얹는다."""
    w, h, panel = 493, 312, 164
    img = Image.new("RGBA", (w, h), (255, 255, 255, 255))
    left = vertical((panel, h), ACCENT, ACCENT_2)
    # 판 아래 큰 우산 — 강조색보다 한 단계 밝은 선이라 물러나 보인다. 왼쪽 판 안에서만 그린다.
    umbrella(ImageDraw.Draw(left), panel - 30, h - 30, 58, (88, 146, 224), 5)
    img.paste(left, (0, 0))
    side = 92
    x, y = (panel - side) // 2, 62
    shadow_under(img, (x, y + 6, x + side, y + side + 6), 24, 8, 70)
    img.alpha_composite(icon(side), (x, y))
    d = ImageDraw.Draw(img)
    name = font(15)
    nw = spaced_width(d, "KASATERM", name, 1.6)
    spaced(d, ((panel - nw) / 2, y + side + 22), "KASATERM", name, (255, 255, 255), 1.6)
    sub = font(11, "Medium")
    tw = d.textlength("작업 OS", font=sub)
    d.text(((panel - tw) / 2, y + side + 45), "작업 OS", font=sub, fill=(222, 234, 250))
    d.line((panel, 0, panel, h), fill=LINE, width=1)
    return img.convert("RGB")


def banner():
    """WixUI 진행 화면 머리(493×58). 왼쪽에 설치기가 검은 제목을 쓰니 밝게 두고, 오른쪽 끝에 아이콘."""
    w, h = 493, 58
    img = Image.new("RGBA", (w, h), (255, 255, 255, 255))
    grad = Image.new("RGB", (w, h))
    px = grad.load()
    for x in range(w):
        t = max(0.0, (x - 200) / (w - 200))
        c = tuple(round(a + (b - a) * t) for a, b in zip((255, 255, 255), WASH))
        for y in range(h):
            px[x, y] = c
    img.paste(grad, (0, 0))
    side = 40
    x, y = w - side - 12, (h - side) // 2
    shadow_under(img, (x, y + 3, x + side, y + side + 3), 10, 4, 50)
    img.alpha_composite(icon(side), (x, y))
    return img.convert("RGB")


def dmg_background(scale):
    """DMG 창(660×400) 바탕 — 아이콘 자리는 앱 (170, 228)·Applications (490, 228), 크기 112."""
    w, h = 660 * scale, 400 * scale
    img = vertical((w, h), PAPER, WASH).convert("RGBA")
    d = ImageDraw.Draw(img)
    s = scale
    mark = 26 * s
    mx, my = 32 * s, 30 * s
    d.rounded_rectangle((mx, my, mx + mark, my + mark), 8 * s, fill=ACCENT)
    umbrella(d, mx + mark / 2, my + mark * 0.52, 7.5 * s, (255, 255, 255), max(2, round(1.7 * s)))
    spaced(d, (mx + mark + 10 * s, my + 4 * s), "KASATERM", font(14 * s), INK, 1.4 * s)
    title = font(22 * s)
    msg = "Applications 폴더로 끌어다 놓으면 설치돼요"
    tw = d.textlength(msg, font=title)
    d.text(((w - tw) / 2, 78 * s), msg, font=title, fill=INK)
    hint = font(13 * s, "Medium")
    sub = "설치한 뒤에는 이 창을 닫고 디스크를 꺼내도 돼요"
    hw = d.textlength(sub, font=hint)
    d.text(((w - hw) / 2, 112 * s), sub, font=hint, fill=MUTE)
    y = 228 * s
    x0, x1 = 252 * s, 404 * s
    dash, gap = 10 * s, 8 * s
    x = x0
    while x < x1 - 18 * s:
        d.line((x, y, min(x + dash, x1 - 18 * s), y), fill=ACCENT, width=round(3 * s))
        x += dash + gap
    d.polygon([(x1, y), (x1 - 18 * s, y - 11 * s), (x1 - 18 * s, y + 11 * s)], fill=ACCENT)
    return img.convert("RGB")


def main():
    WIX.mkdir(parents=True, exist_ok=True)
    DMG.mkdir(parents=True, exist_ok=True)
    dialog().save(WIX / "Dialog.bmp")
    banner().save(WIX / "Banner.bmp")
    with tempfile.TemporaryDirectory() as tmp:
        one, two = Path(tmp) / "background.png", Path(tmp) / "background@2x.png"
        dmg_background(1).save(one)
        dmg_background(2).save(two)
        subprocess.run(["tiffutil", "-cathidpicheck", str(one), str(two), "-out", str(DMG / "background.tiff")],
                       check=True, capture_output=True)


if __name__ == "__main__":
    main()
