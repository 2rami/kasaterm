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
/// wake-watch 를 background 로 띄워 자동 재개(거노: task-notification wake 활용).
const COLLAB_PROTOCOL: &str = "\n\n[협업 — 동료 기다리기]\n\
다른 학생(동료 pane)의 작업이 끝나길 기다려야 할 때는, tell 로 깨우거나 board 를 반복해서 확인하지 말고 아래를 background 로 띄워라(Bash 도구의 run_in_background, 또는 명령 끝에 &):\n\
  kasaterm-cli wake-watch <동료 surface_id>\n\
동료의 surface_id 는 `kasaterm-cli board` 로 확인한다. 동료가 한 턴을 끝내면 이 명령이 스스로 종료되고, 시스템이 너를 자동으로 깨운다(task-notification). 깨어나면 그 출력(\"<동료> 작업 끝남\")을 보고 이어서 진행해라. 이렇게 하면 네 입력창을 더럽히지 않고 동료 완료 즉시 이어받는다.\n\
\n\
[협업 — 학생 채팅]\n\
SendMessage 도구는 **네가 트리플 플래그(--agent-id/--agent-name/--team-name)로 직접 스폰한 학생**에게 지시·브리프를 보낼 때만 써라(to: 스폰 시 지정한 agent-name). 그 외 모든 상대 — 네가 스폰하지 않은 같은 방 pane, 다른 방 pane, 백그라운드 세션, 비-claude pane, 그리고 오케스트레이터에게 하는 보고·질문·완료 통지 — 는 `kasaterm-cli tell <상대 surface_id> \"...\"` 가 정식 경로다(상대 surface_id 는 `kasaterm-cli board` 로 확인, 텍스트는 개행 없는 한 줄). tell 은 받는 pane 에 네 프사와 학생색으로 렌더되니 발신자 표기를 걱정하지 마라. SendMessage 가 'not reachable' 로 실패하면 그 상대는 스폰 관계가 아니라는 뜻이다 — 재시도하지 말고 tell 로 전환해라.\n\
\n\
[협업 — 학생 스폰]\n\
네가 직접 학생을 띄울 때는 **트리플 플래그를 반드시 붙여라** — 안 붙이면 그 학생에게 SendMessage 가 영영 닿지 않는다(인박스 폴러가 arm 되지 않음). 정본 한 줄:\n\
`kasaterm-cli send --surface <새 pane> $'cd <레포> && claude --model \\'claude-opus-5[1m]\\' --effort xhigh --agent-id team-lead --agent-name <ASCII작업명> --team-name <팀명>\\n'`\n\
- 셋(--agent-id/--agent-name/--team-name)은 세트다. 하나라도 빠지면 부팅이 에러난다. 이후 SendMessage 의 `to:` 는 여기 준 --agent-name.\n\
- 모델은 **`claude-opus-5[1m]`** — `opus` alias 는 아직 옛 버전(4.8)을 가리켜 오푸스 5 로 안 뜬다. 가벼운 정찰은 `claude-sonnet-5[1m]`. 대괄호 때문에 따옴표 필수.\n\
- --agent-id 는 `team-lead` 그대로 둬라(학생의 AskUserQuestion 이 그 pane 에서 거노에게 직접 뜬다).\n\
- ⚠️ 스폰 직후 SendMessage 가 'not reachable' 이면 **네 쪽에 트리플이 없어서**다(거노가 연 pane 은 트리플 없이 뜬다). 재시도하지 말고 인박스 파일에 직접 append 해라 — SendMessage 의 실체가 이 파일이라 학생은 똑같이 네이티브로 받는다:\n\
  `~/.claude/teams/<팀명>/inboxes/<agent-name>.json` 에 `{\"from\":\"<네 캐릭터명>\",\"color\":\"cyan\",\"text\":\"<지시>\",\"summary\":\"<요약>\",\"timestamp\":\"<ISO8601 Z>\",\"msgV\":1,\"msg_id\":\"<uuid>\",\"type\":\"message\",\"read\":false}` 를 배열에 추가(디렉토리 없으면 mkdir -p). 폴러가 먹으면 파일이 `[]` 로 비니 그걸로 도착을 확인해라.\n\
- 학생의 보고는 SendMessage 로 받지 못한다(같은 이유) — 브리프에 \"보고·질문·완료 통지는 `kasaterm-cli tell <네 pane id>` 로\" 를 네 pane id 와 함께 명시해라.";

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
