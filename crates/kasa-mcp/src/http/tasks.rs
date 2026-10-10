//! claude 팀 작업 목록(TaskCreate) 읽기 — 칸별 할 일 스냅샷과 `/pane-tasks`.

use super::*;
#[cfg(test)]
use super::test_support::temp_dir;

/// task 디렉토리에서 `[(id, subject, status, owner)]` 파싱. id(숫자) 오름차순. 비-json 제외.
///
/// `owner` 를 같이 싣는 이유: 같은 방 pane 들이 **한 목록을 공유하는 건 설계**라, 주인이
/// 없으면 화면에서 「내 것」과 「방 전체」를 가를 근거가 아무것도 없다(사용자 2026-08-06).
/// 비어 있는 owner 는 주인 없는 방 공용 태스크다 — 그것도 정보다.
fn read_tasks_in_dir(dir: &std::path::Path) -> Vec<(String, String, String, String)> {
    let mut tasks: Vec<(u64, String, String, String, String)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else { continue };
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let subject = v.get("subject").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let status = v.get("status").and_then(|x| x.as_str()).unwrap_or("pending").to_string();
            if subject.is_empty() {
                continue;
            }
            let owner = v.get("owner").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let ord = id.parse::<u64>().unwrap_or(u64::MAX);
            tasks.push((ord, id, subject, status, owner));
        }
    }
    tasks.sort_by_key(|t| t.0);
    tasks.into_iter().map(|(_, id, s, st, o)| (id, s, st, o)).collect()
}

/// session_id → task. 신형 `session-<8hex>` 우선·구형 full-uuid 폴백(solo claude 용).
fn read_claude_tasks(session_id: &str) -> Vec<(String, String, String, String)> {
    if session_id.is_empty() {
        return Vec::new();
    }
    let Some(home) = kasa_socket::home_dir() else {
        return Vec::new();
    };
    let base = home.join(".claude/tasks");
    let prefix: String = session_id.chars().take(8).collect();
    // shim 이 CLAUDE_CODE_TASK_LIST_ID=<full session> 를 주입하면 store dir 이 full-uuid 또는
    // session-<full> 형태일 수 있다. 신형 session-<8hex> → full-uuid → session-<full> 순.
    let candidates = [
        base.join(format!("session-{prefix}")),
        base.join(session_id),
        base.join(format!("session-{session_id}")),
    ];
    match candidates.iter().find(|p| p.is_dir()) {
        Some(dir) => read_tasks_in_dir(dir),
        None => Vec::new(),
    }
}

/// pane 의 **팀 이름** → task 디렉토리(`~/.claude/tasks/<team>/`). 정본 경로다.
///
/// 팀 이름은 board 의 `team`(= shim 이 pane 에 export 한 `KASATERM_TEAM`)이라 pane 마다
/// 정확하고, cwd·mtime 추측이 필요 없다. 같은 방 pane 들이 **같은 목록을 공유하는 건 설계**다
/// (그래서 여러 pane 을 한 번에 물을 때만 호출부가 dedup 한다).
///
/// 이게 없던 동안 태스크가 **모든 pane 에서 0개**로 떴다(사용자: 아루 태스크가 이상하다).
/// 옛 경로 둘이 다 빗나가서다 — store 는 `tasks/<team>/` 인데 세션 경로는 `tasks/session-<8hex>/`
/// 를 찾았고, cwd 폴백은 `teams/<team>/config.json` 의 `members[].cwd` 를 읽는데 그 파일이
/// 이제 안 생긴다(팀 디렉토리엔 `inboxes/` 뿐, 실측 2026-08-05).
fn team_task_dir_by_name(team: &str) -> Option<std::path::PathBuf> {
    let home = kasa_socket::home_dir()?;
    team_task_dir_in(&home.join(".claude/tasks"), team)
}

/// `team_task_dir_by_name` 의 순수 부분 — `$HOME` 없이 테스트할 수 있게 갈라 뒀다.
/// 팀 이름은 그대로 경로 조각이 되므로 구분자·상위참조를 막는다(외부에서 온 문자열).
fn team_task_dir_in(base: &std::path::Path, team: &str) -> Option<std::path::PathBuf> {
    if team.is_empty() || team.contains(['/', '\\']) || team.contains("..") {
        return None;
    }
    let dir = base.join(team);
    dir.is_dir().then_some(dir)
}

