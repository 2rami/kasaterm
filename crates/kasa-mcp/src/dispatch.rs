//! 학생 자동 호출 — 일감 큐를 보고 빈 학생에게 배정하고, 없으면 새로 부른다.
//!
//! 판단이 2층인 이유: "몇 명을 부를지"는 절대 어겨선 안 되는 금지선(동시 상한·파일
//! 충돌·증식)과, 정답이 없는 취향(이 지시가 독립적으로 쪼개지나)이 섞여 있다. 앞은
//! 코드가 결정론적으로 지키고(층1), 뒤만 LLM 에 묻는다(층2, `plan_tasks`). 층2 가
//! 죽어도 층1 만으로 동작한다 — 그때는 지시 전체가 작업 1개다.
//!
//! 호출 수는 판단이 아니라 계산으로 나온다:
//!   필요 인원 = 실행 가능한 독립 작업 수 − 지금 비어 있는 자기 학생 수
//!   스폰 수  = clamp(필요 인원, 0, 상한 − 현재 자기 학생 수)
//! 그래서 일감이 1개면 아무도 부르지 않고, 파일이 겹치면 재우고, 상한이면 대기한다.
//!
//! **자기 학생만 부린다**: 선생님이 직접 쓰는 pane 에 지시를 주입하면 대화 중인 화면을
//! 덮어쓰는 사고가 된다. 그래서 디스패처는 `dispatch-state.json` 에 자기가 스폰한
//! surface 만 적어 두고 그 안에서만 배정한다.

use crate::http::claude_bin;
use anyhow::Result;
use kasa_socket::backend::{Backend, PaneActivity};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// 큐·상태 파일을 읽고 쓰는 사이에 다른 요청이 끼어들면 갱신이 사라진다(read-modify-write).
/// 파일 하나에 프로세스 하나라 프로세스 내 락으로 충분하다.
static STORE_LOCK: Mutex<()> = Mutex::new(());

// ── 저장 모델 ────────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct QueueTask {
    pub id: String,
    /// 학생에게 그대로 주입할 지시문.
    pub brief: String,
    /// 이 작업이 만질 파일 추정 — 충돌 검사의 유일한 근거. 비면 충돌 검사를 못 해
    /// 병렬 배정 대상에서 빠진다(안전 우선).
    #[serde(default)]
    pub files_hint: Vec<String>,
    /// pending | assigned | done | failed
    pub status: String,
    #[serde(default)]
    pub surface: String,
    #[serde(default)]
    pub character: String,
    /// 선행 작업 id — 전부 done 이어야 실행 가능.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// 0 = 선생님 지시에서 나온 것, 1+ = 학생이 만든 것. 1 이상은 새 학생을 못 부른다
    /// (학생이 학생을 부르면 지수로 증식한다).
    #[serde(default)]
    pub depth: u8,
    /// light | heavy — 모델 티어 선택에만 쓴다.
    #[serde(default)]
    pub weight: String,
    /// 이 작업을 만든 지시 원문(추적용).
    #[serde(default)]
    pub origin: String,
    /// 작업 레포 절대경로. split 은 **그때 활성 pane** 의 cwd 를 물려받아, 다른 창을
    /// 보고 있었다면 학생이 엉뚱한 레포에서 뜬다(실측: tmuxify 지시인데 mission-control
    /// 에서 부팅됐다). 그래서 지시를 받은 시점의 경로를 작업에 새겨 두고 스폰할 때 cd 한다.
    #[serde(default)]
    pub cwd: String,
    /// 완료 보고를 받을 pane(`%N`). 비면 보고 지시를 붙이지 않는다 — 아로나 UI 처럼
    /// pane 이 아닌 곳에서 낸 지시는 받을 주소가 없다.
    #[serde(default)]
    pub report_to: String,
    /// 수확한 보고(그 학생의 마지막 답변).
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub created_ts: f64,
    #[serde(default)]
    pub updated_ts: f64,
    /// 배정 시각 — 이 시각 직후엔 claude 가 아직 부팅 중이라 idle 로 보인다. 유예
    /// (`settle_sec`) 안에서는 수확 판정을 하지 않는다.
    #[serde(default)]
    pub assigned_ts: f64,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct DispatchConfig {
    /// 꺼져 있으면 큐만 쌓이고 아무도 부르지 않는다. 처음 들어가는 기능이라 기본 OFF —
    /// 켜는 순간부터는 상한 안에서 선생님 확인 없이 자유롭게 부른다.
    #[serde(default)]
    pub enabled: bool,
    /// 디스패처가 동시에 거느릴 수 있는 자기 학생 수.
    #[serde(default = "d_max_students")]
    pub max_students: usize,
    /// 몇 tick 연속 idle 이어야 "정말 끝났다"로 보는지. `agents --json` 상태가 2초
    /// 캐시라 한 번의 idle 은 방금 배정한 학생일 수도 있다.
    #[serde(default = "d_idle_ticks")]
    pub idle_ticks: u8,
    /// 배정 후 이 시간 안에는 수확 판정을 미룬다(claude 부팅 + 첫 턴 시작 유예).
    #[serde(default = "d_settle_sec")]
    pub settle_sec: f64,
    /// 컨텍스트를 이 % 이상 쓴 학생에겐 새 일을 주지 않는다.
    #[serde(default = "d_ctx_cap")]
    pub context_cap: u8,
    /// 층2 판단기 모델. 판단은 짧아 가벼운 티어로 충분하다.
    #[serde(default = "d_planner_model")]
    pub planner_model: String,
    /// heavy 작업 학생 모델. 빈 값 = 사용자 기본 모델.
    #[serde(default)]
    pub heavy_model: String,
    /// light 작업 학생 모델. 빈 값 = 사용자 기본 모델.
    #[serde(default)]
    pub light_model: String,
    /// 스폰할 때 돌려 쓸 캐릭터 이름. 비면 `characters.json` 순서를 따른다.
    #[serde(default)]
    pub characters: Vec<String>,
}

fn d_max_students() -> usize {
    4
}
fn d_idle_ticks() -> u8 {
    2
}
fn d_settle_sec() -> f64 {
    45.0
}
fn d_ctx_cap() -> u8 {
    85
}
fn d_planner_model() -> String {
    "sonnet".into()
}

impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_students: d_max_students(),
            idle_ticks: d_idle_ticks(),
            settle_sec: d_settle_sec(),
            context_cap: d_ctx_cap(),
            planner_model: d_planner_model(),
            heavy_model: String::new(),
            light_model: String::new(),
            characters: Vec::new(),
        }
    }
}

/// 디스패처가 스폰한 학생 명부 — 이 목록 밖의 pane 은 건드리지 않는다.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub struct DispatchState {
    #[serde(default)]
    pub students: Vec<OwnedStudent>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct OwnedStudent {
    pub surface: String,
    #[serde(default)]
    pub character: String,
    #[serde(default)]
    pub spawned_ts: f64,
}

