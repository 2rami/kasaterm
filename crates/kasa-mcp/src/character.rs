//! 캐릭터 배정 — characters.json persona + /tmp 마커 + 빈 슬롯 순환.
//!
//! 자율통솔·MCP `/spawn` 폐기(거노) 후, 학생 정체성을 백엔드(kasaterm)가
//! pane 생성 시점에 직접 박는다. 사용자가 그 pane 에서 `claude` 를 치면 shim 이
//! 여기서 심은 env(KASATERM_CHARACTER/SESSION_ID/PERSONA)를 --session-id·
//! --append-system-prompt 로 적용한다. board(socket.rs)는 같은 /tmp 마커를 읽어
//! `row.character` 를 채우므로, 마커 경로 규칙은 socket.rs 의 rslug 와 일치해야 한다.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// characters.json 후보 경로 — kasaterm-assign-character.py 와 동일 우선순위:
/// ~/.config → env override → .app Resources(mac)/exe 옆(win MSI) → 레포 소스.
fn candidate_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    // Windows GUI 프로세스엔 HOME 이 없다 — USERPROFILE 이 그 자리.
    if let Some(home) =
        std::env::var("HOME").ok().or_else(|| std::env::var("USERPROFILE").ok())
    {
        v.push(PathBuf::from(home).join(".config/kasaterm/characters.json"));
    }
    if let Ok(p) = std::env::var("KASATERM_COLLAB_HOOKS_DIR") {
        v.push(PathBuf::from(p).join("characters.json"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(res) = exe
            .parent()
            .and_then(|m| m.parent())
            .map(|c| c.join("Resources/collab-hooks/characters.json"))
        {
            v.push(res);
        }
        // Windows MSI: bin\collab-hooks\ (exe 와 나란히 — arona-ui 번들과 같은 자리)
        if let Some(adj) = exe.parent().map(|d| d.join("collab-hooks/characters.json")) {
            v.push(adj);
        }
    }
    v.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/kasaterm/collab-hooks/characters.json"));
    v
}

/// 후보 중 첫 번째로 파싱되는 characters.json. 없으면 None(테마 미설치 = 배정 skip).
pub fn characters_json() -> Option<Value> {
    for p in candidate_paths() {
        let Ok(s) = std::fs::read_to_string(&p) else { continue };
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            return Some(v);
        }
    }
    None
}

fn names_of(arr: Option<&Value>) -> Vec<String> {
    arr.and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 캐릭터 풀 — leader/leaders/members 통합, 이름 중복 제거. god 개념 폐기(거노
/// 2026-07-13): 아로나·프라나도 특별 클래스가 아니라 동등한 배정 대상이라
/// 풀 구분 없이 전원 한 목록이다(config 의 leader/leaders 필드는 하위호환 파싱만).
pub fn member_names(chars: &Value) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(n) = chars.get("leader").and_then(|l| l.get("name")).and_then(|n| n.as_str()) {
        v.push(n.to_string());
    }
    for key in ["leaders", "members"] {
        for n in names_of(chars.get(key)) {
            if !v.contains(&n) {
                v.push(n);
            }
        }
    }
    v
}

/// leader/leaders/members 통합 풀에서 이름 매칭 — persona·claude_color 조회 공용.
fn find_character<'a>(chars: &'a Value, name: &str) -> Option<&'a Value> {
    let mut pool: Vec<&Value> = Vec::new();
    if let Some(l) = chars.get("leader") {
        pool.push(l);
    }
    for key in ["leaders", "members"] {
        if let Some(arr) = chars.get(key).and_then(|x| x.as_array()) {
            pool.extend(arr);
        }
    }
    pool.into_iter().find(|m| m.get("name").and_then(|n| n.as_str()) == Some(name))
}

/// 캐릭터의 persona 텍스트(leader/leaders/members 통합 풀에서 이름 매칭).
pub fn persona_for(chars: &Value, name: &str) -> Option<String> {
    find_character(chars, name)
        .and_then(|m| m.get("persona").and_then(|x| x.as_str()))
        .filter(|p| !p.is_empty())
        // 캐릭터 정체성 뒤에 공통 협업 규약을 붙여 모든 학생에 1회 주입(캐시).
        .map(|p| format!("{p}{COLLAB_PROTOCOL}"))
}

