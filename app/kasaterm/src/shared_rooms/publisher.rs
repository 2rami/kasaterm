//! 고른 원본 대화를 공유방에 올린다 — 처음 게시, 뒤이어 붙는 줄의 증분 전송, 완료 경계마다 포크 자료.
//!
//! 고른 대화만 바인딩으로 적고(계정 scope = origin+account 마다 한 파일, 재인증 뒤에도 남는다) 그것만 보낸다.
//! 올리는 줄은 `display_lines` 로 걸러 숨긴 추론·주입 문맥·이미지·권한·경로를 뺀다. 칸의 대화가 바뀌면 멈추고,
//! 공유가 지워진(404) 대화는 다시 만들지 않는다. 포크 자료의 표시 기록은 native 경계까지 걸러 낸 전체를 싣고,
//! 관문 한도를 넘으면 자르지 않고 오류로 남긴다.

use super::*;
use super::service::Kind;
use kasa_agents::session_fork as native;
use kasa_remote::shared_rooms::client::{run, Client, Error as ClientError};
use kasa_socket::shared_rooms as api;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

const SYNC_EVERY: Duration = Duration::from_secs(3);
const CHECKPOINT_EVERY_MS: u64 = 30_000;
const READ_CHUNK: u64 = 2 << 20;
const MAX_LINE: u64 = 16 << 20;
const MAX_APPEND: usize = kasa_remote::conversation_archive::MAX_APPEND;
const MAX_DISPLAY: usize = kasa_remote::shared_rooms::store::MAX_DISPLAY;
const MAX_OMISSIONS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum BindingState {
    Live,
    /// 원본 칸의 대화가 바뀌었거나 기록을 더 읽을 수 없다. 까닭을 보여 준다.
    Stopped(String),
    /// 공유방에서 이 대화를 지웠다. 다시 만들지 않는다.
    Removed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Binding {
    pub(crate) room_id: String,
    pub(crate) session_id: String,
    pub(crate) source_key: String,
    pub(crate) harness: String,
    pub(crate) native: String,
    pub(crate) transcript: PathBuf,
    pub(crate) surface_key: String,
    pub(crate) title: String,
    pub(crate) cwd: String,
    pub(crate) include_project: bool,
    pub(crate) generation: String,
    pub(crate) revision: u64,
    /// 원본 기록에서 올린 끝(완성된 줄 경계).
    pub(crate) cursor: u64,
    /// 관문 쪽 내용을 처음부터 다시 써야 한다(바인딩을 잃었거나 원본이 줄었다).
    pub(crate) reset: bool,
    pub(crate) boundary_id: Option<String>,
    pub(crate) checkpoint_at_ms: u64,
    /// 마지막으로 포크 자료를 살펴본 커서. 새 줄이 없으면 원본을 다시 훑지 않는다.
    #[serde(default)]
    pub(crate) checkpoint_cursor: u64,
    pub(crate) checkpoint_error: Option<String>,
    pub(crate) state: BindingState,
}

#[derive(Default)]
pub(crate) struct Publisher {
    /// 바인딩을 읽어 온 계정 scope(origin+account). 다른 계정의 바인딩은 보내지 않는다.
    pub(super) scope: Option<String>,
    pub(super) bindings: Vec<Binding>,
    pub(super) syncing: bool,
    pub(super) sequence: u64,
    last_sync: Option<Instant>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Report {
    /// 바인딩 파일의 계정(origin+account).
    pub(super) scope: String,
    /// 이 일을 한 자격(auth scope). 늦은 응답을 가른다.
    pub(super) auth_scope: String,
    pub(super) bindings: Vec<Binding>,
    pub(super) error: Option<String>,
}

fn bindings_path(scope: &str) -> Result<PathBuf, String> {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(format!("shared-room-bindings\0{scope}").as_bytes());
    let name: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    Ok(service::shared_dir()?.join(format!("bindings-{name}.json")))
}

fn load_bindings(scope: &str) -> Vec<Binding> {
    bindings_path(scope).ok().and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Vec<Binding>>(&bytes).ok()).unwrap_or_default()
}

fn save_bindings(scope: &str, bindings: &[Binding]) -> Result<(), String> {
    let path = bindings_path(scope)?;
    let part = path.with_extension("json.part");
    let body = serde_json::to_vec_pretty(bindings).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&part);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&part).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut file, &body).and_then(|()| file.sync_all()).map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&part, &path).map_err(|e| e.to_string())
}