// ── 파일 IO ──────────────────────────────────────────────────────────────────

/// 큐·명부를 둘 디렉터리. `KASATERM_DISPATCH_DIR` 로 갈아탈 수 있어야 하는 이유:
/// 검증용 인스턴스를 하나 더 띄우면 그 부팅이 라이브의 학생 명부를 리셋하고 배정을
/// 되돌린다(파일 하나를 두 프로세스가 소유하는 셈). 헤드리스 검증은 이 env 로 격리한다.
fn cfg_dir() -> Option<std::path::PathBuf> {
    if let Ok(d) = std::env::var("KASATERM_DISPATCH_DIR") {
        if !d.is_empty() {
            return Some(std::path::PathBuf::from(d));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/kasaterm"))
}

fn queue_path() -> Option<std::path::PathBuf> {
    Some(cfg_dir()?.join("queue.json"))
}

fn config_path() -> Option<std::path::PathBuf> {
    Some(cfg_dir()?.join("dispatch.json"))
}

fn state_path() -> Option<std::path::PathBuf> {
    Some(cfg_dir()?.join("dispatch-state.json"))
}

/// tmp + rename — 강제종료가 큐 파일을 반쯤 쓴 상태로 남기면 다음 부팅에 전부 잃는다.
fn write_json<T: serde::Serialize>(path: &std::path::Path, val: &T) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(s) = serde_json::to_string_pretty(val) else { return };
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, s).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: Option<std::path::PathBuf>) -> T {
    let Some(p) = path else { return T::default() };
    let Ok(s) = std::fs::read_to_string(&p) else { return T::default() };
    serde_json::from_str(&s).unwrap_or_default()
}

pub fn read_queue() -> Vec<QueueTask> {
    read_json(queue_path())
}

fn write_queue(items: &[QueueTask]) {
    if let Some(p) = queue_path() {
        write_json(&p, &items);
    }
}

pub fn read_config() -> DispatchConfig {
    let Some(p) = config_path() else { return DispatchConfig::default() };
    let Ok(s) = std::fs::read_to_string(&p) else { return DispatchConfig::default() };
    serde_json::from_str(&s).unwrap_or_default()
}

pub fn write_config(cfg: &DispatchConfig) {
    if let Some(p) = config_path() {
        write_json(&p, cfg);
    }
}

fn read_state() -> DispatchState {
    read_json(state_path())
}

fn write_state(st: &DispatchState) {
    if let Some(p) = state_path() {
        write_json(&p, st);
    }
}

pub fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn new_id(seed: usize) -> String {
    format!("{:08x}{:02x}", (now_unix() * 1000.0) as u64 & 0xffff_ffff, seed & 0xff)
}

// ── 큐 조작 ──────────────────────────────────────────────────────────────────

/// 작업을 큐에 넣는다. `depends_on` 의 임시 인덱스 참조(`#0`,`#1`)를 실제 id 로 바꾼다 —
/// 판단기는 아직 발급되지 않은 id 를 알 수 없어 자기 목록 안 위치로만 의존을 말한다.
pub fn push_tasks(mut tasks: Vec<QueueTask>) -> Vec<String> {
    let _g = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let now = now_unix();
    let ids: Vec<String> = (0..tasks.len()).map(new_id).collect();
    for (i, t) in tasks.iter_mut().enumerate() {
        t.id = ids[i].clone();
        t.status = "pending".into();
        t.created_ts = now;
        t.updated_ts = now;
        t.depends_on = t
            .depends_on
            .iter()
            .map(|d| match d.strip_prefix('#').and_then(|n| n.parse::<usize>().ok()) {
                Some(idx) if idx < ids.len() => ids[idx].clone(),
                _ => d.clone(),
            })
            .collect();
    }
    let mut q = read_queue();
    q.extend(tasks);
    write_queue(&q);
    ids
}

pub fn delete_task(id: &str) -> bool {
    let _g = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut q = read_queue();
    let before = q.len();
    q.retain(|t| t.id != id);
    write_queue(&q);
    before != q.len()
}

/// 부팅 시 정리 — 재시작하면 surface id 가 새로 발급돼 옛 배정은 존재하지 않는 pane 을
/// 가리킨다. 그대로 두면 영원히 수확되지 않는 유령 배정이 된다.
///
/// 되돌릴 배정이 없으면 아무 파일도 만들지 않는다 — 큐를 쓰지 않는 사람의 설정 폴더에
/// 빈 파일을 남기지 않고, 검증 인스턴스가 떠도 라이브 명부를 건드리지 않는다.
pub fn reset_on_boot() {
    let _g = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut q = read_queue();
    let mut changed = false;
    for t in q.iter_mut().filter(|t| t.status == "assigned") {
        t.status = "pending".into();
        t.surface.clear();
        t.character.clear();
        t.assigned_ts = 0.0;
        t.updated_ts = now_unix();
        changed = true;
    }
    if changed {
        write_queue(&q);
        write_state(&DispatchState::default());
    }
}

// ── 층1: 결정론 게이트 ───────────────────────────────────────────────────────

fn is_idle(row: &PaneActivity) -> bool {
    row.status == "idle"
}

/// 이 작업이 지금 실행 가능한가 — 선행 작업이 모두 done.
fn deps_clear(t: &QueueTask, q: &[QueueTask]) -> bool {
    t.depends_on.iter().all(|d| {
        q.iter()
            .find(|x| &x.id == d)
            .map(|x| x.status == "done")
            .unwrap_or(true) // 사라진 선행은 막지 않는다(수동 삭제)
    })
}

fn norm_path(p: &str) -> String {
    p.trim().trim_start_matches("./").to_string()
}

/// 파일 충돌 — 겹치면 병렬로 못 돌린다. 이 레포는 `main.rs` 의 `struct App` 처럼
/// 서로 다른 기능이 같은 정의를 건드리는 지점이 있어 실제로 git 충돌이 난다.
fn collides(files: &[String], busy: &HashSet<String>) -> bool {
    files.iter().any(|f| busy.contains(&norm_path(f)))
}

// ── 디스패처 ─────────────────────────────────────────────────────────────────

/// idle 연속 카운트 — tick 사이에 유지해야 해서 루프가 소유한다.
#[derive(Default)]
pub struct DispatchRuntime {
    idle_streak: HashMap<String, u8>,
    /// 캐릭터 순환 커서.
    cursor: usize,
}

