#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "python-dotenv",
# ]
# ///

"""
Claude Code Statusline — kasaterm 미니멀.

    <학생얼굴> ┃ <model> Opus ┃ <git> main ┃ <dir> tmuxify ┃ 8% ┃ <bolt> xhigh

- 학생: kasaterm 안에선 U+FFFC 자리표시자를 내보내 kasaterm 이 그 자리에 배정 학생
  프로필 얼굴 이미지(아로나 웹뷰 프사)를 얹는다. 밖(일반 터미널)에선 ●+이름 폴백.
- effort: stdin `effort.level`(/effort 시 실시간 갱신), 레벨별 색.
- permission 모드는 표시 안 함 — 하단 기본 힌트 줄(bypass permissions on)과 중복.
"""

import json
import os
import sys
import subprocess
from pathlib import Path

try:
    from dotenv import load_dotenv
    load_dotenv()
except ImportError:
    pass

RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"

# 학생명 → accent hex (kasaterm theme.rs character_accent 와 동일 값)
STUDENT_HEX = {
    "아로나": "4a90e2", "프라나": "e6e9f0", "미도리": "6bcf7f",
    "모모이": "ff6b6b", "유즈": "e64980", "아리스": "4c6ef5",
    "유우카": "7a5fd4", "시로코": "8fb8d8", "호시노": "f2a0c0",
    "코하루": "f27b9b", "히마리": "a88be0", "아루": "e85d4a",
}
# effort 레벨 → 색 (낮음=차분, 높음=경고색)
EFFORT_HEX = {
    "low": "565f89", "medium": "7aa2f7", "high": "e0af68",
    "xhigh": "f7768e", "max": "bb9af7",
}
# 팔레트 (Tokyo Night 계열)
C_MODEL, C_GIT, C_DIR, C_CTX, C_SEP, C_FALLBACK = (
    "7aa2f7", "73daca", "bb9af7", "ff9e64", "565f89", "a0a6b0",
)

ICON_SETS = {
    "nerd-font": {"model": "", "git": "", "folder": "", "effort": ""},
    "unicode": {"model": ">", "git": "⎇", "folder": "▸", "effort": "↯"},
    "plain": {"model": "M", "git": "git", "folder": "dir", "effort": "E"},
}

# 학생 프사 자리표시자 — kasaterm 이 이 U+FFFC 연속을 감지해 프사(bust)로
# 대체(statusline 행 바닥 정렬, 위로 2행 높이). 5칸이어야 2행 키의 정사각
# 얼굴 폭(≈0.95)이 contain-fit 으로 안 쪼그라든다.
SPRITE = "￼￼￼￼￼"


def ansi(hex_color):
    h = hex_color.lstrip("#")
    r, g, b = int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16)
    return f"\033[38;2;{r};{g};{b}m"


def load_config():
    for p in (Path.home() / ".claude" / "statusline-config.json",
              Path(__file__).parent / "config.json"):
        if p.exists():
            try:
                with open(p, "r") as f:
                    return json.load(f)
            except Exception:
                pass
    return {}


def get_git_branch(cwd):
    try:
        r = subprocess.run(
            ["git", "rev-parse", "--abbrev-ref", "HEAD"],
            capture_output=True, text=True, cwd=cwd, timeout=2,
        )
        if r.returncode == 0:
            return r.stdout.strip()
    except Exception:
        pass
    return None


