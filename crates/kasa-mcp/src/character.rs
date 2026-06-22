//! 캐릭터 배정 — characters.json persona + /tmp 마커 + 빈 슬롯 순환.
//!
//! god 자율통솔·MCP `/spawn` 폐기(거노) 후, 학생 정체성을 백엔드(kasaterm)가
//! pane 생성 시점에 직접 박는다. 사용자가 그 pane 에서 `claude` 를 치면 shim 이
//! 여기서 심은 env(KASATERM_CHARACTER/SESSION_ID/PERSONA)를 --session-id·
//! --append-system-prompt 로 적용한다. board(socket.rs)는 같은 /tmp 마커를 읽어
//! `row.character` 를 채우므로, 마커 경로 규칙은 socket.rs 의 rslug 와 일치해야 한다.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// characters.json 후보 경로 — kasaterm-assign-character.py 와 동일 우선순위:
/// ~/.config → env override → .app Resources → 레포 소스.
fn candidate_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
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

/// 학생(워커) 풀 — members[].name 순서. 빈 슬롯 순환 배정에 쓴다.
pub fn member_names(chars: &Value) -> Vec<String> {
    names_of(chars.get("members"))
}

/// god 이름들 — leader + leaders. 배정 풀 제외·is_god 판정에 쓴다.
pub fn god_names(chars: &Value) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(n) = chars.get("leader").and_then(|l| l.get("name")).and_then(|n| n.as_str()) {
        v.push(n.to_string());
    }
    for n in names_of(chars.get("leaders")) {
        if !v.contains(&n) {
            v.push(n);
        }
    }
    v
}

pub fn is_god(chars: &Value, name: &str) -> bool {
    god_names(chars).iter().any(|g| g == name)
}

/// 캐릭터의 persona 텍스트(leader/leaders/members 통합 풀에서 이름 매칭).
pub fn persona_for(chars: &Value, name: &str) -> Option<String> {
    let mut pool: Vec<&Value> = Vec::new();
    if let Some(l) = chars.get("leader") {
        pool.push(l);
    }
    for key in ["leaders", "members"] {
        if let Some(arr) = chars.get(key).and_then(|x| x.as_array()) {
            pool.extend(arr);
        }
    }
    pool.into_iter()
        .find(|m| m.get("name").and_then(|n| n.as_str()) == Some(name))
        .and_then(|m| m.get("persona").and_then(|x| x.as_str()))
        .filter(|p| !p.is_empty())
        // 캐릭터 정체성 뒤에 공통 협업 규약을 붙여 모든 학생·god 에 1회 주입(캐시).
        .map(|p| format!("{p}{COLLAB_PROTOCOL}"))
}

/// 모든 캐릭터 persona 끝에 붙는 협업 규약 — 동료를 기다릴 땐 tell 로 깨우지 말고
/// wake-watch 를 background 로 띄워 자동 재개(거노: task-notification wake 활용).
const COLLAB_PROTOCOL: &str = "\n\n[협업 — 동료 기다리기]\n\
다른 학생(동료 pane)의 작업이 끝나길 기다려야 할 때는, tell 로 깨우거나 board 를 반복해서 확인하지 말고 아래를 background 로 띄워라(Bash 도구의 run_in_background, 또는 명령 끝에 &):\n\
  kasaterm-cli wake-watch <동료 surface_id>\n\
동료의 surface_id 는 `kasaterm-cli board` 로 확인한다. 동료가 한 턴을 끝내면 이 명령이 스스로 종료되고, 시스템이 너를 자동으로 깨운다(task-notification). 깨어나면 그 출력(\"<동료> 작업 끝남\")을 보고 이어서 진행해라. 이렇게 하면 네 입력창을 더럽히지 않고 동료 완료 즉시 이어받는다.";

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
    PathBuf::from("/tmp/kasaterm-collab").join(rslug)
}

/// `/tmp/kasaterm-collab/<rslug>/character-<N>` — board 가 row.character 로 읽는 마커.
pub fn character_marker(rslug: &str, surface_id: &str) -> PathBuf {
    collab_dir(rslug).join(format!("character-{}", surface_id.trim_start_matches('%')))
}

fn lead_marker(rslug: &str) -> PathBuf {
    collab_dir(rslug).join("lead")
}

/// 이 방에서 이미 배정된 캐릭터 이름들(character-* 마커 내용).
pub fn assigned(rslug: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(collab_dir(rslug)) {
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

/// 한 pane(surface)의 character-<N> 마커 내용. 없거나 비면 None.
/// resume 복원처럼 ws.pane_character 엔 없지만 마커엔 있는 캐릭터를 중복 배정에서
/// 피하려 쓴다(assign_character_env).
pub fn read_marker(rslug: &str, surface_id: &str) -> Option<String> {
    std::fs::read_to_string(character_marker(rslug, surface_id))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 빈 슬롯 순환 — members 중 아직 안 쓰인 첫째. 다 차면 배정 수 기준으로 순환.
pub fn assign_next(rslug: &str, members: &[String]) -> Option<String> {
    if members.is_empty() {
        return None;
    }
    let used = assigned(rslug);
    for m in members {
        if !used.contains(m) {
            return Some(m.clone());
        }
    }
    Some(members[used.len() % members.len()].clone())
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

/// lead 마커 — 이 pane 이 god(is_god) 임을 표시.
pub fn write_lead(rslug: &str, surface_id: &str) -> std::io::Result<()> {
    let path = lead_marker(rslug);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, surface_id)
}

/// 새 `claude --session-id` 용 uuid. transcript jsonl 파일명이 되므로 파일명 안전 문자만.
/// uuidgen(macOS/Linux 공통) → 실패 시 시간+pid 폴백.
pub fn new_session_id() -> String {
    if let Ok(out) = std::process::Command::new("uuidgen").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_lowercase();
            if !s.is_empty() {
                return s;
            }
        }
    }
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("kt-{:x}-{}", t, std::process::id())
}