impl Publisher {
    fn change_scope(&mut self, scope: Option<String>) {
        if self.scope == scope { return; }
        self.sequence = self.sequence.wrapping_add(1);
        self.syncing = false;
        self.bindings = scope.as_deref().map(load_bindings).unwrap_or_default();
        self.scope = scope;
    }

    fn same(a: &Binding, b: &Binding) -> bool {
        a.room_id == b.room_id && a.session_id == b.session_id
    }

    /// 배경 일이 돌려준 바인딩을 합친다. 다른 계정 것이면 버린다.
    pub(super) fn absorb(&mut self, report: Report) {
        if report.scope.is_empty() {
            return;
        }
        if self.scope.as_deref() != Some(report.scope.as_str()) {
            if self.scope.is_some() {
                return;
            }
            self.bindings = load_bindings(&report.scope);
            self.scope = Some(report.scope.clone());
        }
        for binding in report.bindings {
            match self.bindings.iter_mut().find(|known| Self::same(known, &binding)) {
                Some(known) if known.generation == binding.generation && known.revision > binding.revision => {}
                Some(known) => *known = binding,
                None => self.bindings.push(binding),
            }
        }
        if let Err(error) = save_bindings(&report.scope, &self.bindings) {
            eprintln!("[shared-rooms] 게시 바인딩을 저장하지 못했어요: {error}");
        }
    }

    pub(super) fn next_due(&self) -> Option<Instant> {
        let live = self.bindings.iter().any(|b| b.state == BindingState::Live);
        (live && !self.syncing).then(|| self.last_sync.map_or_else(Instant::now, |at| at + SYNC_EVERY))
    }
}

impl App {
    pub(crate) fn publish_shared_sources(&mut self, room_id: &str, keys: &[String], include_project: bool) -> Result<String, String> {
        if room_id.is_empty() || keys.is_empty() {
            return Err("게시할 대화를 골라 주세요".into());
        }
        let scope = self.shared_rooms.client.as_ref().filter(|c| c.is_current()).map(|c| c.scope().to_string());
        self.shared_rooms.runtime.publisher.change_scope(scope);
        let origins = self.shared_origins();
        let mut chosen = Vec::new();
        for key in keys {
            let origin = origins.iter().find(|o| o.source.key() == key).ok_or("고른 대화가 바뀌었어요. 목록을 새로 읽어 주세요")?;
            if !origin.source.ready {
                return Err(format!("{} 대화가 아직 준비되지 않았어요", origin.source.title));
            }
            chosen.push(origin.clone());
        }
        if self.shared_rooms.runtime.publisher.syncing { return Err("공유 내용을 동기화하고 있어요. 잠시 뒤 다시 게시해 주세요".into()); }
        let mut sorted: Vec<_> = chosen.iter().map(|origin| format!("{}:{}:{}", origin.source.key(), origin.source.harness, origin.native)).collect();
        sorted.sort();
        let (id, fresh) = self.shared_begin_operation(Kind::Publish,
            format!("publish\0{room_id}\0{include_project}\0{}", sorted.join("\0")), String::new())?;
        if !fresh {
            return Ok(id);
        }
        let epoch = self.shared_operation(&id).map(|op| op.epoch).unwrap_or_default();
        let expected = self.shared_rooms.client.as_ref().map(|c| c.auth_scope().to_string());
        let known = self.shared_rooms.runtime.publisher.bindings.clone();
        self.shared_rooms.runtime.publisher.sequence = self.shared_rooms.runtime.publisher.sequence.wrapping_add(1);
        self.shared_rooms.runtime.publisher.syncing = true;
        let sequence = self.shared_rooms.runtime.publisher.sequence;
        let (proxy, operation, room) = (self.proxy.clone(), id.clone(), room_id.to_string());
        std::thread::spawn(move || {
            let result = publish(&room, chosen, include_project, known, expected.as_deref());
            let scope = result.as_ref().ok().map(|report| report.auth_scope.clone()).or(expected);
            let _ = proxy.send_event(UserEvent::SharedLocal(service::Event::Published { operation, epoch, scope, sequence, result }));
        });
        Ok(id)
    }