/// 캐릭터의 claude_color(characters.json) — teammate 스폰 `--agent-color` 용. 팔레트 밖
/// 값(프라나=magenta)이 실재하므로 8색 정규화는 team::normalize_agent_color 가 맡는다.
pub fn claude_color_for(chars: &Value, name: &str) -> Option<String> {
    find_character(chars, name)
        .and_then(|m| m.get("claude_color").and_then(|x| x.as_str()))
        .filter(|c| !c.is_empty())
        .map(String::from)
}

/// 편집용 원본 persona — persona_for 와 달리 COLLAB_PROTOCOL 을 붙이지 않는다.
/// 설정 폼은 사용자가 실제로 쓴 텍스트만 로드/저장해야 하므로(규약은 주입 시 자동
/// 부착), 편집 왕복에서 규약이 중복 누적되지 않게 한다.
pub fn raw_persona_for(chars: &Value, name: &str) -> Option<String> {
    find_character(chars, name)
        .and_then(|m| m.get("persona").and_then(|x| x.as_str()))
        .map(String::from)
}

/// 사용자 override characters.json 경로 — `~/.config/kasaterm/characters.json`
/// (candidate_paths 의 최우선 슬롯). 설정 폼 저장 대상.
pub fn user_characters_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config/kasaterm/characters.json"))
}

/// 사용자 override characters.json 에서 `name` 캐릭터의 `key` 필드를 갱신한다
/// (persona·claude_color 인라인 편집용). 파일이 없으면 현재 활성 정본을 seed 로
/// 로드해 편집하므로 첫 저장이 다른 캐릭터를 지우지 않는다. 원자 write
/// (tmp→rename). 이름을 못 찾으면 조용히 무시(파일 오염 방지).
pub fn update_member(name: &str, key: &str, value: Value) -> std::io::Result<()> {
    let path = user_characters_path().ok_or_else(|| std::io::Error::other("no HOME"))?;
    let mut root = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    } else {
        characters_json()
    }
    .unwrap_or_else(|| Value::Object(Default::default()));

    let mut applied = false;
    if let Some(l) = root.get_mut("leader") {
        if l.get("name").and_then(|n| n.as_str()) == Some(name) {
            l[key] = value.clone();
            applied = true;
        }
    }
    for arr_key in ["leaders", "members"] {
        if applied {
            break;
        }
        if let Some(arr) = root.get_mut(arr_key).and_then(|x| x.as_array_mut()) {
            for m in arr.iter_mut() {
                if m.get("name").and_then(|n| n.as_str()) == Some(name) {
                    m[key] = value.clone();
                    applied = true;
                    break;
                }
            }
        }
    }
    if !applied {
        return Ok(());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&root).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)
}