/// 한 tick 에 할 일. 판단(`decide`)과 실행(PTY·스폰)을 갈라 두면 판단을 파일도 터미널도
/// 없이 검증할 수 있다 — 이 로직은 잘못 돌면 학생을 이중으로 부르거나 남의 화면을
/// 덮어쓰는 종류라, 눈으로 확인하는 것 말고 테스트가 필요하다.
#[derive(Debug, PartialEq)]
pub enum Decision {
    /// 끝났다 — 그 학생의 마지막 답변을 결과로 걷는다.
    Harvest { idx: usize, result: String },
    /// 배정했던 pane 이 사라졌다 — 대기열로 되돌린다.
    Requeue { idx: usize },
    /// 이미 도는 자기 학생에게 지시를 제출한다.
    AssignExisting { idx: usize, surface: String, character: String },
    /// 새 학생을 부른다(브리프를 부팅 인자로 싣는다).
    SpawnFor { idx: usize, character: String },
}

/// 순수 판단 — 큐·board·명부·설정·idle 연속 카운트를 보고 무엇을 할지만 정한다.
/// 아무것도 쓰지 않고 아무것도 호출하지 않는다.
pub fn decide(
    q: &[QueueTask],
    board: &[PaneActivity],
    state: &DispatchState,
    cfg: &DispatchConfig,
    streak: &HashMap<String, u8>,
    now: f64,
    cursor: &mut usize,
) -> Vec<Decision> {
    let by_surface: HashMap<&str, &PaneActivity> =
        board.iter().map(|r| (r.surface_id.as_str(), r)).collect();
    let mut out = Vec::new();

    // ── 수확 ── 배정 앞에 둔다: 방금 끝난 학생이 같은 tick 에 다음 일을 받게.
    let mut freed: HashSet<String> = HashSet::new();
    for (i, t) in q.iter().enumerate().filter(|(_, t)| t.status == "assigned") {
        match by_surface.get(t.surface.as_str()) {
            // pane 이 사라졌다(선생님이 닫음) → 결과를 못 봤으니 done 이 아니다.
            None => out.push(Decision::Requeue { idx: i }),
            Some(row) => {
                if now - t.assigned_ts < cfg.settle_sec {
                    continue; // 부팅 유예 — 아직 idle 로 보이는 게 정상이다
                }
                if streak.get(t.surface.as_str()).copied().unwrap_or(0) >= cfg.idle_ticks {
                    out.push(Decision::Harvest { idx: i, result: row.last_reply.clone() });
                    freed.insert(t.surface.clone());
                }
            }
        }
    }

    // ── 금지선 ── 진행 중 작업이 잡은 파일 + 남이 지금 만지는 파일.
    // 후자를 넣는 이유: 선생님이 직접 고치는 파일에 학생을 붙이면 그것도 충돌이다.
    let mut busy: HashSet<String> = HashSet::new();
    for t in q.iter().filter(|t| t.status == "assigned") {
        if freed.contains(&t.surface) {
            continue; // 이번 tick 에 끝난 작업의 파일은 이미 풀렸다
        }
        busy.extend(t.files_hint.iter().map(|f| norm_path(f)));
    }
    for row in board.iter().filter(|r| !is_idle(r)) {
        busy.extend(row.files.iter().map(|f| norm_path(f)));
        busy.extend(row.changed_files.iter().map(|f| norm_path(f)));
    }

    // ── 쓸 수 있는 자기 학생 ── idle 이 충분히 이어졌고 컨텍스트 여유가 있는 것.
    let mut free: Vec<String> = state
        .students
        .iter()
        .filter(|s| {
            let Some(row) = by_surface.get(s.surface.as_str()) else { return false };
            let held = q
                .iter()
                .any(|t| t.status == "assigned" && t.surface == s.surface && !freed.contains(&t.surface));
            is_idle(row)
                && streak.get(s.surface.as_str()).copied().unwrap_or(0) >= cfg.idle_ticks
                && row.context_pct < cfg.context_cap
                && !held
        })
        .map(|s| s.surface.clone())
        .collect();

    let mut headcount = state.students.len();
    let mut taken: HashSet<String> = board
        .iter()
        .filter_map(|r| r.character.clone())
        .chain(state.students.iter().map(|s| s.character.clone()))
        .collect();

    for i in 0..q.len() {
        if q[i].status != "pending" || !deps_clear(&q[i], q) {
            continue;
        }
        // files_hint 가 비면 무엇과 겹칠지 알 수 없다 — 다른 작업이 도는 동안은 재운다.
        let blind = q[i].files_hint.is_empty();
        if collides(&q[i].files_hint, &busy) || (blind && !busy.is_empty()) {
            continue;
        }
        if let Some(surface) = free.pop() {
            let character = by_surface
                .get(surface.as_str())
                .and_then(|r| r.character.clone())
                .unwrap_or_default();
            out.push(Decision::AssignExisting { idx: i, surface, character });
        } else if q[i].depth == 0 && headcount < cfg.max_students {
            let character = pick_character(cfg, &taken, cursor);
            taken.insert(character.clone());
            headcount += 1;
            out.push(Decision::SpawnFor { idx: i, character });
        } else {
            continue; // 상한이거나 학생이 만든 작업 — 빈 학생이 날 때까지 대기
        }
        busy.extend(q[i].files_hint.iter().map(|f| norm_path(f)));
    }
    out
}

/// 한 tick — 읽고, 판단하고, 실행하고, 쓴다.
pub fn dispatch_tick(backend: &Arc<dyn Backend>, rt: &mut DispatchRuntime) {
    let cfg = read_config();
    if !cfg.enabled {
        return;
    }
    let _g = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut q = read_queue();
    if q.iter().all(|t| t.status == "done" || t.status == "failed") {
        return;
    }
    let board = backend.collab_board().unwrap_or_default();
    let live: HashSet<&str> = board.iter().map(|r| r.surface_id.as_str()).collect();

    // 살아 있는 pane 만 명부에 남긴다 — 선생님이 닫은 학생은 잊는다.
    let mut state = read_state();
    state.students.retain(|s| live.contains(s.surface.as_str()));

    for row in board.iter() {
        let e = rt.idle_streak.entry(row.surface_id.clone()).or_insert(0);
        *e = if is_idle(row) { e.saturating_add(1) } else { 0 };
    }
    rt.idle_streak.retain(|k, _| live.contains(k.as_str()));

    let now = now_unix();
    let decisions = decide(&q, &board, &state, &cfg, &rt.idle_streak, now, &mut rt.cursor);
    let mut changed = false;

    for d in decisions {
        match d {
            Decision::Harvest { idx, result } => {
                q[idx].result = result;
                q[idx].status = "done".into();
                q[idx].updated_ts = now;
                changed = true;
            }
            Decision::Requeue { idx } => {
                q[idx].status = "pending".into();
                q[idx].surface.clear();
                q[idx].character.clear();
                q[idx].assigned_ts = 0.0;
                q[idx].updated_ts = now;
                changed = true;
            }
            Decision::AssignExisting { idx, surface, character } => {
                let brief = compose_brief(idx, &q, &board);
                if let Err(e) = backend.send_text(Some(&surface), &submit_payload(&brief)) {
                    eprintln!("[dispatch] send failed to {surface}: {e:#}");
                    continue; // 배정하지 않은 채로 남긴다 — 다음 tick 에 재시도
                }
                q[idx].character = character;
                q[idx].status = "assigned".into();
                q[idx].surface = surface;
                q[idx].assigned_ts = now;
                q[idx].updated_ts = now;
                changed = true;
            }
            Decision::SpawnFor { idx, character } => {
                // 맥락을 실은 사본으로 부팅한다 — 큐에는 원래 브리프를 남겨 이력이 지저분해지지 않게.
                let mut booted = q[idx].clone();
                booted.brief = compose_brief(idx, &q, &board);
                match spawn_with_brief(backend, &cfg, &character, &booted) {
                    Ok(sid) if !sid.is_empty() => {
                        state.students.push(OwnedStudent {
                            surface: sid.clone(),
                            character: character.clone(),
                            spawned_ts: now,
                        });
                        q[idx].character = character;
                        q[idx].status = "assigned".into();
                        q[idx].surface = sid;
                        q[idx].assigned_ts = now;
                        q[idx].updated_ts = now;
                        changed = true;
                    }
                    Ok(_) => eprintln!("[dispatch] spawn made no pane"),
                    Err(e) => eprintln!("[dispatch] spawn failed: {e:#}"),
                }
            }
        }
    }

    if changed {
        write_queue(&q);
    }
    write_state(&state);
}