    /// 살아 있는 바인딩을 몇 초마다 한 번씩 이어 보낸다. 칸의 대화가 바뀐 것은 여기서 멈춘다.
    pub(super) fn tick_shared_publisher(&mut self, epoch: u64) {
        let scope = self.shared_rooms.client.as_ref().filter(|c| c.is_current()).map(|c| c.scope().to_string());
        let publisher = &mut self.shared_rooms.runtime.publisher;
        publisher.change_scope(scope.clone());
        if publisher.syncing || publisher.next_due().is_none_or(|due| due > Instant::now()) {
            return;
        }
        // 아래에서 일찍 돌아가도 다음 박자는 SYNC_EVERY 뒤다 — 이벤트 루프를 쉬지 않고 깨우지 않는다.
        publisher.last_sync = Some(Instant::now());
        let Some(scope) = scope else { return };
        let origins = self.shared_origins();
        let publisher = &mut self.shared_rooms.runtime.publisher;
        let mut changed = false;
        for binding in publisher.bindings.iter_mut().filter(|b| b.state == BindingState::Live) {
            if let Some(origin) = origins.iter().find(|o| o.source.key() == binding.surface_key) {
                if origin.native != binding.native || origin.transcript != binding.transcript {
                    binding.state = BindingState::Stopped("원본 칸의 대화가 바뀌어 게시를 멈췄어요".into());
                    changed = true;
                }
            }
        }
        if changed {
            let _ = save_bindings(&scope, &publisher.bindings);
        }
        let live: Vec<Binding> = publisher.bindings.iter().filter(|b| b.state == BindingState::Live).cloned().collect();
        if live.is_empty() {
            return;
        }
        publisher.syncing = true;
        publisher.sequence = publisher.sequence.wrapping_add(1);
        let sequence = publisher.sequence;
        let expected = self.shared_rooms.client.as_ref().map(|c| c.auth_scope().to_string());
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let report = sync(live, expected.as_deref()).unwrap_or_else(|error| Report { error: Some(error), ..Report::default() });
            let scope = Some(report.auth_scope.clone()).filter(|s| !s.is_empty()).or(expected);
            let _ = proxy.send_event(UserEvent::SharedLocal(service::Event::Synced { epoch, scope, sequence, report }));
        });
    }
}

fn client_for(expected: Option<&str>) -> Result<Client, String> {
    let client = capture_client().map_err(|e| e.to_string())?;
    if expected.is_some_and(|scope| scope != client.auth_scope()) {
        return Err("계정이 바뀌었어요. 다시 게시해 주세요.".into());
    }
    Ok(client)
}

fn publish(room: &str, chosen: Vec<sources::Origin>, include_project: bool, known: Vec<Binding>, expected: Option<&str>) -> Result<Report, String> {
    let client = client_for(expected)?;
    let mut bindings = Vec::new();
    for origin in chosen {
        let source_key = format!("{}:{}", origin.source.harness, origin.native);
        let request = api::PublishSession { source_key: source_key.clone(), title: origin.source.title.clone(), harness: origin.source.harness.clone() };
        let session = run(client.publish(room, &request)).map_err(|e| e.to_string())?;
        let previous = known.iter().find(|b| b.room_id == room && b.session_id == session.id && b.state != BindingState::Removed);
        let resumed = previous.filter(|b| b.generation == session.generation && b.revision == session.revision && b.native == origin.native);
        let mut binding = Binding {
            room_id: room.into(), session_id: session.id.clone(), source_key, harness: origin.source.harness.clone(),
            native: origin.native.clone(), transcript: origin.transcript.clone(), surface_key: origin.source.key().into(),
            title: origin.source.title.clone(), cwd: origin.source.cwd.clone(), include_project,
            generation: session.generation.clone(), revision: session.revision, cursor: resumed.map_or(0, |b| b.cursor),
            // 관문에 남은 내용과 우리 커서가 맞는다는 증거가 없으면 처음부터 다시 쓴다.
            reset: resumed.is_none() && session.bytes > 0,
            boundary_id: resumed.filter(|b| b.include_project == include_project).and_then(|b| b.boundary_id.clone()),
            checkpoint_at_ms: 0, checkpoint_cursor: 0, checkpoint_error: None,
            state: BindingState::Live,
        };
        sync_one(&client, &mut binding)?;
        bindings.push(binding);
    }
    Ok(Report { scope: client.scope().into(), auth_scope: client.auth_scope().into(), bindings, error: None })
}

