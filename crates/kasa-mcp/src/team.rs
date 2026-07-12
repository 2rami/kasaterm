//! 네이티브 팀 배선 — `~/.claude/teams/<팀>` config·inbox 파일 관리 + 학생 스폰 커맨드 빌더.
//!
//! Claude Code(v2.1.207 실측)는 teammate 플래그(--agent-id/--agent-name/--team-name 트리플)로
//! 부팅된 세션이 `teams/<팀>/inboxes/<슬러그(agent-name)>.json` 을 스스로 폴링해 새 항목을
//! `<teammate-message>` user 턴으로 주입받는다(SendMessage 의 실체 = 이 파일 append). 폴러는
//! **부팅 시점에만 arm** 되므로 팀 config 는 스폰 전에 디스크에 있어야 한다(kasapane 패턴 F-2).
//! 슬러그 규칙·config 스키마는 비공개 내부 실측이라 Claude Code 버전 업 시 재검증 대상.
//!
//! 네이밍 규칙(거노 확정, 2026-07-13):
//! - **agent-name = 목표 작업명**(예: "native-wiring-backend") — 캐릭터명이 아니다. 캐릭터는
//!   kasaterm 이 pane 에 자동 배정하므로 이름 중복이 불필요하고, ASCII 작업명이면 inbox 슬러그
//!   유일성도 자연 해결된다(한글 작업명은 unique_agent_name 이 꼬리표로 방어).
//! - **메시지 from = 발신 세션의 배정 학생 캐릭터명**(team-lead 고정 금지). 단 config 의 리더
//!   멤버 엔트리 명칭은 team-lead 유지 — 하네스에 team-lead 하드코딩 경로가 있다.
//! - **--agent-type 은 학생 스폰에서 생략** — 역할 표시로 뜨지만 그 agent 정의(도구 제한)를
//!   실제 로드하는 부작용이 있다(거노 실측).
//! - 참고: 진짜 팀모드 자식 프로세스엔 env `CLAUDE_CODE_TEAMMATE_MODE=tmux`·
//!   `CLAUDE_CODE_CHILD_SESSION=1` 마커가 있다 — 판별 필요 시 활용(우리는 세팅하지 않는다:
//!   tmux 위장이 아니라 kasaterm pane 직접 스폰).

use serde_json::{json, Value};
use std::io;
use std::path::{Path, PathBuf};

/// `--agent-color` 가 받는 8색(v2.1.207 실측).
pub const AGENT_COLORS: [&str; 8] =
    ["red", "blue", "green", "yellow", "purple", "orange", "pink", "cyan"];

/// 팔레트 밖 색을 8색으로 정규화 — characters.json 에 팔레트 밖 값이 실재한다
/// (프라나=magenta → pink). 모르는 색은 blue: 스폰이 색 때문에 죽지 않게 보수적 폴백.
pub fn normalize_agent_color(c: &str) -> &'static str {
    let c = c.trim().to_ascii_lowercase();
    if let Some(k) = AGENT_COLORS.iter().find(|k| **k == c) {
        return k;
    }
    match c.as_str() {
        "magenta" => "pink",
        _ => "blue",
    }
}

/// pane accent(RGB) → Claude 8색 최근접(색상환 hue 거리). `--agent-color` 는 배지가
/// 아니라 **teammate TUI 전체를 그 색으로 테마**하므로(거노 팀모드 스크린샷 실측),
/// kasaterm 이 pane 테두리에 칠한 학생 accent 와 일치시켜야 한 화면에서 안 어긋난다.
/// 채도가 거의 없는 회색 계열은 hue 가 무의미 — blue 폴백.
pub fn nearest_agent_color(rgb: [u8; 3]) -> &'static str {
    let (r, g, b) = (rgb[0] as f32, rgb[1] as f32, rgb[2] as f32);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if d < 16.0 {
        return "blue";
    }
    let mut h = if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }
    // 앵커 hue — 터미널 관례 색상환 위치. 학생 accent 6종(gold/teal/mint/coral/sky/lilac)
    // 이 전부 의도한 칸에 떨어지는 것을 테스트로 고정한다.
    const ANCHORS: [(f32, &str); 8] = [
        (0.0, "red"),
        (30.0, "orange"),
        (55.0, "yellow"),
        (120.0, "green"),
        (180.0, "cyan"),
        (220.0, "blue"),
        (275.0, "purple"),
        (320.0, "pink"),
    ];
    let dist = |a: f32| {
        let d = (h - a).abs();
        d.min(360.0 - d)
    };
    ANCHORS
        .iter()
        .min_by(|a, b| dist(a.0).partial_cmp(&dist(b.0)).unwrap())
        .map(|(_, n)| *n)
        .unwrap_or("blue")
}