/// TUI 입력창을 비우고(0x15) 붙여넣기로 감싼 뒤 제출 — 잔류 draft 에 지시가 합승해
/// 뒤섞이는 것을 막는다. `http::submit_payload` 와 같은 이유의 같은 형식.
fn submit_payload(text: &str) -> String {
    format!("\x15\x1b[200~{}\x1b[201~\r", text)
}

/// 이미 쓰이는 이름은 건너뛴다 — 같은 캐릭터 둘이 뜨면 board·교실에서 구분이 안 된다.
fn pick_character(cfg: &DispatchConfig, taken: &HashSet<String>, cursor: &mut usize) -> String {
    let pool: Vec<String> = if !cfg.characters.is_empty() {
        cfg.characters.clone()
    } else {
        crate::character::characters_json()
            .map(|v| crate::character::member_names(&v))
            .unwrap_or_default()
    };
    if pool.is_empty() {
        return String::new();
    }
    for _ in 0..pool.len() {
        let name = pool[*cursor % pool.len()].clone();
        *cursor += 1;
        if !taken.contains(&name) {
            return name;
        }
    }
    pool[*cursor % pool.len()].clone()
}

/// 새 학생을 부르고 브리프를 **부팅 인자로** 싣는다. pane 을 먼저 만들고 나중에 지시를
/// 주입하면 claude 가 뜨기까지의 몇 초를 노려야 하고, 그 사이 텍스트는 셸이 먹는다.
/// `claude '<브리프>'` 는 인터랙티브로 뜨면서 그 프롬프트를 첫 턴으로 실행하니 경합이 없다
/// (실측 확인: 프롬프트 인자가 shim 을 지나서도 첫 턴으로 실행된다).
fn spawn_with_brief(
    backend: &Arc<dyn Backend>,
    cfg: &DispatchConfig,
    character: &str,
    task: &QueueTask,
) -> Result<String> {
    let surface = backend.spawn_student(character)?;
    if surface.is_empty() {
        anyhow::bail!("no pane created");
    }
    send_with_retry(backend, &surface, &boot_command(cfg, task))?;
    Ok(surface)
}

/// 학생이 실제로 받는 지시문 = 원래 브리프 + 이 작업의 맥락.
///
/// 사용법(board·tell·wake-watch)은 학생 시스템 프롬프트에 이미 있으니 반복하지 않는다.
/// 여기서 주는 건 **배정 순간의 사실**이다: 형제 작업이 누구에게 갔는지, 지금 어떤 파일이
/// 잡혀 있는지, 내 앞 작업이 무엇을 알아냈는지, 보고는 어디로 하는지. 이걸 안 주면 학생은
/// 남이 같은 파일을 만지는 줄 모르고, 선행이 이미 밝힌 것을 처음부터 다시 조사한다.
fn compose_brief(idx: usize, q: &[QueueTask], board: &[PaneActivity]) -> String {
    let task = &q[idx];
    let mut out = task.brief.clone();
    let mut ctx: Vec<String> = Vec::new();

    // 같은 지시에서 갈라진 형제 — 누가 무엇을 들고 있는지.
    let siblings: Vec<String> = q
        .iter()
        .enumerate()
        .filter(|(i, s)| {
            *i != idx && !s.origin.is_empty() && s.origin == task.origin && s.status != "done"
        })
        .map(|(_, s)| {
            format!(
                "{} {}{}",
                if s.surface.is_empty() { "(대기)".into() } else { s.surface.clone() },
                if s.character.is_empty() { String::new() } else { format!("{} · ", s.character) },
                s.brief.chars().take(60).collect::<String>()
            )
        })
        .collect();
    if !siblings.is_empty() {
        ctx.push(format!("같은 지시에서 갈라진 형제 작업: {}", siblings.join(" / ")));
    }

    // 지금 남이 잡은 파일 — 겹치면 착수 전에 조율해야 한다.
    let mut held: Vec<String> = Vec::new();
    for row in board.iter().filter(|r| r.status != "idle" && r.surface_id != task.surface) {
        for f in row.files.iter().chain(row.changed_files.iter()) {
            let line = format!("{}({})", norm_path(f), row.surface_id);
            if !held.contains(&line) {
                held.push(line);
            }
        }
    }
    if !held.is_empty() {
        held.truncate(8);
        ctx.push(format!(
            "지금 다른 pane 이 잡은 파일: {}. 네가 만질 것과 겹치면 손대기 전에 그 pane 과 조율해라",
            held.join(", ")
        ));
    }

    // 선행 작업이 알아낸 것 — 다시 조사하지 않게.
    for d in task.depends_on.iter() {
        if let Some(p) = q.iter().find(|x| &x.id == d && x.status == "done") {
            ctx.push(format!(
                "선행 작업 「{}」 결과: {}",
                p.brief.chars().take(40).collect::<String>(),
                p.result.chars().take(400).collect::<String>()
            ));
        }
    }

    if !task.report_to.is_empty() {
        ctx.push(format!(
            "끝나면 `kasaterm-cli tell {} \"<한 줄 보고>\"` 로 알려라. 막히면 같은 방법으로 먼저 물어라",
            task.report_to
        ));
    }

    if !ctx.is_empty() {
        out.push_str("\n\n[맥락] ");
        out.push_str(&ctx.join("\n[맥락] "));
    }
    out
}

