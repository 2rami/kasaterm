use super::*;
use crate::session_transfer::{
    MachineSnapshot, RoomTarget, SessionIdentity, SessionRow, TransferRequest, TransferStatus,
};
use kasa_socket::backend::Backend;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

/// 이보다 오래된 원격 관측이면 도착 방은 놓은 뒤 다시 읽는다.
const FRESH: Duration = Duration::from_secs(5);
/// 누를 때 보낸 출발 조회를 놓은 뒤 기다리는 한도. 조회 자체가 5초에 끊긴다.
const PIN_WAIT: Duration = Duration::from_secs(8);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DragEndpoint {
    LocalPane(String),
    RemotePane {
        label: String,
        base: String,
        pane: String,
    },
}

impl DragEndpoint {
    fn origin_key(&self) -> String {
        match self {
            Self::LocalPane(_) => "local".into(),
            Self::RemotePane { base, .. } => format!("remote:{base}"),
        }
    }

    fn pane(&self) -> &str {
        match self {
            Self::LocalPane(pane) | Self::RemotePane { pane, .. } => pane,
        }
    }

    /// 관측 캐시의 열쇠. 같은 기계를 거울과 사이드바가 다른 이름으로 불러도 한 자리다.
    fn seat(&self) -> (String, String) {
        (self.origin_key(), self.pane().to_string())
    }

    fn with_pane(&self, pane: &str) -> Self {
        match self {
            Self::LocalPane(_) => Self::LocalPane(pane.into()),
            Self::RemotePane { label, base, .. } => Self::RemotePane {
                label: label.clone(),
                base: base.clone(),
                pane: pane.into(),
            },
        }
    }

    /// 사이드바 기기 머리의 라벨. 이 기기는 빈 문자열이다.
    fn host_label(&self) -> &str {
        match self {
            Self::LocalPane(_) => "",
            Self::RemotePane { label, .. } => label,
        }
    }

    fn snapshot(&self, backend: &Arc<dyn Backend>) -> anyhow::Result<MachineSnapshot> {
        match self {
            Self::LocalPane(_) => backend.transfer_snapshot(),
            Self::RemotePane { label, base, .. } => {
                let current = kasa_mcp::machines::find(label)
                    .ok_or_else(|| anyhow::anyhow!("기계 연결이 없어졌어요. 다시 골라 주세요"))?;
                if current.base != *base {
                    anyhow::bail!("기계 연결이 바뀌었어요. 다시 골라 주세요");
                }
                let snapshot = kasa_mcp::remote::transfer_snapshot(base)?;
                if current
                    .machine_id
                    .as_ref()
                    .is_some_and(|id| id != &snapshot.machine_id)
                {
                    anyhow::bail!("기계 정체가 바뀌었어요. 다시 골라 주세요");
                }
                Ok(snapshot)
            }
        }
    }
}

fn endpoint_row<'a>(snapshot: &'a MachineSnapshot, pane: &str) -> anyhow::Result<&'a SessionRow> {
    let mut rows = snapshot.sessions.iter().filter(|row| {
        row.identity.machine_id == snapshot.machine_id && row.identity.pane_id == pane
    });
    let row = rows
        .next()
        .ok_or_else(|| anyhow::anyhow!("선택한 창이 바뀌거나 종료됐어요"))?;
    if rows.next().is_some() || row.identity.token.is_empty() || row.identity.instance.is_empty() {
        anyhow::bail!("선택한 창의 현재 정체를 확인하지 못했어요");
    }
    Ok(row)
}

/// 한 순간 본 세션. 이사 판단은 놓은 뒤 다시 읽지 않고 이것으로 한다 — 그 뒤 바뀐 것은
/// 엔진이 현재 신원과 맞춰 보며 거절한다.
#[derive(Clone, Debug)]
struct Seen {
    identity: SessionIdentity,
    room: Option<String>,
    room_title: Option<String>,
    harness: Option<String>,
    status: String,
    supported: bool,
    /// 그 기기가 실어 준 칸 열쇠. 칸 번호가 다시 쓰여도 열쇠는 새로 난다. 옛 판은 없다.
    surface_key: Option<String>,
    at: Instant,
}

/// 옛 판 구조체에도 컴파일되게 직렬화한 값에서 읽는다.
fn row_surface_key(row: &SessionRow) -> Option<String> {
    serde_json::to_value(row)
        .ok()?
        .get("surface_key")?
        .as_str()
        .filter(|key| !key.is_empty())
        .map(str::to_string)
}

impl Seen {
    fn of(snapshot: &MachineSnapshot, row: &SessionRow, at: Instant) -> Self {
        let room_title = row
            .room_id
            .as_ref()
            .and_then(|id| snapshot.rooms.iter().find(|room| &room.id == id))
            .map(|room| room.title.clone());
        Self {
            identity: row.identity.clone(),
            room: row.room_id.clone(),
            room_title,
            harness: row.harness.clone(),
            status: row.status.clone(),
            supported: snapshot.room_transfer_supported,
            surface_key: row_surface_key(row),
            at,
        }
    }
}

#[derive(Clone, Debug)]
struct Host {
    machine_id: String,
    supported: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Landing {
    Room(String),
    NewRoom,
}

/// 놓은 자리가 가리키는 도착 기계와 방.
#[derive(Clone, Debug)]
struct Arrival {
    machine_id: String,
    supported: bool,
    landing: Landing,
}

impl Arrival {
    fn at_anchor(seen: &Seen) -> Option<Self> {
        Some(Self {
            machine_id: seen.identity.machine_id.clone(),
            supported: seen.supported,
            landing: Landing::Room(seen.room.clone()?),
        })
    }