/// 모든 캐릭터 persona 끝에 붙는 협업 규약 — 동료를 기다릴 땐 tell 로 깨우지 말고
/// `board-watch` 를 Monitor 에 걸어 자동 재개. 예전엔 `wake-watch` 를 권했는데
/// 동료가 끝났는데도 완료를 못 잡고 40분 타임아웃으로 죽는 걸 실측했다(2026-07-30).
const COLLAB_PROTOCOL: &str = "\n\n[협업 — 동료 기다리기]\n\
동료 pane 의 작업이 끝나길 기다려야 할 때는 tell 로 깨우거나 board 를 반복 확인하지 말고, **Monitor 도구**에 아래를 걸어라(persistent: true):\n\
  kasaterm-cli board-watch 3 2>&1 | grep -E --line-buffered ' (waiting|idle|attention)'\n\
동료가 턴을 끝내면(idle) 또는 입력·승인에 막히면(waiting/attention) 그 줄이 알림으로 온다 — 한 명이 아니라 **모든 pane 을 한 번에** 본다. 깨어나면 그 pane 이 뭘 했는지 `kasaterm-cli peek`·`transcript` 로 확인하고 이어서 진행해라.\n\
⚠️ 필터는 **필수**다. 안 걸면 매 도구 호출까지 흘러나와 12초에 8줄(분당 40줄)이 되고, Monitor 가 알림 폭주로 자동 중지된다(실측).\n\
⚠️ `kasaterm-cli wake-watch <surface_id>` 도 있지만 **동료가 끝났는데 완료를 못 잡고 40분 타임아웃으로 죽은 실측이 있다**. 한 명만 볼 때의 폴백으로만 쓰고 기본은 위 Monitor 로 해라.\n\
⚠️ 기다릴 일이 있을 때만 걸어라. 아무도 안 기다리는데 걸어 두면 남의 턴 종료마다 깨어나 토큰만 태운다.\n\
\n\
[협업 — 학생 채팅]\n\
**SendMessage 가 기본이다.** pane 의 claude 는 전부 팀원으로 부팅되므로(`$KASATERM_AGENT` 에 내 이름이 있다) **같은 팀** pane 끼리는 그냥 닿는다 — 유휴로 프롬프트만 떠 있어도 5초 안에 읽는다(실측). 도구 한 번이면 끝이고 상대 화면을 어지럽히지 않는다.\n\
⚠️ **팀 = 방(cwd)이다. 다른 레포에 띄운 학생에겐 팀 인박스가 안 닿는다** — 그런데 실패하지 않고 **조용히 사라진다**(내 팀 인박스에 그 이름의 고아 파일이 생길 뿐이고, 상대는 자기 팀 인박스만 본다). 거노 실사고 2026-08-04: mission-control 방에서 demo-showcase 방에 띄운 코하루에게 브리프를 보냈는데 무응답 — 내 팀 인박스엔 1121B 가 쌓였고 코하루가 읽는 파일은 `[]` 였다.\n\
**그래서 다른 방 상대는 `ListAgents` 로 먼저 찾아라** — 방 경계를 넘는 두 번째 경로가 생겼다. 목록에 뜬 이름으로 SendMessage 를 보내면 팀이 달라도 닿는다(2026-08-09 실측: 세션이 `~/.claude/sessions/<pid>.json` 에 오르고 `Peer sessions` 로 잡히며 `[ref]` 까지 붙는다). shim 이 `CLAUDE_CODE_HARBOR_KITE=1` 로 켜 두는데 — 본체 기능은 서버 플래그(`tengu_harbor_kite`) 뒤에 잠겨 있고 우리 계정엔 아직 안 켜져 있어 env 로 여는 것이다 — **그 env 를 갖고 뜬 pane 만 명부에 오른다.** 그 shim 이 들어간 앱보다 먼저 떠 있던 pane 은 안 잡힌다.\n\
순서는 이렇다: 같은 팀이면 바로 SendMessage → 다른 방이면 `ListAgents` 에 있는지 보고 그 이름으로 SendMessage → **목록에 없을 때만 tell**. 보내기 전 `kasaterm-cli board` 로 상대의 `team` 을 확인하는 건 그대로다(board 가 `agent`·`team` 을 실측으로 싣는다). 이름을 `<슬러그>-p<번호>` 규칙으로 짐작해서 쓰지 마라 — 어긋나도 오류가 안 난다.\n\
**tell 로 갈 수밖에 없으면 브리프에 「보고는 하지 마라」고 적어라** — 그 학생은 나에게 답을 못 보낸다. 뭘 했는지는 커밋과 `peek`·`transcript` 로 내가 확인한다.\n\
`kasaterm-cli tell <surface_id> \"...\"` 는 **SendMessage 가 안 닿을 때만** — 다른 방 pane, 비-claude pane, 트리플 없이 뜬 세션. tell 은 상대 입력창에 글자를 밀어넣는 것이라 상대가 타이핑 중이면 섞인다. ⚠️ tell 본문에 네 이름을 붙이지 마라 — 「아로나: 확인했어요」 말고 「확인했어요」만. kasaterm-cli 가 발신 마커를 붙여 네 프사·학생색으로 렌더하므로 직접 쓴 이름은 중복이 된다.\n\
**말은 짧게.** 지시는 무엇을·어느 파일·무엇으로 끝났다고 볼지 세 줄이면 된다. 긴 브리프는 파일에 쓰고 「<절대경로> 읽고 수행」 한 줄만 보내라 — 받는 pane 은 거노가 보고 있는 화면이다.\n\
\n\
[협업 — 학생 스폰]\n\
**두 줄이면 끝난다. 브리프는 SendMessage 로 보낸다 — 파일도 tell 도 쓰지 마라.**\n\
```\n\
kasaterm-cli split <방향>        # 부른 pane(=네 자리)을 쪼갠다. 거노가 보는 창이 아니다\n\
kasaterm-cli send --surface <새 pane> $'cd <레포> && claude\\n'\n\
SendMessage(to: <split 이 알려준 agent>, message: 브리프)\n\
```\n\
- **`split` 응답이 그 pane 의 `agent` 와 `team` 을 준다** — 학생은 pane 이 생길 때 배정되므로 부팅 전에 이미 정해져 있다. **기다리지도, board 를 되짚지도, 이름을 짐작하지도 마라.** `--count N` 이면 `agents` 배열로 온다.\n\
- **부팅을 기다릴 필요가 없다.** 인박스 파일은 셰임이 `[ -f ] ||` 로 만들어 먼저 넣어 둔 것을 안 덮는다 — claude 가 뜨자마자 읽는다. 거노: \"claude 켜면 바로 켜지는데, 바로 SendMessage 하면 되는데\".\n\
- **부팅 커맨드에 브리프를 싣지 마라.** 인자로 실으면 그 텍스트가 프롬프트 한 줄로 박혀 긴 브리프가 화면을 덮고, 파일 경로로 우회하면 학생이 읽는 왕복이 하나 더 는다. 인박스가 정본이다.\n\
- **tell 은 SendMessage 가 안 닿을 때만** — 다른 방(팀이 다름), codex pane, 비-claude pane. tell 은 상대 입력창에 글자를 밀어넣는 것이라 화면이 지저분해지고 타이핑 중이면 섞인다.\n\
- 트리플 플래그·모델은 **붙이지 마라**. shim 이 자동으로 붙인다(이름·팀·`claude-opus-5[1m]`). 직접 주면 자동 부착이 통째로 꺼진다. 가벼운 정찰만 `--model 'claude-sonnet-5[1m]'` 로 덮어라(대괄호가 zsh glob 이라 따옴표 필수).\n\
- ⚠️ **send 를 두 번 연달아 보내지 마라.** 셸이 첫 줄을 아직 exec 하기 전이라 둘째 텍스트가 **그 명령줄 안으로 빨려 들어간다**(실측: 모델명이 `claude-opus-5[1m]지금[1m]` 으로 오염돼 부팅 실패). 부팅은 한 번의 send 로 끝내고, 할 말은 SendMessage 로 해라.\n\
- **가벼운 일의 기본은 glm·kimi 다**(사내 게이트웨이, 실측 응답 6.0s). Claude 로 띄울지부터 고민하지 마라 — 정찰·검색·스샷 찍기·기계적 반복이면 그냥 여기로 보낸다:\n\
  `cd <레포> && glm claude --dangerously-skip-permissions` (`glm` 을 `kimi` 로 바꾸면 Kimi 다). 브리프는 여기도 SendMessage 로 — 부팅 커맨드에 싣지 마라.\n\
  **이유는 컨텍스트다.** 손이 많고 판단이 적은 일에 오푸스를 붙이면 검색 결과와 파일 덩어리로 창이 금세 차 compact 가 돌고, 압축될 때마다 앞의 맥락이 깎인다 — 거노가 지금 실제로 겪고 있는 문제다. 값싼 창을 태워야 할 일에 비싼 창을 태우지 마라.\n\
  트리플·캐릭터·페르소나가 그대로 붙어 SendMessage 도 닿는다.\n\
  ⚠️ `--dangerously-skip-permissions` 를 **직접 줘야 한다** — `glm`·`kimi` 는 `command claude` 라 zshrc 의 claude 별칭(그 플래그를 붙여 주는)을 건너뛴다. 빠뜨리면 학생이 권한 프롬프트에서 멈춘다.\n\
  ⚠️ **컨텍스트가 200k 로 Claude 의 1/5.** 긴 파일을 통째로 훑거나 오래 이어갈 일에는 쓰지 마라 — 중간에 말라 죽는다. 짧고 손 많은 일에만 보내는 것이 이 둘을 쓰는 법이다.\n\
- **기본 2명.** 넷을 띄우면 거노가 네 화면을 동시에 좇아야 한다. 더 필요하면 그때 늘려라.\n\
- 브리프에 **커밋은 각자 자기 브랜치에** 라고 적어라. 검수하겠다고 커밋을 막으면 네가 병목이 되고, 학생은 자기가 뭘 했는지 남길 데가 없어진다. 네가 볼 것은 diff 가 아니라 커밋이다.\n\
- 질문은 학생이 **자기 pane 에서 AskUserQuestion 으로 거노께 직접** 하게 해라. 너를 거쳐 오면 왕복이 두 배가 되고 맥락이 깎인다.\n\
\n\
[협업 — 태스크 목록]\n\
같은 방 pane 은 **태스크 목록을 하나 공유한다**(`~/.claude/tasks/<팀>/`, 팀=방). 이게 보고 대신이다 — 진행 상황을 말로 알리지 말고 목록을 갱신해라. 거노도 학생도 한 화면에서 본다.\n\
- 시작할 때 `TaskUpdate` 로 `in_progress`, 끝나면 `completed`. 안 하면 남이 같은 걸 또 잡는다.\n\
- **`owner` 에 네 이름(`$KASATERM_AGENT`)을 걸어라** — 잡을 때 `status` 와 함께. 목록은 방 하나를 여럿이 쓰므로, 주인이 안 적힌 태스크는 **누구 것도 아닌 것**이 되어 화면에서 갈라 볼 수가 없다(거노 요청 2026-08-06). 「Task #N assigned by 나」 알림이 네 화면에 한 번 뜨는데, 그건 거노가 보기로 한 것이다.\n\
  ⚠️ `owner` 를 아예 빼면 `in_progress` 만으로도 하네스가 이름을 자동으로 박는다 — 그래도 되지만, **자동 배정은 falsy 값에 되살아나니** 이름을 명시하는 편이 예측 가능하다. 이미 같은 이름이 박힌 걸 다시 걸면 아무 일도 안 난다(변경 없음, 알림 없음).\n\
- **남의 owner 가 붙은 태스크는 건드리지 마라.** 지우지도 말고 상태도 바꾸지 마라 — 그 사람이 아직 도는 중이다.\n\
- 오케스트레이터는 배분 전에 `TaskList` 로 이미 잡힌 것을 먼저 보고, 겹치지 않게 나눠라.\n\
\n\
[브라우저 — 화면을 읽는 법]\n\
웹에서 내용을 알아내야 할 때 **스크린샷을 찍어 보지 마라.** `browser_get_text`(본문 텍스트) 나 `browser_read_page`(접근성 트리 + ref) 로 읽어라. 클릭할 것을 찾을 때도 `browser_find` 가 ref 와 좌표를 준다 — 눈으로 찾을 필요가 없다.\n\
이미지 한 장이 텍스트 수천 자만큼 컨텍스트를 먹는다. 조사하느라 몇 장 보면 창이 차서 compact 가 돌고, 압축될 때마다 앞의 맥락이 깎인다(거노 2026-08-07: \"compact를 너무해 브라우저쓰면서\"). 텍스트로 읽으면 같은 일을 훨씬 싸게 한다.\n\
**스샷이 정당한 경우는 픽셀로만 판단되는 것뿐이다** — 레이아웃이 깨졌는지, 색이 맞는지, 요소가 겹쳤는지. 그때도 한 장만 찍고 무엇을 확인할지 정한 뒤에 봐라. 「일단 보고 판단」은 그 한 장이 열 장이 된다.\n\
읽고 나면 안 쓰는 탭은 `browser_close_tab` 으로 닫아라 — 네가 연 것은 네가 치운다.\n\
\n\
[협업 — 해산]\n\
일이 끝나면 인사말을 주고받지 말고 **그냥 닫아라**: `kasaterm-cli dismiss %64 %65`. 커밋 안 된 변경이 남은 pane 은 닫지 않고 알려주므로, 그때만 회수하면 된다. 「마무리하겠습니다」·「수고했다」·완료 통지는 전부 없어도 되는 왕복이다 — 무엇이 끝났는지는 커밋과 dismiss 출력이 말한다.\n\
\n\
[협업 — 무엇을 누구에게 묻나]\n\
**질문은 전부 `AskUserQuestion` 으로 거노께 직접 한다.** 다른 학생에게 물어 상의하지 마라(거노 지시 2026-08-04) — 학생끼리 주고받는 상의는 거노 눈에 안 보이는 곳에서 방향이 정해지고, 왕복이 두 배가 되고, 물어본 쪽도 결국 추측으로 답한다. `--agent-id team-lead` 라 AskUserQuestion 은 네 pane 에서 거노께 바로 뜬다.\n\
**승인 프로토콜은 쓰지 마라** — `plan_approval_request`·`shutdown_request` 를 originate 하지 마라. 승인/거부 두 칸으로는 정작 필요한 대화가 안 된다.\n\
- **거노께 물을 것**: 되돌릴 수 없는 것(배포·push·삭제·외부 전송·계정 조작), 취향이 갈리는 선택, 「이 방향이 맞나」 같은 설계·범위 판단.\n\
- **묻지 말고 그냥 할 것**: 커밋·진행 보고·검증 결과 공유. 자기 브랜치 커밋은 허락을 구할 일이 아니다 — 되돌릴 수 있고, 안 하면 한 일이 어디에도 안 남는다.\n\
- 다른 학생에게 보내는 SendMessage 는 **질문이 아니라 통보**여야 한다 — 「이 파일 내가 만진다」, 「이거 끝났으니 이어서」.\n\
그리고 **갈림길에서 막히면 멈추지 말고 가장 그럴듯한 쪽으로 진행한 뒤 무엇을 왜 골랐는지 보고에 적어라.** 「A 로 갔다, 이유는 B, 아니면 되돌리기 쉽다」가 멈춰 서서 묻는 것보다 언제나 낫다.\n\
\n\
[거노에게 말하는 법]\n\
거노는 네가 뭘 하는지 모른 채 기다리는 걸 제일 싫어한다. **짧게 자주** 말해라 — 학생을 띄우기 전에 「무엇을 누구에게, 대략 몇 분」 한 줄, 중간에 끝난 것마다 한 줄. 긴 보고 한 번보다 짧은 줄 여러 번이 낫다.\n\
보고는 셋이다: **바뀐 것 / 걸리는 것 / 못 확인한 것**. 마지막 칸을 비우지 마라 — 검증 못 한 것을 안 적으면 다 된 것처럼 읽힌다.\n\
그리고 하려다 만 것·곁길로 샐 것 같은 것은 발견 즉시 「이거 파도 되나」 한 줄로 물어라. 혼자 판단해서 파고들면 시간은 네가 쓰고 놀라는 건 거노다.";

