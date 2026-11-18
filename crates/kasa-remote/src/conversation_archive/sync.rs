use super::{id_for, Append, Entry, MAX_APPEND};
use kasa_device::device_auth::{DeviceCred, Stamp};
use kasa_socket::backend::{Backend, PaneActivity};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 보드를 이만큼은 다시 짓지 않는다. 기록이 자라는 동안 바퀴가 1초마다 돌아 그때마다 보드 전체(칸마다 기록 꼬리
/// 512KB·프로세스 환경)를 지었는데, 여기서 쓰는 것은 칸 목록과 제목·이름·폴더·하네스뿐이다 — 맥북 표본
/// (2026-11-18, 칸 13·일함 8)에서 이 스레드가 바쁜 표본 360 이었다. 제목이 바뀐 것은 이 안에 따라 올라간다.
const BOARD_FRESH: Duration = Duration::from_secs(15);

/// 기록이 붙었는데 보드에 그 칸 줄이 없으면, 이만큼 지난 보드는 그 자리에서 다시 짓는다 — 새로 붙은 칸이
/// 이름·제목 없이 올라가지 않게. 보드에 끝내 안 서는 칸이 있어도 이 박자보다 자주 짓지 않는다.
const BOARD_MISSING_RETRY: Duration = Duration::from_secs(2);

/// 한 바퀴에 이만큼 넘게 올렸으면 기록이 밀려 있다 — 읽기 한도(512KiB)에 걸려 끊긴 바퀴라 곧장 다음 바퀴로 간다.
const BACKLOG_CHUNK: usize = 256 << 10;

/// 대화를 보는 요청이 끝난 뒤에도 이만큼은 「보는 중」이다 — 폰 대화 보기는 1.5초마다 다시 묻는다.
const WATCH_GRACE: Duration = Duration::from_secs(5);

/// 다음 바퀴까지 쉴 시간. 밀린 기록을 따라잡거나 자라는 기록을 누가 보고 있으면 1초, 자라기만 하면 5초, 다 올렸으면
/// 15초. 기록이 자라는 동안 늘 1초로 돌던 것을 줄였다 — 바퀴마다 관문 목록·칸마다 기록 열기가 고정으로 들어 맥북
/// 표본(2026-11-18)에서 이 스레드가 한 코어의 약 9% 였다. 폰은 실시간 대화를 원본에 바로 물으므로 보관본이
/// 몇 초 늦어도 보는 화면은 안 늦는다.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pace {
    Rest,
    Follow,
    Hurry,
}

impl Pace {
    pub fn wait(self) -> Duration {
        Duration::from_secs(match self { Pace::Rest => 15, Pace::Follow => 5, Pace::Hurry => 1 })
    }
}

/// 그 칸 대화를 지금 보는 쪽이 있다 — 폰·다른 기기의 대화 보기는 원본의 `/transcript-raw` 에 새 줄을 기다리는
/// 요청을 걸어 둔다. 그 창구가 이것을 쥔 동안과 놓은 뒤 [`WATCH_GRACE`] 동안을 「보는 중」으로 친다.
pub struct Watching(String);

static WATCHERS: std::sync::LazyLock<std::sync::Mutex<HashMap<String, (usize, Instant)>>> =
    std::sync::LazyLock::new(Default::default);

pub fn watching(surface: &str) -> Watching {
    if let Ok(mut watchers) = WATCHERS.lock() {
        let now = Instant::now();
        watchers.retain(|_, (open, last)| *open > 0 || now.duration_since(*last) < WATCH_GRACE);
        watchers.entry(surface.to_string()).or_insert((0, now)).0 += 1;
    }
    Watching(surface.to_string())
}

impl Drop for Watching {
    fn drop(&mut self) {
        if let Ok(mut watchers) = WATCHERS.lock() {
            if let Some((open, last)) = watchers.get_mut(&self.0) {
                *open = open.saturating_sub(1);
                *last = Instant::now();
            }
        }
    }
}

fn watched_within(surface: &str, grace: Duration) -> bool {
    WATCHERS.lock().ok().and_then(|watchers| watchers.get(surface).copied())
        .is_some_and(|(open, last)| open > 0 || last.elapsed() < grace)
}