fn sync(bindings: Vec<Binding>, expected: Option<&str>) -> Result<Report, String> {
    let client = client_for(expected)?;
    let mut out = Vec::new();
    let mut error = None;
    for mut binding in bindings {
        if !client.is_current() {
            return Err("계정이 바뀌었어요.".into());
        }
        if let Err(why) = sync_one(&client, &mut binding) {
            error.get_or_insert(why);
        }
        out.push(binding);
    }
    Ok(Report { scope: client.scope().into(), auth_scope: client.auth_scope().into(), bindings: out, error })
}

/// 관문 오류 가운데 바인딩 상태를 바꾸는 것. 나머지는 다음 박자에 다시 한다.
fn settle(binding: &mut Binding, error: &ClientError) -> Option<String> {
    match error.status {
        404 => {
            binding.state = BindingState::Removed;
            None
        }
        409 => {
            binding.reset = true;
            None
        }
        _ => Some(error.to_string()),
    }
}

fn sync_one(client: &Client, binding: &mut Binding) -> Result<(), String> {
    if binding.state != BindingState::Live {
        return Ok(());
    }
    if binding.reset {
        // 409 뒤에는 관문의 지금 세대·revision 을 다시 받아 처음부터 쓴다.
        match run(client.room(&binding.room_id)) {
            Ok(room) => match room.sessions.iter().find(|s| s.id == binding.session_id) {
                Some(session) => {
                    binding.generation = session.generation.clone();
                    binding.revision = session.revision;
                    binding.cursor = 0;
                }
                None => {
                    binding.state = BindingState::Removed;
                    return Ok(());
                }
            },
            Err(error) => return settle(binding, &error).map_or(Ok(()), Err),
        }
    }
    let (raw, next, total) = match read_lines(&binding.transcript, binding.cursor) {
        Ok(read) => read,
        Err(why) => {
            binding.state = BindingState::Stopped(why);
            return Ok(());
        }
    };
    if next < binding.cursor {
        // 원본이 줄었다 — 같은 대화를 다시 쓴 것이라 처음부터 다시 올린다.
        binding.reset = true;
        binding.cursor = 0;
        return Ok(());
    }
    let shown = native::display_lines(&binding.harness, &raw);
    if shown.len() > MAX_APPEND {
        binding.state = BindingState::Stopped("한 번에 올릴 수 있는 크기를 넘는 줄이 있어 게시를 멈췄어요".into());
        return Ok(());
    }
    if next > binding.cursor || binding.reset {
        let update = api::SharedAppend {
            expected_revision: binding.revision, generation: binding.generation.clone(),
            from: if binding.reset { 0 } else { binding.cursor }, next, reset: binding.reset,
            raw: shown, caught_up: next == total,
        };
        match run(client.append(&binding.room_id, &binding.session_id, &update)) {
            Ok(session) => {
                if binding.generation != session.generation {
                    binding.boundary_id = None;
                    binding.checkpoint_cursor = 0;
                }
                binding.generation = session.generation;
                binding.revision = session.revision;
                binding.cursor = next;
                binding.reset = false;
            }
            Err(error) => return settle(binding, &error).map_or(Ok(()), Err),
        }
    }
    let now = now_ms();
    let fresh = binding.checkpoint_cursor != binding.cursor || binding.boundary_id.is_none();
    if binding.state == BindingState::Live && fresh && now.saturating_sub(binding.checkpoint_at_ms) >= CHECKPOINT_EVERY_MS {
        binding.checkpoint_at_ms = now;
        match checkpoint(binding) {
            Ok(Some(request)) => match run(client.checkpoint(&binding.room_id, &binding.session_id, &api::PublishCheckpoint {
                expected_generation: binding.generation.clone(), expected_revision: binding.revision, checkpoint: request,
            })) {
                Ok(summary) => {
                    binding.checkpoint_cursor = binding.cursor;
                    binding.boundary_id = Some(summary.boundary_id);
                    binding.checkpoint_error = None;
                }
                Err(error) => {
                    if error.status == 409 {
                        match run(client.room(&binding.room_id)) {
                            Ok(room) => if let Some(session) = room.sessions.iter().find(|s| s.id == binding.session_id) {
                                refresh_checkpoint_binding(binding, session);
                            } else { binding.state = BindingState::Removed; },
                            Err(error) => binding.checkpoint_error = settle(binding, &error),
                        }
                    } else { binding.checkpoint_error = settle(binding, &error); }
                }
            },
            Ok(None) => { binding.checkpoint_cursor = binding.cursor; }
            Err(why) => binding.checkpoint_error = Some(why),
        }
    }
    Ok(())
}