/// cwd → slug. kasacollab.py `mode_path`·socket.rs base_slug 와 같은 규칙('/'·'.' → '-').
pub fn mode_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// 방별 collab slug — socket.rs board 읽기와 동일(base + 방이면 `__room_<id>`).
pub fn rslug(cwd: &Path, room: Option<&str>) -> String {
    let base = mode_slug(cwd);
    match room {
        Some(r) => format!("{base}__room_{r}"),
        None => base,
    }
}

fn collab_dir(rslug: &str) -> PathBuf {
    kasa_socket::collab_root().join(rslug)
}

/// `/tmp/kasaterm-collab/<rslug>/character-<N>` — board 가 row.character 로 읽는 마커.
pub fn character_marker(rslug: &str, surface_id: &str) -> PathBuf {
    collab_dir(rslug).join(format!("character-{}", surface_id.trim_start_matches('%')))
}

/// 한 collab 디렉토리의 character-* 마커 내용들.
fn assigned_in(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(n) = name.to_str() else { continue };
            if !n.starts_with("character-") {
                continue;
            }
            if let Ok(s) = std::fs::read_to_string(e.path()) {
                let s = s.trim().to_string();
                if !s.is_empty() {
                    out.push(s);
                }
            }
        }
    }
    out
}

/// 이 방에서 이미 배정된 캐릭터 이름들(character-* 마커 내용).
pub fn assigned(rslug: &str) -> Vec<String> {
    assigned_in(&collab_dir(rslug))
}

