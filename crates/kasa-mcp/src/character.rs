//! 캐릭터 배정 — characters.json persona + /tmp 마커 + 빈 슬롯 순환.
//!
//! 자율통솔·MCP `/spawn` 폐기(거노) 후, 학생 정체성을 백엔드(kasaterm)가
//! pane 생성 시점에 직접 박는다. 사용자가 그 pane 에서 `claude` 를 치면 shim 이
//! 여기서 심은 env(KASATERM_CHARACTER/SESSION_ID/PERSONA)를 --session-id·
//! --append-system-prompt 로 적용한다. board(socket.rs)는 같은 /tmp 마커를 읽어
//! `row.character` 를 채우므로, 마커 경로 규칙은 socket.rs 의 rslug 와 일치해야 한다.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Windows GUI 프로세스엔 HOME 이 없다 — USERPROFILE 이 그 자리.
fn home() -> Option<PathBuf> {
    std::env::var("HOME").ok().or_else(|| std::env::var("USERPROFILE").ok()).map(PathBuf::from)
}

/// settings.json 의 문자열 설정 하나. 앱의 `socket::read_settings` 와 같은 파일을
/// 보지만 여기서 직접 읽는다 — kasa-mcp → app 은 없는 의존 방향이라 부를 수가 없다.
fn read_setting_str(key: &str) -> Option<String> {
    let p = home()?.join(".config/kasaterm/settings.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
    v.get(key)?.as_str().map(String::from)
}

/// 테마 팩 루트 — `~/.config/kasaterm/themes/`. **폴더 하나가 테마 하나**다:
/// `theme.json`(로스터 + 팔레트) + `sprites/`(캐릭터 그림). 지금까지 흩어져 있던
/// 세 override(`students/`·`characters.json`·`custom_theme`)를 한 단위로 묶은 것이라,
/// 폴더째 주고받으면 그게 곧 테마 배포다.
fn themes_root() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_THEMES_DIR") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    Some(home()?.join(".config/kasaterm/themes"))
}

/// 활성 테마 폴더. settings.json 의 `character_theme` 가 고른다(팔레트 테마 키
/// `theme` 와 다른 이름인 이유가 이것 — 둘은 따로 고른다).
///
/// **폴더에 `theme.json` 이 실재할 때만** 돌려준다. 테마를 지우고 앱을 켜면 설정
/// 키만 남는데 그걸 믿으면 로스터가 통째로 비어 캐릭터 배정이 멈춘다. 없으면
/// 조용히 번들로 돌아가는 쪽이 맞다.
/// 캐시. 바깥 `None` = 아직 안 정함, `Some(None)` = 테마 없음(번들로 간다).
///
/// 캐시가 필요한 이유: `students_dir()` 을 거쳐 **매 프레임** 불린다
/// (`student_has_sprite` 가 스프라이트 슬롯을 세우기 전에 묻는다). 캐시가 없으면
/// 프레임마다 settings.json 을 열게 된다.
static ACTIVE_THEME: std::sync::RwLock<Option<Option<PathBuf>>> = std::sync::RwLock::new(None);

pub fn active_theme_dir() -> Option<PathBuf> {
    if let Some(v) = ACTIVE_THEME.read().unwrap().as_ref() {
        return v.clone();
    }
    let mut w = ACTIVE_THEME.write().unwrap();
    // 잠금을 바꿔 잡는 사이 다른 스레드가 이미 정했을 수 있다.
    if let Some(v) = w.as_ref() {
        return v.clone();
    }
    let v = resolve_active_theme_dir();
    *w = Some(v.clone());
    v
}

/// 테마를 갈아 끼운 뒤 부른다 — 다음 조회가 다시 해석한다.
/// `theme::invalidate_roster()` 와 **짝으로** 불러야 한다. 한쪽만 비우면 그림은 새
/// 테마인데 이름·색은 옛 테마가 되어, 화면이 두 테마를 섞어 보여 준다.
pub fn invalidate_active_theme() {
    *ACTIVE_THEME.write().unwrap() = None;
}