/// pane cwd → 그 cwd 의 **팀(TeamCreate) 세션** task 디렉토리. claude 가 팀 컨텍스트에서
/// TaskCreate 하면 task store 가 개별 대화 세션이 아니라 **팀 세션 id**(`~/.claude/teams/
/// session-<id>` = `~/.claude/tasks/session-<id>`)로 keying 된다(실측: 아로나 task=팀
/// session-4c79638c, 그 팀 cwd=/Users/kasa/Desktop). statusline session 으로 못 잡힐 때
/// (팀 lead≠메인 대화·statusline 미보고) cwd 로 팀을 찾는 폴백. 같은 cwd 팀 여럿이면 mtime 최신.
fn team_task_dir_for_cwd(cwd: &str) -> Option<std::path::PathBuf> {
    if cwd.is_empty() {
        return None;
    }
    let home = kasa_socket::home_dir()?;
    let teams = home.join(".claude/teams");
    let tasks_base = home.join(".claude/tasks");
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(&teams).ok()?.flatten() {
        let Ok(content) = std::fs::read_to_string(entry.path().join("config.json")) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else { continue };
        let matches = v
            .get("members")
            .and_then(|m| m.as_array())
            .map(|arr| arr.iter().any(|mem| mem.get("cwd").and_then(|c| c.as_str()) == Some(cwd)))
            .unwrap_or(false);
        if !matches {
            continue;
        }
        let task_dir = tasks_base.join(entry.file_name());
        if !task_dir.is_dir() {
            continue;
        }
        // 같은 cwd 에 팀 여럿(매 세션 새 팀) — 빈 팀(task 0개)은 건너뛰고, 실제 task 가
        // 있는 팀 중 가장 최근 task 가 쓰인 것을 고른다(빈 새 팀이 옛 task 팀을 가리지 않게).
        let mut latest_task: Option<std::time::SystemTime> = None;
        if let Ok(rd) = std::fs::read_dir(&task_dir) {
            for f in rd.flatten() {
                if f.path().extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(mt) = f.metadata().and_then(|m| m.modified()) {
                    if latest_task.map(|b| mt > b).unwrap_or(true) {
                        latest_task = Some(mt);
                    }
                }
            }
        }
        if let Some(mt) = latest_task {
            if best.as_ref().map(|(b, _)| mt > *b).unwrap_or(true) {
                best = Some((mt, task_dir));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// 이 태스크가 그 pane 것인가. `shared` = 방 저장소에서 읽었는지.
///
/// **방 저장소에서는 주인이 찍혀 있어야 내 것이다.** 전에는 `owner: ""` 를 「방 공용이라
/// 모두의 것」으로 쳤는데, 방 저장소는 그 cwd 에서 돌았던 *모든 옛 세션*이 쌓이는 곳이라
/// 아무도 안 잡고 죽은 태스크가 새 학생 카드마다 통째로 붙었다(실측 2026-08-07 sionic 방:
/// 59개 중 55개가 주인 없음 — 7/24 slack-sentry, 8/5 recall-gui·larva, 8/6 ref2va. 모모이
/// 본인 것은 3개인데 카드엔 58행). 주인 없는 것도 사라지진 않고 UI 가 「미배정 N개」로 접는다.
///
/// 세션 저장소는 반대다 — 그 pane 혼자 쓰는 목록이라 주인 없는 것도 제 것이고, 여기까지
/// 엄격하게 굴면 혼자 도는 pane 은 카드가 통째로 빈다.
fn task_is_mine(owner: &str, me: &str, shared: bool) -> bool {
    if shared {
        !owner.is_empty() && !me.is_empty() && owner == me
    } else {
        owner.is_empty() || (!me.is_empty() && owner == me)
    }
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct PaneTaskView {
    pub pane: String,
    pub id: String,
    pub subject: String,
    pub status: String,
    pub owner: String,
    pub mine: bool,
}

/// 네이티브 보드와 HTTP 화면의 공통 태스크 수집. 디스크와 transcript를 읽으므로
/// GUI 렌더가 아니라 worker에서 호출해야 한다.
pub fn pane_tasks_snapshot(
    backend: &Arc<dyn Backend>,
    surface: Option<&str>,
) -> Vec<PaneTaskView> {
    let board = backend.collab_board().unwrap_or_default();
    let reported: std::collections::HashMap<String, String> = backend
        .pane_session_ids()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut out = Vec::new();
    let mut claimed_team = std::collections::HashSet::new();
    for row in &board {
        if surface.is_some_and(|surface| row.surface_id != surface) {
            continue;
        }
        let reported_sid = reported.get(&row.surface_id).cloned().unwrap_or_default();
        let mut tasks = read_claude_tasks(&reported_sid);
        let mut shared = false;
        let team = row
            .team
            .as_deref()
            .and_then(team_task_dir_by_name)
            .or_else(|| team_task_dir_for_cwd(&row.cwd));
        if tasks.is_empty() {
            if let Some(dir) = &team {
                if claimed_team.insert(dir.clone()) {
                    tasks = read_tasks_in_dir(dir);
                    shared = true;
                }
            }
        }
        let me = row.agent_name.as_deref().unwrap_or("");
        for (id, subject, status, owner) in tasks {
            out.push(PaneTaskView {
                pane: row.surface_id.clone(),
                id,
                subject,
                status,
                mine: task_is_mine(&owner, me, shared),
                owner,
            });
        }
    }
    out
}

/// `GET /pane-tasks?surface=<id>` — claude TaskCreate 태스크를 pane 별로(arona 업무 탭).
/// `pane_session_ids`(bound transcript stem) → 없으면 board cwd 로 팀 task 디렉토리 폴백.
async fn pane_tasks_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let surface = params.get("surface").cloned().unwrap_or_default();
    let out = pane_tasks_snapshot(
        &backend,
        (!surface.is_empty()).then_some(surface.as_str()),
    );
    (cors, Json(serde_json::json!({ "ok": true, "tasks": out })))
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let pane_tasks_backend = backend.clone();
    axum::Router::new()
        .route(
            "/pane-tasks",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                pane_tasks_handler(pane_tasks_backend.clone(), q)
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pane 의 팀 이름으로 task 디렉토리를 집는지. 옛 경로 둘(`tasks/session-<8hex>`,
    /// `teams/<team>/config.json` 의 cwd 매칭)이 모두 빗나가 **모든 pane 의 태스크가 0개**
    /// 로 뜨던 회귀를 못박는다 — store 는 `tasks/<team>/` 이고 그 config.json 은 이제 없다.
    #[test]
    fn team_task_dir_comes_from_the_team_name() {
        let base = temp_dir("team-task-dir");
        let team = "kt-Users-kasa-Desktop-momewomo-sionic-15b5";
        std::fs::create_dir_all(base.join(team)).unwrap();
        assert_eq!(team_task_dir_in(&base, team), Some(base.join(team)));
        // 없는 팀은 빈 값 — 옆 팀 목록을 대신 보여주면 남의 태스크가 뜬다.
        assert_eq!(team_task_dir_in(&base, "kt-other-0000"), None);
        assert_eq!(team_task_dir_in(&base, ""), None);
        // 팀 이름이 그대로 경로 조각이 되므로 탈출 시도는 막는다.
        assert_eq!(team_task_dir_in(&base, "../etc"), None);
        assert_eq!(team_task_dir_in(&base, "a/b"), None);
    }

    #[test]
    fn shared_room_tasks_need_an_owner() {
        // 방 저장소: 주인 없는 것은 아무의 것도 아니다(옛 세션 유령이 카드마다 붙던 원인).
        assert!(!task_is_mine("", "모모이", true));
        assert!(task_is_mine("모모이", "모모이", true));
        assert!(!task_is_mine("히마리", "모모이", true));
        // 이름 없는 pane 은 방 목록에서 아무것도 가져가지 않는다.
        assert!(!task_is_mine("", "", true));

        // 세션 저장소: 그 pane 혼자 쓰므로 주인 없는 것도 제 것.
        assert!(task_is_mine("", "모모이", false));
        assert!(task_is_mine("", "", false));
        assert!(!task_is_mine("히마리", "모모이", false));
    }
}