def report_cwd_to_kasaterm(cwd, session_id):
    """kasaterm pane 안에서만 — claude 내부 cd 를 GUI(파일트리/footer)에 보고.
    claude 는 셸 위에서 돌아 lsof(최상위 셸 cwd)로는 내부 cd 가 안 보여, statusLine 이
    매 렌더 현재 cwd 를 직접 보고한다. pane 밖(KASATERM_PANE_ID 없음)에선 무동작.
    백그라운드(Popen)로 statusline 출력을 지연시키지 않는다."""
    pane = os.environ.get("KASATERM_PANE_ID")
    if not pane or not cwd:
        return
    try:
        subprocess.Popen(
            ["kasaterm-cli", "report-cwd", pane, str(cwd), session_id or ""],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
    except Exception:
        pass


def main():
    try:
        d = json.loads(sys.stdin.read())
    except Exception:
        print(f"{ansi('f7768e')} err{RESET}")
        return

    cfg = load_config()
    ic = ICON_SETS.get(cfg.get("icon_set", "nerd-font"), ICON_SETS["nerd-font"])
    sep_char = cfg.get("separator", "┃")  # 기본 ┃ (거노 설정)

    cwd = d.get("cwd") or os.getcwd()
    session_id = d.get("session_id", "")
    report_cwd_to_kasaterm(cwd, session_id)

    # 세션 id 마커 — SGR8(conceal)로 숨겨 statusline 끝에 싣는다. kasaterm 은 그리드
    # 텍스트에서 `⟦sid8⟧` 를 읽어 "이 pane 이 지금 어느 세션을 표시 중인지"를 진입
    # 즉시 안다(agents 피커 attach 는 이벤트·argv 흔적이 없어 이게 유일한 실시간 채널).
    # 렌더러가 conceal 을 지원한다고 공표(caps.json)한 경우에만 — 아니면 글자가 보인다.
    sid_marker = ""
    if session_id and os.environ.get("KASATERM_PANE_ID"):
        try:
            with open(os.path.expanduser("~/.config/kasaterm/caps.json"), encoding="utf-8") as f:
                if json.load(f).get("sgr_conceal"):
                    sid_marker = f"\033[8m⟦{session_id[:8]}⟧\033[28m"
        except Exception:
            pass

    sep = f" {DIM}{ansi(C_SEP)}{sep_char}{RESET} "
    parts = []

    name = os.environ.get("KASATERM_CHARACTER")
    # 포크/attach 로 세션 id 가 env anchor(KASATERM_SESSION_ID)와 갈라진 백그라운드
    # 세션은 env 캐릭터가 출생 pane 의 동결값이라 오표기(거노: bg 뷰 프사가 딴 학생).
    # 그때만 kasaterm 의 세션→캐릭터 영속 바인딩을 정본으로 읽는다 — 일반 pane 과
    # repersona(학생 명령) 경로는 env 가 최신이므로 건드리지 않는다.
    forked_view = bool(session_id) and session_id != os.environ.get("KASATERM_SESSION_ID")
    if forked_view:
        try:
            with open(os.path.expanduser("~/.config/kasaterm/session_characters.json"), encoding="utf-8") as f:
                name = json.load(f).get(session_id) or name
        except Exception:
            pass
    if name:
        c = ansi(STUDENT_HEX.get(name, C_FALLBACK))
        if os.environ.get("KASATERM_PANE_ID"):
            parts.append(f"{c}{SPRITE}{RESET}")  # kasaterm 이 idle 도트로 대체
        else:
            parts.append(f"{c}●{RESET} {c}{BOLD}{name}{RESET}")

    # 포크/백그라운드 세션 배지. CLAUDE_CODE_CHILD_SESSION 판별은 폐기 — claude 가
    # 모든 세션의 훅/statusline 자식 env 에 무조건 "1"을 심어(2.1.209+ 실측) 전 pane
    # 오발화였다(거노 07-16). 위 캐릭터 바인딩과 같은 anchor 불일치 조건을 쓴다:
    # 세션 id ≠ pane anchor = 포크/attach 뷰. detach 포크는 env 가 출생 pane 동결이라
    # 자기 새 id 와 반드시 갈라지고, 일반 pane·repersona 는 일치라 안 뜬다.
    # 단 사용자 주도 resume(/resume 피커·claude --resume <id>)도 anchor 와 갈라지므로
    # shim 이 export 한 마커(KASATERM_RESUMED_SID=그 sid / KASATERM_RESUME_PICKER)로
    # 걸러낸다(거노: 일반 세션에 bg 오표기). 포크/attach 는 마커가 없어 배지 유지.
    user_resume = bool(session_id) and (
        session_id == os.environ.get("KASATERM_RESUMED_SID")
        or bool(os.environ.get("KASATERM_RESUME_PICKER"))
    )
    if forked_view and not user_resume and os.environ.get("KASATERM_PANE_ID"):
        parts.append(f"{DIM}{ansi(C_FALLBACK)}⑂ bg{RESET}")

    model = (d.get("model") or {}).get("display_name")
    if model:
        # "(1M context)" 등 괄호 꼬리는 뒤 ctx% 의 "·1M" 과 중복 — 떼서 좁은 pane
        # statusLine truncate 를 막는다(거노: 오른쪽 좁은 pane 깨짐).
        model = model.split(" (")[0]
        parts.append(f"{ansi(C_MODEL)}{BOLD}{ic['model']} {model}{RESET}")

    branch = get_git_branch(cwd)
    if branch:
        parts.append(f"{ansi(C_GIT)}{ic['git']} {branch}{RESET}")

    parts.append(f"{ansi(C_DIR)}{ic['folder']} {Path(cwd).name}{RESET}")

    # ctx% 는 "현재 모델 창" 기준이라 모델 전환 시 점프한다(Opus[1m]=1M vs Fable=200k
    # — 같은 31만 토큰이 31% ↔ 100%). 창 크기를 함께 표시해 분모 차이를 자명하게.
    # Fable 5 는 실제 1M 창인데 Claude Code(2.1.207)가 200k 로 잘못 보고하는 버그가
    # 있어(#63015 계열 — 31만 토큰 요청이 실제로 성공함을 확인) 알려진 진짜 창으로
    # % 를 재계산한다. max() 보정이라 메타데이터가 고쳐지면 자동으로 무해해진다.
    KNOWN_WINDOW = {"claude-fable-5": 1_000_000}
    ctx = d.get("context_window") or {}
    pct = ctx.get("used_percentage") or 0
    win = ctx.get("context_window_size") or 0
    mid = (d.get("model") or {}).get("id", "").split("[")[0]
    known = KNOWN_WINDOW.get(mid, 0)
    if known > win:
        win = known
        tot = ctx.get("total_input_tokens") or 0
        pct = min(100.0, tot / win * 100)
    win_s = f"·{win // 1000000}M" if win >= 1000000 else (f"·{win // 1000}k" if win else "")
    c_ctx = ansi("f7768e") if pct >= 90 else ansi(C_CTX)
    parts.append(f"{c_ctx}{pct:.0f}%{DIM}{win_s}{RESET}")

    lvl = (d.get("effort") or {}).get("level")
    if lvl:
        parts.append(f"{ansi(EFFORT_HEX.get(lvl, '7aa2f7'))}{ic['effort']} {lvl}{RESET}")

    # 마커는 세그먼트 뒤 끝자락 — 좁은 pane 에서 잘리면 폴백(argv·타이틀·3s 폴)이 줍는다.
    print(sep.join(parts) + sid_marker)


if __name__ == "__main__":
    main()
