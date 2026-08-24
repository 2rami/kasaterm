#!/usr/bin/env python3
"""호요버스 위키(원신·스타레일·젠레스)용 캐릭터 조사기.

`theme-wiki.py` 와 하는 일은 같지만 **위키 구조가 달라 그대로는 못 쓴다**:

- 한글 이름이 `name_kr` 이 아니라 `{{Other Languages}}` 의 `|ko` 에 있고,
  `{{tt|호두|胡桃}}` 처럼 템플릿에 싸여 있는 경우가 있다.
- 외형 서술이 본문에 없다. 스타레일은 `<이름>/Lore` 의 Appearance 절에 있고,
  **원신은 아예 없다** — 원신 묘사는 참조 이미지를 보고 써야 한다.
- 참조 이미지 파일명 규칙이 다르다(원신 `<이름> Card.png` 전신,
  스타레일 `Character <이름> Splash Art.png`).

    THEME_WIKI_API=... THEME_ROSTER=... THEME_SRC=... theme-wiki-hoyo.py index
    THEME_WIKI_API=... THEME_ROSTER=... THEME_SRC=... theme-wiki-hoyo.py collect

**언제 지우면 되나**: 위 세 꼴을 `theme-wiki.py` 본체가 흡수하고, 그 본체로 호요 위키
한 명을 실제로 수집해 본 뒤다. 통합 자체는 어렵지 않지만 **미검증 통합보다 돌아간
변종을 남기는 쪽이 싸다** — 이 파일은 원신·스타레일 42명의 실데이터를 만든 코드다.
"""
import json, os, re, sys, time, urllib.parse, urllib.request

WIKI = os.environ.get("THEME_WIKI_API", "https://genshin-impact.fandom.com/api.php")
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ROSTER = os.environ.get("THEME_ROSTER", os.path.join(ROOT, "theme-src-genshin/roster.json"))
OUT = os.environ.get("THEME_SRC", os.path.join(ROOT, "theme-src-genshin"))
INDEX = os.path.join(OUT, "_index.json")
SECTIONS = ("Appearance", "Personality", "Character Introduction",
            "Official Introduction", "Character Stories")


def api(**params):
    params.update(format="json", formatversion="2")
    url = WIKI + "?" + urllib.parse.urlencode(params)
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=40) as r:
                return json.load(r)
        except Exception as e:
            if attempt == 2:
                raise
            print(f"  재시도 {attempt+1}/2 ({e})", file=sys.stderr)
            time.sleep(2 * (attempt + 1))


def roster():
    d = json.load(open(ROSTER, encoding="utf-8"))
    # 리더도 그림이 필요하다 — members 에만 있는 게 아니다.
    return [m for m in d.get("leaders", []) + d["members"] if m.get("slug")]


def ko_name(text):
    """한글 이름을 뽑는다. 원문이 세 꼴로 온다 — 앞의 둘을 놓쳐 8명이 비었다(2026-08-22).

        |ko = 벤티                    평문
        |ko = {{tt|감우|甘雨}}         한자 병기 템플릿 — 첫 인자가 한글
        |ko = {{tt|//종려//|鍾離}}     거기에 강조 슬래시까지 붙은 것
        |1_ko = 웰트                  이름이 여럿인 캐릭터는 번호가 붙는다(스타레일)
    """
    m = re.search(r"\|\s*(?:\d+_)?ko\s*=\s*(.+)", text)
    if not m:
        return None
    v = m.group(1).strip()
    tt = re.search(r"\{\{tt\|([^|}]+)", v)
    v = tt.group(1) if tt else re.sub(r"\{\{[^}]*\}\}", "", v)
    v = v.replace("/", "").strip().strip("|")
    return v or None