#[derive(Default)]
pub struct BoardCache {
    at: Option<Instant>,
    rows: Vec<PaneActivity>,
}

impl BoardCache {
    fn older_than(&self, age: Duration) -> bool {
        self.at.is_none_or(|at| at.elapsed() >= age)
    }

    fn refresh(&mut self, backend: &dyn Backend) {
        self.rows = backend.collab_board().unwrap_or_default();
        self.at = Some(Instant::now());
    }
}

#[derive(Deserialize)]
struct List { enabled: bool, conversations: Vec<Entry> }
#[derive(Deserialize)]
struct Written { conversation: Entry }

pub fn spawn(backend: Arc<dyn Backend>) {
    if cfg!(any(test, feature = "test-isolation")) { return; }
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) { return; }
    let _ = std::thread::Builder::new().name("conversation-sync".into()).spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        runtime.block_on(async move {
            let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none()).build() else { return };
            let mut board = BoardCache::default();
            loop {
                let mut pace = Pace::Rest;
                if let (Some((credential, stamp)), Some(machine)) = (kasa_device::device_auth::capture(), kasa_device::mobile::machine_identity()) {
                    pace = synchronize_with(&mut board, backend.as_ref(), &client, &credential, &machine, &|| current(&stamp)).await.unwrap_or(Pace::Rest);
                }
                tokio::time::sleep(pace.wait()).await;
            }
        });
    });
}

fn current(stamp: &Stamp) -> bool { kasa_device::device_auth::with_current(stamp, || Ok(())).is_ok() }

async fn json<T: serde::de::DeserializeOwned>(mut response: reqwest::Response) -> Result<T, ()> {
    if !response.status().is_success() { return Err(()); }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if bytes.len() + chunk.len() > 32 << 20 { return Err(()); }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| ())
}

fn clipped(value: &str, max: usize) -> String {
    let mut value: String = value.chars().filter(|c| !c.is_control()).collect();
    let mut end = value.len().min(max);
    while !value.is_char_boundary(end) { end -= 1; }
    value.truncate(end);
    value
}

pub async fn synchronize(backend: &dyn Backend, client: &reqwest::Client, credential: &DeviceCred, machine: &str, authorized: &dyn Fn() -> bool) -> Result<bool, ()> {
    synchronize_with(&mut BoardCache::default(), backend, client, credential, machine, authorized).await.map(|pace| pace != Pace::Rest)
}