/// Claude Code 바이너리와 동일한 슬러그 규칙(`[^a-zA-Z0-9_-] → "-"`) — inbox 파일명이
/// 이 규칙으로 agent-name 에서 파생되므로, 우리가 만드는 파일명·agent-id 로컬파트도
/// 정확히 같은 규칙이어야 폴러가 읽는다.
pub fn claude_slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect()
}

/// std-only FNV-1a — 슬러그 붕괴(비ASCII → '-')로 잃는 유일성을 짧은 해시 꼬리로 복원.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// agent-name(= 목표 작업명) 확정 — ASCII 안전 이름은 그대로, 비ASCII(한글) 이름엔 해시
/// 꼬리표를 붙인다. 한글은 슬러그가 전부 '-' 로 붕괴해 같은 팀에서 inbox 파일명이 충돌하기
/// 때문("수신생" → `---.json`, 패턴 F-2 함정 ①). 같은 이름 = 같은 꼬리라 정체성은 고정.
pub fn unique_agent_name(name: &str) -> String {
    let ok = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    if !name.is_empty() && name.chars().all(ok) {
        return name.to_string();
    }
    format!("{name}-{:04x}", fnv1a(name) & 0xffff)
}

/// 방 rslug → 팀명(팀 디렉토리명 겸 --team-name). rslug 꼬리(프로젝트명+방)를 남겨 읽히게
/// 하고, 전체 rslug 의 해시 꼬리표가 비ASCII 붕괴·길이 절단의 유일성을 함께 보장한다.
/// 선두 '-' 는 뗀다 — 절대경로 슬러그가 늘 '-' 로 시작해 셸에서 플래그로 오인되는
/// 디렉토리명(`~/.claude/teams/-Users-...`)이 되는 걸 피한다.
pub fn team_name_for(rslug: &str) -> String {
    let slug = claude_slug(rslug);
    // '-' 연쇄(한글 세그먼트 붕괴 흔적)를 하나로 접어 가독 확보. 유일성은 해시가 진다.
    let mut collapsed = String::new();
    let mut prev_dash = false;
    for c in slug.trim_matches('-').chars() {
        if c == '-' {
            if prev_dash {
                continue;
            }
            prev_dash = true;
        } else {
            prev_dash = false;
        }
        collapsed.push(c);
    }
    // claude_slug 출력은 순수 ASCII 라 byte 슬라이스가 안전하다.
    let tail = if collapsed.len() > 40 { &collapsed[collapsed.len() - 40..] } else { &collapsed[..] };
    let tail = tail.trim_matches('-');
    let tail = if tail.is_empty() { "room" } else { tail };
    format!("kt-{tail}-{:04x}", fnv1a(rslug) & 0xffff)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 팀 config 기본 루트(`~/.claude/teams`). 조작 함수는 전부 root 를 인자로 받고
/// (테스트 = tempdir), 앱 호출부가 이 어댑터를 한 번 얹는다.
pub fn teams_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    Path::new(&home).join(".claude").join("teams")
}

fn config_path(root: &Path, team: &str) -> PathBuf {
    root.join(team).join("config.json")
}

/// inbox 파일 경로 — 파일명은 agent-name 의 Claude 슬러그(폴러가 읽는 그 이름).
pub fn inbox_path(root: &Path, team: &str, agent_name: &str) -> PathBuf {
    root.join(team).join("inboxes").join(format!("{}.json", claude_slug(agent_name)))
}

