//! antigravity(agy) 래퍼 — 학생 이름을 슬러그로 고르는 훅 shim.
use super::*;

/// 이름 하나를 sh `case` 패턴으로. **작은따옴표가 필수다** — case 패턴은 glob 이라
/// 따옴표가 없으면 `*`·`?`·`[` 가 든 이름이 엉뚱한 것과 매칭되고, `|`·`)` 는 문법을
/// 깨뜨린다. 지금 로스터는 한글뿐이라 아무 일도 안 일어나지만, 사용자가
/// characters.json 에 넣는 이름을 우리가 정하지 않는다.
pub(crate) fn sh_case_pat(name: &str) -> String {
    format!("'{}'", name.replace('\'', "'\\''"))
}

/// agy(Google Antigravity CLI) 판. claude·codex 와 목적은 같은데 **수단이 셋 다
/// 다르다** — 전부 2026-08-11 실측 확정:
///
/// 1. **주입 자리는 `--agent <이름>`.** `--append-system-prompt` 등가물이 없고
///    (바이너리 문자열 전수 검색), 그 대신 `~/.gemini/agents/<이름>.md` 의 본문이
///    시스템 지시로 들어간다. 캐너리로 확인했다 — 플래그를 주면 첫 줄에 나오고
///    안 주면 안 나온다.
/// 2. **이름만 받는다. 경로는 안 받는다.** `--agent /abs/path.md` 는 조용히
///    무시된다(직접 재 봤다). 그래서 파일이 `~/.gemini/agents/` 에 실재해야
///    한다 — shim dir 에 두고 가리키는 길이 없다. 사용자 홈에 쓰는 유일한
///    shim 이라 이름을 `kasaterm-` 으로 못박고 그 접두 밖은 건드리지 않는다.
/// 3. **홈을 옮기는 env 가 없다.** `CODEX_HOME` 등가물이 없어(`GEMINI_HOME`·
///    `ANTIGRAVITY_HOME` 둘 다 바이너리에 없다) 격리하려면 `HOME` 자체를 갈아야
///    하는데, 그러면 agy 가 shell out 하는 git·ssh·gh 가 전부 딸려 간다. 게다가
///    `.gemini` 만 넘기면 인증이 깨진다(`Library` 도 심링크해야 한다). 그래서
///    **격리하지 않는다** — 파일은 공유하고 pane 별 선택만 플래그로 한다.
///    codex 가 홈을 통째로 미러해야 했던 것에 비하면 오히려 단순하다.
///
/// ⚠️ **없는 에이전트 이름을 줘도 에러가 안 난다** — 조용히 페르소나 없이 돈다.
/// 그래서 파일 쓰기가 실패하면 플래그도 안 붙이는 편이 낫다(붙이면 「됐다」로
/// 보이는데 실제로는 순정이다). 아래 래퍼가 파일 존재를 확인하고 붙인다.
///
/// ⚠️ 본문 상한이 있다. 18KB 는 맨 끝 규칙까지 살아남았고 47KB 는 잘렸다
/// (바이너리에 `customizationBudget`·`HasTruncatedCustomizationType` 심볼이 있다).
/// 지금 페르소나는 규약 포함 8KB 안쪽이라 여유가 있지만, 규약이 두 배가 되면
/// 조용히 잘리는 쪽이라 눈에 안 띈다.
///
/// 훅은 아직 안 붙인다 — agy 훅 스키마를 재지 않았다. 이 함수는 페르소나까지다.
pub(crate) fn install_agy_hook_shim(shim_dir: &std::path::Path) {
    install_agent_identity_helper(shim_dir);
    let Some(chars) = kasa_mcp::character::characters_json() else {
        return;
    };
    // agy 는 이름으로만 에이전트를 찾으므로 파일이 사용자 홈에 실재해야 한다.
    // 없으면 만든다 — 만들기가 실패하면 페르소나 없이 도는 게 맞다(래퍼가
    // 파일 존재를 다시 확인한다).
    let Some(home) = kasa_socket::home_dir() else {
        return;
    };
    let agents_dir = home.join(".gemini").join("agents");
    if let Err(e) = std::fs::create_dir_all(&agents_dir) {
        eprintln!("[shim] mkdir {agents_dir:?} failed: {e} — agy persona skipped");
        return;
    }
    // 이름 → 슬러그 case 를 래퍼에 굽는다. 래퍼는 sh 라 한글 이름밖에 못 받는데
    // (`KASATERM_CHARACTER`) 파일명은 슬러그여야 해서다 — `agy agents` 목록과
    // `--agent` 인자에 한글이 들어가는 건 확인하지 않았고, 확인 안 한 것을
    // 기본 경로에 두지 않는다.
    let mut arms = String::new();
    // 「말투」 토글이 꺼져 있으면 agent 파일을 아예 안 만든다.
    let persona_on = socket::read_claude_persona();
    for name in kasa_mcp::character::member_names(&chars) {
        let Some(slug) = theme::character_slug(&name) else {
            // 로스터 밖 캐릭터 — 슬러그가 없으면 스프라이트도 없다. 페르소나만
            // 따로 주면 화면과 어긋나므로 여기서도 뺀다.
            continue;
        };
        // 토글이 꺼져 있으면 agent 파일 자체를 안 만든다 — 남겨 두면 agy 가 그걸
        // 그대로 읽어 말투가 붙는다.
        let Some(persona) = persona_on
            .then(|| kasa_mcp::character::persona_for(&chars, &name))
            .flatten()
        else {
            continue;
        };
        let body = format!(
            "---\nname: kasaterm-{slug}\ndescription: kasaterm pane persona ({name}) — 자동 생성, 직접 고치지 마세요\n---\n\n{persona}\n"
        );
        let path = agents_dir.join(format!("kasaterm-{slug}.md"));
        if let Err(e) = write_shim_data(&path, body) {
            eprintln!("[shim] write agy agent {path:?} failed: {e}");
            continue;
        }
        arms.push_str(&format!("    {}) S={slug} ;;\n", sh_case_pat(&name)));
    }
    if arms.is_empty() {
        return;
    }
    let wrapper = format!(
        r#"#!/bin/sh
# kasaterm pane-only agy wrapper — 이 pane 에 배정된 학생을 --agent 로 얹는다.
# ~/.gemini 는 agents/kasaterm-*.md 만 쓴다(그 파일들은 GUI 가 부팅 때 굽는다).
# pane 밖에선 이 래퍼가 PATH 에 없어 순정 agy 가 돈다.
SELF_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CLEAN_PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -vxF "$SELF_DIR" | grep -vE '(^|/)kasaterm-shim-[0-9]+/?$' | paste -sd: -)
REAL=$(PATH="$CLEAN_PATH" command -v agy 2>/dev/null)
# 같은 디렉터리가 다른 표기로 PATH 에 남으면 위 grep 이 못 지운다 — Git Bash 는
# /tmp 를 AppData/Local/Temp 의 별칭으로 두어, PATH 엔 `/tmp/kasaterm-shim-N` 이
# 들어 있는데 $0 로 잰 SELF_DIR 은 `/c/Users/.../Temp/kasaterm-shim-N` 이다. 그러면
# REAL 이 이 스크립트 자신이 되어 무한 재귀한다(2026-09-01 Windows 실측: --version
# 조차 안 끝났다). 문자열이 달라도 같은 파일인지로 한 번 더 거른다.
while [ -n "$REAL" ] && [ "$REAL" -ef "$0" ]; do
  CLEAN_PATH=$(printf '%s' "$CLEAN_PATH" | tr ':' '\n' | grep -vxF "$(dirname -- "$REAL")" | paste -sd: -)
  REAL=$(PATH="$CLEAN_PATH" command -v agy 2>/dev/null)
done
if [ -z "$REAL" ]; then
  echo "kasaterm agy shim: real agy not found on PATH" >&2
  exit 127
fi
# 관리 서브커맨드는 순정으로 통과 — 페르소나도 위험 플래그도 뜻이 없고,
# `agy agents` 는 우리가 구운 목록을 그대로 보여줘야 진단이 된다.
SUB=""; for a in "$@"; do case "$a" in -*) ;; *) SUB="$a"; break ;; esac; done
case "$SUB" in
  agent|agents|changelog|help|install|models|plugin|plugins|update)
    exec "$REAL" "$@" ;;
