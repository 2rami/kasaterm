use super::*;

pub(super) struct PaneIdentity {
    pub shown: String,
    pub title: String,
    pub machine: MachineIdentity,
}

pub(super) struct MachineIdentity {
    pub label: String,
    pub detail: String,
    pub remote: bool,
    tint: [u8; 4],
    icon: String,
}

pub(super) fn terminal_identity_pid(pane_id: &str, active_is_terminal: bool) -> Option<&str> {
    active_is_terminal.then_some(pane_id)
}

/// Display the source device's number without changing the local routing key.
pub(super) fn shown_pane_id(pane_id: &str, active_is_terminal: bool) -> String {
    terminal_identity_pid(pane_id, active_is_terminal)
        .and_then(kasa_mcp::remote::remote_info)
        .map(|info| info.remote_id)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| pane_id.to_string())
}

impl MachineIdentity {
    pub fn for_pane(pane_id: Option<&str>, local_name: Option<&str>) -> Self {
        let remote = pane_id.and_then(kasa_mcp::remote::remote_info);
        let (label, detail, is_remote) = match remote {
            Some(info) => {
                let label = if let Some(label) = kasa_mcp::machines::label_for_base(&info.base) {
                    label
                } else if info.label.trim().is_empty() {
                    info.base
                        .trim_start_matches("https://")
                        .trim_start_matches("http://")
                        .trim_end_matches('/')
                        .to_string()
                } else {
                    info.label
                };
                let kind = if info.view { "미러" } else { "원격" };
                (label, format!("{kind} · {}", info.remote_id), true)
            }
            None => (
                local_name.unwrap_or("이 기기").to_string(),
                "로컬".to_string(),
                false,
            ),
        };
        let tint = machine_tint(&label);
        let icon = crate::device_icons::icon(&label);
        Self {
            label,
            detail,
            remote: is_remote,
            tint,
            icon,
        }
    }

    pub fn icon(&self) -> &str {
        &self.icon
    }

    pub fn background(&self, base: [u8; 4]) -> [u8; 4] {
        theme::lerp(base, self.tint, 0.12)
    }

    /// Local panes keep the viewer's base; remote panes carry the same subtle
    /// device tint as their header, without importing the source's light/dark mode.
    pub fn pane_background(&self, base: [u8; 4]) -> Option<[u8; 4]> {
        self.remote.then(|| self.background(base))
    }

    pub fn foreground(&self, background: [u8; 4]) -> [u8; 4] {
        theme::enforce_contrast_at(self.tint, background, 4.5)
    }
}

/// 기기색 기본 팔레트 — 색상환에서 서로 60° 넘게 떨어진 다섯 색. 작업 상태의
/// 빨강·주황·초록은 빌리지 않는다(주황은 「내 손을 기다린다」, 초록은 「끝남」,
/// 빨강은 「고장」이라 기기 이름에 붙으면 신호로 읽힌다). 채도를 높게 둔 이유는
/// 이 색이 그대로 칠해지는 자리가 없기 때문이다 — 헤더 칩은 12%, 배치도 칸은
/// 34% 로 바탕에 섞이는데, 흐린 색은 그 비율에서 밝은 테마의 흰 바탕과 어두운
/// 테마의 검정 바탕 양쪽에서 회색으로 뭉개진다.
pub(crate) const DEVICE_COLOR_PRESETS: &[(&str, [u8; 4])] = &[
    ("blue", [76, 134, 228, 255]),
    ("violet", [158, 104, 230, 255]),
    ("teal", [26, 172, 156, 255]),
    ("rose", [230, 90, 150, 255]),
    ("amber", [208, 154, 34, 255]),
];

/// 배치도 칸이 기기색을 섞는 비율. 헤더 칩(12%)보다 진한 이유는 칸이 작아서다 —
/// 20px 남짓한 사각이 12% 로 물들면 옆 칸과 갈라 보이지 않는다. 채움은 은은하게
/// 두고 **테두리**가 기기색을 더 진하게 문다(2026-09-14 승인: 「은은한 채움+테두리」).
const MINIMAP_TINT: f32 = 0.22;
const MINIMAP_BORDER_TINT: f32 = 0.55;
/// 기기 칸 채움이 이 기기 칸에서 떨어져야 하는 거리(RGB). 바탕과 색조가 비슷한 기기색은
/// 22% 로 섞으면 이 기기 칸과 같아 보여(2026-10-01 실측: 이 기기 #374046, 맥미니 #38454b)
/// 이 거리가 날 때까지 더 섞는다.
const MINIMAP_MIN_GAP: f32 = 32.0;
const MINIMAP_TINT_MAX: f32 = 0.6;

/// 스포이드 슬롯 번호에서 기기 칸을 가르는 기준. 팔레트 칸(0..27)과 한 통을
/// 쓰므로 그보다 훨씬 위에 둔다.
pub(crate) const DEVICE_SLOT_BASE: usize = 1000;

/// 설정 화면 한 줄 — 기기 하나와 지금 색·기본 색.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceColorRow {
    pub label: String,
    pub local: bool,
    /// 지금 유효한 색(#rrggbb).
    pub hex: String,
    /// 저장값이 없을 때 배정될 색(#rrggbb).
    pub default_hex: String,
    /// 사용자가 직접 고른 색이 저장돼 있다.
    pub custom: bool,
}