/// inbox 를 '[]' 로 초기화 — 이미 있으면 보존(쌓인 메시지를 지우면 안 된다).
fn init_inbox(root: &Path, team: &str, agent_name: &str) -> io::Result<()> {
    let p = inbox_path(root, team, agent_name);
    if p.exists() {
        return Ok(());
    }
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, "[]")
}

/// config.json 원자 쓰기(tmp → rename) — 폴러가 반쯤 쓴 JSON 을 읽지 않게(write_marker 관례).
fn write_config(root: &Path, team: &str, cfg: &Value) -> io::Result<()> {
    let p = config_path(root, team);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(cfg).map_err(io::Error::other)?)?;
    std::fs::rename(&tmp, &p)
}

/// 팀 config 읽기 — 없거나 파싱 실패면 None.
pub fn read_config(root: &Path, team: &str) -> Option<Value> {
    let s = std::fs::read_to_string(config_path(root, team)).ok()?;
    serde_json::from_str(&s).ok()
}

/// agent-id — Claude 슬러그 로컬파트 + 팀 도메인(패턴 F: `--agent-id <slug>@<팀>`).
/// config 명부의 agentId 와 스폰 플래그가 정확히 같은 문자열이어야 한다.
pub fn agent_id(team: &str, agent_name: &str) -> String {
    format!("{}@{}", claude_slug(agent_name), team)
}

/// 팀 config 보장 — 없으면 god(team-lead) 단독 명부로 생성하고 team-lead inbox 를
/// 초기화한다. 있으면 그대로 둔다(멤버·leadSessionId 를 덮지 않음). 스키마는 TeamCreate
/// 산출물 실측 미러 — 폴러가 어느 필드를 검증하는지 비공개라 관측 스키마를 그대로 따른다.
pub fn ensure_team(
    root: &Path,
    team: &str,
    lead_session_id: Option<&str>,
    cwd: &str,
) -> io::Result<()> {
    if read_config(root, team).is_none() {
        let now = now_ms();
        let sid = lead_session_id
            .map(String::from)
            .unwrap_or_else(crate::character::new_session_id);
        let cfg = json!({
            "name": team,
            "description": "kasaterm 방 팀 (네이티브 배선)",
            "createdAt": now,
            "leadAgentId": format!("team-lead@{team}"),
            "leadSessionId": sid,
            "members": [{
                "agentId": format!("team-lead@{team}"),
                "name": "team-lead",
                "agentType": "team-lead",
                "joinedAt": now,
                "tmuxPaneId": "",
                "cwd": cwd,
                "subscriptions": []
            }]
        });
        write_config(root, team, &cfg)?;
    }
    init_inbox(root, team, "team-lead")
}

/// 명부 리더 세션id 갱신 — god pane 이 재스폰/스왑으로 새 세션id 를 받아 부팅할 때, 부팅
/// 플래그(--session-id)와 config 의 leadSessionId 가 같은 세션을 가리키게 맞춘다.
/// ensure_team 은 기존 config 불변(첫 생성만 sid 기록)이라 이후 갱신은 이 함수가 진다.
/// 같은 값이면 무쓰기(폴러가 읽는 파일이라 불필요한 rewrite 회피).
pub fn set_lead_session(root: &Path, team: &str, sid: &str) -> io::Result<()> {
    let mut cfg = read_config(root, team)
        .ok_or_else(|| io::Error::other(format!("no team config: {team} (ensure_team 먼저)")))?;
    if cfg.get("leadSessionId").and_then(|v| v.as_str()) == Some(sid) {
        return Ok(());
    }
    cfg["leadSessionId"] = json!(sid);
    write_config(root, team, &cfg)
}

/// 학생 멤버 스펙 — add_member 입력.
pub struct StudentSpec<'a> {
    /// --agent-name 에 들어갈 **목표 작업명**(거노: 캐릭터명 아님, 예: "native-wiring-backend").
    /// ASCII 권장 — 한글이면 unique_agent_name 으로 꼬리표를 붙여 넘길 것.
    pub agent_name: &'a str,
    /// 8색 팔레트 문자열(밖의 값은 normalize_agent_color 로 정규화돼 저장).
    pub color: &'a str,
    pub model: Option<&'a str>,
    pub cwd: &'a str,
    /// kasaterm surface id("%3") — 정보성(TeamCreate 명부의 tmuxPaneId 자리).
    pub pane_id: Option<&'a str>,
}

