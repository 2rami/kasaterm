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
ITALIC = "\033[3m"
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
# 로그인 계정 세그먼트 — 흰색+이탤릭(거노: 색 세그먼트들 사이에서 계정만 구분되게.
# 터미널 셀은 폰트 패밀리 교체가 불가라 "폰트 다르게"는 italic 스타일로 대응)
C_ACCT = "ffffff"

ICON_SETS = {
    "nerd-font": {"model": "", "git": "", "folder": "", "effort": ""},
    "unicode": {"model": ">", "git": "⎇", "folder": "▸", "effort": "↯"},
    "plain": {"model": "M", "git": "git", "folder": "dir", "effort": "E"},
}

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


def active_account_label():
    """kasaterm 설정의 **활성 계정** 라벨 — 거노가 붙인 이름("개인계정"·
    "사이오닉팀플랜"). 활성 계정이 비었으면(기본 로그인) None → 호출측이 공유 캐시
    폴백으로 간다.

    왜 공유 캐시(`~/.claude.json` 의 `oauthAccount`)를 안 쓰나: 그 필드는 **슬롯별이
    아니다**. 마지막으로 로그인한 계정으로 덮이므로 어느 슬롯으로 돌든 같은 이름이
    뜬다(거노: "계정추가하면 1도 그거로 바뀌어"). 2026-08-05 실측 — 이 pane 은
    `acct-2`(사이오닉팀플랜)로 도는데 statusline 은 `개인계정`을 보여주고 있었다.

    **설정 값을 보는 것은 거노 선택이다**(2026-08-05): Info 에서 계정을 바꾸면 모든
    pane 의 statusline 이 즉시 새 계정을 말한다. 대가는 알고 고른 것이다 — 이미 도는
    claude 는 부팅 때 토큰을 물어 **실제로는 옛 계정으로 계속 돈다**. 그 pane 이 진짜
    쓰는 슬롯을 알아야 하면 `CLAUDE_SECURESTORAGE_CONFIG_DIR` 을 봐라(env 는 pane
    생애 동안 고정이다). 새 계정은 다음에 띄우는 pane 부터 실제로 적용된다.

    갱신 시점은 claude 가 statusline 을 다시 그릴 때 = 그 pane 의 다음 턴이다.
    유휴 pane 은 다음 입력까지 옛 이름을 들고 있다 — 이 스크립트를 부르는 건
    kasaterm 이 아니라 claude 라서 우리가 앞당길 수 없다."""
    try:
        cfg = Path.home() / ".config" / "kasaterm" / "settings.json"
        with open(cfg, encoding="utf-8") as f:
            d = json.load(f)
    except Exception:
        return None
    cur = (d.get("claude_account") or "").strip()
    if not cur:
        return None
    # 이름을 안 붙인 슬롯은 그 슬롯의 **이메일**로 부른다 — `acct-1` 같은 내부 id 는
    # 어느 계정인지 하나도 말해 주지 않는다(거노 2026-08-06). 이메일은 kasaterm 이
    # 슬롯 신원을 조회할 때마다 설정에 흘려 둔 것이라 여기선 공짜로 읽는다.
    email = (d.get("claude_account_emails") or {}).get(cur) or ""
    for a in d.get("claude_accounts") or []:
        if a.get("id") == cur:
            return (a.get("label") or "").strip() or email.strip() or cur
    # 설정 목록에 없는 id(손으로 지운 계정 등)는 그대로 — 빈칸보다 낫다.
    return email.strip() or cur


def load_account():
    """로그인한 Claude 계정 — top-level .claude.json 의 oauthAccount dict.
    CLAUDE_CONFIG_DIR 있으면 그 아래, 없으면 ~/.claude.json. 파일 없음/파싱 실패/
    키 없음은 None 반환해 세그먼트만 생략 — statusline 을 절대 죽이지 않는다.

    ⚠️ 이 dict 의 email·orgId·orgName 은 **슬롯별이 아니라 공유 캐시**다. 기본
    로그인으로 도는 pane 에서만 참이므로, 슬롯을 쓰는 pane 은
    `slot_account_label()` 이 먼저 답한다."""
    cfgdir = os.environ.get("CLAUDE_CONFIG_DIR")
    path = (Path(cfgdir) if cfgdir else Path.home()) / ".claude.json"
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f).get("oauthAccount")
    except Exception:
        return None


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