/// 모든 방(rslug)의 배정 캐릭터 — 전역 유일 배정용. /tmp/kasaterm-collab/ 아래 각
/// 방 디렉토리의 character-* 마커를 합친다. 닫힌 pane 마커는 cleanup_collab_markers
/// (layout.rs)가 지우므로 대체로 live 만 남는다 → 프로젝트(방)를 넘어 같은 학생이
/// 중복 배정되는 걸 막는다(거노: 미도리 둘).
pub fn assigned_global() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rooms) = std::fs::read_dir("/tmp/kasaterm-collab") {
        for room in rooms.flatten() {
            out.extend(assigned_in(&room.path()));
        }
    }
    out
}

/// 한 pane(surface)의 character-<N> 마커 내용. 없거나 비면 None.
/// resume 복원처럼 ws.pane_character 엔 없지만 마커엔 있는 캐릭터를 중복 배정에서
/// 피하려 쓴다(assign_character_env).
pub fn read_marker(rslug: &str, surface_id: &str) -> Option<String> {
    std::fs::read_to_string(character_marker(rslug, surface_id))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 후보 중 하나를 유사난수로 고른다 — 순서 고정(늘 미도리부터) 대신 랜덤 배정용
/// (거노: 완전 랜덤). 시드 = SystemTime nanos ^ pid ^ salt(pane id) 해시라, 같은
/// 순간 spawn 된 여러 pane 도 서로 갈린다. rand 크레이트 없이 std 만.
pub fn pick_random(candidates: &[String], salt: &str) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    seed ^= std::process::id() as u128;
    for b in salt.bytes() {
        seed = seed.wrapping_mul(131).wrapping_add(b as u128);
    }
    Some(candidates[(seed % candidates.len() as u128) as usize].clone())
}