    fn new_room(host: &Host) -> Self {
        Self {
            machine_id: host.machine_id.clone(),
            supported: host.supported,
            landing: Landing::NewRoom,
        }
    }
}

fn arrival_at(snapshot: &MachineSnapshot, anchor: &str) -> anyhow::Result<Arrival> {
    let row = endpoint_row(snapshot, anchor)?;
    let room = row
        .room_id
        .as_ref()
        .filter(|id| snapshot.rooms.iter().any(|room| &room.id == *id))
        .ok_or_else(|| anyhow::anyhow!("도착 방이 바뀌었어요. 다시 골라 주세요"))?;
    Ok(Arrival {
        machine_id: snapshot.machine_id.clone(),
        supported: snapshot.room_transfer_supported,
        landing: Landing::Room(room.clone()),
    })
}

/// 놓는 순간 정한 도착지. 고정하지 못한 것만 일꾼 스레드가 읽는다.
enum Destination {
    Fixed(Arrival),
    Room(DragEndpoint),
    Machine(DragEndpoint),
    Broken(String),
}

impl Destination {
    fn resolve(self, backend: &Arc<dyn Backend>) -> anyhow::Result<Arrival> {
        match self {
            Self::Fixed(arrival) => Ok(arrival),
            Self::Broken(reason) => anyhow::bail!(reason),
            Self::Room(anchor) => arrival_at(&anchor.snapshot(backend)?, anchor.pane()),
            Self::Machine(host) => {
                let snapshot = host.snapshot(backend)?;
                Ok(Arrival::new_room(&Host {
                    machine_id: snapshot.machine_id,
                    supported: snapshot.room_transfer_supported,
                }))
            }
        }
    }
}

/// 새 방 이름은 출발 방을 잇는다. 「3번 방」 같은 자리 번호는 도착 기계에선 다른 방을 가리킨다.
fn new_room_title(source: &Seen) -> String {
    let numbered = |title: &str| {
        title
            .strip_suffix("번 방")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    source
        .room_title
        .as_deref()
        .map(str::trim)
        .filter(|title| {
            !title.is_empty()
                && title.chars().count() <= 80
                && !title.chars().any(char::is_control)
                && !numbered(title)
        })
        .unwrap_or("이사 온 방")
        .to_string()
}

fn request_from_pins(source: &Seen, arrival: &Arrival) -> anyhow::Result<TransferRequest> {
    if source.identity.machine_id == arrival.machine_id {
        anyhow::bail!("같은 기계 안에서는 방 이동을 이용해 주세요");
    }
    if !source.supported || !arrival.supported {
        anyhow::bail!("두 기계 모두 도착 방 선택을 지원하는 새 판이 필요해요");
    }
    if source.status == "unknown" {
        anyhow::bail!("출발 세션 상태를 확인할 수 없어 이사를 시작하지 않았어요");
    }
    match source.harness.as_deref() {
        Some("claude" | "codex") => {}
        Some(harness) => anyhow::bail!("이사는 Claude·Codex 대화만 지원해요 ({harness})"),
        None => anyhow::bail!("빈 셸은 기기 간 대화 이사 대상이 아니에요"),
    }
    if source
        .identity
        .session_id
        .as_ref()
        .is_none_or(|id| id.is_empty())
    {
        anyhow::bail!("출발 대화의 세션 번호를 확인하지 못했어요. 잠시 뒤 다시 끌어 주세요");
    }
    let destination_room = match &arrival.landing {
        Landing::Room(id) => RoomTarget::Existing(id.clone()),
        Landing::NewRoom => RoomTarget::New(new_room_title(source)),
    };
    Ok(TransferRequest {
        sessions: vec![source.identity.clone()],
        destination_machine: arrival.machine_id.clone(),
        destination_room,
        confirmed: true,
    })
}

/// 끌기 시작 때 고정한 출발 신원. 원격은 아직 관측이 없으면 누른 순간 보낸 조회를 기다린다.
enum Pin {
    Ready(Seen),
    /// 누를 때 화면에 보이던 칸 열쇠. 늦게 받은 신원은 이 열쇠의 칸일 때만 받는다.
    Awaiting { screen: String },
    Failed(String),
}

const SCREEN_UNKNOWN: &str =
    "끌기 시작한 칸을 목록에서 확인하지 못했어요. 목록을 새로 고친 뒤 다시 끌어 주세요";
const SCREEN_CHANGED: &str =
    "끌기 시작한 뒤 그 자리에 다른 칸이 들어섰어요. 목록을 새로 고친 뒤 다시 끌어 주세요";
const NEEDS_UPDATE: &str = "그 기기를 새 판으로 올려야 끌어서 이사할 수 있어요";

fn confirm_screen(seen: &Seen, screen: &str) -> Pin {
    match seen.surface_key.as_deref() {
        Some(key) if key == screen => Pin::Ready(seen.clone()),
        Some(_) => Pin::Failed(SCREEN_CHANGED.into()),
        None => Pin::Failed(NEEDS_UPDATE.into()),
    }
}

/// 화면이 아는 열쇠가 하나로 모일 때만 그 칸이다. 거울과 목록이 서로 다르면 고르지 않는다.
fn single_screen_key(keys: impl Iterator<Item = String>) -> Option<String> {
    let keys: HashSet<String> = keys
        .filter(|key| !key.is_empty() && !key.starts_with("legacy:"))
        .collect();
    if keys.len() == 1 {
        keys.into_iter().next()
    } else {
        None
    }
}

#[derive(Default)]
struct Mailbox {
    pending: HashSet<DragEndpoint>,
    warming: HashSet<String>,
    refreshed: HashMap<String, Instant>,
    observed: HashMap<(String, String), Seen>,
    hosts: HashMap<String, Host>,
    pins: HashMap<u64, (DragEndpoint, Pin)>,
    /// 아직 놓지 않은 끌기의 고정 번호. 놓으면 일꾼이 가져간다.
    drag: Option<u64>,
    generation: u64,
    /// 도는 이사 — (고정 번호, 도착 기기 라벨, 단계). 사이드바 기기 머리가 읽는다.
    moves: Vec<(u64, String, String)>,
    moves_changed: bool,
    messages: Vec<String>,
    refreshed_moves: Vec<Instant>,
    /// 같은 보기 창 안에서 옮긴 거울 칸의 미리보기 — 원본이 답할 때까지 그대로 둔다.
    previews: HashMap<DragEndpoint, HeldPreview>,
    /// 끝난 원격 자리 이동. GUI 가 미리보기를 거두거나 거울 칸을 옮겨 앉힌다.
    outcomes: Vec<Outcome>,
}

struct HeldPreview {
    window: usize,
    original: kasa_pty::PtyLayout,
    preview: kasa_pty::PtyLayout,
}

struct Outcome {
    source: DragEndpoint,
    ok: bool,
    landed: Option<Landed>,
}

/// 원본 스냅샷으로 다른 방에 도착한 것까지 확인한 이동. 열쇠는 누를 때 고정한 surface_key 다 —
/// 번호만 맞추면 그 사이 재사용된 칸을 옮긴다.
#[derive(Clone, Debug)]
pub(crate) struct Landed {
    pub(crate) base: String,
    pub(crate) source: String,
    pub(crate) source_key: String,
    pub(crate) target: String,
    pub(crate) target_key: Option<String>,
    pub(crate) zone: DropZone,
}

/// 같은 보기 창 안 가장자리 이동의 미리보기를 원본 답까지 맡긴다. 이동이 출발하지 않았으면
/// false — 부른 쪽이 바로 되돌린다.
pub(crate) fn hold_seat_preview(
    source: &DragEndpoint,
    window: usize,
    original: kasa_pty::PtyLayout,
    preview: kasa_pty::PtyLayout,
) -> bool {
    let mut state = mailbox().lock().unwrap();
    let started = state.pending.contains(source) || state.outcomes.iter().any(|o| &o.source == source);
    if started {
        state.previews.insert(source.clone(), HeldPreview { window, original, preview });
    }
    started
}

fn confirmed_move_is_latest(pending: Option<Instant>, refreshed: &[Instant]) -> bool {
    pending.is_some_and(|at| refreshed.contains(&at))
}

fn verify_drag_identity(
    snapshot: &MachineSnapshot,
    pane: &str,
    expected: &SessionIdentity,
) -> anyhow::Result<()> {
    if endpoint_row(snapshot, pane)?.identity != *expected {
        anyhow::bail!("끌기를 시작한 뒤 세션이 바뀌었어요. 새 상태를 확인하고 다시 끌어 주세요");
    }
    Ok(())
}

/// 그 칸이 든 방의 다른 칸들. 방 번호가 없으면 None.
fn roommates(snapshot: &MachineSnapshot, pane: &str) -> Option<std::collections::BTreeSet<String>> {
    let room = endpoint_row(snapshot, pane).ok()?.room_id.clone()?;
    Some(snapshot.sessions.iter()
        .filter(|row| row.identity.machine_id == snapshot.machine_id && row.room_id.as_deref() == Some(room.as_str()))
        .map(|row| row.identity.pane_id.clone())
        .filter(|id| id != pane)
        .collect())
}

/// 다른 방 자리 이동이 원본에 앉았나. 방 번호는 그 방 첫 칸에서 나와 첫 칸이 바뀌면 같이 바뀌므로
/// 번호를 견주지 않고 구성으로 본다 — 가장자리면 두 칸이 한 방에, 가운데(맞바꾸기)면 서로의 옛
/// 방 식구와. 둘 다 외톨이 방이면 맞바꿔도 스냅샷이 같아 None.
fn seat_move_landed(
    zone: DropZone,
    before: &MachineSnapshot,
    now: &MachineSnapshot,
    source: &str,
    target: &str,
) -> Option<bool> {
    if zone != DropZone::Center {
        let room = |pane| endpoint_row(now, pane).ok().and_then(|row| row.room_id.clone());
        return Some(room(source).is_some() && room(source) == room(target));
    }
    let (source_before, target_before) = (roommates(before, source)?, roommates(before, target)?);
    if source_before.is_empty() && target_before.is_empty() {
        return None;
    }
    Some(roommates(now, source)? == target_before && roommates(now, target)? == source_before)
}

/// 원본 배치 채널에서 그 PTY 가 앉은 방(원본 창 번호).
fn feed_room_of(rooms: &[kasa_mcp::layout_feed::Room], pty: &str) -> Option<usize> {
    rooms.iter()
        .find(|room| room.tree.leaves().contains(&pty) || room.members.values().any(|ids| ids.iter().any(|id| id == pty)))
        .map(|room| room.window)
}

fn mailbox() -> &'static Mutex<Mailbox> {
    static MAILBOX: OnceLock<Mutex<Mailbox>> = OnceLock::new();
    MAILBOX.get_or_init(|| Mutex::new(Mailbox::default()))
}

fn pin_landed() -> &'static Condvar {
    static LANDED: OnceLock<Condvar> = OnceLock::new();
    LANDED.get_or_init(Condvar::new)
}

