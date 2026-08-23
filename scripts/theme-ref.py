#!/usr/bin/env python3
"""블루아카가 아닌 작품의 원화(ref.png)를 그 작품 위키에서 받는다.

`theme-wiki.py` 와 역할이 다르다. 그쪽은 **글**(외모 서술)을 긁어 묘사를 짓는 도구고,
이건 **그림**만 받는다. 묘사는 사람이 쓰고 원화는 정체성 참조(ppgen `-ref`)로만 쓰기
때문에, 작품이 바뀔 때마다 필요한 건 그림 한 장씩이다.

⚠️**파일명 규칙이 위키마다 다르고, 기본 API 로는 못 찾는다.** `prop=pageimages` 는
문서 대표 이미지를 주는데 그게 **가챠 카드 아트**라 캐릭터가 배경·이펙트에 파묻혀
정체성 참조로 못 쓴다(2026-08-24 실측, 명조·이터널리턴 첫 판이 전부 그렇게 나왔다).
그래서 `prop=images` 로 문서가 품은 파일을 전부 훑고 아래 패턴으로 전신 그림을 고른다.

    theme-ref.py <work> <slug>=<위키 문서 제목> ...
    theme-ref.py myeongjo jiyan="Jiyan" yangyang="Yangyang"

받은 그림은 `theme-src-<work>/<slug>/ref.png` 로 떨어진다. 이미 있으면 건너뛴다
(`--force` 로 덮어쓴다). 원화는 위키 원본이라 레포에 커밋하지 않는다(.gitignore).
"""
import json
import os
import re
import sys
import time
import urllib.parse
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# 작품 → (위키 서브도메인, 전신 그림 파일명 후보 패턴). 패턴은 앞에서부터 시도하고
# `{n}` 자리에 문서 제목이 들어간다. 정규식이라 특수문자는 escape 해서 넣는다.
WORKS = {
    # 명조: 캐릭터마다 배경 없는 전신 스프라이트가 한 장씩 있다.
    "myeongjo": ("wutheringwaves", [r"^{n} Full Sprite\.png$"]),
    # 이터널리턴: 파일명이 캐릭터 이름 그대로다. `<이름> Mini.png`(원형 아이콘)과
    # 스킨 아트가 같이 잡히므로 확장자까지 정확히 맞는 것만 고른다.
    "eternalreturn": ("eternalreturn", [r"^{n}\.png$"]),
    # 단간론파: 전신 스프라이트가 표정별로 여러 장(`(1)`~`(30)`)이라 번호가 붙는다.
    # 번호 없는 것을 먼저, 없으면 가장 작은 번호를 쓴다(대개 기본 표정).
    "danganronpa": ("danganronpa", [r"^{n} Fullbody Sprite\.png$",
                                    r"^{n} Fullbody Sprite \(\d+\)\.png$",
                                    r"^Danganronpa (\S+ )?{n} Fullbody Sprite.*\.png$",
                                    r"^{n} Transparent Illustration\.png$",
                                    r"^{n} Illustration\.png$"]),
}


def api(wiki, **params):
    params.update(format="json", formatversion="2")
    url = f"https://{wiki}.fandom.com/api.php?" + urllib.parse.urlencode(params)
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=40) as r:
                return json.load(r)
        except Exception as e:
            if attempt == 2:
                raise
            print(f"  재시도 {attempt + 1}/2 ({e})", file=sys.stderr)
            time.sleep(2 * (attempt + 1))


def pick(wiki, title, patterns):
    """문서가 품은 파일 중 전신 그림 하나를 고른다. (파일제목, 사유)."""
    d = api(wiki, action="query", prop="images", titles=title, imlimit=500)
    pages = d.get("query", {}).get("pages", [])
    if not pages or "missing" in pages[0]:
        return None, "문서 없음"
    names = [i["title"] for i in pages[0].get("images", [])]
    if not names:
        return None, "그림 없음"
    esc = re.escape(title)
    for pat in patterns:
        rx = re.compile(pat.replace("{n}", esc), re.I)
        # 번호가 붙는 패턴은 번호순으로 — 문자열 정렬은 (10) 을 (2) 앞에 둔다
        hit = sorted((n for n in names if rx.match(n.removeprefix("File:"))),
                     key=lambda n: int(m.group(1)) if (m := re.search(r"\((\d+)\)", n)) else 0)
        if hit:
            return hit[0], ""
    return None, f"패턴 불일치 (파일 {len(names)}장)"


def fetch(wiki, filetitle, dst):
    d = api(wiki, action="query", prop="imageinfo", titles=filetitle, iiprop="url")
    info = d["query"]["pages"][0].get("imageinfo")
    if not info:
        raise RuntimeError("imageinfo 없음")
    url = info[0]["url"]
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    with urllib.request.urlopen(req, timeout=90) as r:
        blob = r.read()
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    with open(dst, "wb") as f:
        f.write(blob)
    return len(blob)


def main():
    args = sys.argv[1:]
    force = "--force" in args
    args = [a for a in args if a != "--force"]
    if len(args) < 2 or args[0] not in WORKS:
        print(f"사용법: theme-ref.py <{'|'.join(WORKS)}> <slug>=<문서제목> ... [--force]")
        return 1
    work, pairs = args[0], args[1:]
    wiki, patterns = WORKS[work]
    ok = fail = 0
    for pair in pairs:
        slug, _, title = pair.partition("=")
        title = title or slug
        dst = os.path.join(ROOT, f"theme-src-{work}", slug, "ref.png")
        if os.path.exists(dst) and not force:
            print(f"  {slug:<12} skip")
            continue
        try:
            ft, why = pick(wiki, title, patterns)
            if not ft:
                print(f"  {slug:<12} FAIL  {why}")
                fail += 1
                continue
            n = fetch(wiki, ft, dst)
            print(f"  {slug:<12} ok    {ft.removeprefix('File:')} ({n // 1024}KB)")
            ok += 1
        except Exception as e:
            print(f"  {slug:<12} FAIL  {e}")
            fail += 1
    print(f"ok {ok}, fail {fail}")
    return 1 if fail else 0


if __name__ == "__main__":
    sys.exit(main())