/// 스폰된 pane 에 넣을 한 줄. cd 를 앞에 붙이는 이유는 `QueueTask::cwd` 주석에 있다.
fn boot_command(cfg: &DispatchConfig, task: &QueueTask) -> String {
    let model = if task.weight == "light" { &cfg.light_model } else { &cfg.heavy_model };
    let model_arg = if model.is_empty() {
        String::new()
    } else {
        format!(" --model {}", sh_quote(model))
    };
    let cd = if task.cwd.is_empty() {
        String::new()
    } else {
        format!("cd {} && ", sh_quote(&task.cwd))
    };
    // 개행은 셸 인자에서 줄을 끊어 명령을 두 동으로 쪼갠다 — 한 줄로 평탄화한다.
    let brief = task.brief.replace(['\n', '\r'], " ");
    format!("{}claude{} {}\r", cd, model_arg, sh_quote(&brief))
}

/// split 이 id 를 돌려준 직후엔 그 pane 이 아직 send 경로에 등록되지 않아 "surface 없음"
/// 으로 거부된다(실측). 등록은 곧바로 끝나니 짧게 재시도하면 되고, 이걸 안 하면 학생은
/// 떴는데 브리프만 사라진 pane 이 남는다.
fn send_with_retry(backend: &Arc<dyn Backend>, surface: &str, text: &str) -> Result<()> {
    let mut last = None;
    for attempt in 0..6 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        match backend.send_text(Some(surface), text) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("send failed")))
}

/// 셸 single-quote 인용 — 브리프에 백틱·`$`·`&&` 가 섞여도 명령으로 새지 않는다.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 외부에서 온 소식을 지금 일하는 학생들에게 흘린다 — 슬랙 훅·CI 훅이 부르는 통로.
/// 일감이 아니라 정보라 큐에 넣지 않고 바로 제출한다(busy 면 그 학생의 다음 턴에 읽힌다).
///
/// 기본 대상은 디스패처가 부른 학생뿐이다. `all` 이면 board 의 모든 pane 까지 — 선생님이
/// 쓰는 화면에도 들어가니 외부 훅이 명시적으로 골라야 하고, 기본값이 되면 안 된다.
/// 알린 pane 목록을 돌려준다.
pub fn broadcast(backend: &Arc<dyn Backend>, text: &str, all: bool) -> Vec<String> {
    let board = backend.collab_board().unwrap_or_default();
    let live: HashSet<String> = board.iter().map(|r| r.surface_id.clone()).collect();
    let targets: Vec<String> = if all {
        board.iter().map(|r| r.surface_id.clone()).collect()
    } else {
        read_state()
            .students
            .into_iter()
            .map(|s| s.surface)
            .filter(|s| live.contains(s))
            .collect()
    };
    let mut sent = Vec::new();
    for t in targets {
        match backend.send_text(Some(&t), &submit_payload(text)) {
            Ok(()) => sent.push(t),
            Err(e) => eprintln!("[dispatch] broadcast to {t} failed: {e:#}"),
        }
    }
    sent
}

/// 10초 tick. 스케줄러(`schedule_loop`)와 같은 주기 — 학생의 턴은 분 단위라 더 조밀하게
/// 볼 이유가 없고, board 조회는 transcript 를 읽어 공짜가 아니다.
pub async fn dispatch_loop(backend: Arc<dyn Backend>) {
    reset_on_boot();
    let mut rt = DispatchRuntime::default();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        dispatch_tick(&backend, &mut rt);
    }
}

// ── 층2: LLM 판단 ────────────────────────────────────────────────────────────

/// 지시를 작업들로 쪼갠다. 실패하면 지시 전체를 작업 1개로 돌려준다 — 판단기가 죽어도
/// 디스패처는 계속 도는 게 옳다(층1만으로도 배정·수확은 된다).
pub async fn plan_tasks(instruction: &str, backend: &Arc<dyn Backend>) -> (Vec<QueueTask>, String) {
    let cfg = read_config();
    let board = backend.collab_board().unwrap_or_default();
    // 지시를 받은 지금의 경로를 새긴다 — 스폰 시점의 활성 pane 은 다른 레포일 수 있다.
    let cwd = crate::http::resolve_cwd(backend).to_string_lossy().to_string();
    let prompt = planner_prompt(instruction, &board);
    match run_planner(&cfg.planner_model, &prompt).await {
        Ok(text) => match parse_plan(&text, instruction, &cwd) {
            Some(v) if !v.is_empty() => (v, String::new()),
            _ => (
                vec![solo_task(instruction, &cwd)],
                format!("판단 결과를 못 읽어 1건으로: {text:.200}"),
            ),
        },
        Err(e) => (vec![solo_task(instruction, &cwd)], format!("판단기 실패로 1건으로: {e}")),
    }
}

pub fn solo_task(instruction: &str, cwd: &str) -> QueueTask {
    QueueTask {
        id: String::new(),
        brief: instruction.to_string(),
        files_hint: Vec::new(),
        status: "pending".into(),
        surface: String::new(),
        character: String::new(),
        depends_on: Vec::new(),
        depth: 0,
        weight: "heavy".into(),
        origin: instruction.to_string(),
        cwd: cwd.to_string(),
        report_to: String::new(),
        result: String::new(),
        created_ts: 0.0,
        updated_ts: 0.0,
        assigned_ts: 0.0,
    }
}

fn planner_prompt(instruction: &str, board: &[PaneActivity]) -> String {
    let mut who = String::new();
    for r in board.iter() {
        let name = r.character.clone().unwrap_or_else(|| r.surface_id.clone());
        who.push_str(&format!(
            "- {} ({}): {} · 만지는 파일 {}\n",
            name,
            r.status,
            if r.intent.is_empty() { "-" } else { &r.intent },
            if r.files.is_empty() { "없음".to_string() } else { r.files.join(", ") }
        ));
    }
    if who.is_empty() {
        who.push_str("- (지금 일하는 학생 없음)\n");
    }
    format!(
        "너는 작업 분배 판단기다. 아래 지시를 학생(각자 독립된 claude 인스턴스)에게 나눠 줄 작업으로 쪼갠다.\n\
\n지시:\n{instruction}\n\
\n지금 일하는 학생:\n{who}\
\n판단 규칙:\n\
- **쪼갤 이득이 증명될 때만 쪼갠다.** 기본값은 작업 1개다. 한 사람이 순서대로 하는 게 빠른 일을 나누면 병합 비용만 생긴다.\n\
- 같은 파일을 만질 갈래는 절대 나누지 말고, 굳이 나눠야 하면 depends_on 으로 순차화한다.\n\
- files_hint 에는 그 작업이 **편집할** 파일 경로를 추정해 적는다(레포 상대경로). 이게 충돌 검사의 유일한 근거다. 조사만 하는 작업은 빈 배열.\n\
- 작업은 최대 3개. depends_on 은 같은 응답 안 위치를 `#0` `#1` 로 가리킨다.\n\
- weight: 코드를 고치는 일은 heavy, 읽고 찾는 일은 light.\n\
- brief 는 그 학생이 다른 맥락 없이 바로 착수할 수 있게 자족적으로 쓴다(무엇을·어디를·완료 조건).\n\
\n다른 말 없이 JSON 만 출력:\n\
{{\"tasks\":[{{\"brief\":\"...\",\"files_hint\":[\"...\"],\"depends_on\":[],\"weight\":\"heavy\"}}],\"reason\":\"쪼갠 또는 안 쪼갠 이유 한 줄\"}}"
    )
}