/// 조회 한 번을 관측 캐시에 싣고, 그 기계 쪽 출발 신원을 기다리던 끌기를 푼다.
fn observe(
    state: &mut Mailbox,
    endpoint: &DragEndpoint,
    snapshot: anyhow::Result<MachineSnapshot>,
    started: Instant,
) {
    let key = endpoint.origin_key();
    let Mailbox { observed, hosts, pins, .. } = state;
    observed.retain(|(origin, _), _| *origin != key);
    let failure = match &snapshot {
        Ok(snapshot) => {
            hosts.insert(
                key.clone(),
                Host {
                    machine_id: snapshot.machine_id.clone(),
                    supported: snapshot.room_transfer_supported,
                },
            );
            for row in &snapshot.sessions {
                if row.identity.machine_id != snapshot.machine_id
                    || row.status == "unknown"
                    || row.unavailable_reason.is_some()
                    || endpoint_row(snapshot, &row.identity.pane_id).is_err()
                {
                    continue;
                }
                observed.insert(
                    endpoint.with_pane(&row.identity.pane_id).seat(),
                    Seen::of(snapshot, row, started),
                );
            }
            None
        }
        Err(error) => Some(format!("출발 세션을 확인하지 못했어요: {error:#}")),
    };
    for (pinned, pin) in pins.values_mut() {
        let Pin::Awaiting { screen } = pin else {
            continue;
        };
        if pinned.origin_key() != key {
            continue;
        }
        let settled = match (&failure, observed.get(&pinned.seat())) {
            (Some(reason), _) => Pin::Failed(reason.clone()),
            (None, Some(seen)) => confirm_screen(seen, screen),
            (None, None) => Pin::Failed(
                "끌기 시작한 창의 세션을 확인하지 못했어요. 상태를 확인하고 다시 끌어 주세요".into(),
            ),
        };
        *pin = settled;
    }
}

fn wait_pin(generation: u64) -> anyhow::Result<Seen> {
    let state = mailbox().lock().unwrap();
    let (mut state, _) = pin_landed()
        .wait_timeout_while(state, PIN_WAIT, |state| {
            matches!(state.pins.get(&generation), Some((_, Pin::Awaiting { .. })))
        })
        .unwrap();
    match state.pins.remove(&generation) {
        Some((_, Pin::Ready(seen))) => Ok(seen),
        Some((_, Pin::Failed(reason))) => anyhow::bail!(reason),
        Some((_, Pin::Awaiting { .. })) => {
            anyhow::bail!("끌기 시작한 창의 세션을 제때 확인하지 못했어요. 다시 끌어 주세요")
        }
        None => anyhow::bail!("끌기 정보가 사라졌어요. 다시 끌어 주세요"),
    }
}

fn drop_pin(ticket: Option<u64>) {
    if let Some(ticket) = ticket {
        mailbox().lock().unwrap().pins.remove(&ticket);
    }
}

fn set_stage(id: u64, stage: &str) {
    let mut state = mailbox().lock().unwrap();
    if let Some(entry) = state.moves.iter_mut().find(|(at, _, _)| *at == id) {
        entry.2 = stage.into();
        state.moves_changed = true;
    }
}

/// 그 기기로 가는 이사의 지금 단계. 놓은 순간부터 엔진이 끝낼 때까지 기기 머리에 선다.
pub(crate) fn arriving(label: &str) -> Option<String> {
    let state = mailbox().lock().ok()?;
    state
        .moves
        .iter()
        .rev()
        .find(|(_, toward, _)| toward == label)
        .map(|(_, _, stage)| format!("이사 받는 중 · {stage}"))
}

fn remote_drop_command(
    source: &str,
    target: &str,
    zone: DropZone,
) -> (&'static str, serde_json::Value) {
    let direction = match zone {
        DropZone::Center => {
            return (
                "surface.swap",
                serde_json::json!({ "a": source, "b": target }),
            )
        }
        DropZone::Left => "left",
        DropZone::Right => "right",
        DropZone::Up => "up",
        DropZone::Down => "down",
    };
    (
        "surface.move",
        serde_json::json!({ "surface_id": source, "target": target, "direction": direction }),
    )
}