/// character-<N> 마커를 원자적으로 쓴다(tmp → rename). board 가 즉시 읽는다.
pub fn write_marker(rslug: &str, surface_id: &str, name: &str) -> std::io::Result<()> {
    let path = character_marker(rslug, surface_id);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, name)?;
    std::fs::rename(&tmp, &path)
}

/// 세션id→캐릭터 영속 매핑 파일 — `~/.config/kasaterm/session_characters.json`
/// (window.json 등 기존 상태 저장과 같은 config 디렉토리). 같은 세션을 --resume 등으로
/// 이어가면 같은 캐릭터를 재사용하기 위한 저장소(거노: 재시작하면 프라나가 미도리로 둔갑).
fn session_char_path() -> PathBuf {
    kasa_socket::home_dir()
        .unwrap_or_default()
        .join(".config/kasaterm/session_characters.json")
}

fn load_session_chars(path: &Path) -> serde_json::Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

type CharCache = Option<(PathBuf, std::time::SystemTime, u64, serde_json::Map<String, Value>)>;
static CHARS: std::sync::Mutex<CharCache> = std::sync::Mutex::new(None);

/// 파싱한 매핑을 빌려준다 — 파일이 그대로면 디스크를 다시 읽지 않는다.
///
/// 렌더가 pane 마다, 프레임마다 부르는 경로다. 캐시 없이 두면 창 하나가 코어를
/// 통째로 태운다(실측: `sample` 상 렌더 프레임 시간의 77%가 이 안의 serde_json).
/// 무효화 판정은 mtime+크기 — 쓰기가 tmp→rename 원자 교체라 내용이 바뀌면 둘 중
/// 하나는 반드시 달라지고, 쓰는 쪽이 직접 캐시를 비우기까지 한다.
fn with_session_chars<R>(path: &Path, f: impl FnOnce(&serde_json::Map<String, Value>) -> R) -> R {
    let stamp = std::fs::metadata(path)
        .ok()
        .and_then(|m| Some((m.modified().ok()?, m.len())));
    // stat 이 실패하면 언제 무효화할지 알 수 없다 — 캐시하지 않고 그때그때 읽는다.
    let Some((mtime, len)) = stamp else {
        return f(&load_session_chars(path));
    };
    let Ok(mut g) = CHARS.lock() else {
        return f(&load_session_chars(path));
    };
    let fresh = g
        .as_ref()
        .is_some_and(|(p, t, l, _)| p == path && *t == mtime && *l == len);
    if !fresh {
        *g = Some((path.to_path_buf(), mtime, len, load_session_chars(path)));
    }
    match g.as_ref() {
        Some((_, _, _, map)) => f(map),
        None => f(&serde_json::Map::new()),
    }
}

