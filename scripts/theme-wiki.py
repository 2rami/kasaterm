#!/usr/bin/env python3
"""테마 로스터의 캐릭터 정보를 공식 위키에서 수집한다.

파이프라인의 **조사 단계**다. 스프라이트 생성기(`ppgen`)는 캐릭터를 텍스트 한 줄로만
받으므로, 그 한 줄의 품질이 결과를 정한다 — 「분홍 머리 소녀」로는 아무나 나오고
「뒤집힌 분홍 헤일로, 형광 분홍 안감 재킷」이면 그 캐릭터가 나온다. 그 묘사를 사람이
기억으로 쓰면 틀리므로 위키에서 긁는다.

두 단계로 나눠 둔 이유는 **이름 매칭이 어려워서**다. 로스터엔 「케이」라고 적혀 있는데
위키 문서 제목은 `Tendou Kei` 이고, 로마자 변환으로는 못 맞춘다(아리스=Alice).
그래서 위키의 `name_kr` 필드로 한글→문서제목 인덱스를 한 번 만들어 두고(`index`),
그 인덱스로 조회한다(`collect`).

    theme-wiki.py index                     # 한글 이름 → 문서 제목 (한 번만)
    theme-wiki.py collect                   # 로스터 전원 수집
    theme-wiki.py collect 케이 미도리        # 일부만

수집물은 `theme-src/<slug>/wiki.json` 에 쌓인다. 이미 있으면 건너뛰므로 중간에 끊겨도
다시 부르면 이어서 받는다(`--force` 로 덮어쓴다).
"""
import json
import os
import re
import sys
import time
import urllib.parse
import urllib.request

WIKI = os.environ.get("THEME_WIKI_API", "https://bluearchive.fandom.com/api.php")
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# 로스터·출력 자리는 `theme-sprites.py` 와 **같은 환경변수**를 쓴다. 두 스크립트가
# 한 파이프라인의 앞뒤인데 이쪽만 번들 고정이면, 새 테마를 조사하라고 불러도 조용히
# 블루아카 명부를 읽고 `theme-src/` 에 쌓는다(오류가 안 난다).
ROSTER = os.environ.get("THEME_ROSTER") or os.path.join(
    ROOT, "app/kasaterm/collab-hooks/characters.json"
)
OUT = os.environ.get("THEME_SRC") or os.path.join(ROOT, "theme-src")
INDEX = os.path.join(OUT, "_index.json")
# 문서 하나에 관심 있는 절만. 나머지(전투 수치·스토리)는 프롬프트에 쓸모가 없다.
# 절 이름도 위키마다 다르다 — BA 는 `Halo`/`Uniform`, 프로젝트 세카이는
# `Casual Outfit`/`Unit Outfit` 이다. 없는 절은 조용히 건너뛰므로 넉넉히 적어 둔다.
SECTIONS = tuple(
    s.strip()
    for s in os.environ.get(
        "THEME_WIKI_SECTIONS",
        "Appearance,Halo,Uniform,Personality,Casual Outfit,Unit Outfit",
    ).split(",")
    if s.strip()
)
# 현지화 이름이 담긴 인포박스 필드 — 위키마다 이름이 다르다(BA `name_kr`,
# 프로젝트 세카이 `hangul`). 먼저 맞는 것을 쓴다.
KR_FIELDS = ("name_kr", "hangul")
# 캐릭터 문서를 모아 둔 카테고리. BA 는 `Students`, 다른 작품은 대개 `Characters` 다.
CATEGORY = os.environ.get("THEME_WIKI_CATEGORY", "Category:Students")


def api(**params):
    params.update(format="json", formatversion="2")
    url = WIKI + "?" + urllib.parse.urlencode(params)
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=40) as r:
                return json.load(r)
        except Exception as e:  # 위키는 가끔 503 을 준다 — 한 번 튕겼다고 전체를 버리지 않는다
            if attempt == 2:
                raise
            print(f"  재시도 {attempt + 1}/2 ({e})", file=sys.stderr)
            time.sleep(2 * (attempt + 1))


def roster():
    """전원 — `leader`·`leaders` 까지. `theme-sprites.py members()` 와 같은 규칙이다.

    `members` 만 보면 비서 캐릭터가 조사에서 빠지고, 그러면 그 한 명만 묘사도
    참조 그림도 없이 남는다. 그림 쪽은 저쪽 주석이 이미 같은 함정을 막아 뒀는데
    조사 쪽만 안 따라와 있었다.
    """
    d = json.load(open(ROSTER, encoding="utf-8"))
    pool = [d.get("leader")] + list(d.get("leaders") or []) + list(d.get("members") or [])
    out, seen = [], set()
    for m in pool:
        slug = (m or {}).get("slug")
        if slug and slug not in seen:
            seen.add(slug)
            out.append(m)
    return out