/// 바퀴 사이에 보드를 들고 다니는 쪽 — [`BOARD_FRESH`]. 돌려주는 것은 다음 바퀴까지의 [`Pace`].
pub async fn synchronize_with(board: &mut BoardCache, backend: &dyn Backend, client: &reqwest::Client, credential: &DeviceCred, machine: &str, authorized: &dyn Fn() -> bool) -> Result<Pace, ()> {
    if !authorized() { return Err(()); }
    let url = format!("{}/relay/conversations", credential.relay.trim_end_matches('/'));
    let listed: List = json(client.get(&url).bearer_auth(&credential.token).send().await.map_err(|_| ())?).await?;
    if !listed.enabled { return Ok(Pace::Rest); }
    let mut known: HashMap<_, _> = listed.conversations.into_iter().filter(|e| e.machine_id == machine).map(|e| (e.id.clone(), e)).collect();
    if board.older_than(BOARD_FRESH) { board.refresh(backend); }
    let mut ids: Vec<_> = backend.pane_windows().into_iter().map(|(id, _)| id)
        .chain(board.rows.iter().map(|row| row.surface_id.clone())).collect();
    ids.sort(); ids.dedup();
    let mut pace = Pace::Rest;
    for id in ids {
        if !authorized() { return Err(()); }
        if crate::remote::is_remote_pane(&id) { continue; }
        let Ok(identity) = backend.collab_pane_identity(&id) else { continue };
        let Some(key) = identity["surface_key"].as_str() else { continue };
        let candidate = known.values().filter(|e| e.surface_key == key).max_by_key(|e| e.updated_at);
        let mut from = candidate.map_or(0, |e| e.cursor);
        let Ok(mut chunk) = backend.transcript_archive(&id, from) else { continue };
        if chunk.source.is_empty() || chunk.raw.len() > MAX_APPEND { continue; }
        let archive_id = id_for(machine, &chunk.source);
        let previous = known.get(&archive_id);
        if previous.is_some_and(|e| e.deleted) { continue; }
        let wanted = previous.map_or(0, |e| e.cursor);
        if from != wanted {
            let source = chunk.source.clone();
            from = wanted;
            let Ok(fresh) = backend.transcript_archive(&id, from) else { continue };
            if fresh.source != source { continue; }
            chunk = fresh;
        }
        if chunk.raw.len() > MAX_APPEND || !authorized() { continue; }
        if !board.rows.iter().any(|row| row.surface_id == id) && board.older_than(BOARD_MISSING_RETRY) {
            board.refresh(backend);
        }
        let row = board.rows.iter().find(|row| row.surface_id == id);
        let harness = row.and_then(|row| row.harness.as_deref())
            .unwrap_or(if chunk.source.starts_with("rollout-") { "codex" } else { "claude" });
        if !matches!(harness, "claude" | "codex") { continue; }
        let title = clipped(row.map(|r| r.title.as_str()).unwrap_or(""), 512);
        let name = clipped(row.and_then(|r| r.character.as_deref()).unwrap_or(""), 128);
        let cwd = clipped(row.map(|r| r.cwd.as_str()).unwrap_or(""), 4096);
        let caught_up = chunk.offset == from && chunk.raw.is_empty();
        if previous.is_some_and(|e| e.ready && caught_up && e.surface_key == key && e.title == title && e.name == name && e.cwd == cwd) {
            continue;
        }
        let update_len = chunk.raw.len();
        let update = Append {
            source: chunk.source, surface_key: key.into(), title, name, harness: harness.into(), cwd,
            expected_revision: previous.map_or(0, |e| e.revision), from: if chunk.reset { 0 } else { from }, next: chunk.offset,
            reset: chunk.reset || previous.is_none(), caught_up, raw: chunk.raw,
        };
        if !authorized() { return Err(()); }
        let response = client.post(&url).bearer_auth(&credential.token).json(&update).send().await.map_err(|_| ())?;
        if response.status() == reqwest::StatusCode::CONFLICT { continue; }
        let saved: Written = match json(response).await { Ok(saved) => saved, Err(_) => continue };
        if !authorized() { return Err(()); }
        if saved.conversation.id != archive_id || saved.conversation.machine_id != machine { return Err(()); }
        known.insert(archive_id, saved.conversation);
        if !caught_up {
            let hurry = update_len >= BACKLOG_CHUNK || watched_within(&id, WATCH_GRACE);
            pace = pace.max(if hurry { Pace::Hurry } else { Pace::Follow });
        }
    }
    Ok(pace)
}

#[cfg(test)]
mod pace_tests {
    use super::*;

    #[test]
    fn a_held_or_just_finished_transcript_request_counts_as_watching() {
        let surface = format!("%kasatest-watch-{}", std::process::id());
        assert!(!watched_within(&surface, WATCH_GRACE));
        let held = watching(&surface);
        assert!(watched_within(&surface, Duration::ZERO), "기다리는 요청이 걸린 동안은 보는 중");
        drop(held);
        assert!(watched_within(&surface, WATCH_GRACE), "끝난 직후도 보는 중 — 폰은 1.5초마다 다시 묻는다");
        assert!(!watched_within(&surface, Duration::ZERO), "유예가 지나면 안 보는 것");
        assert!(!watched_within("%kasatest-watch-other", WATCH_GRACE), "다른 칸과 섞이지 않는다");
    }

    #[test]
    fn the_fastest_need_of_a_round_sets_its_wait() {
        assert_eq!(Pace::Rest.max(Pace::Follow).max(Pace::Hurry), Pace::Hurry);
        assert_eq!([Pace::Hurry, Pace::Follow, Pace::Rest].map(Pace::wait), [1, 5, 15].map(Duration::from_secs));
    }
}