fn refresh_checkpoint_binding(binding: &mut Binding, session: &api::SharedSession) {
    binding.generation = session.generation.clone();
    binding.revision = session.revision;
    binding.cursor = session.cursor;
    binding.boundary_id = session.checkpoint.as_ref().filter(|c| c.has_project == binding.include_project).map(|c| c.boundary_id.clone());
    binding.checkpoint_error = Some("공유 자료가 갱신돼 최신 완료 지점을 다시 확인하고 있어요".into());
}

/// `from` 부터 완성된 줄. 아주 긴 한 줄(그림 붙은 말)은 다음 개행까지 더 읽는다.
fn read_lines(path: &std::path::Path, from: u64) -> Result<(String, u64, u64), String> {
    let mut file = std::fs::File::open(path).map_err(|_| "원본 기록을 더 읽을 수 없어 게시를 멈췄어요".to_string())?;
    let total = file.metadata().map_err(|e| e.to_string())?.len();
    if total < from {
        return Ok((String::new(), 0, total));
    }
    file.seek(SeekFrom::Start(from)).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut limit = READ_CHUNK;
    loop {
        buf.clear();
        Read::by_ref(&mut file).take(limit).read_to_end(&mut buf).map_err(|e| e.to_string())?;
        if let Some(end) = buf.iter().rposition(|&b| b == b'\n') {
            buf.truncate(end + 1);
            break;
        }
        if buf.len() < limit as usize {
            buf.clear();
            break;
        }
        if limit >= MAX_LINE {
            return Err("원본 기록의 한 줄이 너무 길어 게시를 멈췄어요".into());
        }
        limit = (limit * 4).min(MAX_LINE);
        file.seek(SeekFrom::Start(from)).map_err(|e| e.to_string())?;
    }
    let next = from + buf.len() as u64;
    Ok((String::from_utf8_lossy(&buf).into_owned(), next, total))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 새 완료 경계가 생겼으면 포크 자료를 만든다. 아직 완료된 턴이 없거나 경계가 그대로면 None.
fn checkpoint(binding: &Binding) -> Result<Option<api::ForkCheckpoint>, String> {
    let exported = match native::export_native_snapshot(&binding.harness, &binding.transcript, &binding.native,
        native::ForkBoundary::LatestComplete, &native::ExportLimits::default())
    {
        Ok(exported) => exported,
        Err(native::ForkError::Incomplete(_)) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if exported.boundary_end > binding.cursor { return Err("새 완료 지점의 대화를 먼저 동기화하고 있어요".into()); }
    if binding.boundary_id.as_deref() == Some(exported.snapshot.boundary_id.as_str()) {
        return Ok(None);
    }
    let mut head = Vec::new();
    std::fs::File::open(&binding.transcript).map_err(|e| e.to_string())?
        .take(exported.boundary_end).read_to_end(&mut head).map_err(|e| e.to_string())?;
    let display_raw = native::display_lines(&binding.harness, &String::from_utf8_lossy(&head));
    if display_raw.len() > MAX_DISPLAY {
        return Err(format!("화면용 대화가 {}바이트로 한도 {}바이트를 넘어 포크 자료를 올리지 못했어요", display_raw.len(), MAX_DISPLAY));
    }
    let mut omissions = exported.omissions;
    let project = if binding.include_project {
        let repo = PathBuf::from(&binding.cwd);
        let captured = kasa_remote::shared_fork_project::capture_project(&repo, &[], exported.boundary_at_ms, &Default::default())
            .map_err(|e| format!("프로젝트 자료를 만들지 못했어요: {e}"))?;
        omissions.extend(captured.omissions.into_iter().map(|o| format!("프로젝트: {o}")));
        if !captured.newer_than_boundary.is_empty() {
            omissions.push(format!("프로젝트: 대화 경계 뒤에 고친 파일 {}개", captured.newer_than_boundary.len()));
        }
        Some(captured.snapshot)
    } else {
        None
    };
    omissions.retain(|o| !o.is_empty());
    for o in omissions.iter_mut() {
        if o.chars().count() > 256 {
            *o = o.chars().take(255).collect::<String>() + "…";
        }
    }
    if omissions.len() > MAX_OMISSIONS {
        let rest = omissions.len() - (MAX_OMISSIONS - 1);
        omissions.truncate(MAX_OMISSIONS - 1);
        omissions.push(format!("그 밖의 뺀 것 {rest}종류"));
    }
    Ok(Some(api::ForkCheckpoint {
        format_version: api::FORMAT_VERSION, native: exported.snapshot, captured_at_ms: now_ms(),
        display_raw, project, omissions,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kasa-shared-publisher-{tag}-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_only_complete_lines_and_long_lines_in_full() {
        let dir = temp("lines");
        let path = dir.join("t.jsonl");
        let long = "x".repeat((READ_CHUNK as usize) + 10);
        std::fs::write(&path, format!("{{\"a\":1}}\n{{\"b\":\"{long}\"}}\npartial")).unwrap();
        let (raw, next, total) = read_lines(&path, 0).unwrap();
        assert_eq!(raw, "{\"a\":1}\n", "첫 덩어리는 완성된 첫 줄까지");
        let (raw2, next2, _) = read_lines(&path, next).unwrap();
        assert!(raw2.ends_with("\"}\n") && raw2.len() > READ_CHUNK as usize, "긴 줄은 끝까지 더 읽는다");
        let (rest, next3, _) = read_lines(&path, next2).unwrap();
        assert_eq!((rest.as_str(), next3), ("", next2), "쓰다 만 줄은 남긴다");
        assert!(total > next3);
        let (_, shrunk, _) = read_lines(&path, total + 5).unwrap();
        assert_eq!(shrunk, 0, "원본이 줄면 0 을 알린다");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn removed_sessions_are_never_recreated_and_conflicts_reset() {
        let mut binding = Binding {
            room_id: "r".into(), session_id: "s".into(), source_key: "claude:x".into(), harness: "claude".into(), native: "x".into(),
            transcript: PathBuf::new(), surface_key: "k".into(), title: "t".into(), cwd: String::new(), include_project: false,
            generation: "g".into(), revision: 1, cursor: 10, reset: false, boundary_id: None, checkpoint_at_ms: 0,
            checkpoint_cursor: 0, checkpoint_error: None, state: BindingState::Live,
        };
        assert_eq!(settle(&mut binding, &ClientError { status: 409, code: "shared_conflict".into() }), None);
        assert!(binding.reset);
        assert_eq!(settle(&mut binding, &ClientError { status: 404, code: "shared_missing".into() }), None);
        assert_eq!(binding.state, BindingState::Removed);
        assert!(settle(&mut binding, &ClientError { status: 503, code: "shared_storage".into() }).is_some());
        let mut publisher = Publisher::default();
        publisher.bindings.push(binding.clone());
        assert!(publisher.next_due().is_none(), "지운 대화는 다시 보내지 않는다");
    }
}