def claim(index, key, title, kr, variant):
    """한 이름을 여럿이 노릴 때 **기본 문서**가 이긴다.

    수영복·대체 의상 문서도 같은 `name_kr` 을 달고 있어서, 그냥 대입하면 나중에 온
    쪽이 기본 문서를 덮는다. 그래도 오류가 안 나고 수집물이 그럴듯해서 **스프라이트를
    다 굽고 눈으로 볼 때까지 모른다** — 칸나는 수영복, 시로코는 Terror 로 잡혀 있었다
    (2026-08-11).

    ⚠️제목만 봐서는 못 가른다. 수영복은 `(Swimsuit ver.)` 가 붙지만 「Shiroko Terror」
    는 괄호도 없고 name_kr 도 기본과 **글자까지 같다**. 정본은 위키의 `Variant`
    카테고리다 — 제목 규칙은 그 카테고리가 없을 때 쓰는 보조 순위일 뿐이다.
    """
    rank = (0 if variant else 1, 0 if "(" in title else 1, len(kr.split()))
    prev = index.get(key)
    if prev is None or rank > prev[1]:
        index[key] = (title, rank)


def kr_name(text):
    """인포박스에서 현지화 이름을 뽑는다.

    필드 이름이 위키마다 다르다 — 블루아카는 `name_kr`, 프로젝트 세카이는 `hangul`
    이다. 그리고 값에 로마자가 괄호로 따라붙는 위키가 있다(`호시노 이치카 (Hoshino
    Ichika)`). 그대로 키로 쓰면 로스터의 「이치카」와 영영 안 만나므로 괄호를 턴다.
    """
    for f in KR_FIELDS:
        m = re.search(r"\|\s*" + f + r"\s*=\s*([^\n|]+)", text)
        if not m:
            continue
        kr = re.sub(r"\s*\(.*$", "", m.group(1)).strip()
        if kr:
            return kr
    return None