fn resolve_active_theme_dir() -> Option<PathBuf> {
    // env 가 설정을 이긴다 — 헤드리스 검증이 사용자 settings.json 을 건드리지 않고
    // 테마를 갈아 끼울 유일한 손잡이다(`KASATERM_STUDENTS_DIR` 과 같은 역할).
    let id = std::env::var("KASATERM_CHARACTER_THEME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| read_setting_str("character_theme"))?;
    active_theme_dir_in(&themes_root()?, &id)
}

/// root 주입 버전(테스트용) — themes_root·settings 해석과 분리해 env 없이 검증한다.
fn active_theme_dir_in(root: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() {
        return None;
    }
    let d = root.join(id);
    d.join("theme.json").is_file().then_some(d)
}

/// 설치된 테마 `(id, label)` 목록 — 설정 화면의 선택지. 번들은 폴더가 없으니 여기
/// 안 들어간다(호출부가 맨 앞에 따로 세운다). label 은 `theme.json` 의 `label`,
/// 없으면 기존 스키마의 `theme` 필드, 그것도 없으면 폴더 이름.
pub fn list_themes() -> Vec<(String, String)> {
    themes_root().map(|r| list_themes_in(&r)).unwrap_or_default()
}

/// root 주입 버전(테스트용).
fn list_themes_in(root: &Path) -> Vec<(String, String)> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<(String, String)> = rd
        .flatten()
        .filter_map(|e| {
            let f = e.path().join("theme.json");
            if !f.is_file() {
                return None;
            }
            let id = e.file_name().to_str()?.to_string();
            let label = std::fs::read_to_string(&f)
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| {
                    v.get("label")
                        .or_else(|| v.get("theme"))
                        .and_then(|x| x.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| id.clone());
            Some((id, label))
        })
        .collect();
    out.sort();
    out
}

/// characters.json 후보 경로 — kasaterm-assign-character.py 와 동일 우선순위:
/// 활성 테마 → ~/.config → env override → .app Resources(mac)/exe 옆(win MSI) → 레포 소스.
fn candidate_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    // 테마가 최우선 — 고른 테마의 로스터가 개별 override 보다 앞선다. 뒤에 두면
    // 옛 characters.json 이 남아 있는 사용자에게 테마 선택이 아무 일도 안 한다.
    if let Some(d) = active_theme_dir() {
        v.push(d.join("theme.json"));
    }
    if let Some(home) = home() {
        v.push(home.join(".config/kasaterm/characters.json"));
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

/// 방 학교 판정에서 빼는 소속 — 학원이 아니라 소속 기관이고 인원도 둘뿐이라,
/// 이게 방 학교로 잡히면 그 방은 두 명 쓰고 곧장 고갈된다.
const NOT_A_SCHOOL: &str = "샬레";

/// 이름 → 학원. 없는 캐릭터(옛 테마·커스텀)는 `None`.
pub fn school_of(chars: &Value, name: &str) -> Option<String> {
    let s = find_character(chars, name)?.get("school")?.as_str()?;
    (s != NOT_A_SCHOOL).then(|| s.to_string())
}

/// 후보를 **그 방에 이미 있는 학생들과 같은 학원**으로 좁힌다.
///
/// 한 방(프로젝트)에 같은 학원 학생들이 모이면 화면이 한 덩어리로 읽힌다 —
/// 거노 2026-08-11: "방마다 같은학원소속이나 연관되게 생성되면 재밌을듯".
///
/// 좁힌 결과가 비면 **빈 Vec 을 돌려준다**. 호출부가 원래 후보로 폴백해야 한다 —
/// 학원을 맞추는 것보다 학생이 겹치지 않는 게 먼저다(같은 방에 같은 얼굴이 둘이면
/// 누가 누군지 사라진다).
///
/// 방이 비어 있으면(첫 학생) 좁히지 않는다. 그 첫 배정이 그 방의 학원을 정한다.
pub fn prefer_same_school(chars: &Value, free: &[String], room: &[String]) -> Vec<String> {
    let here: std::collections::HashSet<String> =
        room.iter().filter_map(|n| school_of(chars, n)).collect();
    if here.is_empty() {
        return Vec::new();
    }
    free.iter()
        .filter(|n| school_of(chars, n).is_some_and(|s| here.contains(&s)))
        .cloned()
        .collect()
}

/// 방의 **첫 학생**을 고를 때, 다른 방이 이미 쓰는 학원을 피한다.
///
/// 첫 배정이 그 방의 학원을 정하므로(`prefer_same_school`), 여기서 갈라 두면 방마다
/// 다른 학원이 서서 화면에서 방이 구분된다. 학원 수보다 방이 많으면 빈 Vec 을 주고,
/// 그때는 겹쳐도 된다 — 방이 갈리는 것보다 학생이 안 겹치는 게 먼저다.
pub fn prefer_fresh_school(chars: &Value, free: &[String], elsewhere: &[String]) -> Vec<String> {
    let used: std::collections::HashSet<String> =
        elsewhere.iter().filter_map(|n| school_of(chars, n)).collect();
    free.iter()
        .filter(|n| school_of(chars, n).is_some_and(|s| !used.contains(&s)))
        .cloned()
        .collect()
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

/// 설정 폼이 persona·색을 저장할 파일 — `candidate_paths` 의 최우선 슬롯과 **같은
/// 자리여야 한다**. 활성 테마가 있으면 그 테마의 `theme.json`, 없으면 기존
/// `~/.config/kasaterm/characters.json`.
///
/// 여기가 읽기 우선순위와 어긋나면 저장은 성공하는데 읽기는 테마가 이겨서, 편집한
/// persona 가 화면에 영영 안 나타난다 — 오류도 안 나므로 알아챌 방법이 없다.
pub fn user_characters_path() -> Option<PathBuf> {
    if let Some(d) = active_theme_dir() {
        return Some(d.join("theme.json"));
    }
    Some(home()?.join(".config/kasaterm/characters.json"))
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

/// 모든 캐릭터 persona 끝에 붙는 협업 규약 — 동료를 기다리는 기본은 **그냥 기다리는
/// 것**이다. 학생 보고(SendMessage)가 알아서 도착하므로 완료 감시는 중복이고,
/// board-watch 는 모든 pane 을 보므로 `idle` 을 넣으면 남의 턴 종료마다 깨운다
/// (거노 2026-08-10: "어차피 끝나면 보고하는데 필요없지 않나"). 그래서 Monitor 는
/// **보고가 올 수 없는 상태**(승인 막힘·죽음·경로 끊김)에만 남겼다.
const COLLAB_PROTOCOL: &str = "\n\n[협업 — 동료 기다리기]\n\
**기본은 그냥 기다리는 것이다.** 학생에게 「끝나면 알려라」고 시켰으면 SendMessage 가 알아서 도착한다 — 상대가 유휴로 떠 있어도 읽는다. 거기에 감시를 겹치면 같은 완료를 두 번 받고, board-watch 는 **모든 pane** 을 보므로 내가 안 기다리는 남의 턴 종료마다 깨어나 토큰만 태운다.\n\
**Monitor 는 보고가 올 수 없을 때만 건다** — 승인 프롬프트에 막혔거나, 죽었거나, 보고 경로가 끊긴 것. 그 셋은 상대가 스스로 알릴 수가 없다(persistent: true):\n\
  kasaterm-cli board-watch 3 2>&1 | grep -E --line-buffered ' (waiting|attention)|\\[done:'\n\
⚠️ **`idle` 은 넣지 마라.** 「쉬는 중」일 뿐 완료가 아니고, 그 한 단어가 남의 턴마다 깨우는 원인이다. 완료의 정본은 `[done:` 이며 그것도 보고를 안 시킨 일감에만 필요하다.\n\
⚠️ 필터는 **필수**다. 안 걸면 매 도구 호출까지 흘러나와 12초에 8줄(분당 40줄)이 되고, Monitor 가 알림 폭주로 자동 중지된다(실측).\n\
⚠️ **침묵을 성공으로 읽지 마라.** `SendMessage` 의 success 는 도달 증명이 아니고(죽은 상대에게도 「Message sent」가 온다), 이름이 어긋나면 오류 없이 사라진다. 끝났는지는 상대가 남긴 것(커밋·파일·`peek`·`transcript`)으로 확인해라.\n\
⚠️ `kasaterm-cli wake-watch <surface_id>` 는 **동료가 끝났는데 완료를 못 잡고 40분 타임아웃으로 죽은 실측이 있다**. 쓰지 마라.\n\
\n\
[협업 — 학생 채팅]\n\
**SendMessage 가 기본이고, 방(cwd)이 달라도 닿는다.** pane claude 는 트리플 없이 세션 이름만 갖고 뜨므로 전부 cross-session 명부(`~/.claude/sessions/`)에 오른다 — 다른 레포에 띄운 학생에게도 그냥 간다. 유휴로 프롬프트만 떠 있어도 읽는다. 도구 한 번이면 끝이고 상대 화면을 어지럽히지 않는다.\n\\
**보내기 전에 `ListAgents` 로 이름을 확인해라.** 거기 뜬 이름을 `to` 에 그대로 넣는다(`[ref]` 는 이름이 겹치거나 오류가 시킬 때만 덧붙인다). 이름을 `<슬러그>-p<번호>` 규칙으로 짐작하지 마라 — 어긋나도 오류가 안 나고 조용히 사라진다.\n\\
⚠️ **`SendMessage` 의 `success` 는 도달 증명이 아니다** — 이미 죽은 상대에게 보내도 「Message sent」가 돌아온다(실측). 지시가 먹었는지는 상대가 남긴 것(커밋·파일·`peek`)으로 확인해라.\n\\
⚠️ **트리플(`--agent-id`)을 직접 주고 claude 를 띄우지 마라.** 그 세션은 명부에서 통째로 제외돼(등록 함수 첫 줄 `if(W4()!=null) return false`) 남을 못 찾고 남도 못 찾는다 — **발신·수신 양쪽이 다 죽는다**(2026-08-09 실측, 이것 때문에 트리플을 걷어냈다). shim 이 알아서 이름만 붙이니 손대지 마라.\n\\
**태스크 목록은 이제 pane 마다 따로다**(팀이 없어졌다). 남이 뭘 하는지는 `kasaterm-cli board` 로 본다.\n\\
`kasaterm-cli tell <surface_id> \"...\"` 는 **SendMessage 가 안 닿을 때만** — 비-claude pane(codex 등)이나 `ListAgents` 에 안 뜨는 세션. tell 은 상대 입력창에 글자를 밀어넣는 것이라 상대가 타이핑 중이면 섞인다. ⚠️ tell 본문에 네 이름을 붙이지 마라 — 「아로나: 확인했어요」 말고 「확인했어요」만. kasaterm-cli 가 발신 마커를 붙여 네 프사·학생색으로 렌더하므로 직접 쓴 이름은 중복이 된다.\n\\
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
**모델은 「가벼우냐」가 아니라 「컨텍스트를 태우느냐」로 가른다.** glm·kimi 는 200k 라 Claude 의 1/5 다 — 가볍더라도 **오래 훑는 일**(큰 바이너리 grep, 파일 수십 개 열기)에 붙이면 중간에 말라 죽고, 반대로 **짧지만 무거운 판단**(갈림길 결정, 함정 해석)은 창을 거의 안 먹으면서 품질이 갈린다. 그래서:\n\
- **glm·kimi** — 답이 정해진 수집(grep·검색·스샷·목록화), 결과가 짧게 요약돼 돌아오는 일. `cd <레포> && glm claude --dangerously-skip-permissions` (`glm`→`kimi` 로 바꾸면 Kimi). 브리프는 SendMessage 로.\n\
- **opus·fable(나)** — 갈림길 판단, 설계, 남의 결과를 종합해 다음을 정하는 일.\n\
**비싼 창을 「읽느라」 태우지 마라** — 2026-08-09 실측: 279MB 바이너리를 grep·dd 로 반복해 훑는 일을 내가 직접 하다 창을 크게 태웠다. 그건 glm 에게 「이 오프셋 주변 문자열을 뽑아 와라」로 넘겼어야 했고, 내가 할 것은 그 결과가 무슨 뜻인지 해석하는 쪽이었다.\n\
⚠️ `--dangerously-skip-permissions` 를 **직접 줘야 한다** — `glm`·`kimi` 는 `command claude` 라 zshrc 의 claude 별칭을 건너뛴다. 빠뜨리면 학생이 권한 프롬프트에서 멈춘다.\n\
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
[협업 — 완료 보고]\n\
**남이 시킨 작업(브리프)을 끝냈으면 마지막 액션으로 보고해라 — 성공이든 실패든:**\n\
  `kasaterm-cli done succeeded \"한 줄: 뭘 했고, 뭘 확인 못 했고, 뭐가 남았나\"`\n\
실패로 끝났으면 `succeeded` 대신 `failed`. **이 보고까지가 작업이다** — 안 하면 오케스트레이터는 네 화면을 읽어 「끝났나 보다」를 추측해야 하고, 추측은 어긋난다(idle 은 「쉬는 중」이지 「다 됐다」가 아니다).\n\
- board 에 결과·요약·경과가 정본으로 뜨고, 네가 새 브리프를 받아 다시 일을 시작하면 자동으로 걷힌다.\n\
- 실패를 프로즈로만 남기지 마라 — 기계가 못 읽는다. `failed` 로 보고하고 요약에 원인 한 줄.\n\
- 스스로 시작한 일(브리프 없음)엔 안 해도 된다 — 이건 배정받은 일의 완료 신호다.\n\
\n\
[협업 — 해산]\n\
일이 끝나면 인사말을 주고받지 말고 **그냥 닫아라**: `kasaterm-cli dismiss %64 %65`. 커밋 안 된 변경이 남은 pane 은 닫지 않고 알려주므로, 그때만 회수하면 된다. 「마무리하겠습니다」·「수고했다」·완료 인사는 전부 없어도 되는 왕복이다 — 무엇이 끝났는지는 커밋과 `done` 보고가 말한다.\n\
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
///
/// Windows 네이티브 경로는 먼저 Git bash 형태(`C:\Users\x` → `/c/Users/x`)로 정규화한다.
/// 이유가 둘이다. ① 훅(`kasaterm-steer-hook.sh` 의 `pwd | sed 's#[/.]#-#g'`)은 Git bash
/// 안에서 도니 언제나 그 형태를 만든다 — 정규화 없이는 훅과 앱이 서로 다른 방을 보고
/// 영영 안 만난다. ② 정규화 전 슬러그는 `C:\...` 라 **절대경로**고, `collab_root().join()`
/// 에 절대경로를 주면 base 가 통째로 버려져 마커가 collab 루트가 아니라 **프로젝트 폴더
/// 안에** 쓰인다(실측). unix 경로는 이 단계를 그대로 통과하므로 동작이 바뀌지 않는다.
pub fn mode_slug(cwd: &Path) -> String {
    posix_style(&cwd.to_string_lossy())
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// `C:\Users\x` → `/c/Users/x`. 이미 posix 형태거나 드라이브 문자가 없으면 구분자만 바꾼다.
/// UNC 확장 접두사(`\\?\`)는 `canonicalize` 가 붙여 주는 것이라 먼저 떼야 한다.
///
/// Windows 밖에선 통째로 통과시킨다 — unix 폴더 이름엔 역슬래시가 실제로 들어갈
/// 수 있어서, 거기서까지 구분자로 접으면 멀쩡하던 슬러그가 sh 훅(`pwd | sed`)과
/// 갈린다. 고칠 대상은 Windows 뿐이다.
fn posix_style(raw: &str) -> String {
    if !cfg!(windows) {
        return raw.to_string();
    }
    let raw = raw.strip_prefix(r"\\?\").unwrap_or(raw);
    let mut chars = raw.chars();
    let drive = chars.next().filter(|c| c.is_ascii_alphabetic());
    let has_colon = chars.next() == Some(':');
    match (drive, has_colon) {
        (Some(d), true) => {
            let rest = raw[2..].replace('\\', "/");
            let rest = rest.strip_prefix('/').unwrap_or(&rest);
            format!("/{}/{}", d.to_ascii_lowercase(), rest)
        }
        _ => raw.replace('\\', "/"),
    }
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

/// 마커 파일 이름인가 — `character-<N>`. `write_marker` 의 원자 교체가 잠깐 남기는
/// `character-<N>.tmp` 는 뺀다(같은 접두사로 시작해 그냥 두면 유령 배정으로 잡힌다).
fn is_marker_file(name: &str) -> bool {
    name.starts_with("character-") && !name.ends_with(".tmp")
}

/// 마커 본문에서 캐릭터 이름만. 형식은 `<이름>\n<쓴 kasaterm pid>` 이고 pid 줄은
/// sweep 전용이라 이름을 읽는 모든 경로가 첫 줄만 본다. pid 가 없는 옛 마커도 그대로
/// 읽힌다(한 줄뿐이라 첫 줄 = 전부).
fn marker_name(body: &str) -> Option<String> {
    let s = body.lines().next()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// 한 collab 디렉토리의 character-* 마커 내용들.
fn assigned_in(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(n) = name.to_str() else { continue };
            if !is_marker_file(n) {
                continue;
            }
            if let Some(s) = std::fs::read_to_string(e.path()).ok().as_deref().and_then(marker_name)
            {
                out.push(s);
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
    std::fs::read_to_string(character_marker(rslug, surface_id)).ok().as_deref().and_then(marker_name)
}

/// 죽은 kasaterm 이 남긴 character-* 마커를 지운다. `live` 는 「이 pid 가 지금 도는
/// kasaterm 인가」.
///
/// **왜 필요한가.** 마커는 pane 을 *정상적으로 닫을 때만* 지워진다
/// (`cleanup_collab_markers`). 그래서 앱이 죽거나 그냥 재시작되면 옛 마커가 전부 남고,
/// 배정은 그것들을 taken 으로 세므로 쓸 수 있는 학생이 재시작마다 줄어든다. 실측
/// 2026-08-09 에 마커 17개 > 총원 12명이 되어 풀이 통째로 말랐고, 그때 폴백이
/// members 전체로 되돌아가 **아루가 셋**이 됐다. 지금은 그 폴백이 이 방 live 만 피하는
/// 단계를 거치지만, 그건 피해를 줄일 뿐 마르는 것 자체를 못 막는다.
///
/// 살았는지를 pane 번호로는 못 판단한다 — 번호는 방 간에도 창 간에도 재사용된다.
/// 그래서 마커를 쓸 때 **쓴 프로세스의 pid** 를 함께 적고 여기서 그 pid 를 본다.
/// pid 줄이 없는 마커는 이 코드가 나오기 전 것이므로 지운다 — 지금 도는 kasaterm 이
/// 쓴 것이라면 반드시 pid 가 있다.
///
/// 지워도 살아있는 pane 은 안 다친다. 배정의 정본은 `ws.pane_character` 이고 마커는
/// 그 사본이라, 잘못 지워봐야 **다른 인스턴스가 그 캐릭터를 겹쳐 쓸 수 있다**가 전부다.
/// 반대로 안 지우면 풀이 마른다.
pub fn sweep_stale_markers(live: impl Fn(u32) -> bool) -> usize {
    sweep_stale_markers_in(&kasa_socket::collab_root(), live)
}

/// [`sweep_stale_markers`] 의 본체 — 루트를 받는 건 테스트 때문이다. 이 함수는 **모든
/// 방**을 훑으므로 실제 `/tmp/kasaterm-collab` 에 대고 테스트하면 돌고 있는 pane 의
/// 마커를 지운다.
fn sweep_stale_markers_in(root: &Path, live: impl Fn(u32) -> bool) -> usize {
    let mut n = 0;
    let Ok(rooms) = std::fs::read_dir(root) else { return 0 };
    for room in rooms.flatten() {
        let Ok(rd) = std::fs::read_dir(room.path()) else { continue };
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(f) = name.to_str() else { continue };
            if !is_marker_file(f) {
                continue;
            }
            let owner = std::fs::read_to_string(e.path())
                .ok()
                .and_then(|s| s.lines().nth(1)?.trim().parse::<u32>().ok());
            if owner.is_some_and(&live) {
                continue;
            }
            if std::fs::remove_file(e.path()).is_ok() {
                n += 1;
            }
        }
    }
    n
}

/// 후보 중 **가장 적게 쓰인** 것들만 남긴다. 배정 풀이 말랐을 때의 폴백이다.
///
/// pane 수가 학생 총원을 넘으면 중복은 비둘기집이라 못 막는다. 막을 수 있는 건
/// **몰리는 것**이다 — 예전 폴백은 전체에서 그냥 랜덤이라 이미 셋인 학생이 넷이 됐다
/// (실측 2026-08-11: pane 15 > 총원 12 인데 아루 3·프라나 3 이고 한 번도 안 쓰인
/// 학생이 남아 있었다). 최소 사용 횟수인 쪽만 남기면 15명이어도 3명만 2회씩이 된다.
///
/// `taken` 은 **중복을 살린** 목록이어야 한다. HashSet 을 넘기면 횟수가 사라져
/// 이 함수가 하는 일이 없어진다.
pub fn least_used(candidates: &[String], taken: &[String]) -> Vec<String> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let mut n: std::collections::HashMap<&str, usize> =
        candidates.iter().map(|c| (c.as_str(), 0)).collect();
    for t in taken {
        if let Some(c) = n.get_mut(t.as_str()) {
            *c += 1;
        }
    }
    let lo = n.values().copied().min().unwrap_or(0);
    candidates.iter().filter(|c| n[c.as_str()] == lo).cloned().collect()
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
///
/// 둘째 줄에 이 프로세스 pid 를 남긴다 — 마커가 죽은 뒤에도 남는 문제를
/// [`sweep_stale_markers`] 가 풀 때 「누가 쓴 것인가」의 유일한 단서다. 이름을 읽는
/// 경로는 전부 첫 줄만 보므로 board·배정에는 아무 변화가 없다.
pub fn write_marker(rslug: &str, surface_id: &str, name: &str) -> std::io::Result<()> {
    let path = character_marker(rslug, surface_id);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, format!("{name}\n{}", std::process::id()))?;
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
mod same_school_tests {
    use super::{prefer_same_school, school_of};
    use serde_json::json;

    fn chars() -> serde_json::Value {
        json!({
            "leaders": [{"name": "아로나", "slug": "arona", "school": "샬레"}],
            "members": [
                {"name": "미도리", "slug": "midori", "school": "밀레니엄"},
                {"name": "유즈",   "slug": "yuzu",   "school": "밀레니엄"},
                {"name": "케이",   "slug": "kei",    "school": "밀레니엄"},
                {"name": "아루",   "slug": "aru",    "school": "게헨나"},
                {"name": "히나",   "slug": "hina",   "school": "게헨나"},
                {"name": "코하루", "slug": "koharu", "school": "트리니티"},
            ]
        })
    }

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pulls_the_rooms_own_school() {
        let free = v(&["유즈", "케이", "아루", "히나", "코하루"]);
        let got = prefer_same_school(&chars(), &free, &v(&["미도리"]));
        assert_eq!(got, v(&["유즈", "케이"]));
    }

    /// 첫 배정은 좁히지 않는다 — 그 학생이 그 방의 학원을 정한다.
    #[test]
    fn empty_room_is_not_narrowed() {
        let free = v(&["미도리", "아루"]);
        assert!(prefer_same_school(&chars(), &free, &[]).is_empty());
    }

    /// 학원이 마르면 빈 Vec — 호출부가 원래 후보로 폴백해야 한다. 학원을 맞추는 것보다
    /// 같은 방에서 학생이 안 겹치는 게 먼저다.
    #[test]
    fn exhausted_school_falls_through() {
        let free = v(&["아루", "히나", "코하루"]);
        assert!(prefer_same_school(&chars(), &free, &v(&["미도리", "유즈", "케이"])).is_empty());
    }

    /// 샬레는 학원이 아니다 — 둘뿐이라 방 학교로 잡히면 그 방이 즉시 마른다.
    #[test]
    fn shale_never_becomes_a_rooms_school() {
        assert_eq!(school_of(&chars(), "아로나"), None);
        let free = v(&["미도리", "아루"]);
        assert!(prefer_same_school(&chars(), &free, &v(&["아로나"])).is_empty());
    }

    /// 방에 두 학원이 섞여 있으면(폴백으로 그렇게 된다) 둘 다 인정한다 — 한쪽만
    /// 고르면 이미 있는 다른 쪽이 영영 안 늘어난다.
    #[test]
    fn mixed_room_keeps_both_schools() {
        let free = v(&["유즈", "히나", "코하루"]);
        let got = prefer_same_school(&chars(), &free, &v(&["미도리", "아루"]));
        assert_eq!(got, v(&["유즈", "히나"]));
    }

    #[test]
    fn fresh_school_avoids_other_rooms() {
        use super::prefer_fresh_school;
        let free = v(&["유즈", "케이", "아루", "히나", "코하루"]);
        // 다른 방이 밀레니엄과 게헨나를 쓰고 있다 → 트리니티만 남는다.
        let got = prefer_fresh_school(&chars(), &free, &v(&["미도리", "아루"]));
        assert_eq!(got, v(&["코하루"]));
    }

    /// 학원보다 방이 많아지면 빈 Vec — 방이 갈리는 것보다 학생이 안 겹치는 게 먼저다.
    #[test]
    fn fresh_school_gives_up_when_all_used() {
        use super::prefer_fresh_school;
        let free = v(&["유즈", "히나"]);
        let all = v(&["미도리", "아루", "코하루"]);
        assert!(prefer_fresh_school(&chars(), &free, &all).is_empty());
    }

    /// 로스터에 없는 이름(옛 마커·커스텀 캐릭터)이 방에 있어도 안 흔들린다.
    #[test]
    fn unknown_names_are_ignored() {
        let free = v(&["유즈", "아루"]);
        let got = prefer_same_school(&chars(), &free, &v(&["미도리", "모르는이름"]));
        assert_eq!(got, v(&["유즈"]));
    }
}

#[cfg(test)]
mod least_used_tests {
    use super::least_used;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn unused_candidates_win() {
        let members = v(&["아루", "미도리", "케이"]);
        let taken = v(&["아루", "아루", "미도리"]);
        assert_eq!(least_used(&members, &taken), v(&["케이"]));
    }

    /// pane 이 총원을 넘으면 중복은 못 막는다. 막아야 하는 건 **몰리는 것**이다 —
    /// 이미 둘인 학생이 셋이 되는 대신 아직 하나인 쪽이 뽑혀야 한다.
    #[test]
    fn spreads_instead_of_piling_up() {
        let members = v(&["아루", "미도리", "케이"]);
        let taken = v(&["아루", "아루", "미도리", "케이"]);
        assert_eq!(least_used(&members, &taken), v(&["미도리", "케이"]));
    }

    /// 실측 재현: 총원 12, pane 15 에서 아루 3·프라나 3 이 나왔던 분포.
    /// 이 폴백이 있었다면 3회짜리는 후보에서 빠졌어야 한다.
    #[test]
    fn the_aru_times_three_case() {
        let members = v(&["아루", "프라나", "히마리", "호시노", "유우카", "코하루"]);
        let taken = v(&[
            "아루", "아루", "아루", "프라나", "프라나", "프라나", "히마리", "히마리",
            "호시노", "호시노", "유우카", "유우카",
        ]);
        // 한 번도 안 쓰인 코하루만 남아야 한다 — 예전 폴백은 여기서 아루를 넷째로
        // 뽑을 수 있었다.
        assert_eq!(least_used(&members, &taken), v(&["코하루"]));
    }

    /// `taken` 을 HashSet 으로 넘기면(=중복이 사라지면) 이 함수는 무의미해진다.
    /// 호출부가 중복을 살린 Vec 을 준다는 전제를 여기 박아 둔다.
    #[test]
    fn counts_duplicates_not_presence() {
        let members = v(&["아루", "미도리"]);
        let dedup = v(&["아루", "미도리"]);
        assert_eq!(least_used(&members, &dedup), v(&["아루", "미도리"]));
        let with_dupes = v(&["아루", "아루", "미도리"]);
        assert_eq!(least_used(&members, &with_dupes), v(&["미도리"]));
    }

    #[test]
    fn empty_candidates_stay_empty() {
        assert!(least_used(&[], &v(&["아루"])).is_empty());
    }

    /// 로스터에 없는 이름(커스텀 캐릭터·옛 마커)이 섞여도 셈이 흔들리면 안 된다.
    #[test]
    fn ignores_names_outside_the_roster() {
        let members = v(&["아루", "미도리"]);
        let taken = v(&["아루", "모르는이름", "또다른이름"]);
        assert_eq!(least_used(&members, &taken), v(&["미도리"]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 테마 폴더 판정은 `theme.json` 의 **실재**로만 한다.
    ///
    /// 설정 키만 믿으면, 테마 폴더를 지운 뒤 앱을 켰을 때 로스터가 통째로 비어
    /// 캐릭터 배정이 멈춘다. 그건 오류로 안 드러나고 「학생이 안 뜬다」로만 보인다.
    #[test]
    fn theme_pack_discovery() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kasaterm-themes-{}-{n}", std::process::id()));

        // label 있음 / label 없이 기존 스키마의 theme 필드만 / 둘 다 없음.
        for (id, body) in [
            ("eternal-return", r#"{"label":"이터널 리턴","members":[]}"#),
            ("wuwa", r#"{"theme":"명조","members":[]}"#),
            ("nameless", r#"{"members":[]}"#),
        ] {
            std::fs::create_dir_all(root.join(id)).unwrap();
            std::fs::write(root.join(id).join("theme.json"), body).unwrap();
        }
        // theme.json 없는 폴더는 테마가 아니다 — sprites 만 놓다 만 자리일 수 있다.
        std::fs::create_dir_all(root.join("half-made/sprites")).unwrap();

        let got = list_themes_in(&root);
        assert_eq!(
            got,
            vec![
                ("eternal-return".into(), "이터널 리턴".into()),
                ("nameless".into(), "nameless".into()),
                ("wuwa".into(), "명조".into()),
            ]
        );

        assert_eq!(
            active_theme_dir_in(&root, "eternal-return"),
            Some(root.join("eternal-return"))
        );
        // 폴더는 있는데 theme.json 이 없다 → 번들로 폴백.
        assert_eq!(active_theme_dir_in(&root, "half-made"), None);
        // 지워진 테마가 설정에 남아 있는 경우.
        assert_eq!(active_theme_dir_in(&root, "deleted"), None);
        assert_eq!(active_theme_dir_in(&root, ""), None);

        let _ = std::fs::remove_dir_all(&root);
    }

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

    /// 훅이 만드는 슬러그와 같은 자리로 접히는지. 어긋나면 앱과 훅이 서로 다른
    /// collab 방을 보고, 슬러그가 절대경로로 남으면 `collab_root().join()` 이 base 를
    /// 버려 마커가 collab 루트가 아니라 프로젝트 폴더 안에 쓰인다.
    ///
    /// 접기 자체가 Windows 전용이라(`posix_style`) 테스트도 거기서만 돈다.
    #[cfg(windows)]
    #[test]
    fn mode_slug_folds_windows_paths_like_git_bash() {
        // Git bash 의 `pwd` 는 `/c/Users/...` 를 주고 훅은 거기에 s#[/.]#-#g 를 건다.
        let from_hook: String = "/c/Users/kshkj/desktop/kasaterm"
            .chars()
            .map(|c| if c == '/' || c == '.' { '-' } else { c })
            .collect();
        assert_eq!(mode_slug(Path::new(r"C:\Users\kshkj\desktop\kasaterm")), from_hook);
        // canonicalize 가 붙이는 확장 접두사도 같은 자리로.
        assert_eq!(
            mode_slug(Path::new(r"\\?\C:\Users\kshkj\desktop\kasaterm")),
            from_hook,
        );
        // 슬러그는 반드시 상대경로 — 절대경로면 join 이 base 를 통째로 버린다.
        assert!(!Path::new(&mode_slug(Path::new(r"C:\Users\x"))).is_absolute());
    }

    /// unix 경로는 정규화를 그대로 통과해야 한다(기존 방 이름이 바뀌면 안 된다).
    #[test]
    fn mode_slug_leaves_unix_paths_alone() {
        assert_eq!(
            mode_slug(Path::new("/Users/kasa/dev/kasaterm")),
            "-Users-kasa-dev-kasaterm"
        );
        assert_eq!(mode_slug(Path::new("/tmp/room/mine")), "-tmp-room-mine");
        assert_eq!(mode_slug(Path::new("/a/b.c")), "-a-b-c");
    }

    /// 마커의 둘째 줄(주인 pid)이 배정 풀 고갈을 막는 유일한 단서다. 살아있는 주인 것은
    /// 남기고, 죽은 주인 것과 주인을 모르는 옛 형식은 지운다.
    #[test]
    fn sweep_keeps_live_owner_only() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kasaterm-sweep-{}-{n}", std::process::id()));
        let room = root.join("-tmp-room-a");
        let other = root.join("-tmp-room-b");
        std::fs::create_dir_all(&room).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(room.join("character-1"), "아루\n111").unwrap();
        std::fs::write(room.join("character-2"), "미도리\n222").unwrap();
        std::fs::write(other.join("character-1"), "유즈\n111").unwrap();
        // 이 코드가 나오기 전 형식 — 주인을 알 수 없으니 지운다.
        std::fs::write(other.join("character-9"), "프라나").unwrap();
        // 원자 교체가 남긴 조각은 마커가 아니다.
        std::fs::write(other.join("character-9.tmp"), "노아\n111").unwrap();

        assert_eq!(sweep_stale_markers_in(&root, |pid| pid == 111), 2);
        assert!(room.join("character-1").exists(), "살아있는 주인의 마커는 남는다");
        assert!(!room.join("character-2").exists(), "죽은 주인의 마커는 지운다");
        assert!(other.join("character-1").exists(), "다른 방도 주인 기준으로 남긴다");
        assert!(!other.join("character-9").exists(), "pid 없는 옛 마커는 지운다");
        assert!(other.join("character-9.tmp").exists(), ".tmp 는 건드리지 않는다");

        // 이름을 읽는 쪽은 pid 줄을 못 본다 — 배정·board 가 "아루\n111" 로 굳으면 안 된다.
        let mut names = assigned_in(&room);
        names.sort();
        assert_eq!(names, vec!["아루".to_string()]);

        let _ = std::fs::remove_dir_all(&root);
    }
}