def build_index(category="Category:Playable Characters"):
    members = api(action="query", list="categorymembers", cmtitle=category, cmlimit=500)
    titles = [m["title"] for m in members["query"]["categorymembers"] if ":" not in m["title"]]
    print(f"{category}: {len(titles)}개 문서")
    index = {}
    for i in range(0, len(titles), 20):
        d = api(action="query", titles="|".join(titles[i:i+20]), prop="revisions",
                rvprop="content", rvslots="main")
        for p in d.get("query", {}).get("pages", []):
            if "revisions" not in p:
                continue
            kr = ko_name(p["revisions"][0]["slots"]["main"]["content"])
            if not kr:
                continue
            # 「카미사토 아야카」를 로스터는 「아야카」로만 적는다 — 낱말 별칭도 건다.
            keys = [kr] + ([kr.split()[-1], kr.split()[0]] if " " in kr else [])
            for i, k in enumerate(keys):
                if i == 0 or k not in index:
                    index[k] = p["title"]
        print(f"  {min(i+20, len(titles))}/{len(titles)}")
        time.sleep(0.2)
    os.makedirs(OUT, exist_ok=True)
    json.dump(index, open(INDEX, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(f"인덱스 {len(index)}개 → {INDEX}")
    return index


def clean(s):
    s = re.sub(r"\[\[([^\]|]*\|)?([^\]]*)\]\]", r"\2", s)
    # 본문을 담는 템플릿은 인자를 살린다. 통째로 지우면 절이 빈 문자열이 되고,
    # 빈 절은 버려지므로 「성격 자료가 아예 없다」로 보인다(2026-08-22, 스타레일 21명 전원).
    s = re.sub(r"\{\{(?:Description|Quote|Story)\|(.*?)\}\}", r"\1", s, flags=re.S | re.I)
    s = re.sub(r"<br\s*/?>", " ", s)
    s = re.sub(r"\{\{[^{}]*\}\}", "", s)
    s = re.sub(r"<[^>]+>", "", s)
    s = re.sub(r"'{2,}", "", s)
    s = re.sub(r"^=+\s*", "", s, flags=re.M)
    return " ".join(s.split())


def sections(text):
    out = {}
    for name in SECTIONS:
        m = re.search(r"==+\s*" + name + r"\s*==+(.*?)(?=\n=+\s*\w|\Z)", text, re.S | re.I)
        if m:
            body = clean(m.group(1))
            if body:
                out[name.lower().replace(" ", "_")] = body[:1800]
    return out


def infobox(text):
    """소속·원소/운명·무기 — 페르소나의 「소속」 칸 근거."""
    out = {}
    for f in ("element", "path", "weapon", "region", "affiliation", "faction",
              "type", "attribute", "title", "constellation"):
        m = re.search(r"\|\s*" + f + r"\s*=\s*([^\n|]+)", text, re.I)
        if m:
            v = clean(m.group(1))
            if v:
                out[f] = v[:120]
    return out


def portrait(title):
    """전신 일러스트를 우선한다 — 스프라이트 참조라 얼굴만으론 부족하다."""
    short = title
    cands = [f"File:{short} Card.png", f"File:Character {short} Splash Art.png",
             f"File:{short} Wish.png", f"File:Character {short} Portrait.png",
             f"File:{short} Icon.png", f"File:Character {short} Icon.png"]
    d = api(action="query", titles="|".join(cands), prop="imageinfo", iiprop="url|size")
    got = {p["title"]: p["imageinfo"][0] for p in d.get("query", {}).get("pages", [])
           if "imageinfo" in p}
    for c in cands:  # 후보 순서가 곧 선호 순서다
        if c in got:
            return got[c]["url"], c
    return None


def fetch(url, dst, force=False):
    if os.path.exists(dst) and not force:
        return True
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            data = r.read()
    except Exception as e:
        print(f"    포트레이트 실패: {e}", file=sys.stderr)
        return False
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    open(dst, "wb").write(data)
    return True


def page(title):
    d = api(action="query", titles=title, prop="revisions", rvprop="content",
            rvslots="main", redirects=1)
    pages = d.get("query", {}).get("pages", [])
    if not pages or "revisions" not in pages[0]:
        return None, None
    return pages[0]["title"], pages[0]["revisions"][0]["slots"]["main"]["content"]


def collect(names=None, force=False):
    index = json.load(open(INDEX, encoding="utf-8")) if os.path.exists(INDEX) else {}
    todo = roster()
    if names:
        todo = [m for m in todo if m["name"] in names]
    missing, done = [], 0
    for m in todo:
        dst = os.path.join(OUT, m["slug"], "wiki.json")
        if os.path.exists(dst) and not force:
            done += 1
            continue
        title = index.get(m["name"]) or m.get("wiki_title")
        if not title:
            missing.append(m["name"])
            continue
        real, text = page(title)
        if not text:
            missing.append(m["name"])
            continue
        rec = {"name": m["name"], "slug": m["slug"], "school": m.get("school", ""),
               "title": real, "sections": sections(text), "infobox": infobox(text)}
        # 외형은 본문에 없다 — 서브문서에서 마저 긁는다.
        for sub in ("/Lore", "/Profile"):
            _, t2 = page(title + sub)
            if t2:
                for k, v in sections(t2).items():
                    rec["sections"].setdefault(k, v)
        p = portrait(real)
        if p:
            rec["portrait"] = {"url": p[0], "file": p[1]}
            fetch(p[0], os.path.join(OUT, m["slug"], "ref.png"), force)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        json.dump(rec, open(dst, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
        done += 1
        got = ",".join(rec["sections"]) or "-"
        print(f"  {m['name']:10} → {real:24} 절[{got}] {'ref' if p else 'REF없음'}")
        time.sleep(0.2)
    print(f"수집 {done}/{len(todo)}")
    if missing:
        print(f"못 찾음 {len(missing)}: {', '.join(missing)}")
    return 0


if __name__ == "__main__":
    args = sys.argv[1:]
    cmd = args[0] if args else "collect"
    if cmd == "index":
        build_index(os.environ.get("THEME_WIKI_CATEGORY", "Category:Playable Characters"))
    elif cmd == "collect":
        rest = [a for a in args[1:] if not a.startswith("-")]
        sys.exit(collect(rest or None, force="--force" in args))
    else:
        print(__doc__)
        sys.exit(2)
