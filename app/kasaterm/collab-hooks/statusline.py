#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "python-dotenv",
# ]
# ///

"""
Claude Code Statusline — kasaterm 미니멀.

    <model> Opus 5 1M ┃ <git> main ┃ <dir> tmuxify ┃ 8% ┃ <bolt> xhigh

- 학생: kasaterm 안에선 U+FFFC 표식 한 칸만 내보낸다(프사는 pane 헤더가 이미 보여준다).
  kasaterm 이 그 칸을 blank 로 지우므로 화면엔 왼쪽 공백 한 칸으로 남고, 구분자를 안
  붙여 그 뒤로 바로 첫 세그먼트가 온다. 밖(일반 터미널)에선 ●+이름 폴백.
- 창 크기는 모델 옆에 붙인다 — 200k 인지 1M 인지가 모델의 성질이고, 뒤 퍼센트는
  그 분모로 계산된 값이라 숫자 하나만 있으면 된다(거노 2026-08-11).
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

# kasaterm 안에서는 Nerd Font 글리프 대신 공식 Claude SVG를 얹는다. 이 PUA는
# 화면에 그릴 문자가 아니라 renderer가 찾아 blank 처리할 한 칸 표식이다. 일반
# 터미널은 SVG 오버레이가 없으므로 기존 icon_set 글리프를 그대로 쓴다.
MODEL_MARKER_CLAUDE = "\ue0c0"
MODEL_MARKER_GPT = "\ue0c1"

# kasaterm pane 표식 — 예전엔 U+FFFC 5칸이 학생 프사(bust) 자리표시자였는데,
# 프사를 걷어내면서(거노 2026-08-11) 1칸으로 줄였다. **지우지는 마라.** 이 문자가
# 화면에 있느냐가 kasaterm 쪽에서 세 가지 판정의 근거다: agents 목록 뷰인지
# (render.rs `has_profile_slot`), statusline 이 stale 이라 재실행해야 하는지
# (socket.rs), 입력박스 위 전신 학생을 어느 행 기준으로 세울지(render.rs standing).
# kasaterm 은 이 칸을 blank 로 지우므로 화면에는 공백 한 칸으로만 남는다.
SPRITE = "￼"


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


# 계정 세그먼트는 걷었다(거노 2026-08-11: "계정없애고"). 슬롯 라벨을 읽던
# `active_account_label`·`load_account` 도 함께 지웠다 — 계정을 어느 pane 이 쓰는지는
# Info 패널이 답하고, 상태줄은 매 턴 눈에 들어오는 자리라 안 바뀌는 값을 둘 곳이 아니다.


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


def report_cwd_to_kasaterm(cwd, session_id, ctx_window=0, ctx_tokens=0,
                           model_id="", effort=""):
    """kasaterm pane 안에서만 — claude 내부 cd 와 컨텍스트 창을 GUI 에 보고.
    claude 는 셸 위에서 돌아 lsof(최상위 셸 cwd)로는 내부 cd 가 안 보여, statusLine 이
    매 렌더 현재 cwd 를 직접 보고한다. pane 밖(KASATERM_PANE_ID 없음)에선 무동작.
    백그라운드(Popen)로 statusline 출력을 지연시키지 않는다.

    창 크기를 같이 보내는 이유: transcript 의 model 엔 `[1m]` 태그가 안 실려(API 응답이
    `claude-opus-5`) GUI 가 1M 세션을 200k 로 오판했다 — 18만 토큰이 92%(200k) vs
    19%(1M). 하네스가 훅 stdin 으로 주는 창 크기가 유일한 정본이라 그걸 그대로 넘긴다.

    model_id·effort 는 앱을 재시작해도 그 pane 이 쓰던 모델·effort 로 되살아나게 하려고
    싣는다. ★model_id 는 `[1m]` 이 붙은 **원본**이어야 한다 — board 에 뜨는 모델명은 API
    응답 표기(`claude-opus-5`)라 그걸 복원 명령에 되먹이면 1M 세션이 200k 로 강등된다.
    훅 stdin 의 `model.id` 만이 CLI 에 그대로 돌려줄 수 있는 값이다."""
    pane = os.environ.get("KASATERM_PANE_ID")
    if not pane or not cwd:
        return
    # 뒤 네 칸은 **자리를 비우지 않고 항상** 보낸다. 예전엔 창을 모르면 생략했는데,
    # 그러면 뒤에 붙는 model·effort 의 위치가 밀려 수신부가 엉뚱한 칸으로 읽는다.
    # 0/빈 문자열도 "미보고"로 해석되므로(수신부가 0 을 안 채택) 동작은 그대로다.
    argv = [
        "kasaterm-cli", "report-cwd", pane, str(cwd), session_id or "",
        str(int(ctx_window or 0)), str(int(ctx_tokens or 0)),
        model_id or "", effort or "",
    ]
    try:
        subprocess.Popen(
            argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
    except Exception:
        pass


# Fable 5 는 실제 1M 창인데 Claude Code(2.1.207)가 200k 로 잘못 보고하는 버그가 있어
# (#63015 계열 — 31만 토큰 요청이 실제로 성공함을 확인) 알려진 진짜 창으로 재계산한다.
# max() 보정이라 메타데이터가 고쳐지면 자동으로 무해해진다.
KNOWN_WINDOW = {"claude-fable-5": 1_000_000}


def resolve_context(d):
    """훅 stdin → (창 크기, 사용률%, 사용 토큰). 표시와 GUI 보고가 같은 값을 쓰도록
    한 곳에서만 계산한다. 모델명으로 창을 추정하지 않는다 — 하네스가 준 값이 정본."""
    ctx = d.get("context_window") or {}
    win = ctx.get("context_window_size") or 0
    pct = ctx.get("used_percentage") or 0
    tot = ctx.get("total_input_tokens") or 0
    mid = (d.get("model") or {}).get("id", "").split("[")[0]
    known = KNOWN_WINDOW.get(mid, 0)
    if known > win:
        win = known
        pct = min(100.0, tot / win * 100) if win else 0
    return win, pct, tot


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
    ctx_win, ctx_pct, ctx_tot = resolve_context(d)
    # ★ `id` 를 **가공 없이** 넘긴다. 아래 표시용으로 쓰는 display_name 이나
    # resolve_context 의 `.split("[")[0]` 을 보내면 `[1m]` 이 떨어져, 복원 때
    # 되먹였을 때 1M 세션이 200k 로 강등된다.
    report_cwd_to_kasaterm(
        cwd, session_id, ctx_win, ctx_tot,
        (d.get("model") or {}).get("id", ""),
        (d.get("effort") or {}).get("level") or "",
    )

    sep = f" {DIM}{ansi(C_SEP)}{sep_char}{RESET} "
    parts = []
    # pane 안에서 왼쪽 끝에 놓는 표식. `parts` 에 안 넣는 이유는 구분자다 — 넣으면
    # `￼ ┃ ` 로 네 칸이 비고, 프사를 걷어낸 뒤로 그 자리가 그냥 구멍이 된다
    # (거노 2026-08-11: "학생프사 없어진 자리 비어이쓴는데 왼쪽으로 밀착해").
    # kasaterm 이 이 한 칸을 blank 로 지우므로 실제로는 공백 한 칸만 남는다.
    prefix = ""

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
        if os.environ.get("KASATERM_PANE_ID"):
            # pane 안에서는 학생을 여기 안 쓴다(거노 2026-08-11) — 이름도 프사도
            # pane 헤더가 이미 보여주므로 상태줄에 또 있으면 같은 정보가 두 번이다.
            # 표식만 남긴다(위 SPRITE 주석: 이게 없으면 kasaterm 쪽 판정 셋이 죽는다).
            prefix = SPRITE
        else:
            # 일반 터미널에는 pane 헤더가 없다 — 거기서는 누구인지 여기서만 알 수 있다.
            c = ansi(STUDENT_HEX.get(name, C_FALLBACK))
            parts.append(f"{c}●{RESET} {c}{BOLD}{name}{RESET}")

    # ⑂bg 백그라운드 배지 제거(거노: 복원 세션에 자꾸 백그라운드로 떠 짜증). anchor
    # (KASATERM_SESSION_ID) 불일치 판정은 detach 포크·앱 재시작 복원·continuation 을
    # 구분 못 해 오발화가 잦았고, pane_foreground_session HTTP 왕복 정밀 판별도
    # --resume 복원 경로에선 여전히 오판했다. pane→세션 바인딩이 복원까지 정확해진
    # 뒤에 정밀 재도입할 것. forked_view 는 위 프사 이름 교정에만 쓴다.

    # 창 크기는 모델의 성질이라 모델 옆에 붙인다(거노 2026-08-11: "컨텍스트량 1m은
    # 모델로 옮기고 200k인지 그건지"). 같은 Opus 라도 `[1m]` 으로 띄웠는지에 따라
    # 분모가 다섯 배 갈리는데, 그 사실이 퍼센트 옆에 있으면 "왜 갑자기 뛰었지"를
    # 모델과 못 잇는다. `display_name` 의 "(1M context)" 꼬리는 안 쓴다 — 200k 일 땐
    # 아무 말도 안 해서, 둘을 구분하려면 우리가 창 크기로 직접 적어야 한다.
    model = (d.get("model") or {}).get("display_name")
    if model:
        model = model.split(" (")[0]
        win_s = (f"1M" if ctx_win >= 1_000_000
                 else (f"{ctx_win // 1000}k" if ctx_win else ""))
        tail = f"{DIM} {win_s}{RESET}" if win_s else ""
        model_id = ((d.get("model") or {}).get("id") or "").lower()
        if os.environ.get("KASATERM_PANE_ID"):
            if (model_id.startswith(("gpt-", "codex-"))
                    or (model_id.startswith("o") and model_id[1:2].isdigit())):
                model_icon = MODEL_MARKER_GPT
            elif model_id.startswith("claude-"):
                model_icon = MODEL_MARKER_CLAUDE
            else:
                model_icon = ic["model"]
        else:
            model_icon = ic["model"]
        parts.append(f"{ansi(C_MODEL)}{BOLD}{model_icon} {model}{RESET}{tail}")

    branch = get_git_branch(cwd)
    if branch:
        parts.append(f"{ansi(C_GIT)}{ic['git']} {branch}{RESET}")

    parts.append(f"{ansi(C_DIR)}{ic['folder']} {Path(cwd).name}{RESET}")

    # 퍼센트 하나만. 분모는 위 모델 옆에 적혀 있으므로 여기서 또 말할 필요가 없다.
    c_ctx = ansi("f7768e") if ctx_pct >= 90 else ansi(C_CTX)
    parts.append(f"{c_ctx}{ctx_pct:.0f}%{RESET}")

    lvl = (d.get("effort") or {}).get("level")
    if lvl:
        parts.append(f"{ansi(EFFORT_HEX.get(lvl, '7aa2f7'))}{ic['effort']} {lvl}{RESET}")

    # ultracode 배지는 여기 있었지만 뺐다 — 세그먼트 **맨 끝**이라 좁은 pane 에서
    # 제일 먼저 잘리고, 안 잘려도 눈이 잘 안 갔다(거노 2026-08-11: 마커를 심어
    # 놓고 물어야 그제서야 "아 보이네"). 지금은 kasaterm 이 같은 마커를 읽어
    # **입력박스 테두리**를 보라색으로 물들인다 — 타이핑하는 자리라 놓칠 수 없다.
    # `ultracode-mark.py`(UserPromptSubmit)가 마커를 쓰는 쪽은 그대로다.

    print(prefix + sep.join(parts))


if __name__ == "__main__":
    main()
