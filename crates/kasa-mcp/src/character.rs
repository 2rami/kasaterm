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

/// settings.json 의 문자열 설정 하나. 앱의 `socket::read_settings` 와 **같은 파일**을
/// 봐야 한다 — kasa-mcp → app 은 없는 의존 방향이라 부를 수 없어 경로 규칙만 옮겨
/// 왔다. `KASATERM_SETTINGS_FILE` 을 빠뜨렸더니 설정 화면은 이 파일을, 로더는 저
/// 파일을 읽어 「테마를 바꿨는데 아무 일도 안 일어나는」 상태가 됐다(2026-08-13 실측).
fn read_setting_str(key: &str) -> Option<String> {
    let p = match std::env::var("KASATERM_SETTINGS_FILE") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => home()?.join(".config/kasaterm/settings.json"),
    };
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
    v.get(key)?.as_str().map(String::from)
}

/// 테마 팩 루트 — `~/.config/kasaterm/themes/`. **폴더 하나가 테마 하나**다:
/// `theme.json`(로스터 + 팔레트) + `sprites/`(캐릭터 그림). 지금까지 흩어져 있던
/// 세 override(`students/`·`characters.json`·`custom_theme`)를 한 단위로 묶은 것이라,
/// 폴더째 주고받으면 그게 곧 테마 배포다.
pub fn themes_root() -> Option<PathBuf> {
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

/// 지금 고른 테마 id — 빈 문자열이면 번들. **`active_theme_dir` 과 같은 손잡이를
/// 본다**(env 우선, 그다음 설정). 화면이 「고른 것」을 표시할 때 이걸 써야 실제로
/// 도는 테마와 어긋나지 않는다.
pub fn active_theme_id() -> String {
    std::env::var("KASATERM_CHARACTER_THEME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| read_setting_str("character_theme"))
        .unwrap_or_default()
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

/// 활성 테마 항목을 뺀 기본 로스터 — 테마를 안 골랐을 때와 같은 것. 진행 중
/// pane 을 기본(번들) 학생으로 바꾸는 피커의 「기본」 묶음이 쓴다(2026-08-24
/// 지시: 어느 테마가 활성이어도 다른 테마 캐릭터로 바꿀 수 있어야 한다).
pub fn base_characters_json() -> Option<Value> {
    let skip = active_theme_dir().map(|d| d.join("theme.json"));
    for p in candidate_paths() {
        if skip.as_deref() == Some(p.as_path()) {
            continue;
        }
        let Ok(s) = std::fs::read_to_string(&p) else { continue };
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            return Some(v);
        }
    }
    None
}

/// 설치 테마 하나의 로스터(theme.json). id 는 경로 조각이 되므로 구분자를 거부한다.
pub fn theme_characters_json(id: &str) -> Option<Value> {
    if id.is_empty() || id.contains('/') || id.contains('\\') || id.contains("..") {
        return None;
    }
    let s = std::fs::read_to_string(themes_root()?.join(id).join("theme.json")).ok()?;
    serde_json::from_str(&s).ok()
}

/// persona 합집합 조회 — 활성 → 기본(번들) → 설치 테마 전부. 진행 중 pane 이
/// 다른 테마 캐릭터로 재배정된 뒤 재시작·resume 할 때, 활성 로스터에 없는
/// 이름이라도 원 소속 테마의 말투를 찾아 입힌다.
pub fn persona_for_any(name: &str) -> Option<String> {
    for chars in [characters_json(), base_characters_json()].into_iter().flatten() {
        if let Some(p) = persona_for(&chars, name) {
            return Some(p);
        }
    }
    let root = themes_root()?;
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let Ok(s) = std::fs::read_to_string(e.path().join("theme.json")) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&s) else { continue };
        if let Some(p) = persona_for(&v, name) {
            return Some(p);
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
        .map(|p| format!("{p}{}", collab_protocol()))
}

/// 이 학생이 뜰 때 붙일 `--model` 값. `claude-opus-5[1m]` 처럼 CLI 가 그대로
/// 받는 자유 문자열이라 이 자리에서 후보를 좁히지 않는다 — 2026-08-24 지시로
/// 커스텀 모델을 로스터 파일에 직접 적을 수 있어야 해서다. 비었거나 없으면
/// 설정창의 전역 모델로 떨어진다.
pub fn model_for(chars: &Value, name: &str) -> Option<String> {
    find_character(chars, name)
        .and_then(|m| m.get("model").and_then(|x| x.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// 이 학생을 띄울 실행 통로 — `kimi`·`glm` 처럼 claude 를 감싸 게이트웨이로
/// 보내는 런처 이름이다.
///
/// `model` 과 축이 다르므로 한 필드에 못 섞는다: 게이트웨이 모델은 `--model`
/// 로는 못 닿고 래퍼가 환경(프록시 주소·키)을 씌워야만 붙는다. 반대로 래퍼를
/// 쓰면서 그 안에서 다시 모델을 고를 수도 있어, 둘은 곱해지는 관계다.
pub fn backend_for(chars: &Value, name: &str) -> Option<String> {
    find_character(chars, name)
        .and_then(|m| m.get("backend").and_then(|x| x.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        // 실행 파일 이름이 되어 셸에 그대로 넘어가는 값이다 — 경로 구분자나
        // 공백이 섞이면 임의 명령이 되므로 한 낱말만 통과시킨다.
        .filter(|s| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .map(String::from)
}

/// 설정창 드롭다운이 늘어놓을 모델 후보 하나.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    /// 화면에 뜨는 이름.
    pub label: String,
    /// `--model` 값. 비면 안 붙인다.
    pub model: String,
    /// 실행 래퍼 이름. 비면 순정 claude.
    pub backend: String,
}

/// 내장 후보 — 로스터 파일에 `models` 가 없을 때의 기본값.
fn builtin_model_choices() -> Vec<ModelChoice> {
    let m = |label: &str, model: &str, backend: &str| ModelChoice {
        label: label.to_string(),
        model: model.to_string(),
        backend: backend.to_string(),
    };
    vec![
        m("기본", "", ""),
        m("Opus 5 (1M)", "claude-opus-5[1m]", ""),
        m("Sonnet 5 (1M)", "claude-sonnet-5[1m]", ""),
        m("Haiku", "haiku", ""),
        m("Kimi", "", "kimi"),
        m("GLM", "", "glm"),
    ]
}

/// 드롭다운 후보 목록. 로스터 파일 최상위 `models` 배열을 읽으므로 원본을 고쳐
/// 커스텀 모델을 늘릴 수 있다(2026-08-24 지시: 커스텀 모델은 json/yaml 로 설정
/// 가능하게). 배열이 없거나 쓸 만한 항목이 하나도 없으면 내장 목록으로 간다.
///
/// 첫 칸은 늘 "안 고름"이어야 한다 — 목록이 사용자 것으로 통째로 바뀌어도 전역
/// 기본으로 되돌릴 길이 사라지면 안 되므로, 빈 항목이 없으면 앞에 끼워 넣는다.
pub fn model_choices(chars: &Value) -> Vec<ModelChoice> {
    let mut out: Vec<ModelChoice> = Vec::new();
    if let Some(arr) = chars.get("models").and_then(|x| x.as_array()) {
        for it in arr {
            let get = |k: &str| {
                it.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string()
            };
            let (model, backend) = (get("model"), get("backend"));
            // label 이 없으면 값으로 대신 부른다 — 이름 없는 칸은 고를 수가 없다.
            let label = match get("label") {
                l if !l.is_empty() => l,
                _ if !model.is_empty() => model.clone(),
                _ if !backend.is_empty() => backend.clone(),
                _ => "기본".to_string(),
            };
            out.push(ModelChoice { label, model, backend });
        }
    }
    if out.is_empty() {
        return builtin_model_choices();
    }
    if !out.iter().any(|c| c.model.is_empty() && c.backend.is_empty()) {
        out.insert(
            0,
            ModelChoice { label: "기본".into(), model: String::new(), backend: String::new() },
        );
    }
    out
}

/// 협업 규약 파일이 놓일 자리 — 읽기는 `characters.json` 과 **같은 우선순위**다
/// (테마 → 사용자 override → 번들 → 개발 트리). 로스터와 규약을 한 벌로 갈아끼울
/// 수 있어야 테마가 자기 규칙을 들고 올 수 있다.
fn protocol_candidate_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = active_theme_dir() {
        v.push(d.join("collab-protocol.md"));
    }
    if let Some(home) = home() {
        v.push(home.join(".config/kasaterm/collab-protocol.md"));
    }
    if let Ok(p) = std::env::var("KASATERM_COLLAB_HOOKS_DIR") {
        v.push(PathBuf::from(p).join("collab-protocol.md"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(res) = exe
            .parent()
            .and_then(|m| m.parent())
            .map(|c| c.join("Resources/collab-hooks/collab-protocol.md"))
        {
            v.push(res);
        }
        if let Some(adj) = exe.parent().map(|d| d.join("collab-hooks/collab-protocol.md")) {
            v.push(adj);
        }
    }
    v.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../app/kasaterm/collab-hooks/collab-protocol.md"),
    );
    v
}

/// 설정 화면이 협업 규약을 저장할 자리 — `protocol_candidate_paths` 의 최우선
/// 슬롯과 **같아야 한다**. 어긋나면 저장은 성공하는데 테마 쪽이 읽기에서 이겨,
/// 고친 규약이 학생에게 영영 안 실린다(오류도 안 난다). `user_characters_path`
/// 가 같은 이유로 같은 모양이다.
pub fn user_collab_protocol_path() -> Option<PathBuf> {
    if let Some(d) = active_theme_dir() {
        return Some(d.join("collab-protocol.md"));
    }
    Some(home()?.join(".config/kasaterm/collab-protocol.md"))
}

/// 모든 학생 persona 뒤에 붙는 협업 규약. 파일이 있으면 그것, 없으면 코드 기본값.
///
/// 파일로 뺀 이유: 규약이 Rust 상수로만 있으면 **배포본을 받은 사람은 손댈 방법이
/// 아예 없다** — 소스를 받아 다시 굽는 것 말고는(2026-08-13 지적: "카사텀
/// 쓰는사람들은 못바꾸잖아"). 캐릭터 성격은 이미 characters.json 이라 편집
/// 가능했는데 공통 규약만 코드에 남아 있었다.
///
/// 기본값을 코드에 남겨 두는 것은 파일이 없어도 앱이 온전히 돌게 하기 위해서다 —
/// 규약이 빈 채로 학생이 뜨면 서로를 못 부르고 보고도 안 올라온다.
///
/// 캐시하지 않는다. 이 함수는 pane 이 뜰 때만 불리므로(persona 는 spawn 시 env 로
/// 박힌다) 비용이 무시할 만하고, 캐시하면 파일을 고쳐도 앱을 껐다 켜기 전엔 안
/// 먹어 「고쳤는데 그대로다」가 된다. 지금 방식은 **다음에 뜨는 pane 부터** 적용이라
/// 예측이 쉽다.
pub fn collab_protocol() -> String {
    for p in protocol_candidate_paths() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if !s.trim().is_empty() {
                // 캐릭터 정체성과 규약 사이를 늘 빈 줄로 벌린다 — 파일 첫 줄이
                // 바로 대괄호 섹션이면 앞 문장에 이어붙어 한 문단이 된다.
                return format!("\n\n{}", s.trim_start_matches('\n'));
            }
        }
    }
    DEFAULT_COLLAB_PROTOCOL.to_string()
}

/// 캐릭터의 claude_color(characters.json) — teammate 스폰 `--agent-color` 용. 팔레트 밖
/// 값(프라나=magenta)이 실재하므로 8색 정규화는 team::normalize_agent_color 가 맡는다.
pub fn claude_color_for(chars: &Value, name: &str) -> Option<String> {
    find_character(chars, name)
        .and_then(|m| m.get("claude_color").and_then(|x| x.as_str()))
        .filter(|c| !c.is_empty())
        .map(String::from)
}

/// 캐릭터의 header_color(hex) — 웹텀 pane 목록이 학생 이름을 학생색으로 칠하는
/// 용. GUI 의 `theme::character_accent` 가 읽는 것과 같은 필드다.
pub fn header_color_for(chars: &Value, name: &str) -> Option<String> {
    find_character(chars, name)
        .and_then(|m| m.get("header_color").and_then(|x| x.as_str()))
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

/// 학생 정의 한 명을 사람이 고칠 수 있는 YAML 로 편다.
///
/// **범용 YAML 직렬화가 아니다.** 우리가 내는 형태 — 값이 스칼라뿐인 평평한
/// map — 만 다루고, 중첩 map 이나 배열을 만나면 그 값은 JSON 표기로 그대로
/// 적는다(왕복은 여전히 성립한다). 범용 파서를 새 의존성으로 들이는 대신 좁게
/// 가는 쪽을 골랐다: 여기서 다루는 값의 형태를 우리가 통제하고, 못 읽을 때
/// 조용히 뭉개는 대신 저장을 거부하면 정의가 깨질 길이 없다.
pub fn member_to_yaml(v: &Value) -> String {
    let Some(map) = v.as_object() else { return String::new() };
    let mut out = String::new();
    for (k, val) in map {
        match val {
            Value::String(s) if s.contains('\n') => {
                // 여러 줄은 블록 스칼라로. `|-` 로 끝 개행을 지우지 않는 이유는
                // 원문에 있던 마지막 개행까지 그대로 살리기 위해서다 — `|` 는
                // 끝 개행 하나를 남기므로, 원문이 개행으로 안 끝나면 그만큼
                // 어긋난다. 그래서 원문 기준으로 골라 준다.
                let chomp = if s.ends_with('\n') { "|" } else { "|-" };
                out.push_str(&format!("{k}: {chomp}\n"));
                for line in s.trim_end_matches('\n').split('\n') {
                    out.push_str(&format!("  {line}\n"));
                }
            }
            Value::String(s) => {
                out.push_str(&format!("{k}: {}\n", yaml_scalar(s)));
            }
            other => {
                // 스칼라가 아니거나 문자열이 아닌 값 — JSON 표기가 곧 YAML 의
                // 흐름 표기라 그대로 통한다.
                out.push_str(&format!("{k}: {other}\n"));
            }
        }
    }
    out
}

/// 한 줄 문자열을 YAML 스칼라로. 따옴표 없이 두면 다른 타입으로 읽히거나
/// 문법이 깨지는 값만 감싼다.
fn yaml_scalar(s: &str) -> String {
    let plain_ok = !s.is_empty()
        && s.trim() == s
        && !s.contains(['#', ':', '"', '\'', '\\', '\t'])
        && !s.starts_with(['-', '?', '&', '*', '!', '|', '>', '%', '@', '`', '[', '{'])
        // 따옴표 없이 두면 불리언·널·숫자로 읽혀 문자열이 아니게 되는 값들.
        && !matches!(s, "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~")
        && s.parse::<f64>().is_err();
    if plain_ok {
        s.to_string()
    } else {
        Value::String(s.to_string()).to_string()
    }
}

/// `member_to_yaml` 이 낸 것을 되돌린다. 우리가 내는 문법만 받는다 — 앵커·흐름
/// map·다중 문서 같은 건 오류로 돌려보내 저장을 막는다.
pub fn member_from_yaml(src: &str) -> Result<Value, String> {
    let mut map = serde_json::Map::new();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            i += 1;
            continue;
        }
        if line.starts_with([' ', '\t']) {
            return Err(format!("{}번째 줄이 들여쓰기로 시작해요 — 키가 없어요", i + 1));
        }
        let Some((k, rest)) = line.split_once(':') else {
            return Err(format!("{}번째 줄에 `키: 값` 의 콜론이 없어요", i + 1));
        };
        let key = k.trim().to_string();
        if key.is_empty() {
            return Err(format!("{}번째 줄의 키가 비었어요", i + 1));
        }
        let rest = rest.trim();
        if rest == "|" || rest == "|-" {
            // 블록 스칼라 — 들여쓴 줄을 전부 모은다. 빈 줄은 본문의 일부이므로
            // 들여쓰기가 없어도 이어 간다(끊으면 문단이 통째로 잘린다).
            let mut body: Vec<String> = Vec::new();
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                if l.trim().is_empty() {
                    body.push(String::new());
                    i += 1;
                    continue;
                }
                let Some(stripped) = l.strip_prefix("  ") else { break };
                body.push(stripped.to_string());
                i += 1;
            }
            while body.last().is_some_and(String::is_empty) {
                body.pop();
            }
            let mut text = body.join("\n");
            if rest == "|" {
                text.push('\n');
            }
            map.insert(key, Value::String(text));
            continue;
        }
        // 한 줄 값. JSON 으로 읽히면 그 타입(따옴표 친 문자열·숫자·불리언),
        // 아니면 평문 문자열이다.
        let val = serde_json::from_str::<Value>(rest).unwrap_or_else(|_| {
            if rest.is_empty() { Value::String(String::new()) } else { Value::String(rest.to_string()) }
        });
        map.insert(key, val);
        i += 1;
    }
    if map.is_empty() {
        return Err("내용이 비었어요".to_string());
    }
    Ok(Value::Object(map))
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
///
/// ⚠️ **같은 이름이 여러 군데 적혀 있으면 전부 고친다** — 첫 매치에서 멈추면 안
/// 된다. `leader` 는 `leaders[0]` 을 한 번 더 적어 둔 하위호환 필드라(로스터
/// 빌드가 **이름으로** 접는다) 리더는 늘 두 번 적혀 있는데, 한쪽만 고치면
/// 이름을 바꿨을 때 두 이름이 갈려 접히지 않고 **로스터에 유령이 하나 늘어난다**
/// (79→80명, 실측). UI 에는 캐릭터를 지우는 창구가 없으니 밟으면 파일을 손으로
/// 고쳐야 한다. persona 도 같은 이유로 전부 고쳐야 옳다 — 안 그러면 안 고쳐진
/// 쪽에 옛 성격이 그림자로 남는다.
///
/// 이름이 로스터의 키라 같은 이름은 곧 같은 사람이다(중복 이름은 저장 단계에서
/// 막힌다) — 그래서 "전부"가 "엉뚱한 사람까지"가 되지 않는다.
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
        if let Some(arr) = root.get_mut(arr_key).and_then(|x| x.as_array_mut()) {
            for m in arr.iter_mut() {
                if m.get("name").and_then(|n| n.as_str()) == Some(name) {
                    m[key] = value.clone();
                    applied = true;
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

/// 로스터에서 `name` 캐릭터의 **정의 전체**를 새 것으로 갈아 끼운다(원본 편집 저장).
///
/// `update_member` 와 갈라 두는 이유는 키 삭제 때문이다 — 원본에서 한 줄을 지우면
/// 그 필드가 없어져야 하는데, 키 하나씩 덮는 경로로는 지운 것이 그대로 남는다.
///
/// ⚠️ `update_member` 와 같은 함정을 공유한다: **같은 이름이 여러 군데 적혀 있으면
/// 전부 갈아야 한다.** `leader` 는 `leaders[0]` 을 한 번 더 적어 둔 하위호환 필드라
/// 리더는 늘 두 번 적혀 있는데, 한쪽만 갈고 이름까지 바꾸면 두 이름이 갈려 로스터에
/// 유령이 하나 는다(로스터 빌드가 **이름으로** 접기 때문).
pub fn replace_member(name: &str, def: &Value) -> std::io::Result<()> {
    if !def.is_object() {
        return Err(std::io::Error::other("정의가 map 이 아님"));
    }
    let path = user_characters_path().ok_or_else(|| std::io::Error::other("no HOME"))?;
    let mut root = if path.exists() {
        std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok())
    } else {
        characters_json()
    }
    .unwrap_or_else(|| Value::Object(Default::default()));

    let mut applied = false;
    if let Some(l) = root.get_mut("leader") {
        if l.get("name").and_then(|n| n.as_str()) == Some(name) {
            *l = def.clone();
            applied = true;
        }
    }
    for arr_key in ["leaders", "members"] {
        if let Some(arr) = root.get_mut(arr_key).and_then(|x| x.as_array_mut()) {
            for m in arr.iter_mut() {
                if m.get("name").and_then(|n| n.as_str()) == Some(name) {
                    *m = def.clone();
                    applied = true;
                }
            }
        }
    }
    if !applied {
        return Err(std::io::Error::other(format!("로스터에 '{name}' 이 없음")));
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&root).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)
}

/// 로스터에서 한 명의 정의를 그대로 떠 온다(원본 뷰가 보여 줄 것).
pub fn member_def(chars: &Value, name: &str) -> Option<Value> {
    find_character(chars, name).cloned()
}

/// 협업 규약의 **정본은 코드가 아니라 `collab-hooks/collab-protocol.md`** 이고,
/// 여기서는 그것을 컴파일 타임에 박아 둘 뿐이다(`collab_protocol()` 의 최종 fallback).
///
/// 이 방향인 이유: 규약이 Rust 문자열 리터럴이면 편집이 사실상 불가능하다 —
/// 줄마다 `\n\` 이스케이프가 붙고, 고치면 다시 구워야 하고, **배포본을 받은
/// 사람은 손댈 창구가 아예 없다**(2026-08-13 지적 "카사텀 쓰는사람들은 못바꾸잖아").
/// 반대로 파일만 두고 상수를 없애면 파일이 유실됐을 때 규약이 통째로 빠져 학생이
/// 서로를 못 부른다. 그래서 **파일이 정본, 상수는 그 파일의 컴파일 타임 사본**이다 —
/// `include_str!` 이라 둘이 어긋날 수가 없다(손으로 베낀 기본값이었다면 한쪽만
/// 고쳐지는 사고가 반드시 난다).
///
/// 내용 자체의 배경: 동료를 기다리는 기본은 **그냥 기다리는 것**이다. 학생 보고
/// (SendMessage)가 알아서 도착하므로 완료 감시는 중복이고, board-watch 는 모든
/// pane 을 보므로 `idle` 을 넣으면 남의 턴 종료마다 깨운다(거노 2026-08-10:
/// "어차피 끝나면 보고하는데 필요없지 않나"). 그래서 Monitor 는 **보고가 올 수
/// 없는 상태**(승인 막힘·죽음·경로 끊김)에만 남겼다.
const DEFAULT_COLLAB_PROTOCOL: &str = concat!(
    "\n\n",
    include_str!("../../../app/kasaterm/collab-hooks/collab-protocol.md")
);

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

#[cfg(test)]
mod collab_protocol_tests {
    use super::*;

    /// 규약이 빈 채로 나가면 학생이 서로를 못 부르고 보고도 안 올라온다 — 그런데
    /// 규약은 화면에 안 보이므로 비어도 알아챌 방법이 없다. 파일이 지워지거나
    /// 경로가 어긋나면 여기서 먼저 걸린다.
    #[test]
    fn default_protocol_is_not_empty_and_starts_with_a_blank_line() {
        assert!(
            DEFAULT_COLLAB_PROTOCOL.len() > 1000,
            "규약 정본 파일이 비었거나 경로가 어긋났다"
        );
        // 캐릭터 정체성 문장에 규약이 이어붙으면 한 문단이 된다.
        assert!(DEFAULT_COLLAB_PROTOCOL.starts_with("\n\n["));
    }

    /// 파일에서 읽은 규약도 코드 기본값과 **같은 모양**이어야 한다 — 앞의 빈 줄
    /// 두 칸까지. 한쪽만 벌어지면 사용자가 파일을 놓는 순간 규약이 앞 문장에
    /// 달라붙는데, 그건 학생 프롬프트 안에서만 일어나 눈에 안 띈다.
    #[test]
    fn file_backed_protocol_keeps_the_same_leading_gap() {
        let dir = std::env::temp_dir().join(format!("kasa-proto-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("collab-protocol.md"), "[협업 — 시험]\n본문\n").unwrap();
        // SAFETY: 이 테스트만 이 변수를 읽는다(같은 프로세스의 다른 테스트는 규약
        // 파일 경로를 안 본다).
        unsafe { std::env::set_var("KASATERM_COLLAB_HOOKS_DIR", &dir) };
        let got = collab_protocol();
        unsafe { std::env::remove_var("KASATERM_COLLAB_HOOKS_DIR") };
        let _ = std::fs::remove_dir_all(&dir);

        // 테마·홈 override 가 실재하면 그쪽이 먼저 이긴다 — 그때는 이 단언을 건너뛴다.
        if got.contains("[협업 — 시험]") {
            assert!(got.starts_with("\n\n["), "앞 빈 줄 두 칸이 유지돼야 한다");
        }
    }

    /// 저장 자리가 읽기 최우선 슬롯과 어긋나면, 고친 규약이 조용히 안 실린다
    /// (`user_characters_path` 가 같은 이유로 같은 모양이다).
    #[test]
    fn save_path_matches_the_first_read_candidate() {
        let Some(save) = user_collab_protocol_path() else {
            return; // HOME 없음 — CI 컨테이너
        };
        assert_eq!(Some(&save), protocol_candidate_paths().first());
    }

    /// 원본 뷰의 왕복 — 여기가 깨지면 사용자가 YAML 로 한 번 보기만 해도 학생
    /// 정의가 조용히 변형된다. 실제 로스터에 있는 값의 모양을 그대로 담았다.
    #[test]
    fn yaml_roundtrip_keeps_every_value() {
        let src = serde_json::json!({
            "name": "미도리",
            "slug": "midori",
            "school": "밀레니엄",
            // `#` 로 시작 — 따옴표를 안 씌우면 YAML 주석이 되어 값이 통째로 사라진다.
            "header_color": "#6BCF7F",
            "model": "claude-opus-5[1m]",
            // 빈 문자열 — 평문으로 두면 null 로 읽힌다.
            "backend": "",
            // 여러 줄 — 블록 스칼라로 나가야 한다.
            "persona": "너는 미도리.\n차분한 존댓말로 말한다.\n\n보고는 결과 위주.",
        });
        let y = member_to_yaml(&src);
        assert!(y.contains("persona: |-"), "여러 줄은 블록 스칼라로: {y}");
        assert!(y.contains("header_color: \"#6BCF7F\""), "# 값은 따옴표로: {y}");
        let back = member_from_yaml(&y).expect("되읽기");
        assert_eq!(src, back, "왕복에서 값이 바뀌었다\n--- yaml ---\n{y}");
    }

    /// 문자열로 남아야 할 값이 다른 타입으로 읽히지 않는지. `"true"` 가 불리언이
    /// 되면 로스터를 쓰는 쪽이 문자열을 기대하다 조용히 빈 값을 본다.
    #[test]
    fn yaml_keeps_stringy_looking_values_as_strings() {
        for v in ["true", "false", "null", "no", "123", "1.5", "~", "- 하이픈"] {
            let src = serde_json::json!({ "k": v });
            let back = member_from_yaml(&member_to_yaml(&src)).expect(v);
            assert_eq!(back["k"], serde_json::json!(v), "{v} 가 문자열로 안 돌아왔다");
        }
    }

    /// 문법이 틀리면 **저장을 거부**해야 한다 — 조용히 일부만 읽어 저장하면
    /// 지워지지 않았어야 할 필드가 사라진다.
    #[test]
    fn yaml_rejects_broken_input() {
        assert!(member_from_yaml("콜론이 없는 줄").is_err());
        assert!(member_from_yaml("  들여쓰기로 시작").is_err());
        assert!(member_from_yaml("").is_err());
    }
}