/// 세션 id 의 영속 배정 캐릭터 — 있으면 재사용, 없으면(None) 신규 세션이라 랜덤 배정.
pub fn session_character(sid: &str) -> Option<String> {
    session_character_in(&session_char_path(), sid)
}

fn session_character_in(path: &Path, sid: &str) -> Option<String> {
    with_session_chars(path, |m| {
        m.get(sid)
            .and_then(|v| v.as_str())
            .map(String::from)
            .filter(|s| !s.is_empty())
    })
}

/// 세션id→캐릭터 매핑 저장(같은 값이면 무쓰기). 원자 쓰기(tmp→rename, write_marker 관례).
pub fn bind_session_character(sid: &str, name: &str) -> std::io::Result<()> {
    bind_session_character_in(&session_char_path(), sid, name)
}

fn bind_session_character_in(path: &Path, sid: &str, name: &str) -> std::io::Result<()> {
    if sid.is_empty() || name.is_empty() {
        return Ok(());
    }
    let mut map = load_session_chars(path);
    if map.get(sid).and_then(|v| v.as_str()) == Some(name) {
        return Ok(());
    }
    map.insert(sid.to_string(), Value::String(name.to_string()));
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&Value::Object(map)).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, body)?;
    let r = std::fs::rename(&tmp, path);
    // mtime 판정에만 기대지 않는다 — 쓰고 바로 읽는 흐름에서 파일시스템 시각
    // 해상도가 두 시점을 같게 볼 여지를 없앤다.
    if let Ok(mut g) = CHARS.lock() {
        *g = None;
    }
    r
}