def build_index(category=None):
    """위키 전체를 훑어 한글 이름 → 문서 제목 표를 만든다."""
    category = category or CATEGORY
    members = api(
        action="query", list="categorymembers", cmtitle=category, cmlimit=500
    )["query"]["categorymembers"]
    titles = [m["title"] for m in members if ":" not in m["title"]]
    print(f"{category}: {len(titles)}개 문서")

    index = {}
    for i in range(0, len(titles), 20):
        batch = titles[i : i + 20]
        d = api(
            action="query",
            titles="|".join(batch),
            prop="revisions|categories",
            rvprop="content",
            rvslots="main",
            cllimit="max",
        )
        for p in d.get("query", {}).get("pages", []):
            if "revisions" not in p:
                continue
            variant = any(
                c["title"] == "Category:Variant" for c in p.get("categories", [])
            )
            text = p["revisions"][0]["slots"]["main"]["content"]
            kr = kr_name(text)
            if not kr:
                continue
            # 「텐도 케이」와 「케이」 둘 다 키로 넣는다 — 로스터는 이름만 쓴다.
            claim(index, kr, p["title"], kr, variant)
            parts = kr.split()
            if len(parts) > 1:
                claim(index, parts[-1], p["title"], kr, variant)
        print(f"  {min(i + 20, len(titles))}/{len(titles)}")
        time.sleep(0.3)

    index = {k: v[0] for k, v in index.items()}
    os.makedirs(OUT, exist_ok=True)
    json.dump(index, open(INDEX, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(f"인덱스 {len(index)}개 → {INDEX}")
    return index


def clean(s):
    """위키 마크업을 프롬프트에 넣을 수 있는 평문으로."""
    s = re.sub(r"\[\[([^\]|]*\|)?([^\]]*)\]\]", r"\2", s)  # [[A|B]] → B
    s = re.sub(r"\{\{[^{}]*\}\}", "", s)  # 템플릿
    s = re.sub(r"<[^>]+>", "", s)  # html
    s = re.sub(r"'{2,}", "", s)  # 굵게/기울임
    s = re.sub(r"^=+\s*", "", s, flags=re.M)  # 잘려 들어온 헤딩 꼬리
    return " ".join(s.split())


def sections(text):
    out = {}
    for name in SECTIONS:
        # 다음 절에서 멈춘다. lookahead 가 `\n==[^=]` 였을 땐 `=== Uniform ===` 같은
        # 하위 절을 못 잘라 Halo 안에 Uniform 이 통째로 딸려 들어왔다 — 레벨을 가리지
        # 않고 `\n=+` 로 끊어야 한다.
        m = re.search(
            r"==+\s*" + name + r"\s*==+(.*?)(?=\n=+\s*\w|\Z)", text, re.S | re.I
        )
        if m:
            body = clean(m.group(1))
            if body:
                out[name.lower()] = body[:1500]
    return out


def portrait_url(title):
    """문서의 대표 이미지 URL.

    파일명 규칙이 위키마다 다르다 — BA 는 `<이름> Portrait.png`, 프로젝트 세카이는
    `<이름>.png` 와 `<이름> (icon).png` 다. 후보를 한 번에 물어보고 **가장 큰 것**을
    고른다(작은 아이콘보다 전신 아트가 정체성 참조로 낫다). 없는 후보는 응답에서
    빠질 뿐이라 넉넉히 넣어도 비용이 같다.
    """
    short = title.split()[-1]
    cands = [
        f"File:{short} Portrait.png",
        f"File:{short} Icon.png",
        f"File:{short}.png",
        f"File:{short} (icon).png",
    ]
    d = api(action="query", titles="|".join(cands), prop="imageinfo", iiprop="url|size")
    best = None
    for p in d.get("query", {}).get("pages", []):
        if "imageinfo" not in p:
            continue
        ii = p["imageinfo"][0]
        # 큰 쪽이 참조 이미지로 낫다(Portrait > Icon).
        if best is None or ii["width"] * ii["height"] > best[1]:
            best = (ii["url"], ii["width"] * ii["height"], p["title"])
    return best


def fetch_portrait(url, dst, force=False):
    """참조 이미지를 내려받는다 — URL 만 적어 두면 아무도 못 쓴다.

    `theme-sprites.py gen --ref` 는 `theme-src/<slug>/ref.png` 를 찾고, 없으면
    「theme-wiki.py 로 포트레이트를 먼저 받아라」로 멈춘다. 그런데 그 받는 코드가
    여기 없어서 지금까지 손으로 받아 왔다 — 그래서 시로코는 참조 없이, 칸나는 묘사만으로
    구워졌다(2026-08-11).

    ⚠️저작물이라 레포에 넣지 않는다. `.gitignore` 가 `theme-src/*/ref.png` 를 막는다.
    """
    if os.path.exists(dst) and not force:
        return
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    try:
        with urllib.request.urlopen(req, timeout=40) as r:
            data = r.read()
    except Exception as e:
        print(f"    포트레이트 실패: {e}", file=sys.stderr)
        return
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    open(dst, "wb").write(data)


def collect(names=None, force=False):
    if not os.path.exists(INDEX):
        print("인덱스가 없다 — 먼저 `theme-wiki.py index` 를 돌려라", file=sys.stderr)
        return 1
    index = json.load(open(INDEX, encoding="utf-8"))
    todo = roster()
    if names:
        todo = [m for m in todo if m["name"] in names]

    missing, done = [], 0
    for m in todo:
        dst = os.path.join(OUT, m["slug"], "wiki.json")
        if os.path.exists(dst) and not force:
            done += 1
            continue
        title = index.get(m["name"])
        if not title:
            missing.append(m["name"])
            continue

        d = api(
            action="query",
            titles=title,
            prop="revisions",
            rvprop="content",
            rvslots="main",
        )
        pages = d.get("query", {}).get("pages", [])
        if not pages or "revisions" not in pages[0]:
            missing.append(m["name"])
            continue
        text = pages[0]["revisions"][0]["slots"]["main"]["content"]

        rec = {
            "name": m["name"],
            "slug": m["slug"],
            "school": m.get("school", ""),
            "title": title,
            "sections": sections(text),
        }
        for f in ("name_jp", "school", "club"):
            mm = re.search(r"\|\s*" + f + r"\s*=\s*([^\n|]+)", text)
            if mm and mm.group(1).strip():
                rec.setdefault("wiki", {})[f] = mm.group(1).strip()
        p = portrait_url(title)
        if p:
            rec["portrait"] = {"url": p[0], "file": p[2]}
            fetch_portrait(p[0], os.path.join(OUT, m["slug"], "ref.png"), force)

        os.makedirs(os.path.dirname(dst), exist_ok=True)
        json.dump(rec, open(dst, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
        done += 1
        print(f"  {m['name']:8} → {title}")
        time.sleep(0.3)

    print(f"수집 {done}/{len(todo)}")
    if missing:
        print(f"못 찾음 {len(missing)}: {', '.join(missing)}")
        print("→ theme-src/_index.json 에 그 이름을 직접 넣어라(값은 위키 문서 제목)")
    return 0


if __name__ == "__main__":
    args = sys.argv[1:]
    cmd = args[0] if args else "collect"
    if cmd == "index":
        build_index()
    elif cmd == "collect":
        rest = [a for a in args[1:] if not a.startswith("-")]
        sys.exit(collect(rest or None, force="--force" in args))
    else:
        print(__doc__)
        sys.exit(2)
