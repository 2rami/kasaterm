#!/usr/bin/env python3
"""god 모드 방에서 이 pane 에 캐릭터를 할당한다 — 마커 선점 + 헤더 rename.
말투(persona)는 board-context.py 가 매 턴 additionalContext 로 단독 주입하므로
(append-system-prompt 중복 제거 → persona 단일 경로 일원화), 여기선 마커만 박는다.

characters.json 우선순위: ~/.config/kasaterm/characters.json → 번들(이 스크립트
옆) → 둘 다 없으면 no-op(기능 전체 skip = 현행 무변화). 번들 기본 파일은 만들지
않는다 — 오리지널 테마는 사용자가 나중에 채운다.

할당: 방(cwd slug)에 character 마커가 하나도 없으면 leader, 있으면 members 중
아직 안 쓰인 첫 번째. /tmp/kasaterm-collab/<slug>/character-<pane> 마커(내용=이름).
동시 스폰 race 는 flock 으로 직렬화. 새로 할당할 때만 헤더를 '● <이름>' 으로 rename
(재스폰/재호출은 마커 그대로 — idempotent no-op).
"""
import os, json, glob, subprocess

try:
    import fcntl
except ImportError:
    fcntl = None


def load_characters():
    paths = [
        os.path.expanduser("~/.config/kasaterm/characters.json"),
        os.path.join(os.path.dirname(os.path.abspath(__file__)), "characters.json"),
    ]
    for p in paths:
        try:
            return json.load(open(p))
        except Exception:
            continue
    return None


def main():
    pane = os.environ.get("KASATERM_PANE_ID")
    if not pane:
        return
    chars = load_characters()
    if not chars:
        return  # characters 없음 → 기능 skip(현행)

    slug = os.getcwd().replace("/", "-").replace(".", "-")
    base = f"/tmp/kasaterm-collab/{slug}"
    os.makedirs(base, exist_ok=True)
    my_marker = os.path.join(base, "character-" + pane.lstrip("%"))

    name = None
    newly_assigned = False
    lf = open(os.path.join(base, "character.lock"), "w")
    if fcntl is not None:
        fcntl.flock(lf.fileno(), fcntl.LOCK_EX)
    try:
        if os.path.exists(my_marker):
            name = open(my_marker).read().strip()
        else:
            used = set()
            for m in glob.glob(os.path.join(base, "character-*")):
                if m.endswith(".lock"):
                    continue
                try:
                    used.add(open(m).read().strip())
                except Exception:
                    pass
            # god 풀(leaders) 이 있으면 방(slug) 해시로 god 캐릭터 선택 — 방마다
            # 아로나/프라나 다르게(거노). 없으면 leader 단일(현행 호환).
            pool = chars.get("leaders") or ([chars["leader"]] if chars.get("leader") else [])
            god = pool[sum(slug.encode()) % len(pool)] if pool else {}
            if god.get("name") and god.get("name") not in used:
                name = god.get("name")
            else:
                cand = next((m for m in (chars.get("members") or [])
                             if m.get("name") and m.get("name") not in used), None)
                name = cand.get("name") if cand else None
            if name:
                tmp = my_marker + ".tmp"
                open(tmp, "w").write(name)
                os.replace(tmp, my_marker)
                newly_assigned = True
    finally:
        if fcntl is not None:
            fcntl.flock(lf.fileno(), fcntl.LOCK_UN)
        lf.close()

    if not name:
        return  # 캐릭터가 다 찼음(members 초과) → persona 없이

    # 새 할당일 때만 헤더 rename(재스폰은 이미 붙어 있음 — 강제 제출 스팸 방지).
    if newly_assigned:
        cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
        try:
            subprocess.run([cli, "rename", pane, f"● {name}"],
                           timeout=2, capture_output=True)
        except Exception:
            pass


if __name__ == "__main__":
    main()