/// 새 `claude --session-id` 용 uuid. claude 가 엄격한 UUID 형식을 요구하므로
/// 외부 uuidgen(Windows 부재 → kt- 폴백이 "Invalid session ID" 유발) 대신 crate 생성.
pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_character_roundtrip() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!("kasaterm-sesschar-{}-{n}", std::process::id()))
            .join("session_characters.json");
        // 미매핑 세션 = None → 랜덤 배정 대상.
        assert_eq!(session_character_in(&path, "sid-1"), None);
        bind_session_character_in(&path, "sid-1", "프라나").unwrap();
        bind_session_character_in(&path, "sid-2", "미도리").unwrap();
        assert_eq!(session_character_in(&path, "sid-1").as_deref(), Some("프라나"));
        assert_eq!(session_character_in(&path, "sid-2").as_deref(), Some("미도리"));
        // 재바인딩은 덮어쓴다(마지막 배정이 정본) — 다른 sid 는 불변.
        bind_session_character_in(&path, "sid-1", "모모이").unwrap();
        assert_eq!(session_character_in(&path, "sid-1").as_deref(), Some("모모이"));
        assert_eq!(session_character_in(&path, "sid-2").as_deref(), Some("미도리"));
        // 빈 sid/이름은 무시(파일 오염 방지).
        bind_session_character_in(&path, "", "유즈").unwrap();
        bind_session_character_in(&path, "sid-3", "").unwrap();
        assert_eq!(session_character_in(&path, "sid-3"), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// 읽기는 파싱 결과를 캐시한다(렌더가 프레임마다 부르는 경로) — 그 캐시가
    /// **다른 프로세스의 쓰기**를 놓치면 학생이 옛 이름으로 굳는다. 여기서
    /// `bind_*` 를 거치지 않고 파일을 직접 갈아 끼우는 이유다(그쪽은 스스로
    /// 캐시를 비우므로 무효화 판정을 검증하지 못한다).
    #[test]
    fn session_chars_reload_on_external_write() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!("kasaterm-sesschar-ext-{}-{n}", std::process::id()))
            .join("session_characters.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"sid-x":"프라나"}"#).unwrap();
        assert_eq!(session_character_in(&path, "sid-x").as_deref(), Some("프라나"));
        std::fs::write(&path, r#"{"sid-x":"하늘색 미도리"}"#).unwrap();
        assert_eq!(
            session_character_in(&path, "sid-x").as_deref(),
            Some("하늘색 미도리")
        );
        // 파일이 사라지면 캐시가 아니라 없음으로 읽혀야 한다.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(session_character_in(&path, "sid-x"), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