/// 학생 멤버 추가(같은 agentId 면 교체) + inbox 초기화. config 가 없으면 에러 —
/// ensure_team 을 먼저 불러 god 명부가 잡힌 상태를 강제한다(폴러 arm 전제).
pub fn add_member(root: &Path, team: &str, spec: &StudentSpec) -> io::Result<()> {
    let mut cfg = read_config(root, team)
        .ok_or_else(|| io::Error::other(format!("no team config: {team} (ensure_team 먼저)")))?;
    let aid = agent_id(team, spec.agent_name);
    let mut entry = json!({
        "agentId": aid,
        "name": spec.agent_name,
        "color": normalize_agent_color(spec.color),
        "planModeRequired": false,
        "joinedAt": now_ms(),
        "tmuxPaneId": spec.pane_id.unwrap_or(""),
        "cwd": spec.cwd,
        "subscriptions": [],
        "isActive": true
    });
    if let Some(m) = spec.model.filter(|m| !m.is_empty()) {
        entry["model"] = json!(m);
    }
    let members = cfg
        .get_mut("members")
        .and_then(|m| m.as_array_mut())
        .ok_or_else(|| io::Error::other("config 에 members 배열 없음"))?;
    members.retain(|m| m.get("agentId").and_then(|a| a.as_str()) != Some(aid.as_str()));
    members.push(entry);
    write_config(root, team, &cfg)?;
    init_inbox(root, team, spec.agent_name)
}

/// 멤버 제거 + inbox 파일 삭제. team-lead 는 제거 불가(명부 리더 보존).
/// 반환: 명부에서 실제로 빠졌는지.
pub fn remove_member(root: &Path, team: &str, agent_name: &str) -> io::Result<bool> {
    if agent_name == "team-lead" {
        return Err(io::Error::other("team-lead 는 제거 불가"));
    }
    let Some(mut cfg) = read_config(root, team) else { return Ok(false) };
    let aid = agent_id(team, agent_name);
    let Some(members) = cfg.get_mut("members").and_then(|m| m.as_array_mut()) else {
        return Ok(false);
    };
    let before = members.len();
    members.retain(|m| m.get("agentId").and_then(|a| a.as_str()) != Some(aid.as_str()));
    let removed = members.len() != before;
    if removed {
        write_config(root, team, &cfg)?;
    }
    let _ = std::fs::remove_file(inbox_path(root, team, agent_name));
    Ok(removed)
}

/// SystemTime → ISO-8601 UTC("2026-07-13T01:23:45.678Z") — 하네스 메시지 timestamp 형식.
/// chrono 없이 std-only(civil-from-days 역산, Howard Hinnant 알고리즘).
fn iso8601_now() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() as i64;
    let ms = d.subsec_millis();
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{day:02}T{h:02}:{mi:02}:{s:02}.{ms:03}Z")
}

/// inbox 로 보낼 메시지 — `from` 은 **발신 세션의 배정 학생 캐릭터명**(거노: team-lead 고정
/// 금지, 예: "프라나"). 리더 명부 엔트리(team-lead)와 별개 — 메시지 표기만 캐릭터.
pub struct InboxMessage<'a> {
    pub from: &'a str,
    pub text: &'a str,
    pub summary: Option<&'a str>,
    /// 발신 캐릭터 색(8색 팔레트) — 수신측 teammate-message 표시용.
    pub color: Option<&'a str>,
}