esac
# KASATERM_LAUNCH_IDENTITY
# 이 pane 의 학생 — `시로코` 런처로 갈아탔으면 그 파일이 env 를 이긴다(env 는 셸
# spawn 시 고정이라 늦게 못 바꾼다). codex 래퍼와 같은 규약.
C="$KASATERM_CHARACTER"
OVC="$SELF_DIR/repersona-${{KASATERM_PANE_ID}}.character"
[ -n "$KASATERM_PANE_ID" ] && [ -f "$OVC" ] && C=$(cat "$OVC")
S=""
case "$C" in
{arms}esac
# 파일이 실재할 때만 붙인다. agy 는 **없는 에이전트 이름에 에러를 안 낸다** —
# 조용히 페르소나 없이 도는데, 플래그가 붙어 있으면 로그만 보고는 붙은 줄 안다.
if [ -n "$S" ] && [ -f "$HOME/.gemini/agents/kasaterm-$S.md" ]; then
  case " $* " in
    *" --agent "*) ;;
    *) set -- --agent "kasaterm-$S" "$@" ;;
  esac
fi
# 승인 우회 — claude 의 `--dangerously-skip-permissions`, codex 의
# `--dangerously-bypass-approvals-and-sandbox` 와 같은 자리다. 없으면 헤드리스에서
# 도구가 auto-deny 되어 빈 출력이 나오고, 오케스트레이터는 그걸 「일하는 중」으로
# 읽는다. 샌드박스를 직접 지정한 호출은 그 뜻을 존중한다.
case " $* " in
  *" --dangerously-skip-permissions "*|*" --sandbox "*) ;;
  *) set -- --dangerously-skip-permissions "$@" ;;
esac
exec "$REAL" "$@"
"#
    );
    let wrapper_path = shim_dir.join("agy");
    let wrapper = wrapper.replace("# KASATERM_LAUNCH_IDENTITY", &identity_bootstrap_sh("agy", ""));
    if let Err(e) = write_shim(&wrapper_path, wrapper) {
        eprintln!("[shim] write agy wrapper failed: {e}");
    }
}
