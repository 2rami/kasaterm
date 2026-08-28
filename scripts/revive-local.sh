#!/bin/zsh
# 원격(다른 기계)으로 이사 보낸 학생을 **이 기계에서 다시 켠다**.
#
# 언제 쓰나 — 이사 뒤 원격에 못 닿게 됐을 때(네트워크가 바뀌어 터널이 끊긴 경우).
# 그때 pane 은 원격 링크를 문 채라 화면이 이사 순간에 얼어붙고, 입력은 원격으로
# 새어 나가 아무 반응이 없다. 저장된 대화 id 는 그대로 남아 있으므로 링크만 걷어
# 내면 복원이 `claude --resume` 으로 같은 대화를 이 기계에서 이어 준다.
#
# ⚠️ pane 안에서 돌리지 마라 — 앱을 끄는 순간 그 pane 째 죽는다.
#    `! scripts/revive-local.sh` 로 돌린다.
#
# 원격 쪽 세션은 **안 죽인다**. 닿지 않을 뿐 살아 있고, 그 기계로 돌아가면 그대로
# 있다. 여기서 이어가는 대화와 갈라지는 것은 감수하는 것이다 — 되돌릴 길이 아직
# 없어서, 지금 일을 이어가는 쪽을 택한다.
set -u
CFG=$HOME/.config/kasaterm/session.json
APP=$HOME/Applications/kasaterm.app

pid=$(pgrep -f "$APP/Contents/MacOS/kasaterm" | head -1)
if [[ -n "$pid" ]]; then
  echo "앱 종료 (pid $pid)"
  kill "$pid"
  for i in {1..40}; do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
  kill -0 "$pid" 2>/dev/null && { echo "앱이 안 꺼졌다 — 멈춘다"; exit 1; }
fi

cp "$CFG" "$CFG.bak-$(date +%Y%m%d-%H%M%S)" && echo "백업 완료"

python3 - "$CFG" <<'PY'
import json, sys, pathlib

# 되살릴 폴더는 **대화 파일이 실제로 있는 자리**에서 역산한다. 손으로 적은 표를
# 쓰면 안 된다 — `--resume` 은 cwd 로 대화 폴더를 찾으므로 한 칸만 어긋나도 그
# 학생만 조용히 빈 세션으로 뜬다(실측: 넷이 어긋나 있었다).
def slug(p):
    s = str(p)
    for ch in "/_.":
        s = s.replace(ch, "-")
    return s

home = pathlib.Path.home()
cands = [home, home / "Desktop", home / "Desktop/momewomo"]
root = home / "Desktop/momewomo"
if root.is_dir():
    for d in root.iterdir():
        if d.is_dir():
            cands.append(d)
            # 회사 레포는 한 겹 더 들어가 있다
            if d.name == "sionic":
                cands.extend(x for x in d.iterdir() if x.is_dir())
by_slug = {slug(c): c for c in cands}

# sid 앞 8자 → 그 대화가 놓인 폴더
home_by_sid = {}
for f in (home / ".claude/projects").rglob("*.jsonl"):
    got = by_slug.get(f.parent.name)
    if got:
        home_by_sid[f.stem] = str(got)

p = pathlib.Path(sys.argv[1])
d = json.loads(p.read_text())
n = 0

def walk(node):
    global n
    if isinstance(node, dict):
        leaf = node.get("leaf") if isinstance(node.get("leaf"), dict) else None
        tgt = leaf if leaf is not None else (node if "pane_id" in node else None)
        if tgt is not None and tgt.get("remote_base"):
            pane = tgt.get("pane_id", "?")
            tgt.pop("remote_base", None)
            tgt.pop("remote_pane", None)
            sid = tgt.get("session_id") or ""
            cwd = home_by_sid.get(sid)
            if cwd:
                tgt["cwd"] = cwd
                tgt["was_agent"] = "claude"   # 복원이 --resume 을 걸 조건
            n += 1
            print(f"  {pane:>5} {cwd or '(대화 없음 — 셸로만 뜬다)'}")
        for v in node.values():
            walk(v)
    elif isinstance(node, list):
        for v in node:
            walk(v)

walk(d)
p.write_text(json.dumps(d, ensure_ascii=False))
print(f"원격 링크 {n}개 해제")
PY

echo "앱 시작"
open -a "$APP"