/// 상대 inbox 에 메시지 append — SendMessage 도구의 실체와 동일한 파일 조작이라, 수신
/// 세션(폴러 arm 상태)에 네이티브 teammate-message user 턴으로 주입된다. 스키마 불일치
/// 항목은 하네스가 조용히 drop 하므로 필드 구성을 바꾸지 말 것(패턴 F-2 함정 ③).
pub fn append_message(
    root: &Path,
    team: &str,
    to_agent_name: &str,
    msg: &InboxMessage,
) -> io::Result<()> {
    let p = inbox_path(root, team, to_agent_name);
    let mut arr: Vec<Value> = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut entry = json!({
        "from": msg.from,
        "text": msg.text,
        "timestamp": iso8601_now(),
        "msgV": 1,
        "msg_id": crate::character::new_session_id(),
        "type": "message",
        "read": false
    });
    if let Some(s) = msg.summary.filter(|s| !s.is_empty()) {
        entry["summary"] = json!(s);
    }
    if let Some(c) = msg.color.filter(|c| !c.is_empty()) {
        entry["color"] = json!(normalize_agent_color(c));
    }
    arr.push(entry);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string(&arr).map_err(io::Error::other)?)?;
    std::fs::rename(&tmp, &p)
}

/// teammate 스폰 argv(claude 실행파일 제외). agent_name 은 **목표 작업명**(거노 네이밍
/// 규칙). 트리플(agent-id/agent-name/team-name)은 필수 세트("must all be provided
/// together" 실측). `--parent-session-id` 는 금지 — 그 세션에 idle 알림이 새는 부작용
/// (패턴 F-2 함정 ④).
pub fn spawn_args(
    team: &str,
    agent_name: &str,
    color: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> Vec<String> {
    let mut v = vec![
        "--agent-id".to_string(),
        agent_id(team, agent_name),
        "--agent-name".to_string(),
        agent_name.to_string(),
        "--team-name".to_string(),
        team.to_string(),
        "--agent-color".to_string(),
        normalize_agent_color(color).to_string(),
    ];
    if let Some(m) = model.filter(|m| !m.is_empty()) {
        v.push("--model".to_string());
        v.push(m.to_string());
    }
    if let Some(e) = effort.filter(|e| !e.is_empty()) {
        v.push("--effort".to_string());
        v.push(e.to_string());
    }
    v
}

/// POSIX 셸 안전 인용 — 한글·공백 인자를 pane 주입 한 줄로 보낼 때.
fn sh_quote(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "@%_+=:,./-".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// pane 에 그대로 주입 가능한 스폰 한 줄. kasaterm pane 안이면 claude shim 이
/// --settings·--session-id·persona 를 마저 입힌다(패턴 F 실측 조합).
pub fn spawn_command_line(
    team: &str,
    agent_name: &str,
    color: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> String {
    let mut s = String::from("claude");
    for a in spawn_args(team, agent_name, color, model, effort) {
        s.push(' ');
        s.push_str(&sh_quote(&a));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir()
            .join(format!("kasaterm-team-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn slug_matches_binary_rule() {
        assert_eq!(claude_slug("momoi"), "momoi");
        assert_eq!(claude_slug("team-lead_2"), "team-lead_2");
        assert_eq!(claude_slug("수신생"), "---");
        assert_eq!(claude_slug("유즈-yz"), "---yz");
        assert_eq!(claude_slug("a b.c/d"), "a-b-c-d");
    }

    #[test]
    fn unique_agent_name_disambiguates_korean() {
        // ASCII 안전 이름은 그대로 — 꼬리표 없음.
        assert_eq!(unique_agent_name("momoi"), "momoi");
        assert_eq!(unique_agent_name("team-lead"), "team-lead");
        // 같은 길이 한글 이름은 슬러그가 같아지는 게 문제였다 — 꼬리표가 갈라야 한다.
        let a = unique_agent_name("유즈");
        let b = unique_agent_name("아리스");
        assert_ne!(claude_slug(&a), claude_slug(&b));
        // 결정론: 같은 이름 = 같은 agent-name(재스폰 시 정체성 유지).
        assert_eq!(a, unique_agent_name("유즈"));
        assert!(a.starts_with("유즈-"));
    }

    #[test]
    fn team_name_is_slug_safe_and_unique() {
        let t1 = team_name_for("-Users-kasa-Desktop-momewomo-tmuxify__room_room-1");
        let t2 = team_name_for("-Users-kasa-Desktop-momewomo-tmuxify__room_room-2");
        assert_ne!(t1, t2);
        assert!(t1.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        assert!(!t1.starts_with('-'));
        // 비ASCII 만 다른 두 rslug(한글 디렉토리)도 해시 꼬리가 가른다.
        assert_ne!(team_name_for("-Users-kasa-내드라이브"), team_name_for("-Users-kasa-새폴더브"));
        // 결정론.
        assert_eq!(t1, team_name_for("-Users-kasa-Desktop-momewomo-tmuxify__room_room-1"));
    }

    #[test]
    fn ensure_add_remove_roundtrip() {
        let root = tmp_root("roundtrip");
        let team = "kt-test-0001";
        ensure_team(&root, team, Some("lead-sid-1"), "/tmp/proj").unwrap();
        // god 단독 명부 + team-lead inbox '[]'.
        let cfg = read_config(&root, team).unwrap();
        assert_eq!(cfg["leadAgentId"], format!("team-lead@{team}"));
        assert_eq!(cfg["leadSessionId"], "lead-sid-1");
        assert_eq!(cfg["members"].as_array().unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(inbox_path(&root, team, "team-lead")).unwrap(),
            "[]"
        );
        // 재호출은 기존 config 를 덮지 않는다.
        ensure_team(&root, team, Some("other-sid"), "/tmp/other").unwrap();
        assert_eq!(read_config(&root, team).unwrap()["leadSessionId"], "lead-sid-1");
        // god 재스폰/스왑: set_lead_session 이 리더 세션id 만 갱신, 명부는 불변.
        set_lead_session(&root, team, "lead-sid-2").unwrap();
        let cfg = read_config(&root, team).unwrap();
        assert_eq!(cfg["leadSessionId"], "lead-sid-2");
        assert_eq!(cfg["members"].as_array().unwrap().len(), 1);

        let agent = unique_agent_name("모모이");
        let spec = StudentSpec {
            agent_name: &agent,
            color: "red",
            model: Some("claude-fable-5"),
            cwd: "/tmp/proj",
            pane_id: Some("%3"),
        };
        add_member(&root, team, &spec).unwrap();
        // 같은 학생 재추가는 교체 — 명부에 1개만.
        add_member(&root, team, &spec).unwrap();
        let cfg = read_config(&root, team).unwrap();
        let members = cfg["members"].as_array().unwrap();
        assert_eq!(members.len(), 2);
        let m = &members[1];
        assert_eq!(m["agentId"], agent_id(team, &agent));
        assert_eq!(m["color"], "red");
        assert_eq!(m["model"], "claude-fable-5");
        // inbox 는 슬러그 파일명으로 초기화되고, 쌓인 메시지는 재추가에도 보존된다.
        let ib = inbox_path(&root, team, &agent);
        assert!(ib.exists());
        std::fs::write(&ib, "[{\"from\":\"team-lead\"}]").unwrap();
        add_member(&root, team, &spec).unwrap();
        assert_eq!(std::fs::read_to_string(&ib).unwrap(), "[{\"from\":\"team-lead\"}]");

        assert!(remove_member(&root, team, &agent).unwrap());
        assert!(!inbox_path(&root, team, &agent).exists());
        assert_eq!(read_config(&root, team).unwrap()["members"].as_array().unwrap().len(), 1);
        assert!(!remove_member(&root, team, &agent).unwrap());
        assert!(remove_member(&root, team, "team-lead").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_args_triple_and_no_parent_session() {
        let args = spawn_args("kt-x-0001", "모모이-1a2b", "magenta", Some("claude-fable-5"), Some("xhigh"));
        let s = args.join(" ");
        // "모모이" 3글자 + 리터럴 '-' → 슬러그 로컬파트는 대시 4개.
        assert!(s.contains("--agent-id ----1a2b@kt-x-0001"));
        assert!(s.contains("--agent-name 모모이-1a2b"));
        assert!(s.contains("--team-name kt-x-0001"));
        // magenta 는 8색 밖 — pink 로 정규화.
        assert!(s.contains("--agent-color pink"));
        assert!(s.contains("--model claude-fable-5"));
        assert!(s.contains("--effort xhigh"));
        assert!(!s.contains("--parent-session-id"));
        // 모델·effort 미지정이면 플래그 자체가 빠진다(shim 주입과 중복 방지).
        let bare = spawn_args("kt-x-0001", "momoi", "blue", None, None).join(" ");
        assert!(!bare.contains("--model") && !bare.contains("--effort"));
    }

    #[test]
    fn nearest_color_matches_student_accents() {
        // theme.rs character_accent 6종 — pane 테두리와 TUI 테마가 일치해야 하는 실값.
        assert_eq!(nearest_agent_color([0xff, 0xd9, 0x3d]), "yellow"); // 아로나 gold
        assert_eq!(nearest_agent_color([0x4e, 0xcd, 0xc4]), "cyan"); // 프라나 teal
        assert_eq!(nearest_agent_color([0x6b, 0xcf, 0x7f]), "green"); // 미도리 mint
        assert_eq!(nearest_agent_color([0xff, 0x6b, 0x6b]), "red"); // 모모이 coral
        assert_eq!(nearest_agent_color([0x4a, 0x90, 0xe2]), "blue"); // 유즈 sky
        assert_eq!(nearest_agent_color([0xb1, 0x97, 0xfc]), "purple"); // 아리스 lilac
        // 회색(무채도)은 hue 판정 불가 — blue 폴백.
        assert_eq!(nearest_agent_color([0x80, 0x80, 0x80]), "blue");
        // 경계 확인: 주황·핑크 계열.
        assert_eq!(nearest_agent_color([0xff, 0x88, 0x00]), "orange");
        assert_eq!(nearest_agent_color([0xff, 0x69, 0xb4]), "pink"); // hotpink
    }

    #[test]
    fn append_message_uses_sender_character() {
        let root = tmp_root("msg");
        let team = "kt-msg-0001";
        ensure_team(&root, team, None, "/tmp/proj").unwrap();
        let spec = StudentSpec {
            agent_name: "native-wiring-backend",
            color: "red",
            model: None,
            cwd: "/tmp/proj",
            pane_id: None,
        };
        add_member(&root, team, &spec).unwrap();
        // from = 발신 세션의 배정 캐릭터명(거노) — team-lead 고정이 아니다.
        let msg = InboxMessage {
            from: "프라나",
            text: "브리핑이다",
            summary: Some("브리핑"),
            color: Some("magenta"),
        };
        append_message(&root, team, "native-wiring-backend", &msg).unwrap();
        append_message(&root, team, "native-wiring-backend", &msg).unwrap();
        let raw =
            std::fs::read_to_string(inbox_path(&root, team, "native-wiring-backend")).unwrap();
        let arr: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        assert_eq!(arr.len(), 2);
        let e = &arr[0];
        assert_eq!(e["from"], "프라나");
        assert_eq!(e["text"], "브리핑이다");
        assert_eq!(e["summary"], "브리핑");
        assert_eq!(e["color"], "pink"); // magenta → 8색 정규화
        assert_eq!(e["type"], "message");
        assert_eq!(e["msgV"], 1);
        assert_eq!(e["read"], false);
        // timestamp: "YYYY-MM-DDTHH:MM:SS.mmmZ" 형태(하네스 zod 통과 형식).
        let ts = e["timestamp"].as_str().unwrap();
        assert!(ts.len() == 24 && ts.ends_with('Z') && &ts[10..11] == "T", "ts={ts}");
        // msg_id 는 항목마다 달라야 중복 제거에 안 걸린다.
        assert_ne!(arr[0]["msg_id"], arr[1]["msg_id"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn command_line_quotes_korean() {
        let line = spawn_command_line("kt-x-0001", "모모이-1a2b", "red", None, None);
        assert!(line.starts_with("claude --agent-id ----1a2b@kt-x-0001"));
        // 한글 인자는 홑따옴표 인용 — pane 한 줄 주입에서 안 깨진다.
        assert!(line.contains("--agent-name '모모이-1a2b'"));
    }
}