impl App {
    pub(crate) fn warm_drag_identity(&self, endpoint: DragEndpoint) {
        self.refresh_origin(endpoint, false);
    }

    fn refresh_origin(&self, endpoint: DragEndpoint, force: bool) {
        let Some(backend) = self.socket_backend.clone() else {
            return;
        };
        let key = endpoint.origin_key();
        {
            let mut state = mailbox().lock().unwrap();
            let recent = state
                .refreshed
                .get(&key)
                .is_some_and(|at| at.elapsed() < Duration::from_secs(2));
            // 이미 도는 조회가 끝나면 그 결과로 기다리는 끌기를 푼다 — 겹쳐 보내지 않는다.
            if (recent && !force) || !state.warming.insert(key.clone()) {
                return;
            }
            state.refreshed.insert(key.clone(), Instant::now());
        }
        let backend: Arc<dyn Backend> = backend;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let snapshot = endpoint.snapshot(&backend);
            let mut state = mailbox().lock().unwrap();
            observe(&mut state, &endpoint, snapshot, started);
            state.warming.remove(&key);
            drop(state);
            pin_landed().notify_all();
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    /// 도착 후보 기기를 미리 읽어 둔다. 놓는 순간 도착 방을 재조회 없이 고정하려는 것이다.
    fn warm_hosts(&self) {
        for machine in self.info.machines_col.machines.iter().filter(|m| m.online) {
            let Some(peer) = kasa_mcp::machines::find(&machine.label) else {
                continue;
            };
            self.warm_drag_identity(DragEndpoint::RemotePane {
                label: machine.label.clone(),
                base: peer.base,
                pane: String::new(),
            });
        }
    }

    /// 이 기기 칸은 앱이 쥔 상태에서 바로 읽는다 — 조회가 내놓는 것과 같은 신원이다.
    fn local_seen(&self, pane: &str) -> anyhow::Result<Seen> {
        let machine = crate::transfer_endpoints::machine_context()?.clone();
        let snapshot = self.transfer_snapshot_gui(&machine);
        let row = endpoint_row(&snapshot, pane)?;
        let mut seen = Seen::of(&snapshot, row, Instant::now());
        seen.harness = self
            .pty
            .get(pane)
            .and_then(|session| session.active_agent())
            .map(|agent| agent.as_str().to_string());
        Ok(seen)
    }

    /// 누를 때 화면에 보이던 그 칸의 열쇠 — 거울 연결이 확인한 것과 기기 목록에 실린 것.
    fn screen_key(&self, label: &str, base: &str, pane: &str) -> Option<String> {
        let mirrored = self.pty.keys().filter_map(|id| {
            let info = kasa_mcp::remote::remote_info(id)?;
            (info.remote_id == pane && kasa_mcp::machines::same_machine_bases(&info.base, base))
                .then(|| kasa_mcp::remote::remote_surface_key(id))
                .flatten()
        });
        let listed = kasa_mcp::machines::cached_pane(label, pane)
            .and_then(|row| row.get("surface_key")?.as_str().map(str::to_string));
        single_screen_key(mirrored.chain(listed))
    }

    fn local_host(&self) -> anyhow::Result<Host> {
        let machine = crate::transfer_endpoints::machine_context()?.clone();
        let snapshot = self.transfer_snapshot_gui(&machine);
        Ok(Host {
            machine_id: snapshot.machine_id,
            supported: snapshot.room_transfer_supported,
        })
    }

    fn insert_pin(&self, endpoint: DragEndpoint, pin: Pin, current: bool) -> u64 {
        let mut state = mailbox().lock().unwrap();
        if current {
            if let Some(old) = state.drag.take() {
                state.pins.remove(&old);
            }
        }
        state.generation += 1;
        let generation = state.generation;
        state.pins.insert(generation, (endpoint, pin));
        if current {
            state.drag = Some(generation);
        }
        generation
    }

    pub(crate) fn begin_drag_identity(&mut self, endpoint: DragEndpoint) -> bool {
        let pin = match &endpoint {
            DragEndpoint::LocalPane(pane) => match self.local_seen(pane) {
                Ok(seen) => Pin::Ready(seen),
                Err(error) => Pin::Failed(format!("{error:#}")),
            },
            DragEndpoint::RemotePane { label, base, pane } => match self.screen_key(label, base, pane) {
                None => {
                    kasa_mcp::machines::poke();
                    Pin::Failed(SCREEN_UNKNOWN.into())
                }
                // 관측이 화면의 칸이면 그 신원을 쓴다. 그 뒤 세션이 바뀌었으면 엔진의 신원 대조가 거절한다.
                Some(screen) => match mailbox().lock().unwrap().observed.get(&endpoint.seat()).cloned() {
                    Some(seen) if seen.surface_key.as_deref() == Some(screen.as_str()) => Pin::Ready(seen),
                    Some(seen) if seen.surface_key.is_none() => Pin::Failed(NEEDS_UPDATE.into()),
                    _ => Pin::Awaiting { screen },
                },
            },
        };
        let ready = matches!(pin, Pin::Ready(_));
        let awaiting = matches!(pin, Pin::Awaiting { .. });
        self.insert_pin(endpoint.clone(), pin, true);
        if awaiting {
            self.refresh_origin(endpoint, true);
        }
        self.warm_hosts();
        ready
    }

    /// 놓은 끌기의 고정 번호를 가져간다. 다른 칸을 눌렀던 것이면 버린다.
    fn claim_pin(&self, source: &DragEndpoint) -> Option<u64> {
        let mut state = mailbox().lock().unwrap();
        let generation = state.drag.take()?;
        if state.pins.get(&generation).is_some_and(|(pinned, _)| pinned == source) {
            return Some(generation);
        }
        state.pins.remove(&generation);
        None
    }

    /// 끌기 없이 들어온 이동 — 이 기기 칸은 지금 상태가 곧 시작 상태다.
    fn pin_now(&mut self, source: &DragEndpoint) -> Option<u64> {
        if let DragEndpoint::LocalPane(pane) = source {
            let pin = match self.local_seen(pane) {
                Ok(seen) => Pin::Ready(seen),
                Err(error) => Pin::Failed(format!("{error:#}")),
            };
            return Some(self.insert_pin(source.clone(), pin, false));
        }
        self.warm_drag_identity(source.clone());
        self.set_toast("끌기 시작 시점의 세션을 확인하지 못했어요. 잠시 뒤 다시 끌어 주세요".into());
        None
    }

    fn destination_for(&self, destination: &DragEndpoint) -> Destination {
        match destination {
            DragEndpoint::LocalPane(anchor) => match self.local_seen(anchor) {
                Ok(seen) => Arrival::at_anchor(&seen).map_or_else(
                    || Destination::Broken("도착 방을 확인하지 못했어요. 다시 골라 주세요".into()),
                    Destination::Fixed,
                ),
                Err(error) => Destination::Broken(format!("{error:#}")),
            },
            DragEndpoint::RemotePane { .. } => mailbox()
                .lock()
                .unwrap()
                .observed
                .get(&destination.seat())
                .filter(|seen| seen.at.elapsed() <= FRESH)
                .and_then(Arrival::at_anchor)
                .map_or_else(|| Destination::Room(destination.clone()), Destination::Fixed),
        }
    }

    pub(crate) fn route_drag_move(
        &mut self,
        source: DragEndpoint,
        destination: DragEndpoint,
        zone: DropZone,
    ) -> bool {
        let ticket = self.claim_pin(&source);
        if matches!(
            (&source, &destination),
            (DragEndpoint::LocalPane(_), DragEndpoint::LocalPane(_))
        ) {
            drop_pin(ticket);
            return false;
        }
        let Some(ticket) = ticket.or_else(|| self.pin_now(&source)) else {
            return true;
        };
        match (&source, &destination) {
            (
                DragEndpoint::RemotePane {
                    base: source_base, ..
                },
                DragEndpoint::RemotePane {
                    base: target_base, ..
                },
            ) if source_base == target_base => {
                self.queue_remote_seat_move(source, destination, zone, ticket);
            }
            _ => {
                let toward = destination.host_label().to_string();
                let arrival = self.destination_for(&destination);
                self.queue_drag_transfer(source, ticket, arrival, toward);
            }
        }
        true
    }

    /// 기기 머리(방 밖)에 놓으면 그 기기에 새 방을 세우고 그리로 옮긴다.
    pub(crate) fn route_drag_to_machine(&mut self, source: DragEndpoint, label: &str) -> bool {
        let ticket = self.claim_pin(&source);
        let online = self
            .info
            .machines_col
            .machines
            .iter()
            .any(|m| m.label == label && m.online);
        let peer = kasa_mcp::machines::find(label).filter(|_| online);
        let Some(peer) = peer else {
            drop_pin(ticket);
            self.set_toast("그 기기에 연결되어 있지 않아요. 연결 상태를 확인해 주세요".into());
            return true;
        };
        if matches!(&source, DragEndpoint::RemotePane { base, .. } if *base == peer.base) {
            drop_pin(ticket);
            self.set_toast("같은 기기 안에서는 방 카드나 칸 위에 놓아 주세요".into());
            return true;
        }
        let Some(ticket) = ticket.or_else(|| self.pin_now(&source)) else {
            return true;
        };
        let host = DragEndpoint::RemotePane {
            label: label.into(),
            base: peer.base,
            pane: String::new(),
        };
        let cached = mailbox().lock().unwrap().hosts.get(&host.origin_key()).cloned();
        let arrival = cached.map_or_else(
            || Destination::Machine(host),
            |host| Destination::Fixed(Arrival::new_room(&host)),
        );
        self.queue_drag_transfer(source, ticket, arrival, label.into());
        true
    }

    /// 다른 기기 칸을 이 기기 머리에 놓으면 이 기기에 새 방을 세워 데려온다.
    pub(crate) fn route_drag_home(&mut self, source: DragEndpoint) -> bool {
        let ticket = self.claim_pin(&source);
        if matches!(source, DragEndpoint::LocalPane(_)) {
            drop_pin(ticket);
            return false;
        }
        let Some(ticket) = ticket.or_else(|| self.pin_now(&source)) else {
            return true;
        };
        let arrival = match self.local_host() {
            Ok(host) => Destination::Fixed(Arrival::new_room(&host)),
            Err(error) => Destination::Broken(format!("{error:#}")),
        };
        self.queue_drag_transfer(source, ticket, arrival, String::new());
        true
    }

    pub(crate) fn drag_endpoint(&self, leaf: &str) -> DragEndpoint {
        let pane = self.leaf_pty_id(leaf);
        self.drag_endpoint_for_pid(&pane)
    }

    pub(crate) fn drag_endpoint_for_pid(&self, pane: &str) -> DragEndpoint {
        match kasa_mcp::remote::remote_info(pane) {
            Some(info) => DragEndpoint::RemotePane {
                label: Some(info.label)
                    .filter(|label| !label.is_empty())
                    .or_else(|| kasa_mcp::machines::label_for_base(&info.base))
                    .unwrap_or_default(),
                base: info.base,
                pane: info.remote_id,
            },
            None => DragEndpoint::LocalPane(pane.to_string()),
        }
    }

    /// 놓은 순간 진행을 세운다. 성공은 엔진이 도착을 확인한 뒤에만 말한다.
    fn start_move(&mut self, source: &DragEndpoint, ticket: u64, toward: Option<String>, toast: &str) -> bool {
        let mut state = mailbox().lock().unwrap();
        if !state.pending.insert(source.clone()) {
            state.pins.remove(&ticket);
            drop(state);
            self.set_toast("이 창의 이사 결과를 확인하고 있어요".into());
            return false;
        }
        if let Some(toward) = toward {
            state.moves.push((ticket, toward, "출발 세션 확인".into()));
            state.moves_changed = true;
        }
        drop(state);
        self.set_toast(toast.into());
        self.chrome_dirty = true;
        true
    }

    fn queue_remote_seat_move(
        &mut self,
        source: DragEndpoint,
        destination: DragEndpoint,
        zone: DropZone,
        ticket: u64,
    ) {
        let Some(backend) = self.socket_backend.clone() else {
            drop_pin(Some(ticket));
            self.set_toast("이 백엔드에서는 원격 방 이동을 지원하지 않아요".into());
            return;
        };
        if source.pane() == destination.pane() {
            drop_pin(Some(ticket));
            return;
        }
        if !self.start_move(&source, ticket, None, "원격 방으로 창을 옮기고 있어요") {
            return;
        }
        let target_seen = mailbox()
            .lock()
            .unwrap()
            .observed
            .get(&destination.seat())
            .filter(|seen| seen.at.elapsed() <= FRESH)
            .cloned();
        let backend: Arc<dyn Backend> = backend;
        let proxy = self.proxy.clone();
        let move_started = Instant::now();
        self.remote_view_push_at = Some(move_started);
        std::thread::spawn(move || {
            let mut submitted = false;
            let mut landed = None;
            let result = (|| -> anyhow::Result<bool> {
                let expected = wait_pin(ticket)?;
                let origin = source.snapshot(&backend)?;
                verify_drag_identity(&origin, source.pane(), &expected.identity)?;
                let source_row = endpoint_row(&origin, source.pane())?;
                let target_row = endpoint_row(&origin, destination.pane())?;
                // Both endpoints are on one host; one snapshot validates a coherent pair.
                let target_room = match &target_seen {
                    Some(seen) => {
                        verify_drag_identity(&origin, destination.pane(), &seen.identity)?;
                        if target_row.room_id != seen.room {
                            anyhow::bail!("도착 창의 방이 바뀌었어요. 다시 끌어 주세요");
                        }
                        seen.room.clone()
                    }
                    None => target_row.room_id.clone(),
                };
                let cross_room = source_row.room_id != target_room;
                if cross_room && target_room.is_none() {
                    anyhow::bail!("도착 방을 확인하지 못했어요. 다시 골라 주세요");
                }
                let DragEndpoint::RemotePane { base, .. } = &source else {
                    unreachable!()
                };
                let (method, params) = remote_drop_command(
                    &source_row.identity.pane_id,
                    &target_row.identity.pane_id,
                    zone,
                );
                // 외톨이 두 칸 맞바꾸기는 스냅샷으로 못 가려 원본 배치 채널의 창 번호로 본다 — 보내기 전 자리.
                let feed_before = kasa_mcp::layout_watch::settled(base).and_then(|(_, rooms)| {
                    Some((feed_room_of(&rooms, source.pane())?, feed_room_of(&rooms, destination.pane())?))
                });
                submitted = true;
                kasa_mcp::remote::remote_cmd(base, method, params)?;
                if cross_room {
                    let deadline = Instant::now() + Duration::from_secs(10);
                    loop {
                        let current = source.snapshot(&backend)?;
                        verify_drag_identity(&current, source.pane(), &expected.identity)?;
                        let settled = seat_move_landed(zone, &origin, &current, source.pane(), destination.pane())
                            .or_else(|| {
                                let (from, to) = feed_before?;
                                let (_, rooms) = kasa_mcp::layout_watch::settled(base)?;
                                Some((feed_room_of(&rooms, source.pane()), feed_room_of(&rooms, destination.pane()))
                                    == (Some(to), Some(from)))
                            });
                        if settled == Some(true) {
                            landed = expected.surface_key.clone().map(|source_key| Landed {
                                base: base.clone(),
                                source: source.pane().to_string(),
                                source_key,
                                target: destination.pane().to_string(),
                                target_key: target_seen.as_ref().and_then(|seen| seen.surface_key.clone())
                                    .or_else(|| row_surface_key(target_row)),
                                zone,
                            });
                            return Ok(true);
                        }
                        if Instant::now() >= deadline {
                            anyhow::bail!("요청은 보냈지만 도착 방을 아직 확인하지 못했어요");
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
                Ok(false)
            })();
            let ok = result.is_ok();
            let unknown = submitted && !ok;
            let refreshed = result.is_ok()
                && match &source {
                    DragEndpoint::RemotePane { base, .. } => {
                        kasa_mcp::machines::refresh_panes_now(base).is_ok()
                    }
                    _ => false,
                };
            let message = match result {
                Ok(true) => "선택한 원격 방에 도착한 것을 확인했어요".into(),
                Ok(false) if refreshed => "원격 자리 배치를 갱신했어요".into(),
                Ok(false) => "자리 이동 요청을 보냈어요. 배치를 확인하고 있어요".into(),
                Err(error) => format!("창 이동 결과를 확인해 주세요: {error:#}"),
            };
            let mut state = mailbox().lock().unwrap();
            if refreshed {
                state.refreshed_moves.push(move_started);
            }
            state.outcomes.push(Outcome { source: source.clone(), ok, landed });
            state.messages.push(message);
            state.moves.retain(|(at, _, _)| *at != ticket);
            state.moves_changed = true;
            if !unknown {
                state.pending.remove(&source);
            }
            drop(state);
            kasa_mcp::machines::poke();
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    fn queue_drag_transfer(
        &mut self,
        source: DragEndpoint,
        ticket: u64,
        destination: Destination,
        toward: String,
    ) {
        let Some(backend) = self.socket_backend.clone() else {
            drop_pin(Some(ticket));
            self.set_toast("이 백엔드에서는 기기 간 이사를 지원하지 않아요".into());
            return;
        };
        if !self.start_move(&source, ticket, Some(toward), "이사를 시작했어요 · 출발 세션과 도착 방을 확인하고 있어요") {
            return;
        }
        let backend: Arc<dyn Backend> = backend;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let prepared = (|| -> anyhow::Result<TransferRequest> {
                let seen = wait_pin(ticket)?;
                set_stage(ticket, "도착 방 확인");
                let _ = proxy.send_event(UserEvent::Redraw);
                request_from_pins(&seen, &destination.resolve(&backend)?)
            })();
            let mut unknown = false;
            match prepared {
                Ok(request) => {
                    set_stage(ticket, "옮기는 중");
                    let _ = proxy.send_event(UserEvent::Redraw);
                    crate::session_transfer::execute_with_progress(&backend, request, |result| {
                        unknown |= result.status == TransferStatus::Unknown;
                        match result.status {
                            TransferStatus::Waiting => set_stage(ticket, "지금 작업이 끝나길 기다리는 중"),
                            TransferStatus::Running => set_stage(ticket, "옮기는 중"),
                            _ => {}
                        }
                        mailbox().lock().unwrap().messages.push(result.message);
                        let _ = proxy.send_event(UserEvent::Redraw);
                    });
                }
                Err(error) => mailbox()
                    .lock()
                    .unwrap()
                    .messages
                    .push(format!("{error:#}")),
            }
            let mut state = mailbox().lock().unwrap();
            state.moves.retain(|(at, _, _)| *at != ticket);
            state.moves_changed = true;
            // An indeterminate migration must not become a second migration on the next drop.
            if !unknown {
                state.pending.remove(&source);
            }
            drop(state);
            kasa_mcp::machines::poke();
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    pub(crate) fn drain_drag_transfers(&mut self) {
        let (messages, refreshed_moves, moved, outcomes) = {
            let mut state = mailbox().lock().unwrap();
            let outcomes: Vec<(Outcome, Option<HeldPreview>)> = std::mem::take(&mut state.outcomes)
                .into_iter()
                .map(|outcome| {
                    let preview = state.previews.remove(&outcome.source);
                    (outcome, preview)
                })
                .collect();
            (
                std::mem::take(&mut state.messages),
                std::mem::take(&mut state.refreshed_moves),
                std::mem::take(&mut state.moves_changed),
                outcomes,
            )
        };
        for (outcome, preview) in outcomes {
            if let Some(held) = preview.filter(|_| !outcome.ok) {
                self.roll_back_seat_preview(held.window, held.original, &held.preview);
            }
            if let Some(landed) = outcome.landed {
                self.land_confirmed_remote_move(&landed);
            }
        }
        // Only a post-command read for the latest move can release the rollback guard.
        if confirmed_move_is_latest(self.remote_view_push_at, &refreshed_moves) {
            self.remote_view_push_at = None;
            self.sync_remote_view_layouts();
        }
        if moved {
            self.chrome_dirty = true;
        }
        if let Some(message) = messages.into_iter().last() {
            self.set_toast(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_transfer::{RoomInfo, SessionIdentity};

    #[test]
    fn a_confirmed_move_releases_only_its_own_pending_guard() {
        let earlier = Instant::now();
        let latest = earlier + Duration::from_millis(1);
        assert!(confirmed_move_is_latest(Some(latest), &[latest]));
        assert!(!confirmed_move_is_latest(Some(latest), &[earlier]));
        assert!(!confirmed_move_is_latest(Some(latest), &[]));
        assert!(!confirmed_move_is_latest(None, &[latest]));
    }

    fn snapshot(machine: &str, room: &str) -> MachineSnapshot {
        MachineSnapshot {
            machine_id: machine.into(),
            room_transfer_supported: true,
            rooms: vec![RoomInfo {
                id: room.into(),
                title: "same title".into(),
            }],
            sessions: vec![SessionRow {
                identity: SessionIdentity {
                    machine_id: machine.into(),
                    pane_id: "%1".into(),
                    token: "token".into(),
                    instance: "instance".into(),
                    session_id: Some("conversation".into()),
                    ..Default::default()
                },
                status: "idle".into(),
                harness: Some("codex".into()),
                room_id: Some(room.into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn seen(snapshot: &MachineSnapshot, pane: &str) -> Seen {
        Seen::of(snapshot, endpoint_row(snapshot, pane).unwrap(), Instant::now())
    }

    fn request(
        source: &MachineSnapshot,
        destination: &MachineSnapshot,
    ) -> anyhow::Result<TransferRequest> {
        request_from_pins(&seen(source, "%1"), &arrival_at(destination, "%1")?)
    }

    fn remote(pane: &str) -> DragEndpoint {
        DragEndpoint::RemotePane {
            label: "host".into(),
            base: "http://host".into(),
            pane: pane.into(),
        }
    }

    /// `(칸, 방)` 들로 한 기계 스냅샷. 방 번호는 원본처럼 그 방 첫 칸에서 따온다.
    fn seated(rooms: &[&[&str]]) -> MachineSnapshot {
        let mut snap = snapshot("m", "unused");
        snap.rooms.clear();
        snap.sessions = rooms.iter().flat_map(|panes| {
            let room = format!("room:{}", panes[0]);
            panes.iter().map(move |pane| SessionRow {
                identity: SessionIdentity { machine_id: "m".into(), pane_id: (*pane).into(), token: format!("t{pane}"),
                    instance: "i".into(), ..Default::default() },
                status: "idle".into(),
                room_id: Some(room.clone()),
                ..Default::default()
            })
        }).collect();
        snap
    }

    #[test]
    fn cross_room_moves_are_judged_by_who_shares_a_room_not_by_room_numbers() {
        let before = seated(&[&["%0", "%1"], &["%2", "%3"]]);
        // %0 을 %2 앞(왼쪽)에 — 도착 방의 첫 칸이 바뀌어 방 번호가 room:%0 이 된다.
        let moved = seated(&[&["%1"], &["%0", "%2", "%3"]]);
        assert_eq!(seat_move_landed(DropZone::Left, &before, &moved, "%0", "%2"), Some(true));
        assert_eq!(seat_move_landed(DropZone::Left, &before, &before, "%0", "%2"), Some(false));
        // 첫 칸끼리 맞바꾸면 두 방 번호가 서로 바뀐다 — 번호로 보면 「안 옮겨졌다」로 읽혔다.
        let swapped = seated(&[&["%2", "%1"], &["%0", "%3"]]);
        assert_eq!(seat_move_landed(DropZone::Center, &before, &swapped, "%0", "%2"), Some(true));
        assert_eq!(seat_move_landed(DropZone::Center, &before, &before, "%0", "%2"), Some(false));
        let lonely = seated(&[&["%0"], &["%2"]]);
        let lonely_swapped = seated(&[&["%2"], &["%0"]]);
        assert_eq!(seat_move_landed(DropZone::Center, &lonely, &lonely_swapped, "%0", "%2"), None, "스냅샷으론 못 가린다");
    }

    #[test]
    fn feed_room_finds_tabs_too() {
        let room = |window, leaf: &str, tabs: &[&str]| kasa_mcp::layout_feed::Room {
            window,
            tree: kasa_pty::PtyLayout::single(leaf),
            members: [(leaf.to_string(), tabs.iter().map(|t| t.to_string()).collect())].into_iter().collect(),
        };
        let rooms = [room(0, "%0", &["%0", "%5"]), room(3, "%2", &[])];
        assert_eq!(feed_room_of(&rooms, "%5"), Some(0));
        assert_eq!(feed_room_of(&rooms, "%2"), Some(3));
        assert_eq!(feed_room_of(&rooms, "%9"), None);
    }

    #[test]
    fn equal_pane_numbers_use_machine_identity_and_protocol_room() {
        let request = request(
            &snapshot("source", "room:source"),
            &snapshot("target", "room:target"),
        )
        .unwrap();
        assert_eq!(request.sessions[0].machine_id, "source");
        assert_eq!(request.destination_machine, "target");
        assert_eq!(
            request.destination_room,
            RoomTarget::Existing("room:target".into())
        );
    }

    #[test]
    fn remote_center_swaps_and_edge_moves() {
        let (method, params) = remote_drop_command("%1", "%2", DropZone::Center);
        assert_eq!(method, "surface.swap");
        assert_eq!(params, serde_json::json!({ "a": "%1", "b": "%2" }));
        let (method, params) = remote_drop_command("%1", "%2", DropZone::Left);
        assert_eq!(method, "surface.move");
        assert_eq!(
            params,
            serde_json::json!({ "surface_id": "%1", "target": "%2", "direction": "left" })
        );
    }

    #[test]
    fn cross_device_drag_requires_existing_claude_or_codex_conversation() {
        let target = snapshot("target", "room:target");
        for harness in ["claude", "codex"] {
            let mut source = snapshot("source", "room:source");
            source.sessions[0].harness = Some(harness.into());
            assert!(request(&source, &target).is_ok());
            source.sessions[0].identity.session_id = None;
            assert!(request(&source, &target).is_err());
        }
        let mut source = snapshot("source", "room:source");
        source.sessions[0].harness = None;
        assert!(request(&source, &target)
            .unwrap_err()
            .to_string()
            .contains("빈 셸"));
        source.sessions[0].harness = Some("other-harness".into());
        assert!(request(&source, &target)
            .unwrap_err()
            .to_string()
            .contains("other-harness"));
    }

    #[test]
    fn stale_room_unknown_source_and_same_machine_are_rejected() {
        let source = snapshot("source", "room:source");
        let mut target = snapshot("target", "room:target");
        target.rooms.clear();
        assert!(request(&source, &target).is_err());
        assert!(request(&source, &source).is_err());
        let mut unknown = source;
        unknown.sessions[0].status = "unknown".into();
        assert!(request(&unknown, &snapshot("target", "room:target")).is_err());
    }

    #[test]
    fn foreign_rows_and_local_mirror_numbers_cannot_select_a_source() {
        let mut source = snapshot("source", "room:source");
        source.sessions[0].identity.machine_id = "other".into();
        source.sessions[0].local_panes.push("%1".into());
        assert!(endpoint_row(&source, "%1").is_err());
        source.sessions[0].identity.machine_id = "source".into();
        source.sessions[0].identity.pane_id = "%2".into();
        assert!(endpoint_row(&source, "%1").is_err());
    }

    #[test]
    fn identity_pinned_before_drag_rejects_restarted_or_reused_pane() {
        let original = snapshot("source", "room:source");
        let pinned = original.sessions[0].identity.clone();
        assert!(verify_drag_identity(&original, "%1", &pinned).is_ok());
        for replacement in ["token", "instance", "session"] {
            let mut fresh = original.clone();
            match replacement {
                "token" => fresh.sessions[0].identity.token = "replacement".into(),
                "instance" => fresh.sessions[0].identity.instance = "restarted".into(),
                _ => fresh.sessions[0].identity.session_id = Some("replacement-session".into()),
            }
            assert!(verify_drag_identity(&fresh, "%1", &pinned).is_err());
        }
    }

    #[test]
    fn warm_cache_coalesces_remote_panes_by_origin_without_colliding_local() {
        assert_eq!(remote("%1").origin_key(), remote("%2").origin_key());
        assert_ne!(
            remote("%1").origin_key(),
            DragEndpoint::LocalPane("%1".into()).origin_key()
        );
        assert_eq!(remote("%1").with_pane("%2"), remote("%2"));
    }

    #[test]
    fn missing_identity_duplicate_pane_and_old_capability_are_rejected() {
        let mut source = snapshot("source", "room:source");
        source.sessions[0].identity.token.clear();
        assert!(endpoint_row(&source, "%1").is_err());
        source.sessions[0].identity.token = "token".into();
        source.sessions.push(source.sessions[0].clone());
        assert!(endpoint_row(&source, "%1").is_err());
        source.sessions.pop();
        source.room_transfer_supported = false;
        assert!(request(&source, &snapshot("target", "room:target")).is_err());
        let host = Host { machine_id: "target".into(), supported: false };
        assert!(request_from_pins(&seen(&snapshot("source", "room:source"), "%1"), &Arrival::new_room(&host)).is_err());
    }

    #[test]
    fn device_head_drop_opens_a_new_room_named_after_the_source_room() {
        let mut source = snapshot("source", "room:source");
        let host = Host { machine_id: "target".into(), supported: true };
        let titled = request_from_pins(&seen(&source, "%1"), &Arrival::new_room(&host)).unwrap();
        assert_eq!(titled.destination_machine, "target");
        assert_eq!(titled.destination_room, RoomTarget::New("same title".into()));
        for title in ["3번 방", "   ", "\u{7}bell"] {
            source.rooms[0].title = title.into();
            let fallback = request_from_pins(&seen(&source, "%1"), &Arrival::new_room(&host)).unwrap();
            assert_eq!(fallback.destination_room, RoomTarget::New("이사 온 방".into()), "{title:?}");
        }
        let same = Host { machine_id: "source".into(), supported: true };
        assert!(request_from_pins(&seen(&source, "%1"), &Arrival::new_room(&same)).is_err());
    }

    fn awaiting(screen: &str) -> Pin {
        Pin::Awaiting { screen: screen.into() }
    }

    /// 칸 열쇠를 실은 기기의 조회. 이 열쇠가 구조체에 생기기 전 판에서는 직렬화에서 빠진다.
    fn keyed(machine: &str, key: &str) -> (MachineSnapshot, bool) {
        let mut value = serde_json::to_value(snapshot(machine, "room:source")).unwrap();
        value["sessions"][0]["surface_key"] = serde_json::json!(key);
        let snapshot: MachineSnapshot = serde_json::from_value(value).unwrap();
        let carried = row_surface_key(&snapshot.sessions[0]).is_some();
        (snapshot, carried)
    }

    #[test]
    fn a_late_identity_is_accepted_only_for_the_surface_seen_at_press() {
        let mut late = seen(&snapshot("source", "room:source"), "%1");
        late.surface_key = Some("seen-at-press".into());
        assert!(matches!(confirm_screen(&late, "seen-at-press"), Pin::Ready(_)));
        assert!(matches!(confirm_screen(&late, "other"), Pin::Failed(reason) if reason == SCREEN_CHANGED));
        late.surface_key = None;
        assert!(matches!(confirm_screen(&late, "seen-at-press"), Pin::Failed(reason) if reason == NEEDS_UPDATE));

        let keys = |list: &[&str]| single_screen_key(list.iter().map(|key| key.to_string()));
        assert_eq!(keys(&["a", "a", "", "legacy:%3"]), Some("a".into()));
        assert_eq!(keys(&["a", "b"]), None);
        assert_eq!(keys(&["", "legacy:%3"]), None);
        assert_eq!(keys(&[]), None);
    }

    #[test]
    fn a_drag_waiting_for_its_start_identity_takes_only_that_hosts_answer() {
        let mut state = Mailbox::default();
        state.pins.insert(1, (remote("%1"), awaiting("key-a")));
        state.pins.insert(2, (DragEndpoint::LocalPane("%1".into()), awaiting("key-a")));
        state.pins.insert(3, (remote("%1"), awaiting("key-b")));
        let started = Instant::now();
        let (answer, carried) = keyed("source", "key-a");
        observe(&mut state, &remote(""), Ok(answer), started);
        if carried {
            let Some((_, Pin::Ready(pinned))) = state.pins.get(&1) else { panic!("remote pin unresolved") };
            assert_eq!(pinned.identity.token, "token");
            assert_eq!(pinned.at, started);
            assert!(matches!(state.pins.get(&3), Some((_, Pin::Failed(reason))) if reason == SCREEN_CHANGED));
        } else {
            assert!(matches!(state.pins.get(&1), Some((_, Pin::Failed(reason))) if reason == NEEDS_UPDATE));
        }
        assert!(matches!(state.pins.get(&2), Some((_, Pin::Awaiting { .. }))));
        assert_eq!(state.hosts[&remote("").origin_key()].machine_id, "source");

        state.pins.insert(4, (remote("%9"), awaiting("key-a")));
        state.pins.insert(5, (remote("%1"), awaiting("key-a")));
        let (mut busy, _) = keyed("source", "key-a");
        busy.sessions[0].unavailable_reason = Some("목록을 읽는 동안 세션이 바뀌었어요".into());
        observe(&mut state, &remote(""), Ok(busy), Instant::now());
        assert!(matches!(state.pins.get(&4), Some((_, Pin::Failed(_)))));
        assert!(matches!(state.pins.get(&5), Some((_, Pin::Failed(_)))));
        assert!(!state.observed.contains_key(&remote("%1").seat()));

        state.pins.insert(6, (remote("%1"), awaiting("key-a")));
        observe(&mut state, &remote(""), Err(anyhow::anyhow!("timeout")), Instant::now());
        assert!(matches!(state.pins.get(&6), Some((_, Pin::Failed(reason))) if reason.contains("timeout")));
    }
}