/// `claude -p` 1회성 호출. tokio 의 process feature 를 켜지 않으려고(같은 워크스페이스를
/// 동시에 빌드하는 쪽까지 재컴파일된다) blocking 스레드에서 std Command 를 쓴다.
/// `output()` 은 파이프를 스스로 드레인하니 출력이 커도 자식이 막히지 않는다.
async fn run_planner(model: &str, prompt: &str) -> Result<String> {
    let (model, prompt) = (model.to_string(), prompt.to_string());
    let job = tokio::task::spawn_blocking(move || run_planner_blocking(&model, &prompt));
    match tokio::time::timeout(std::time::Duration::from_secs(120), job).await {
        Ok(joined) => joined?,
        // 스레드는 자식이 끝나며 스스로 풀린다 — 결과만 버린다.
        Err(_) => anyhow::bail!("planner timeout"),
    }
}

/// 이름이 아니라 절대경로로 부른다 — `.app` 의 PATH 에는 claude 가 없어 이름 호출은
/// 조용히 실패한다(과거에 이 함정을 밟았다).
fn run_planner_blocking(model: &str, prompt: &str) -> Result<String> {
    let mut cmd = std::process::Command::new(claude_bin());
    cmd.arg("-p").arg(prompt);
    if !model.is_empty() {
        cmd.arg("--model").arg(model);
    }
    // 중첩 claude 마커가 남아 있으면 자식이 자기를 팀원·pane 학생으로 착각한다.
    for k in [
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "KASATERM_SESSION_ID",
        "KASATERM_PANE_ID",
        "KASATERM_CHARACTER",
        "ANTHROPIC_MODEL",
    ] {
        cmd.env_remove(k);
    }
    cmd.stdin(std::process::Stdio::null());
    let out = cmd.output()?;
    if !out.status.success() {
        anyhow::bail!(
            "planner exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 응답에서 JSON 본문만 뽑는다 — 모델이 설명을 앞뒤에 붙여도 살아남게.
fn parse_plan(text: &str, origin: &str, cwd: &str) -> Option<Vec<QueueTask>> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    let arr = v.get("tasks")?.as_array()?;
    let mut out = Vec::new();
    for t in arr.iter().take(3) {
        let brief = t.get("brief").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        if brief.is_empty() {
            continue;
        }
        out.push(QueueTask {
            id: String::new(),
            brief,
            files_hint: t
                .get("files_hint")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|s| s.as_str()).map(|s| s.to_string()).collect())
                .unwrap_or_default(),
            status: "pending".into(),
            surface: String::new(),
            character: String::new(),
            depends_on: t
                .get("depends_on")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|s| s.as_str()).map(|s| s.to_string()).collect())
                .unwrap_or_default(),
            depth: 0,
            weight: t.get("weight").and_then(|x| x.as_str()).unwrap_or("heavy").to_string(),
            origin: origin.to_string(),
            cwd: cwd.to_string(),
            report_to: String::new(),
            result: String::new(),
            created_ts: 0.0,
            updated_ts: 0.0,
            assigned_ts: 0.0,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(sid: &str, status: &str, files: &[&str]) -> PaneActivity {
        PaneActivity {
            surface_id: sid.into(),
            status: status.into(),
            files: files.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn task(id: &str, status: &str, files: &[&str], deps: &[&str]) -> QueueTask {
        QueueTask {
            id: id.into(),
            brief: "일".into(),
            files_hint: files.iter().map(|s| s.to_string()).collect(),
            status: status.into(),
            surface: String::new(),
            character: String::new(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            depth: 0,
            weight: "heavy".into(),
            origin: String::new(),
            cwd: "/repo".into(),
            report_to: String::new(),
            result: String::new(),
            created_ts: 0.0,
            updated_ts: 0.0,
            assigned_ts: 0.0,
        }
    }

    #[test]
    fn deps_block_until_done() {
        let q = vec![task("a", "assigned", &[], &[]), task("b", "pending", &[], &["a"])];
        assert!(!deps_clear(&q[1], &q), "선행이 안 끝났으면 실행 불가");
        let q = vec![task("a", "done", &[], &[]), task("b", "pending", &[], &["a"])];
        assert!(deps_clear(&q[1], &q));
    }

    #[test]
    fn missing_dep_does_not_block() {
        let q = vec![task("b", "pending", &[], &["삭제된id"])];
        assert!(deps_clear(&q[0], &q), "사라진 선행은 영구 정지시키지 않는다");
    }

    #[test]
    fn file_collision_detected() {
        let busy: HashSet<String> = ["app/kasaterm/src/main.rs".to_string()].into_iter().collect();
        assert!(collides(&["./app/kasaterm/src/main.rs".into()], &busy), "./ 접두는 같은 파일");
        assert!(!collides(&["crates/kasa-mcp/src/http.rs".into()], &busy));
    }

    #[test]
    fn plan_parses_and_rewrites_index_deps() {
        let text = r##"설명이 앞에 붙어도 된다
        {"tasks":[{"brief":"A 고치기","files_hint":["a.rs"],"weight":"heavy"},
                  {"brief":"B 검증","depends_on":["#0"],"weight":"light"}],"reason":"순차"}"##;
        let v = parse_plan(text, "원문", "/repo").expect("parse");
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].depends_on, vec!["#0"], "push 전에는 인덱스 표기 그대로");
        assert_eq!(v[0].files_hint, vec!["a.rs"]);
        assert_eq!(v[1].weight, "light");
    }

    #[test]
    fn plan_rejects_garbage() {
        assert!(parse_plan("JSON 이 없다", "x", "/repo").is_none());
        assert!(parse_plan("{\"nope\":1}", "x", "/repo").is_none());
    }

    #[test]
    fn brief_carries_the_context_a_student_cannot_see() {
        let mut q = vec![
            task("t1", "done", &["a.rs"], &[]),
            task("t2", "assigned", &["b.rs"], &[]),
            task("t3", "pending", &["c.rs"], &["t1"]),
        ];
        for t in q.iter_mut() {
            t.origin = "설정 화면 손보기".into(); // 같은 지시에서 갈라진 형제
        }
        q[0].brief = "A: 자간 배선 조사".into();
        q[0].result = "cell_tighten() 이 env 만 읽는다".into();
        q[1].brief = "B: 슬라이더 위젯".into();
        q[1].surface = "%2".into();
        q[1].character = "유즈".into();
        q[2].brief = "C: 렌더 연결".into();
        q[2].report_to = "%3".into();

        let board = vec![row("%2", "working", &["app/kasaterm/src/settings.rs"])];
        let brief = compose_brief(2, &q, &board);

        assert!(brief.starts_with("C: 렌더 연결"), "원래 지시가 앞에 온다");
        assert!(brief.contains("%2") && brief.contains("B: 슬라이더 위젯"), "형제가 누구인지: {brief}");
        assert!(!brief.contains("A: 자간 배선 조사 /"), "끝난 형제는 진행 목록에 안 넣는다");
        assert!(brief.contains("app/kasaterm/src/settings.rs(%2)"), "남이 잡은 파일: {brief}");
        assert!(brief.contains("cell_tighten() 이 env 만 읽는다"), "선행이 알아낸 것을 물려준다");
        assert!(brief.contains("tell %3"), "보고 주소");
    }

    #[test]
    fn brief_stays_bare_when_there_is_no_context() {
        // 혼자 도는 첫 작업 — 붙일 사실이 없으면 원래 지시 그대로 둔다(잡음 금지).
        let q = vec![task("t1", "pending", &["a.rs"], &[])];
        assert_eq!(compose_brief(0, &q, &[]), "일");
    }

    #[test]
    fn boot_command_pins_the_repo_and_model() {
        let mut cfg = cfg_for(2);
        cfg.heavy_model = "claude-opus-5[1m]".into();
        cfg.light_model = "sonnet".into();
        let mut t = task("t1", "pending", &[], &[]);
        t.brief = "여러 줄\n지시문".into();
        t.cwd = "/Users/kasa/repo".into();

        let heavy = boot_command(&cfg, &t);
        assert!(heavy.starts_with("cd '/Users/kasa/repo' && claude "), "레포를 고정한다: {heavy}");
        assert!(heavy.contains("--model 'claude-opus-5[1m]'"), "대괄호 모델도 인용된다: {heavy}");
        assert!(!heavy.contains('\n'), "개행은 셸 명령을 쪼개니 남기지 않는다");
        assert!(heavy.ends_with("'여러 줄 지시문'\r"), "브리프는 부팅 인자로 실린다: {heavy}");

        t.weight = "light".into();
        assert!(boot_command(&cfg, &t).contains("--model 'sonnet'"));

        // cwd 를 모르면 cd 를 붙이지 않는다 — 빈 경로로 cd 하면 명령 전체가 실패한다.
        t.cwd.clear();
        assert!(boot_command(&cfg, &t).starts_with("claude "));
    }

    #[test]
    fn quote_survives_shell_metachars() {
        let q = sh_quote("rm -rf $HOME `whoami` && echo 'x'");
        assert!(q.starts_with('\'') && q.ends_with('\''));
        assert!(q.contains("'\\''"), "내부 인용부호는 이스케이프");
    }

    #[test]
    fn character_pick_avoids_taken() {
        let cfg = DispatchConfig {
            characters: vec!["미도리".into(), "유즈".into(), "아리스".into()],
            ..Default::default()
        };
        let taken: HashSet<String> = ["미도리".to_string()].into_iter().collect();
        let mut cur = 0;
        assert_eq!(pick_character(&cfg, &taken, &mut cur), "유즈", "쓰이는 이름은 건너뛴다");
    }

    // ── 판단(decide) ──

    fn cfg_for(max: usize) -> DispatchConfig {
        DispatchConfig {
            enabled: true,
            max_students: max,
            characters: vec!["미도리".into(), "유즈".into(), "아리스".into()],
            ..Default::default()
        }
    }

    fn owned(surface: &str, character: &str) -> OwnedStudent {
        OwnedStudent { surface: surface.into(), character: character.into(), spawned_ts: 0.0 }
    }

    /// idle 이 충분히 이어진 상태 — 실제로는 tick 을 여러 번 돌아 쌓인다.
    fn settled(surfaces: &[&str], ticks: u8) -> HashMap<String, u8> {
        surfaces.iter().map(|s| ((*s).to_string(), ticks)).collect()
    }

    #[test]
    fn single_task_never_spawns_when_a_student_is_free() {
        let q = vec![task("t1", "pending", &["a.rs"], &[])];
        let board = vec![row("%1", "idle", &[])];
        let state = DispatchState { students: vec![owned("%1", "미도리")] };
        let mut cur = 0;
        let d = decide(&q, &board, &state, &cfg_for(4), &settled(&["%1"], 2), 1000.0, &mut cur);
        assert_eq!(
            d,
            vec![Decision::AssignExisting { idx: 0, surface: "%1".into(), character: String::new() }],
            "빈 학생이 있으면 부르지 않고 그에게 준다"
        );
    }

    #[test]
    fn spawns_only_up_to_the_cap() {
        // 실행 가능한 독립 작업 4건, 상한 2, 빈 학생 0 → 2건만 스폰되고 나머지는 대기.
        let q = vec![
            task("t1", "pending", &["a.rs"], &[]),
            task("t2", "pending", &["b.rs"], &[]),
            task("t3", "pending", &["c.rs"], &[]),
            task("t4", "pending", &["d.rs"], &[]),
        ];
        let mut cur = 0;
        let d = decide(&q, &[], &DispatchState::default(), &cfg_for(2), &HashMap::new(), 1000.0, &mut cur);
        assert_eq!(d.len(), 2, "상한을 넘겨 부르지 않는다");
        assert!(matches!(d[0], Decision::SpawnFor { idx: 0, .. }));
        assert!(matches!(d[1], Decision::SpawnFor { idx: 1, .. }));
        // 서로 다른 캐릭터여야 board 에서 구분된다.
        let names: Vec<&str> = d
            .iter()
            .map(|x| match x {
                Decision::SpawnFor { character, .. } => character.as_str(),
                _ => "",
            })
            .collect();
        assert_ne!(names[0], names[1]);
    }

    #[test]
    fn colliding_files_wait_instead_of_spawning() {
        let q = vec![
            task("t1", "assigned", &["app/main.rs"], &[]),
            task("t2", "pending", &["app/main.rs"], &[]),
        ];
        let board = vec![row("%1", "working", &[])];
        let mut state = DispatchState { students: vec![owned("%1", "미도리")] };
        state.students[0].surface = "%1".into();
        let mut q = q;
        q[0].surface = "%1".into();
        q[0].assigned_ts = 999.0;
        let mut cur = 0;
        let d = decide(&q, &board, &state, &cfg_for(4), &HashMap::new(), 1000.0, &mut cur);
        assert!(d.is_empty(), "같은 파일을 만질 작업엔 아무도 붙이지 않는다");
    }

    #[test]
    fn student_made_task_never_spawns() {
        let mut q = vec![task("t1", "pending", &["a.rs"], &[])];
        q[0].depth = 1;
        let mut cur = 0;
        let d = decide(&q, &[], &DispatchState::default(), &cfg_for(4), &HashMap::new(), 1000.0, &mut cur);
        assert!(d.is_empty(), "학생이 만든 작업은 증식을 막으려 새 학생을 못 부른다");
    }

    #[test]
    fn settle_window_blocks_premature_harvest() {
        let mut q = vec![task("t1", "assigned", &["a.rs"], &[])];
        q[0].surface = "%1".into();
        q[0].assigned_ts = 990.0; // 10초 전 — 기본 유예 45초 안
        let mut board = vec![row("%1", "idle", &[])];
        board[0].last_reply = "아직 시작도 안 했다".into();
        let state = DispatchState { students: vec![owned("%1", "미도리")] };
        let mut cur = 0;
        let d = decide(&q, &board, &state, &cfg_for(4), &settled(&["%1"], 9), 1000.0, &mut cur);
        assert!(d.is_empty(), "부팅 직후의 idle 을 완료로 오해하면 안 된다");
    }

    #[test]
    fn idle_streak_required_before_harvest() {
        let mut q = vec![task("t1", "assigned", &["a.rs"], &[])];
        q[0].surface = "%1".into();
        q[0].assigned_ts = 100.0;
        let mut board = vec![row("%1", "idle", &[])];
        board[0].last_reply = "다 했어요".into();
        let state = DispatchState { students: vec![owned("%1", "미도리")] };
        let mut cur = 0;
        // idle 1회 — agents 상태가 2초 캐시라 이것만으론 못 믿는다.
        let d = decide(&q, &board, &state, &cfg_for(4), &settled(&["%1"], 1), 1000.0, &mut cur);
        assert!(d.is_empty(), "idle 1 tick 은 완료 근거가 못 된다");
        let d = decide(&q, &board, &state, &cfg_for(4), &settled(&["%1"], 2), 1000.0, &mut cur);
        assert_eq!(d, vec![Decision::Harvest { idx: 0, result: "다 했어요".into() }]);
    }

    #[test]
    fn harvest_frees_the_student_in_the_same_tick() {
        let mut q = vec![
            task("t1", "assigned", &["a.rs"], &[]),
            task("t2", "pending", &["b.rs"], &[]),
        ];
        q[0].surface = "%1".into();
        q[0].assigned_ts = 100.0;
        let mut board = vec![row("%1", "idle", &[])];
        board[0].last_reply = "끝".into();
        let state = DispatchState { students: vec![owned("%1", "미도리")] };
        let mut cur = 0;
        let d = decide(&q, &board, &state, &cfg_for(1), &settled(&["%1"], 3), 1000.0, &mut cur);
        assert_eq!(d.len(), 2, "수확과 재배정이 한 tick 에 이어진다");
        assert!(matches!(d[0], Decision::Harvest { idx: 0, .. }));
        assert_eq!(
            d[1],
            Decision::AssignExisting { idx: 1, surface: "%1".into(), character: String::new() },
            "상한 1이라 새로 부를 수 없지만 방금 빈 학생을 쓴다"
        );
    }

    #[test]
    fn vanished_pane_requeues_without_result() {
        let mut q = vec![task("t1", "assigned", &["a.rs"], &[])];
        q[0].surface = "%9".into();
        q[0].assigned_ts = 100.0;
        let mut cur = 0;
        let d = decide(&q, &[], &DispatchState::default(), &cfg_for(4), &HashMap::new(), 1000.0, &mut cur);
        assert_eq!(d, vec![Decision::Requeue { idx: 0 }], "닫힌 학생의 작업은 done 이 아니다");
    }

    #[test]
    fn busy_or_full_context_student_is_not_reused() {
        let q = vec![task("t1", "pending", &["a.rs"], &[])];
        // 컨텍스트가 꽉 찬 학생 — 새 일을 주면 바로 한도에 부딪힌다.
        let mut board = vec![row("%1", "idle", &[])];
        board[0].context_pct = 90;
        let state = DispatchState { students: vec![owned("%1", "미도리")] };
        let mut cur = 0;
        let d = decide(&q, &board, &state, &cfg_for(1), &settled(&["%1"], 5), 1000.0, &mut cur);
        assert!(d.is_empty(), "상한이 1이고 그 학생은 쓸 수 없으니 대기");
    }

    #[test]
    fn others_working_files_are_off_limits() {
        // 선생님이나 다른 학생이 지금 만지는 파일 — 디스패처가 붙이면 그것도 충돌이다.
        let q = vec![task("t1", "pending", &["app/kasaterm/src/main.rs"], &[])];
        let board = vec![row("%2", "working", &["app/kasaterm/src/main.rs"])];
        let mut cur = 0;
        let d = decide(&q, &board, &DispatchState::default(), &cfg_for(4), &HashMap::new(), 1000.0, &mut cur);
        assert!(d.is_empty(), "남이 만지는 파일에는 학생을 붙이지 않는다");
    }

    #[test]
    fn blind_task_runs_alone_but_waits_when_others_work() {
        // files_hint 가 비면 충돌 판정이 불가 — 혼자일 때만 돈다.
        let q = vec![task("t1", "pending", &[], &[])];
        let mut cur = 0;
        let d = decide(&q, &[], &DispatchState::default(), &cfg_for(4), &HashMap::new(), 1000.0, &mut cur);
        assert_eq!(d.len(), 1, "아무도 일하지 않으면 파일 미상이어도 착수");
        let board = vec![row("%2", "working", &["x.rs"])];
        let d = decide(&q, &board, &DispatchState::default(), &cfg_for(4), &HashMap::new(), 1000.0, &mut cur);
        assert!(d.is_empty(), "누군가 일하는 중엔 파일 미상 작업을 재운다");
    }

    #[test]
    fn dependent_task_waits_for_its_predecessor() {
        let mut q = vec![task("t1", "pending", &["a.rs"], &[]), task("t2", "pending", &["b.rs"], &["t1"])];
        q[0].files_hint = vec!["a.rs".into()];
        let mut cur = 0;
        let d = decide(&q, &[], &DispatchState::default(), &cfg_for(4), &HashMap::new(), 1000.0, &mut cur);
        assert_eq!(d.len(), 1, "선행이 안 끝났으면 후행은 배정 대상이 아니다");
        assert!(matches!(d[0], Decision::SpawnFor { idx: 0, .. }));
    }

    #[test]
    fn idle_row_detection() {
        assert!(is_idle(&row("%1", "idle", &[])));
        assert!(!is_idle(&row("%1", "working", &[])));
        assert!(!is_idle(&row("%1", "waiting", &[])), "권한 대기는 비어 있는 게 아니다");
    }
}