struct DeviceColors {
    /// 사용자가 고른 색 — 기기 열쇠(`identity_key`) → 색.
    overrides: HashMap<String, [u8; 4]>,
    /// 정규화한 이름 → `id:<기기 id>`. 같은 기계를 기기마다 다른 이름으로 부르므로(맥미니는
    /// 맥북을 「맥북」, 맥북은 자기를 컴퓨터 이름으로) 색은 이름이 아니라 id 에 붙는다.
    aliases: HashMap<String, String>,
    /// 명부(이 기기 + 등록 기계)에 배정한 기본색 — 정규화한 이름 → 색.
    assigned: HashMap<String, [u8; 4]>,
    /// 화면에 낼 순서: (표시 이름, 이 기기인가).
    roster: Vec<(String, bool)>,
    loaded_at: std::time::Instant,
}

static DEVICE_COLORS: std::sync::RwLock<Option<DeviceColors>> = std::sync::RwLock::new(None);
static LOCAL_MACHINE_ID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

fn normalize_device(label: &str) -> String {
    // 맥 컴퓨터 이름은 「MacBook Pro」 사이가 NBSP 라, 공백 종류만 다른 두 항목이 생겼다.
    label.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// 색을 찾고 적는 열쇠 — id 를 아는 기기는 `id:<id>`, 모르면 정규화한 이름.
fn identity_key(aliases: &HashMap<String, String>, label: &str) -> String {
    let name = normalize_device(label);
    aliases.get(&name).cloned().unwrap_or(name)
}

/// 이름 → id 표. 다른 기기에서 배운 것 위에 명부, 그 위에 이 기기를 얹는다 — 가까운 쪽이 이긴다.
fn device_aliases(settings: &serde_json::Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut put = |label: &str, id: &str| {
        if !label.trim().is_empty() && !id.trim().is_empty() {
            out.insert(normalize_device(label), format!("id:{}", id.trim()));
        }
    };
    for (label, id) in settings.get("device_color_ids").and_then(|v| v.as_object()).into_iter().flatten() {
        if let Some(id) = id.as_str() { put(label, id); }
    }
    for machine in kasa_mcp::machines::machines() {
        if let Some(id) = machine.machine_id.as_deref() { put(&machine.label, id); }
    }
    if let Some(id) = local_machine_id() {
        for label in crate::info::cached_local_machine_name().into_iter().chain([kasa_mcp::machines::self_label().as_str()]) {
            put(label, &id);
        }
    }
    out
}

fn stamp_of(at: &serde_json::Map<String, serde_json::Value>, label: &str) -> u64 {
    at.get(label).and_then(|v| v.as_u64()).unwrap_or(0)
}

fn device_hash(key: &str) -> usize {
    key.bytes()
        .fold(2166136261_u32, |hash, byte| (hash ^ byte as u32).wrapping_mul(16777619)) as usize
}

/// 명부 없이 이름만으로 고르는 색 — 명부에 없는 기기(끊긴 링크의 주소 등)가 쓴다.
fn hashed_device_color(key: &str) -> [u8; 4] {
    DEVICE_COLOR_PRESETS[device_hash(key) % DEVICE_COLOR_PRESETS.len()].1
}

/// 명부의 기기들에 서로 다른 기본색을 배정한다. 시작점은 **영구 id** 의 해시라
/// 같은 기계가 어느 창에서 보이든(그쪽에선 「이 기기」, 이쪽에선 명부 이름) 같은
/// 색에서 출발하고, 둘이 같은 칸에 떨어지면 뒤에 오는 쪽이 다음 빈 칸으로 비킨다.
/// 순서도 id 로 정렬해 두 기계가 서로를 볼 때 같은 결론에 닿게 한다.
fn assign_defaults(roster: &[(String, Option<String>)]) -> HashMap<String, [u8; 4]> {
    let mut order: Vec<(String, String)> = roster
        .iter()
        .map(|(label, id)| {
            let key = id
                .as_deref()
                .filter(|id| !id.trim().is_empty())
                .map(|id| format!("id:{}", id.trim()))
                .unwrap_or_else(|| format!("label:{}", normalize_device(label)));
            (key, normalize_device(label))
        })
        .collect();
    order.sort();
    order.dedup_by(|a, b| a.1 == b.1);
    let n = DEVICE_COLOR_PRESETS.len();
    let mut taken = vec![false; n];
    let mut by_identity = HashMap::new();
    let mut out = HashMap::new();
    for (key, label) in order {
        if let Some(color) = by_identity.get(&key) {
            out.insert(label, *color);
            continue;
        }
        let start = device_hash(&key) % n;
        let pick = (0..n)
            .map(|step| (start + step) % n)
            .find(|slot| !taken[*slot])
            .unwrap_or(start);
        taken[pick] = true;
        by_identity.insert(key, DEVICE_COLOR_PRESETS[pick].1);
        out.insert(label, DEVICE_COLOR_PRESETS[pick].1);
    }
    out
}

fn local_machine_id() -> Option<String> {
    LOCAL_MACHINE_ID
        .get_or_init(kasa_mcp::mobile::machine_identity)
        .clone()
}

/// 같은 기기를 가리키는 항목이 여럿이면 가장 늦게 고친 것이 이긴다.
fn read_overrides(settings: &serde_json::Value, aliases: &HashMap<String, String>) -> HashMap<String, [u8; 4]> {
    let at = settings.get("device_colors_at").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let mut newest: HashMap<String, (u64, [u8; 4])> = HashMap::new();
    let mut entries: Vec<(&String, &serde_json::Value)> =
        settings.get("device_colors").and_then(|v| v.as_object()).into_iter().flatten().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    for (label, value) in entries {
        let Some(rgb) = value.as_str().and_then(parse_color_input) else { continue };
        let ms = stamp_of(&at, label);
        let slot = newest.entry(identity_key(aliases, label)).or_insert((ms, [rgb[0], rgb[1], rgb[2], 255]));
        if ms > slot.0 {
            *slot = (ms, [rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    newest.into_iter().map(|(key, (_, color))| (key, color)).collect()
}

fn build_device_colors() -> DeviceColors {
    let settings = socket::read_settings();
    let aliases = device_aliases(&settings);
    let overrides = read_overrides(&settings, &aliases);
    let mut roster: Vec<(String, Option<String>)> = Vec::new();
    let mut shown: Vec<(String, bool)> = Vec::new();
    if let Some(local) = crate::info::cached_local_machine_name() {
        roster.push((local.to_string(), local_machine_id()));
        shown.push((local.to_string(), true));
    }
    for machine in kasa_mcp::machines::machines() {
        if machine.label.trim().is_empty()
            || shown.iter().any(|(l, _)| normalize_device(l) == normalize_device(&machine.label))
        {
            continue;
        }
        roster.push((machine.label.clone(), machine.machine_id.clone()));
        shown.push((machine.label.clone(), false));
    }
    DeviceColors {
        overrides,
        aliases,
        assigned: assign_defaults(&roster),
        roster: shown,
        loaded_at: std::time::Instant::now(),
    }
}

/// 설정과 기계 명부를 다시 읽어 색표를 세운다. 설정을 고친 뒤와 부팅 때 부르고,
/// 그 사이에는 `machine_tint` 가 몇 초에 한 번 스스로 새로 읽는다 — 명부는
/// 기계가 알려 오면서 늘어나는데 그때마다 신호를 받을 길이 없어서다.
pub(crate) fn reload_device_colors() {
    let fresh = build_device_colors();
    if let Ok(mut slot) = DEVICE_COLORS.write() {
        *slot = Some(fresh);
    }
}

fn ensure_device_colors() {
    let stale = DEVICE_COLORS
        .read()
        .ok()
        .map(|slot| {
            slot.as_ref()
                .is_none_or(|c| c.loaded_at.elapsed() > std::time::Duration::from_secs(5))
        })
        .unwrap_or(false);
    if stale {
        reload_device_colors();
    }
}

fn with_device_colors<T>(f: impl FnOnce(&DeviceColors) -> T) -> Option<T> {
    ensure_device_colors();
    DEVICE_COLORS.read().ok().and_then(|slot| slot.as_ref().map(f))
}

/// 기기 이름 → 기기색. 사용자가 고른 색 → 명부 배정색 → 이름 해시 순이다.
/// 같은 이름은 앱을 다시 열거나 로컬/원격 위치가 바뀌어도 같은 색을 갖는다.
pub(crate) fn machine_tint(label: &str) -> [u8; 4] {
    let key = normalize_device(label);
    with_device_colors(|c| {
        c.overrides
            .get(&identity_key(&c.aliases, label))
            .or_else(|| c.assigned.get(&key))
            .copied()
    })
    .flatten()
    .unwrap_or_else(|| hashed_device_color(&key))
}

/// 설정 화면에 늘어놓을 기기 목록 — 이 기기가 맨 위, 그 뒤 명부 순서.
pub(crate) fn device_color_rows() -> Vec<DeviceColorRow> {
    reload_device_colors();
    with_device_colors(|c| {
        c.roster
            .iter()
            .map(|(label, local)| {
                let key = normalize_device(label);
                let default = c
                    .assigned
                    .get(&key)
                    .copied()
                    .unwrap_or_else(|| hashed_device_color(&key));
                let custom = c.overrides.get(&identity_key(&c.aliases, label)).copied();
                let hex = |c: [u8; 4]| theme::hex_str([c[0], c[1], c[2]]);
                DeviceColorRow {
                    label: label.clone(),
                    local: *local,
                    hex: hex(custom.unwrap_or(default)),
                    default_hex: hex(default),
                    custom: custom.is_some(),
                }
            })
            .collect()
    })
    .unwrap_or_default()
}

/// `#rrggbb` · `rrggbb` · `#rgb` · `rgb(r, g, b)` · `r, g, b` 를 다 받는다 —
/// 디자인 도구마다 복사되는 꼴이 달라, hex 만 받으면 붙여넣기가 절반은 실패한다.
pub(crate) fn parse_color_input(text: &str) -> Option<[u8; 3]> {
    let t = text.trim();
    if let Some(rgb) = theme::parse_hex(t) {
        return Some(rgb);
    }
    let h = t.trim_start_matches('#');
    if h.len() == 3 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
        let v = u32::from_str_radix(h, 16).ok()?;
        let x = |n: u32| ((n & 0xf) * 17) as u8;
        return Some([x(v >> 8), x(v >> 4), x(v)]);
    }
    let inner = t
        .strip_prefix("rgb(")
        .or_else(|| t.strip_prefix("RGB("))
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(t);
    let parts: Vec<&str> = inner
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() != 3 {
        return None;
    }
    let mut out = [0u8; 3];
    for (slot, part) in out.iter_mut().zip(parts) {
        *slot = part.parse::<u16>().ok().filter(|v| *v <= 255)? as u8;
    }
    Some(out)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// 색 표와 「언제 바꿨나」 표를 함께 고친다. 시각은 다른 기기와 맞출 때 누가 이기는지를
/// 정한다(더 새로운 쪽) — 지운 것도 시각만 남겨 저쪽의 옛 색이 되살아나지 않게 한다.
fn write_overrides(
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>, &mut serde_json::Map<String, serde_json::Value>),
) {
    let settings = socket::read_settings();
    let mut map = settings
        .get("device_colors")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let mut at = settings
        .get("device_colors_at")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    edit(&mut map, &mut at);
    socket::write_setting("device_colors", serde_json::Value::Object(map));
    socket::write_setting("device_colors_at", serde_json::Value::Object(at));
    reload_device_colors();
}

fn stamp(at: &mut serde_json::Map<String, serde_json::Value>, label: &str, ms: u64) {
    at.insert(label.trim().to_string(), serde_json::json!(ms));
}

fn current_aliases() -> HashMap<String, String> {
    with_device_colors(|c| c.aliases.clone()).unwrap_or_default()
}

/// 같은 기기를 가리키는 항목을 이름이 달라도 색 표·시각 표에서 다 걷는다.
fn forget_device(
    map: &mut serde_json::Map<String, serde_json::Value>,
    at: &mut serde_json::Map<String, serde_json::Value>,
    aliases: &HashMap<String, String>,
    key: &str,
) {
    map.retain(|k, _| identity_key(aliases, k) != key);
    at.retain(|k, _| identity_key(aliases, k) != key);
}

/// 다른 기기와 맞추기 위한 표 — 색과 바꾼 시각, 그리고 이름마다 아는 기기 id. id 가 있어야
/// 저쪽이 자기가 부르는 이름(명부 이름)과 이쪽 이름을 같은 기계로 묶는다. `/term/device-colors`.
pub(crate) fn device_color_sync_table() -> serde_json::Value {
    let settings = socket::read_settings();
    let aliases = device_aliases(&settings);
    let mut ids = serde_json::Map::new();
    for table in ["device_colors", "device_colors_at"] {
        for label in settings.get(table).and_then(|v| v.as_object()).into_iter().flatten().map(|(k, _)| k) {
            if let Some(id) = aliases.get(&normalize_device(label)).and_then(|k| k.strip_prefix("id:")) {
                ids.insert(label.clone(), serde_json::json!(id));
            }
        }
    }
    serde_json::json!({
        "colors": settings.get("device_colors").cloned().unwrap_or(serde_json::json!({})),
        "at": settings.get("device_colors_at").cloned().unwrap_or(serde_json::json!({})),
        "ids": ids,
    })
}

/// 저쪽 표에서 들일 것 — 새로 배운 이름 → id, 그리고 (기기 열쇠, 저쪽 이름, 시각, 색).
struct MergePlan {
    learned: Option<serde_json::Map<String, serde_json::Value>>,
    newer: Vec<(String, String, u64, Option<String>)>,
    aliases: HashMap<String, String>,
}

/// 같은 기기는 이름이 달라도 id 로 묶어 견준다. 저쪽이 id 를 붙여 보낸 항목은 그 id 가 열쇠다 —
/// 어느 기계를 뜻했는지는 적은 쪽이 안다. 배운 이름은 이쪽이 모르는 것만 남긴다(가까운 쪽이 정본).
fn plan_merge(settings: &serde_json::Value, mut aliases: HashMap<String, String>, remote: &serde_json::Value) -> MergePlan {
    let empty = serde_json::Map::new();
    let remote_at = remote.get("at").and_then(|v| v.as_object()).unwrap_or(&empty);
    let remote_colors = remote.get("colors").and_then(|v| v.as_object());
    let remote_ids = remote.get("ids").and_then(|v| v.as_object());
    let mut learned = settings.get("device_color_ids").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let mut learned_new = false;
    for (label, id) in remote_ids.into_iter().flatten() {
        let Some(id) = id.as_str().map(str::trim).filter(|id| !id.is_empty()) else { continue };
        let name = normalize_device(label);
        if name.is_empty() || aliases.contains_key(&name) { continue; }
        aliases.insert(name, format!("id:{id}"));
        learned.insert(label.trim().to_string(), serde_json::json!(id));
        learned_new = true;
    }
    let key_of = |label: &str| {
        remote_ids
            .and_then(|ids| ids.get(label))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|id| format!("id:{id}"))
            .unwrap_or_else(|| identity_key(&aliases, label))
    };
    let local_at = settings.get("device_colors_at").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let local_stamp = |key: &str| {
        local_at.iter().filter(|(k, _)| identity_key(&aliases, k) == key).filter_map(|(_, v)| v.as_u64()).max().unwrap_or(0)
    };
    let mut newer: Vec<(String, String, u64, Option<String>)> = Vec::new();
    for (label, ms) in remote_at {
        let Some(ms) = ms.as_u64() else { continue };
        let key = key_of(label);
        if ms <= local_stamp(&key) || newer.iter().any(|(k, _, m, _)| *k == key && *m >= ms) { continue; }
        newer.retain(|(k, ..)| *k != key);
        let color = remote_colors
            .and_then(|c| c.get(label))
            .and_then(|v| v.as_str().and_then(parse_color_input))
            .map(theme::hex_str);
        newer.push((key, label.clone(), ms, color));
    }
    MergePlan { learned: learned_new.then_some(learned), newer, aliases }
}

/// 다른 기기의 표를 받아 **더 새로운 항목만** 들인다(색을 고친 것도, 지운 것도). 시각은
/// 저쪽 것을 그대로 적어 양쪽이 같은 값으로 수렴한다. 바뀐 것이 있으면 true.
pub(crate) fn merge_device_colors(remote: &serde_json::Value) -> bool {
    if remote.get("at").and_then(|v| v.as_object()).is_none() { return false; }
    let settings = socket::read_settings();
    let plan = plan_merge(&settings, device_aliases(&settings), remote);
    let learned = plan.learned.is_some();
    if let Some(map) = plan.learned {
        socket::write_setting("device_color_ids", serde_json::Value::Object(map));
    }
    if plan.newer.is_empty() {
        if learned { reload_device_colors(); }
        return learned;
    }
    let aliases = plan.aliases;
    write_overrides(|map, at| {
        for (key, label, ms, color) in plan.newer {
            forget_device(map, at, &aliases, &key);
            if let Some(hex) = color {
                map.insert(label.trim().to_string(), serde_json::Value::String(hex));
            }
            stamp(at, &label, ms);
        }
    });
    true
}

/// 기기 하나의 색을 설정에 굳힌다. 이름 키는 표시 이름 그대로 두어 파일을 열었을
/// 때 어느 기계인지 읽히게 하고, 찾을 때는 id(모르면 접은 이름)로 묶는다.
pub(crate) fn set_device_color(label: &str, rgb: [u8; 3]) {
    let aliases = current_aliases();
    let key = identity_key(&aliases, label);
    write_overrides(|map, at| {
        forget_device(map, at, &aliases, &key);
        map.insert(
            label.trim().to_string(),
            serde_json::Value::String(theme::hex_str(rgb)),
        );
        stamp(at, label, now_ms());
    });
}

/// 파일에는 안 쓰고 **화면 색만** 바꾼다 — 피커를 끄는 동안 매 움직임이 여기로
/// 온다. 손을 뗄 때 `set_device_color` 가 같은 값을 굳힌다.
pub(crate) fn preview_device_color(label: &str, rgb: [u8; 3]) {
    ensure_device_colors();
    if let Ok(mut slot) = DEVICE_COLORS.write() {
        if let Some(c) = slot.as_mut() {
            let key = identity_key(&c.aliases, label);
            c.overrides.insert(key, [rgb[0], rgb[1], rgb[2], 255]);
            // 미리보기 중에 자동 재적재가 돌면 파일값으로 되돌아가 색이 튄다.
            c.loaded_at = std::time::Instant::now();
        }
    }
}

pub(crate) fn reset_device_color(label: &str) {
    let aliases = current_aliases();
    let key = identity_key(&aliases, label);
    write_overrides(|map, at| {
        forget_device(map, at, &aliases, &key);
        stamp(at, label, now_ms());
    });
}

pub(crate) fn reset_all_device_colors() {
    write_overrides(|map, at| {
        let now = now_ms();
        let labels: Vec<String> = map.keys().cloned().collect();
        map.clear();
        for label in labels { stamp(at, &label, now); }
    });
}

/// The interior is opaque so active/hover fills cannot erase device identity.
/// Attention still owns the outer border and its pulse is painted above this.
/// 넓은 판(파일트리·깃 패널)에 기기색을 묻히는 정도. 배치도 칸(22%)만큼 칠하면 옆 pane 과
/// 딴 색이 되어 어색하다(2026-09-17 지적) — 어느 기기인지만 알리게 옅게.
const PANEL_TINT: f32 = 0.07;

/// 파일트리·깃 패널 배경 — 다른 기기 것을 볼 때만 기기색을 옅게 묻힌다.
pub(crate) fn panel_background(base: [u8; 4], machine: Option<&str>) -> [u8; 4] {
    machine.map_or(base, |label| theme::lerp(base, machine_tint(label), PANEL_TINT))
}

pub(crate) fn minimap_background(base: [u8; 4], machine: Option<&str>) -> [u8; 4] {
    machine.map_or(base, |label| minimap_fill(base, machine_tint(label)))
}

/// 이 기기 칸이 보이는 색 — 반투명 테두리색(`border` 0x66)이 판 바탕에 얹힌 것.
fn minimap_local_cell(base: [u8; 4]) -> [u8; 4] {
    theme::lerp(base, theme::border(), 0x66 as f32 / 255.0)
}

fn rgb_gap(a: [u8; 4], b: [u8; 4]) -> f32 {
    (0..3).map(|i| (a[i] as f32 - b[i] as f32).powi(2)).sum::<f32>().sqrt()
}

/// 기기색을 22% 부터 섞되, 이 기기 칸과 `MINIMAP_MIN_GAP` 만큼 떨어질 때까지 더 섞는다.
fn minimap_fill(base: [u8; 4], tint: [u8; 4]) -> [u8; 4] {
    let local = minimap_local_cell(base);
    let mut t = MINIMAP_TINT;
    loop {
        let fill = theme::lerp(base, tint, t);
        if t >= MINIMAP_TINT_MAX || rgb_gap(fill, local) >= MINIMAP_MIN_GAP {
            return fill;
        }
        t = (t + 0.02).min(MINIMAP_TINT_MAX);
    }
}

/// 배치도 칸 테두리 — 채움보다 진하게 기기색을 문다. 기기가 없으면 `fallback`.
pub(crate) fn minimap_border(base: [u8; 4], machine: Option<&str>, fallback: [u8; 4]) -> [u8; 4] {
    machine.map_or(fallback, |label| theme::lerp(base, machine_tint(label), MINIMAP_BORDER_TINT))
}

pub(super) fn draw_card(
    g: &mut gpu::GpuRenderer,
    identity: &PaneIdentity,
    rect: (f32, f32, f32, f32),
) {
    let (rx, ry, rw, rh) = rect;
    if rw < 40.0 || rh < 32.0 {
        return;
    }
    let compact = rh < 148.0 || rw < 200.0;
    let padding = if rh < 64.0 || rw < 96.0 {
        6.0
    } else if compact {
        12.0
    } else {
        18.0
    };
    let width = (rw - 16.0).min(if compact { 248.0 } else { 304.0 });
    let content_w = (width - padding * 2.0).max(0.0);
    let number_font = (rh - padding * 2.0 - 8.0).min(if compact { 24.0 } else { 32.0 });
    let number_width = g.measure_chrome_text(&identity.shown, number_font, true);
    let number_font = (number_font * (content_w / number_width.max(1.0)).min(1.0)).max(9.0);
    let title_font = 13.0;
    let machine_font = 12.0;
    let show_machine = rh >= 90.0 && content_w >= 72.0;
    let show_title = rh >= 142.0 && !identity.title.trim().is_empty();
    let show_detail = rh >= 186.0 && content_w >= 120.0;
    let height = (padding * 2.0
        + number_font
        + if show_title { 24.0 } else { 0.0 }
        + if show_machine { 36.0 } else { 0.0 }
        + if show_detail { 20.0 } else { 0.0 })
    .min(rh - 8.0);
    let x = rx + (rw - width) / 2.0;
    let y = ry + (rh - height) / 2.0;
    let surface = theme::panel_bg();
    let fg = theme::enforce_contrast_at(theme::text(), surface, 4.5);
    let dim = theme::enforce_contrast_at(theme::text_dim(), surface, 4.5);
    g.push_clip(rx, ry, rw, rh);
    round_rect(g, x, y, width, height, 12.0, theme::border());
    round_rect(
        g,
        x + 1.0,
        y + 1.0,
        width - 2.0,
        height - 2.0,
        11.0,
        surface,
    );
    let number = crate::info::fit_text(g, &identity.shown, content_w, number_font, true);
    let number_w = g.measure_chrome_text(&number, number_font, true);
    g.draw_text(
        x + (width - number_w) / 2.0,
        y + padding,
        &number,
        gpu::DrawOpts {
            font_size: number_font,
            color: fg,
            bold: true,
            italic: false,
        },
    );
    let mut next_y = y + padding + number_font;
    if show_title {
        let title = crate::info::fit_text(g, &identity.title, content_w, title_font, false);
        let title_w = g.measure_chrome_text(&title, title_font, false);
        g.draw_text(
            x + (width - title_w) / 2.0,
            next_y + 6.0,
            &title,
            gpu::DrawOpts {
                font_size: title_font,
                color: fg,
                bold: false,
                italic: false,
            },
        );
        next_y += 24.0;
    }
    if show_machine {
        let machine = &identity.machine;
        let label = crate::info::fit_text(g, &machine.label, content_w - 36.0, machine_font, false);
        let label_w = g.measure_chrome_text(&label, machine_font, false);
        let chip_w = label_w + 36.0;
        let chip_x = x + (width - chip_w) / 2.0;
        let chip_y = next_y + 12.0;
        let chip_bg = machine.background(surface);
        let chip_fg = machine.foreground(chip_bg);
        round_rect(g, chip_x, chip_y, chip_w, 24.0, 6.0, chip_bg);
        g.queue_icon(machine.icon(), chip_x + 8.0, chip_y + 5.0, 14.0, chip_fg);
        g.draw_text(
            chip_x + 28.0,
            chip_y + 6.0,
            &label,
            gpu::DrawOpts {
                font_size: machine_font,
                color: chip_fg,
                bold: false,
                italic: false,
            },
        );
        next_y += 36.0;
    }
    if show_detail {
        let detail = crate::info::fit_text(g, &identity.machine.detail, content_w, 11.0, false);
        let detail_w = g.measure_chrome_text(&detail, 11.0, false);
        g.draw_text(
            x + (width - detail_w) / 2.0,
            next_y + 8.0,
            &detail,
            gpu::DrawOpts {
                font_size: 11.0,
                color: dim,
                bold: false,
                italic: false,
            },
        );
    }
    g.pop_clip();
}

#[cfg(test)]
mod tests {
    use super::{
        assign_defaults, hashed_device_color, machine_tint, parse_color_input, read_overrides,
        terminal_identity_pid, MachineIdentity, DEVICE_COLOR_PRESETS, MINIMAP_BORDER_TINT,
        MINIMAP_TINT,
    };

    fn distance(a: [u8; 4], b: [u8; 4]) -> f32 {
        (0..3)
            .map(|i| (a[i] as f32 - b[i] as f32).powi(2))
            .sum::<f32>()
            .sqrt()
    }

    #[test]
    fn minimap_keeps_machine_fill_when_active_or_inactive_in_both_modes() {
        for base in [[25, 27, 34, 255], [45, 47, 54, 255], [240, 240, 246, 255]] {
            assert_eq!(super::minimap_background(base, None), base);
            let mini = super::minimap_background(base, Some("맥미니"));
            let book = super::minimap_background(base, Some("맥북"));
            assert_ne!(mini, base);
            assert_ne!(book, base);
        }
    }

    /// 기본 팔레트의 어느 두 색도, 헤더 칩·배치도 칸 비율로 밝은/어두운 바탕에
    /// 섞인 뒤에도 서로 갈라 보여야 한다 — 「확실히 구별」의 수치 기준.
    #[test]
    fn default_device_colors_stay_apart_on_light_and_dark_bases() {
        for base in [[25, 27, 34, 255], [240, 240, 246, 255]] {
            for (i, (_, a)) in DEVICE_COLOR_PRESETS.iter().enumerate() {
                for (_, b) in &DEVICE_COLOR_PRESETS[i + 1..] {
                    let mini_a = crate::theme::lerp(base, *a, MINIMAP_BORDER_TINT);
                    let mini_b = crate::theme::lerp(base, *b, MINIMAP_BORDER_TINT);
                    assert!(distance(mini_a, mini_b) >= 24.0, "{a:?} vs {b:?} on {base:?}");
                    let fill_a = crate::theme::lerp(base, *a, MINIMAP_TINT);
                    let fill_b = crate::theme::lerp(base, *b, MINIMAP_TINT);
                    assert!(distance(fill_a, fill_b) >= 12.0, "{a:?} vs {b:?} on {base:?}");
                    let chip_a = crate::theme::lerp(base, *a, 0.12);
                    let chip_b = crate::theme::lerp(base, *b, 0.12);
                    assert!(distance(chip_a, chip_b) >= 8.0, "{a:?} vs {b:?} on {base:?}");
                }
                // 섞인 뒤에도 바탕에서 떠야 한다.
                assert!(distance(crate::theme::lerp(base, *a, MINIMAP_BORDER_TINT), base) >= 24.0);
                assert!(distance(crate::theme::lerp(base, *a, MINIMAP_TINT), base) >= 12.0);
            }
        }
    }

    /// 명부의 기기들은 서로 다른 기본색을 받고, 같은 영구 id 는 이름이 달라도
    /// (저쪽에선 「이 기기」 이름, 이쪽에선 명부 이름) 같은 색을 받는다.
    #[test]
    fn roster_defaults_are_distinct_and_follow_machine_id() {
        let roster = vec![
            ("nachoneko의 Mac mini".to_string(), Some("id-mini".to_string())),
            ("맥북".to_string(), Some("id-book".to_string())),
            ("작업실 PC".to_string(), None),
        ];
        let a = assign_defaults(&roster);
        let mut seen: Vec<[u8; 4]> = a.values().copied().collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 3, "{a:?}");
        let other_side = vec![
            ("맥미니".to_string(), Some("id-mini".to_string())),
            ("MacBook Pro".to_string(), Some("id-book".to_string())),
        ];
        let b = assign_defaults(&other_side);
        assert_eq!(a["nachoneko의 mac mini"], b["맥미니"]);
        assert_eq!(a["맥북"], b["macbook pro"]);
    }

    #[test]
    fn aliases_of_one_machine_do_not_consume_extra_colors() {
        let mut roster = vec![
            ("Mini".to_string(), Some("id-mini".to_string())),
            ("Book".to_string(), Some("id-book".to_string())),
        ];
        let before = assign_defaults(&roster);
        roster.push(("Mini via SSH".to_string(), Some("id-mini".to_string())));
        roster.push(("Book Bonjour".to_string(), Some("id-book".to_string())));
        let after = assign_defaults(&roster);
        assert_eq!(after["mini"], after["mini via ssh"]);
        assert_eq!(after["book"], after["book bonjour"]);
        assert_eq!(before["mini"], after["mini"]);
        assert_eq!(before["book"], after["book"]);
        assert_ne!(after["mini"], after["book"]);
    }

    #[test]
    fn overrides_win_and_names_fold_case_and_space() {
        let settings = serde_json::json!({
            "device_colors": { " MacBook ": "#112233", "맥미니": "rgb(1, 2, 3)", "bad": "zzz" }
        });
        let o = read_overrides(&settings, &std::collections::HashMap::new());
        assert_eq!(o.get("macbook"), Some(&[0x11, 0x22, 0x33, 255]));
        assert_eq!(o.get("맥미니"), Some(&[1, 2, 3, 255]));
        assert_eq!(o.get("bad"), None);
        assert_eq!(hashed_device_color(" MACBOOK "), hashed_device_color(" MACBOOK "));
        assert_eq!(machine_tint(" MACBOOK "), machine_tint("macbook"));
    }

    /// 같은 기계를 기기마다 다른 이름으로 불러도(명부 이름·컴퓨터 이름·NBSP) id 로 한 색이다.
    #[test]
    fn one_machine_keeps_one_color_across_names() {
        assert_eq!(super::normalize_device("건호의 MacBook\u{a0}Pro"), super::normalize_device(" 건호의 MacBook  Pro "));
        let aliases: std::collections::HashMap<String, String> = [
            ("나쵸네코".to_string(), "id:mini".to_string()),
            ("nachoneko".to_string(), "id:mini".to_string()),
        ].into_iter().collect();
        let settings = serde_json::json!({
            "device_colors": {
                "나쵸네코": "#548a84",
                "nachoneko": "#e99dbe",
                "건호의 MacBook\u{a0}Pro": "#f0dbae",
                "건호의 MacBook Pro": "#4c86e4"
            },
            "device_colors_at": {
                "nachoneko": 1790513120019u64,
                "건호의 MacBook\u{a0}Pro": 1790513122318u64,
                "건호의 MacBook Pro": 1790550916056u64
            }
        });
        let o = read_overrides(&settings, &aliases);
        assert_eq!(o.get("id:mini"), Some(&[0xe9, 0x9d, 0xbe, 255]), "시각 없는 옛 색보다 늦게 고친 색");
        assert_eq!(o.get("건호의 macbook pro"), Some(&[0x4c, 0x86, 0xe4, 255]), "공백 종류만 다른 두 항목은 하나");
        assert_eq!(o.len(), 2);
    }

    /// 2026-10-01 두 기기의 실제 색 표: 맥북은 맥미니를 「나쵸네코」(시각 없는 청록), 맥미니는 자기를
    /// 「nachoneko」(분홍)로 들고 있었다. id 를 주고받으면 둘 다 같은 색에 닿는다.
    #[test]
    fn two_devices_converge_on_one_color_per_machine() {
        let mac_aliases: std::collections::HashMap<String, String> = [
            ("건호의 macbook pro".to_string(), "id:book".to_string()),
            ("나쵸네코".to_string(), "id:mini".to_string()),
        ].into_iter().collect();
        let mac = serde_json::json!({
            "device_colors": { "나쵸네코": "#548a84", "nachoneko": "#e99dbe",
                               "건호의 MacBook\u{a0}Pro": "#f0dbae", "건호의 MacBook Pro": "#4c86e4" },
            "device_colors_at": { "nachoneko": 1790513120019u64, "건호의 MacBook\u{a0}Pro": 1790513122318u64,
                                  "건호의 MacBook Pro": 1790550916056u64 }
        });
        let mini = serde_json::json!({
            "device_colors": { "nachoneko": "#e99dbe", "건호의 MacBook\u{a0}Pro": "#f0dbae", "건호의 MacBook Pro": "#4c86e4" },
            "device_colors_at": { "nachoneko": 1790513120019u64, "건호의 MacBook\u{a0}Pro": 1790513122318u64,
                                  "건호의 MacBook Pro": 1790550916056u64 }
        });
        let mini_aliases: std::collections::HashMap<String, String> = [
            ("nachoneko".to_string(), "id:mini".to_string()),
            ("맥북".to_string(), "id:book".to_string()),
        ].into_iter().collect();
        let table = |settings: &serde_json::Value, ids: serde_json::Value| serde_json::json!({
            "colors": settings["device_colors"], "at": settings["device_colors_at"], "ids": ids,
        });

        // 맥북이 맥미니 표를 받는다 — 맥미니가 자기 이름에 id 를 붙여 보낸다.
        let plan = super::plan_merge(&mac, mac_aliases.clone(), &table(&mini, serde_json::json!({ "nachoneko": "mini" })));
        assert!(plan.learned.as_ref().is_some_and(|m| m.contains_key("nachoneko")), "{:?}", plan.learned);
        assert!(plan.newer.is_empty(), "같은 시각 항목은 이미 있다: {:?}", plan.newer);
        let o = read_overrides(&mac, &plan.aliases);
        assert_eq!(o.get("id:mini"), Some(&[0xe9, 0x9d, 0xbe, 255]), "시각 없는 청록보다 늦게 고친 분홍");
        assert_eq!(o.get("id:book"), Some(&[0x4c, 0x86, 0xe4, 255]));

        // 맥미니가 맥북 표를 받는다 — 맥북 이름이 「맥북」과 같은 기계임을 배운다.
        let plan = super::plan_merge(&mini, mini_aliases.clone(), &table(&mac, serde_json::json!({
            "건호의 MacBook\u{a0}Pro": "book", "건호의 MacBook Pro": "book", "나쵸네코": "mini",
        })));
        assert_eq!(plan.aliases.get("건호의 macbook pro").map(String::as_str), Some("id:book"), "{:?}", plan.learned);
        let o = read_overrides(&mini, &plan.aliases);
        assert_eq!(o.get("id:book"), Some(&[0x4c, 0x86, 0xe4, 255]), "맥미니에서 「맥북」 칸도 맥북이 고른 파랑");
        assert_eq!(o.get("id:mini"), Some(&[0xe9, 0x9d, 0xbe, 255]));
        assert!(plan.newer.is_empty(), "같은 시각은 다시 들이지 않는다: {:?}", plan.newer);
    }

    /// 기기색이 바탕과 색조가 비슷해도 기기 칸은 이 기기 칸과 갈려 보여야 한다.
    #[test]
    fn device_cell_stands_apart_from_local_cell_even_with_a_muted_tint() {
        for base in [[0x30, 0x31, 0x3b, 255], [25, 27, 34, 255], [240, 240, 246, 255]] {
            for tint in [[0x54, 0x8a, 0x84, 255], [0xf0, 0xdb, 0xae, 255], DEVICE_COLOR_PRESETS[2].1] {
                let fill = super::minimap_fill(base, tint);
                assert!(distance(fill, super::minimap_local_cell(base)) >= 24.0, "{tint:?} on {base:?} → {fill:?}");
            }
        }
    }

    #[test]
    fn color_input_accepts_hex_short_hex_and_rgb_triples() {
        assert_eq!(parse_color_input("#4c86e4"), Some([0x4c, 0x86, 0xe4]));
        assert_eq!(parse_color_input("4C86E4"), Some([0x4c, 0x86, 0xe4]));
        assert_eq!(parse_color_input("#abc"), Some([0xaa, 0xbb, 0xcc]));
        assert_eq!(parse_color_input("rgb(76, 134, 228)"), Some([76, 134, 228]));
        assert_eq!(parse_color_input("76,134,228"), Some([76, 134, 228]));
        assert_eq!(parse_color_input("76 134 228"), Some([76, 134, 228]));
        assert_eq!(parse_color_input("300,0,0"), None);
        assert_eq!(parse_color_input("#12"), None);
        assert_eq!(parse_color_input(""), None);
    }

    #[test]
    fn remote_pane_background_keeps_viewer_brightness_and_stable_device_tint() {
        for base in [[26, 29, 35, 255], [240, 241, 243, 255]] {
            let mut machine = MachineIdentity {
                label: "맥미니".into(), detail: "미러".into(), remote: false,
                tint: machine_tint("맥미니"),
                icon: "server".into(),
            };
            assert_eq!(machine.pane_background(base), None);
            machine.remote = true;
            let tinted = machine.pane_background(base).unwrap();
            assert_ne!(tinted, base);
            assert_eq!(tinted, machine.background(base));
            let light = |c: [u8; 4]| c[..3].iter().map(|v| u16::from(*v)).sum::<u16>() > 380;
            assert_eq!(light(tinted), light(base));
        }
    }

    #[test]
    fn non_terminal_tab_does_not_inherit_outer_terminal_identity() {
        assert_eq!(terminal_identity_pid("%remote", false), None);
        assert_eq!(terminal_identity_pid("%remote", true), Some("%remote"));
    }
}
