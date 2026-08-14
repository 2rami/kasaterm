#!/usr/bin/env python3
"""로스터의 desc 로 학생 스프라이트를 만들고 앱 자산 자리에 놓는다.

파이프라인의 **생성 단계**다. 앞 단계(`theme-wiki.py`)가 만든 위키 수집물에서 사람이
`theme-src/<slug>/desc.txt` 한 줄을 쓰고, 여기서 그 한 줄을 `ppgen` 에 먹여 4상태
스프라이트를 만든 뒤 `app/kasaterm/assets/students/` 로 옮긴다.

    theme-sprites.py gen               # desc.txt 있는 학생 전부 생성 (이미 된 건 건너뜀)
    theme-sprites.py gen kei rio       # 일부만
    theme-sprites.py gen --ref         # 공식 포트레이트를 정체성 참조로 함께 넘긴다
    theme-sprites.py install           # 생성물을 앱 자산 자리로 복사
    theme-sprites.py status            # 어디까지 됐는지
    theme-sprites.py sheet             # 원본↔생성물 대조 시트 한 장(눈으로 검수)

생성은 1명당 2분 남짓(호출 5회)이라 `--jobs` 로 병렬로 돌린다. 중간에 끊겨도 다시
부르면 이어서 한다 — 판정은 모든 상태의 프레임이 다 있는지다(`generated`).

⚠️ **OpenGateway 로는 못 굽는다.** 동작 프레임은 base 캐릭터를 참조 이미지로 넘겨
그리는데 게이트웨이엔 그 경로(`/images/edits`)가 없다. 참조 이미지를 실어 보내도
오류 없이 **무시하고 새 그림을 준다** — 필드 검증이 아예 없어서다(2026-08-11 실측).
그래서 프로바이더는 codex(구독 OAuth, 무과금) · openai(종량) · fal(종량) 중에서 쓴다.
키는 ppgen 이 설정에서 읽는다(`~/Library/Application Support/perfectpixel/config.json`).
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "theme-src")
DST = os.path.join(ROOT, "app/kasaterm/assets/students")
ROSTER = os.path.join(ROOT, "app/kasaterm/collab-hooks/characters.json")
PPGEN = os.environ.get("PPGEN", "/tmp/ppgen")
# 생성 백엔드. `codex` 는 ChatGPT 구독 OAuth 로 도는 CLI 라 호출당 요금이 없고,
# 참조 이미지(-i)를 받아 편집도 된다 — 그래서 기본값이다. `fal` 은 종량 과금이고,
# OpenAI 호환 게이트웨이는 /images/edits 가 아예 없어 동작 프레임을 못 만든다.
#
# `openai` 는 종량 과금이지만 codex 한도에 걸렸을 때의 대안이다(2026-08-11 실측).
# ⚠️ **입력 이미지가 조직당 분당 5개**다. 참조 편집은 호출마다 이미지가 들어가므로
# 이게 처리량 상한이 된다 — `--jobs 4` 는 분당 약 5.8회라 429 로 절반이 떨어졌고
# `--jobs 2`(약 2.9회)에서 전부 통과했다. 병렬을 올리지 마라.
PROVIDER = os.environ.get("THEME_PROVIDER", "codex")

# 상태 → (ppgen 프리셋 이름 겸 앱 자산 폴더 이름, 프레임 수).
# 프레임 수는 기존 12명의 자산과 같다 — ppgen 프리셋이 주는 수와 이미 일치한다.
STATES = [
    ("idle", 4),
    ("walk", 6),
    ("wave", 4),
    ("cheer", 4),
]


def dst_frame(slug, state, i):
    """앱이 프레임을 읽는 자리 — 모션 폴더 안의 `<slug>-<i>.png`.

    폴더가 나뉘기 전에는 한 폴더에 `<slug>-walk-<i>.png` 처럼 평평하게 쌓았다.
    1500장이 한 자리에 섞여 있으면 사람이 자기 그림을 어디에 넣어야 하는지
    알 수 없어서 갈랐다(2026-08-14 지시).
    """
    return os.path.join(DST, state, f"{slug}-{i}.png")


def dst_profile(slug):
    return os.path.join(DST, "profile", f"{slug}.png")


def dst_gif(slug):
    return os.path.join(DST, "gif", f"{slug}.gif")


PROFILE_PX = 96
# 검수 문턱. 멀쩡한 프레임은 예외 없이 런 2 로 나온다(정상 7명 실측). 처음엔 6 으로
# 뒀다가 런 6 짜리 뭉갠 프레임이 문턱에 딱 걸쳐 통과했다 — 「정상과 불량 사이」가 아니라
# **정상 값에 붙여** 잡아야 한다. 색은 21~29 로 흩어져 뭉갬 판정에 약하니 보조로만 쓴다.
MIN_COLORS = 16
MAX_RUN = 4
ATTEMPTS = 3


def members():
    d = json.load(open(ROSTER, encoding="utf-8"))
    return [m for m in d["members"] if m.get("slug")]


def slugs():
    return [m["slug"] for m in members()]


def desc_of(slug):
    p = os.path.join(SRC, slug, "desc.txt")
    if not os.path.exists(p):
        return None
    t = open(p, encoding="utf-8").read().strip()
    return " ".join(t.split()) or None


def generated(slug):
    """**모든 상태의 프레임이 다 있는가.**

    manifest.json 존재만 보면 안 된다 — ppgen 은 일부 상태가 실패해도 매니페스트를
    남긴다. 그래서 cheer 나 wave 가 통째로 빠진 채 「생성됨」이 되고, `gen` 이
    `--force` 없이는 건너뛰어 영영 안 채워진다. 실제로 429(분당 상한)에 걸린 3명이
    이 경로로 조용히 미완성으로 남았고, install 단계에서야 프레임 없음으로 드러났다.
    """
    root = os.path.join(SRC, slug, "out")
    if not os.path.exists(os.path.join(root, "manifest.json")):
        return False
    for state, count in STATES:
        for i in range(count):
            if not os.path.exists(os.path.join(root, "frames", state, f"frame-{i:02d}.png")):
                return False
    return True


def frame_quality(path):
    """프레임이 뭉갰는지 본다. (고유색, 중앙 런길이) 를 돌려준다.

    **왜 자체 검수가 필요한가.** ppgen 도 상태마다 점수를 매기지만 그건 프레임 수와
    모션 다양성을 보지, 픽셀 양자화가 무너진 것은 못 잡는다 — 케이 실측에서 점수 69로
    통과한 프레임이 고유색 10개·런 14px 로 형체를 알아볼 수 없었다(정상은 색 28~29·런 2).
    같은 프롬프트를 다시 돌리면 멀쩡히 나오는 생성 편차라, 배치로 67명을 돌리면 몇 명은
    반드시 이렇게 나온다. 그래서 여기서 걸러 재생성한다.
    """
    from PIL import Image
    import statistics

    im = Image.open(path).convert("RGBA")
    colors = len(set(im.convert("RGB").get_flattened_data()))
    px = im.load()
    w, h = im.size
    runs = []
    for y in range(0, h, 4):
        run = 1
        for x in range(1, w):
            if px[x, y] == px[x - 1, y]:
                run += 1
            else:
                if px[x - 1, y][3] > 8:
                    runs.append(run)
                run = 1
    return colors, (statistics.median(runs) if runs else 999)


def check(slug):
    """생성물이 쓸 만한지. 나쁘면 이유 문자열, 괜찮으면 빈 문자열."""
    out = os.path.join(SRC, slug, "out")
    for state, count in STATES:
        for i in (0, count // 2):
            p = os.path.join(out, "frames", state, f"frame-{i:02d}.png")
            if not os.path.exists(p):
                return f"{state}/frame-{i:02d} 없음"
            colors, run = frame_quality(p)
            if colors < MIN_COLORS or run > MAX_RUN:
                return f"{state} 뭉개짐(색{colors} 런{run:.0f})"
    return ""


def installed(slug):
    """설치돼 있고 **생성물보다 낡지 않은가**.

    파일 존재만 보면 재생성이 조용히 사라진다 — `gen --ref --force` 로 다시 구운
    케이가 「이미 설치됨」으로 건너뛰어져 앱에는 옛 분홍머리가 그대로 남아 있었다.
    오류가 안 나서 그리드를 눈으로 볼 때까지 몰랐다.
    """
    dst = dst_profile(slug)
    if not os.path.exists(dst):
        return False
    src = os.path.join(SRC, slug, "out", "frames", "idle", "frame-00.png")
    if os.path.exists(src) and os.path.getmtime(src) > os.path.getmtime(dst):
        return False
    return True


def generate(slug, force=False, use_ref=False):
    """`use_ref` 면 공식 포트레이트를 정체성 참조로 넘긴다.

    묘사만으로도 대개 맞게 나오지만 **색이 어긋나는 일이 있다** — 케이는 desc 에
    `white hair` 라고 썼는데도 문장 안의 pink(헤일로·안감)에 끌려 분홍 머리로 나왔다.
    픽셀 검수는 이걸 못 잡는다(그림 자체는 멀쩡하다). 참조 이미지를 주면 고정되지만
    생성이 4배 느려서(204초 vs 45초) 기본값으로 두지 않는다 — 묘사로 전부 만든 뒤
    눈으로 보고 어긋난 애만 이걸로 다시 굽는 게 싸다.
    """
    if generated(slug) and not force:
        return slug, "skip", ""
    desc = desc_of(slug)
    if not desc:
        return slug, "no-desc", ""
    out = os.path.join(SRC, slug, "out")
    cmd = [
        PPGEN,
        "-provider", PROVIDER,
        "-desc", desc,
        "-states", ",".join(s[0] for s in STATES),
        "-out", out,
    ]
    if use_ref:
        ref = os.path.join(SRC, slug, "ref.png")
        if not os.path.exists(ref):
            return slug, "no-ref", "theme-wiki.py 로 포트레이트를 먼저 받아라"
        cmd[3:3] = ["-ref", ref]
    why = ""
    for attempt in range(ATTEMPTS):
        shutil.rmtree(out, ignore_errors=True)
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=1800)
        if r.returncode != 0 or not generated(slug):
            tail = (r.stdout + r.stderr).strip().splitlines()
            why = tail[-1] if tail else f"exit {r.returncode}"
            continue
        why = check(slug)
        if not why:
            return slug, "ok", f"시도{attempt + 1}" if attempt else ""
    return slug, "fail", why


def install(slug, force=False):
    """생성물을 앱이 읽는 자리로 복사한다.

    앱은 모션 폴더 안의 `<slug>-<i>.png` 로 찾는데 ppgen 은 `frames/<state>/
    frame-NN.png` 로 낸다. 크기(256px)는 이미 같아서 리사이즈 없이 복사만 하면 된다.
    """
    if installed(slug) and not force:
        return slug, "skip", ""
    out = os.path.join(SRC, slug, "out")
    if not generated(slug):
        return slug, "not-generated", ""
    for sub in [s for s, _ in STATES] + ["profile", "gif"]:
        os.makedirs(os.path.join(DST, sub), exist_ok=True)

    for state, count in STATES:
        for i in range(count):
            src = os.path.join(out, "frames", state, f"frame-{i:02d}.png")
            if not os.path.exists(src):
                return slug, "fail", f"{state} frame-{i:02d} 없음"
            shutil.copy2(src, dst_frame(slug, state, i))

    gif = os.path.join(out, "gif", "idle.gif")
    if os.path.exists(gif):
        shutil.copy2(gif, dst_gif(slug))

    make_profile(os.path.join(out, "frames", "idle", "frame-00.png"), dst_profile(slug))
    return slug, "ok", ""


def make_profile(src, dst):
    """idle 첫 프레임의 머리~어깨를 잘라 프로필로 쓴다.

    캐릭터마다 프레임 안에서 차지하는 크기가 달라 고정 좌표로 자르면 누구는 얼굴이
    잘리고 누구는 배경만 남는다. 알파 경계를 먼저 구해 그 위쪽 정사각형을 잡는다.

    ⚠️ 경계 맨 위에서 자르면 안 된다 — 블루아카이브 캐릭터는 **헤일로가 머리 위에
    떠 있어** bbox 상단이 헤일로 꼭대기다. 거기서부터 자르면 얼굴이 아래 가장자리로
    밀려 머리카락만 찬 그림이 나온다(케이로 실측). 그 빈 띠만큼 내려 잡는다.
    """
    from PIL import Image

    im = Image.open(src).convert("RGBA")
    alpha = im.getchannel("A")
    box = alpha.getbbox()
    if not box:
        im.resize((PROFILE_PX, PROFILE_PX), Image.NEAREST).save(dst)
        return
    x0, y0, x1, y1 = box
    # 머리가 시작하는 줄 = 위에서부터 처음으로 폭이 넉넉해지는 줄. 헤일로는 얇은
    # 링이라 이 문턱을 못 넘고, 머리는 넘는다.
    px = alpha.load()
    need = max(4, (x1 - x0) // 4)
    head = y0
    for y in range(y0, y1):
        if sum(1 for x in range(x0, x1) if px[x, y] > 8) >= need:
            head = y
            break
    side = max(16, int((y1 - y0) * 0.5))
    cx = (x0 + x1) // 2
    left = max(0, min(cx - side // 2, im.width - side))
    top = max(0, min(head, im.height - side))
    im.crop((left, top, left + side, top + side)) \
      .resize((PROFILE_PX, PROFILE_PX), Image.NEAREST) \
      .save(dst)


def run(fn, targets, jobs, force, **kw):
    counts = {}
    with ThreadPoolExecutor(max_workers=jobs) as ex:
        for slug, st, msg in ex.map(lambda s: fn(s, force, **kw), targets):
            counts[st] = counts.get(st, 0) + 1
            if st not in ("ok", "skip"):
                print(f"  {slug:10} {st} {msg}")
            elif st == "ok":
                print(f"  {slug:10} ok")
    print(", ".join(f"{k} {v}" for k, v in sorted(counts.items())))
    return 0 if not (counts.keys() - {"ok", "skip"}) else 1


def sheet(targets, out, cell=110):
    """공식 원본과 생성물을 나란히 놓은 **대조 시트** 한 장.

    정체성이 어긋났는지는 자동 검사로 못 잡는다 — 케이는 desc 에 `white hair` 라고
    적혀 있는데도 분홍 머리로 나왔고, 그림 자체는 멀쩡해서 픽셀 검사를 다 통과했다.
    사람 눈이 제일 빠르고 정확하므로, 전원을 한 장에 모아 보고 어긋난 애만 다시 굽는다.

    왼쪽이 위키 원본(`ref.png`), 오른쪽이 생성된 프로필. 참조 없이 묘사만으로 구운
    학생은 왼쪽이 비어 있고 — **그건 대조할 원본이 없다는 뜻이지 통과가 아니다**.
    """
    from PIL import Image, ImageDraw

    cols = 6
    rows = (len(targets) + cols - 1) // cols
    pad, label = 6, 14
    cw, ch = cell * 2 + pad, cell + label + pad
    im = Image.new("RGB", (cols * cw + pad, rows * ch + pad), (24, 26, 31))
    draw = ImageDraw.Draw(im)

    for n, slug in enumerate(targets):
        x0 = pad + (n % cols) * cw
        y0 = pad + (n // cols) * ch
        for i, path in enumerate(
            [os.path.join(SRC, slug, "ref.png"), dst_profile(slug)]
        ):
            box = (x0 + i * cell, y0, x0 + (i + 1) * cell, y0 + cell)
            if not os.path.exists(path):
                draw.rectangle(box, outline=(70, 60, 60))
                draw.text((box[0] + cell // 2 - 8, box[1] + cell // 2 - 5), "—", fill=(120, 110, 110))
                continue
            th = Image.open(path).convert("RGBA")
            th.thumbnail((cell, cell), Image.LANCZOS)
            bg = Image.new("RGB", th.size, (24, 26, 31))
            bg.paste(th, (0, 0), th)
            im.paste(bg, (box[0] + (cell - th.width) // 2, box[1] + (cell - th.height) // 2))
        draw.text((x0 + 2, y0 + cell + 2), slug, fill=(150, 158, 170))

    im.save(out)
    print(f"{len(targets)}명 → {out}  ({im.width}x{im.height})")
    missing = [s for s in targets if not os.path.exists(os.path.join(SRC, s, "ref.png"))]
    if missing:
        print(f"원본 없음 {len(missing)}: {' '.join(missing)} — 대조 불가")
    return 0


def phash(path, side=16):
    """축소 흑백 이미지를 평균으로 이진화한 지각 해시.

    파일 해시로는 **같은 그림을 두 번 그린 것을 못 잡는다** — 생성은 매번 픽셀이
    달라서 md5 가 절대 안 겹친다. 실루엣이 같으면 걸리게 하려면 축소해서 봐야 한다.

    투명 배경을 흰색에 합성하고 나서 흑백으로 바꾼다. 알파를 버리고 바로 `convert("L")`
    하면 **투명 픽셀이 검정 0 이 되어** 캐릭터가 아니라 배경 모양을 해싱하게 되고,
    그러면 여백이 비슷한 학생끼리 전부 「닮았다」로 뜬다.
    """
    from PIL import Image

    im = Image.open(path).convert("RGBA")
    bg = Image.new("RGBA", im.size, (255, 255, 255, 255))
    bg.alpha_composite(im)
    g = bg.convert("L").resize((side, side), Image.LANCZOS)
    px = list(g.tobytes())
    avg = sum(px) / len(px)
    return [v > avg for v in px]


def dupes(targets, out, top=12, cell=110):
    """서로 제일 닮은 학생 쌍을 가까운 순으로 늘어놓은 **중복 점검 시트**.

    로스터에 같은 캐릭터가 두 번 들어갔거나, 생성이 무너져 옆 학생과 같은 그림이
    나온 것을 잡는다. 후자는 desc 가 멀쩡해도 일어나므로 묘사 검사로는 못 잡는다.

    슬러그·이름·위키 문서·desc 의 **완전 중복은 먼저 따로 알린다** — 그건 눈으로
    볼 것도 없이 데이터가 잘못된 것이라 시트에 섞으면 묻힌다.

    한 줄이 한 쌍이고 `원본A 생성A │ 생성B 원본B` 순서다. 가운데 둘을 붙여 놓는 건
    비교할 대상이 그 둘이기 때문이고, 바깥의 원본은 「원래 다른 캐릭터가 맞나」를
    같은 줄에서 확인하라고 둔다.

    ⚠️거리는 참고값이지 판정이 아니다. 같은 화풍·같은 교복이면 다른 캐릭터도 가깝게
    나온다 — 실제로 겹쳤는지는 사람이 보고 정한다.
    """
    from PIL import Image, ImageDraw

    # ① 데이터 자체가 겹치는 것 — 그림을 보기 전에 알려야 한다
    import collections

    ros = [m for m in members() if m["slug"] in set(targets)]
    for field in ("slug", "name"):
        dup = [k for k, n in collections.Counter(m[field] for m in ros).items() if n > 1]
        if dup:
            print(f"⚠️{field} 중복: {' '.join(dup)}")
    for label, path, key in (
        ("위키 문서", "wiki.json", lambda p: json.load(open(p))["title"]),
        ("묘사", "desc.txt", lambda p: open(p, encoding="utf-8").read().strip()),
    ):
        seen = collections.defaultdict(list)
        for m in ros:
            p = os.path.join(SRC, m["slug"], path)
            if os.path.exists(p):
                seen[key(p)].append(m["slug"])
        for v in (v for v in seen.values() if len(v) > 1):
            print(f"⚠️{label}가 같다: {' '.join(v)}")

    # ② 그림이 닮은 쌍
    have = [s for s in targets if os.path.exists(dst_profile(s))]
    h = {s: phash(dst_profile(s)) for s in have}
    pairs = sorted(
        (sum(x != y for x, y in zip(h[a], h[b])), a, b)
        for i, a in enumerate(sorted(have))
        for b in sorted(have)[i + 1 :]
    )[:top]
    if not pairs:
        print("비교할 스프라이트가 부족하다")
        return 1

    pad, label_h, gap = 6, 14, 18
    rw = cell * 4 + gap + pad
    rh = cell + label_h + pad
    im = Image.new("RGB", (rw + pad, len(pairs) * rh + pad), (24, 26, 31))
    draw = ImageDraw.Draw(im)

    for n, (d, a, b) in enumerate(pairs):
        y0 = pad + n * rh
        cells = [
            (os.path.join(SRC, a, "ref.png"), 0),
            (dst_profile(a), 1),
            (dst_profile(b), 2),
            (os.path.join(SRC, b, "ref.png"), 3),
        ]
        for path, i in cells:
            x = pad + i * cell + (gap if i >= 2 else 0)
            if not os.path.exists(path):
                draw.rectangle((x, y0, x + cell, y0 + cell), outline=(70, 60, 60))
                continue
            th = Image.open(path).convert("RGBA")
            th.thumbnail((cell, cell), Image.LANCZOS)
            bgc = Image.new("RGB", th.size, (24, 26, 31))
            bgc.paste(th, (0, 0), th)
            im.paste(bgc, (x + (cell - th.width) // 2, y0 + (cell - th.height) // 2))
        # 가운데 경계 — 어느 둘을 비교하는 줄인지 눈에 박히게
        draw.line((pad + cell * 2 + gap // 2, y0, pad + cell * 2 + gap // 2, y0 + cell),
                  fill=(60, 64, 74))
        draw.text((pad + 2, y0 + cell + 2), f"{a}  ·  {b}    거리 {d}/256",
                  fill=(150, 158, 170))

    im.save(out)
    for d, a, b in pairs:
        print(f"  {d:3}/256  {a} ↔ {b}")
    print(f"가까운 쌍 {len(pairs)} → {out}  ({im.width}x{im.height})")
    print(f"가장 가까운 거리 {pairs[0][0]}/256 — 0 이면 같은 그림이다")
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["gen", "install", "status", "sheet", "dupes"])
    ap.add_argument("slugs", nargs="*")
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--ref", action="store_true",
                    help="공식 포트레이트를 정체성 참조로 넘긴다(느리지만 색이 안 어긋난다)")
    ap.add_argument("--out", default="/tmp/theme-sheet.png", help="sheet/dupes 저장 경로")
    ap.add_argument("--top", type=int, default=12, help="dupes: 가까운 쌍 몇 개까지 볼지")
    a = ap.parse_args()

    targets = a.slugs or slugs()
    if a.cmd == "status":
        d = [s for s in targets if desc_of(s)]
        g = [s for s in targets if generated(s)]
        i = [s for s in targets if installed(s)]
        print(f"로스터 {len(targets)} · desc {len(d)} · 생성 {len(g)} · 설치 {len(i)}")
        rest = [s for s in targets if desc_of(s) and not generated(s)]
        if rest:
            print(f"생성 대기 {len(rest)}: {' '.join(rest[:20])}{' …' if len(rest) > 20 else ''}")
        nod = [s for s in targets if not desc_of(s)]
        if nod:
            print(f"desc 없음 {len(nod)}: {' '.join(nod[:20])}{' …' if len(nod) > 20 else ''}")
        return 0
    if a.cmd == "sheet":
        return sheet(targets, a.out)
    if a.cmd == "dupes":
        return dupes(targets, a.out, a.top)
    if a.cmd == "gen":
        if not os.path.exists(PPGEN):
            print(f"ppgen 이 없다: {PPGEN} (PPGEN 환경변수로 지정)", file=sys.stderr)
            return 2
        return run(generate, targets, a.jobs, a.force, use_ref=a.ref)
    return run(install, targets, a.jobs, a.force)


if __name__ == "__main__":
    sys.exit(main())