def report_cwd_to_kasaterm(cwd, session_id, ctx_window=0, ctx_tokens=0):
    """kasaterm pane 안에서만 — claude 내부 cd 와 컨텍스트 창을 GUI 에 보고.
    claude 는 셸 위에서 돌아 lsof(최상위 셸 cwd)로는 내부 cd 가 안 보여, statusLine 이
    매 렌더 현재 cwd 를 직접 보고한다. pane 밖(KASATERM_PANE_ID 없음)에선 무동작.
    백그라운드(Popen)로 statusline 출력을 지연시키지 않는다.

    창 크기를 같이 보내는 이유: transcript 의 model 엔 `[1m]` 태그가 안 실려(API 응답이
    `claude-opus-5`) GUI 가 1M 세션을 200k 로 오판했다 — 18만 토큰이 92%(200k) vs
    19%(1M). 하네스가 훅 stdin 으로 주는 창 크기가 유일한 정본이라 그걸 그대로 넘긴다."""
    pane = os.environ.get("KASATERM_PANE_ID")
    if not pane or not cwd:
        return
    argv = ["kasaterm-cli", "report-cwd", pane, str(cwd), session_id or ""]
    # 창을 모를 때(0)만 생략 — 그때 GUI 는 종전 추정 폴백으로 떨어진다.
    if ctx_window:
        argv += [str(int(ctx_window)), str(int(ctx_tokens))]
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
    report_cwd_to_kasaterm(cwd, session_id, ctx_win, ctx_tot)

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
        # pane 안이든 밖이든 같은 `● 이름`. 다른 것은 kasaterm 이 읽을 표식뿐이다.
        mark = SPRITE if os.environ.get("KASATERM_PANE_ID") else ""
        parts.append(f"{c}●{RESET} {c}{BOLD}{name}{RESET}{mark}")

    # 계정 — 프사 바로 오른쪽. kasaterm 설정의 **활성 계정** 라벨이 1순위다(거노가
    # 붙인 이름). 활성 계정이 없으면 기본 로그인이고 그때만 공유 캐시가 참이라 종전
    # 규칙으로 간다: team/enterprise 는 조직명, 그 외는 사용자 이름(displayName).
    # 개인계정 organizationName 은 "<이름>'s Organization" 이라 무의미해 "개인" 대신
    # 실제 이름을 쓰고(거노), displayName 없으면 이메일 로컬파트, 그마저 없으면
    # "개인" 폴백.
    label = active_account_label()
    if not label:
        acct = load_account()
        if acct:
            otype = (acct.get("organizationType") or "").lower()
            org = (acct.get("organizationName") or "").strip()
            if "team" in otype or "enterprise" in otype:
                label = org or "팀"
            else:
                label = ((acct.get("displayName") or "").strip()
                         or (acct.get("emailAddress") or "").split("@")[0]
                         or "개인")
    if label:
        parts.append(f"{ansi(C_ACCT)}{ITALIC}{label[:14]}{RESET}")

    # ⑂bg 백그라운드 배지 제거(거노: 복원 세션에 자꾸 백그라운드로 떠 짜증). anchor
    # (KASATERM_SESSION_ID) 불일치 판정은 detach 포크·앱 재시작 복원·continuation 을
    # 구분 못 해 오발화가 잦았고, pane_foreground_session HTTP 왕복 정밀 판별도
    # --resume 복원 경로에선 여전히 오판했다. pane→세션 바인딩이 복원까지 정확해진
    # 뒤에 정밀 재도입할 것. forked_view 는 위 프사 이름 교정에만 쓴다.

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
    win, pct = ctx_win, ctx_pct
    win_s = f"·{win // 1000000}M" if win >= 1000000 else (f"·{win // 1000}k" if win else "")
    c_ctx = ansi("f7768e") if pct >= 90 else ansi(C_CTX)
    parts.append(f"{c_ctx}{pct:.0f}%{DIM}{win_s}{RESET}")

    lvl = (d.get("effort") or {}).get("level")
    if lvl:
        parts.append(f"{ansi(EFFORT_HEX.get(lvl, '7aa2f7'))}{ic['effort']} {lvl}{RESET}")

    # ultracode 배지는 여기 있었지만 뺐다 — 세그먼트 **맨 끝**이라 좁은 pane 에서
    # 제일 먼저 잘리고, 안 잘려도 눈이 잘 안 갔다(거노 2026-08-11: 마커를 심어
    # 놓고 물어야 그제서야 "아 보이네"). 지금은 kasaterm 이 같은 마커를 읽어
    # **입력박스 테두리**를 보라색으로 물들인다 — 타이핑하는 자리라 놓칠 수 없다.
    # `ultracode-mark.py`(UserPromptSubmit)가 마커를 쓰는 쪽은 그대로다.

    # 마커는 세그먼트 뒤 끝자락 — 좁은 pane 에서 잘리면 폴백(argv·타이틀·3s 폴)이 줍는다.
    print(sep.join(parts))


if __name__ == "__main__":
    main()
