//! 작업 모드(정리·조율)와 권한 표. 정본은 나쵸다 — `GET/POST /api/app/work-mode`,
//! `GET /api/app/capabilities`(나쵸 `docs/development/api/desk-api.md`). 폰과 PC 가 같은 값을
//! 읽어야 해서 기기마다 설정 파일에 두지 않는다. 카사텀 설정의 `work_mode_cache` 는 나쵸에 못 닿을
//! 때 보여 줄 「마지막으로 확인한 값」일 뿐이다.
//!
//! 지키는 선:
//! - 쓰기는 사람이 탭을 누를 때 한 번뿐이다. 창구나 화면 상태로 모드를 짐작하거나 바꾸지 않는다.
//! - 모드는 권한이 아니다. 바꿔도 위임 범위·확인 규칙이 그대로고 도는 일이 멈추지 않는다.
//! - 모드는 둘뿐이다. 나쵸가 다른 이름을 보내도 받지 않는다 — 무제한·무확인 같은 모드가 끼어들
//!   자리가 없다.
//! - 나쵸 키가 없는 기기는 읽기만 한다. 다른 기기를 거쳐 쓰는 길은 두지 않는다.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkMode {
    /// 사람이 터미널을 직접 볼 때 — 지금 창의 일을 정리해 보여 주고 끼어들지 않는다.
    Organize,
    /// 나쵸에게 맡길 때 — 모든 기기의 일을 한 판에, 사람 차례가 맨 위.
    Coordinate,
}

impl WorkMode {
    pub(crate) const ALL: [Self; 2] = [Self::Organize, Self::Coordinate];

    pub(crate) fn parse(word: &str) -> Option<Self> {
        match word {
            "organize" => Some(Self::Organize),
            "coordinate" => Some(Self::Coordinate),
            _ => None,
        }
    }

    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Organize => "organize",
            Self::Coordinate => "coordinate",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Organize => "정리",
            Self::Coordinate => "조율",
        }
    }

    /// 나쵸 권한 표에 설명 문구가 없을 때 쓰는 한 줄.
    pub(crate) const fn blurb(self) -> &'static str {
        match self {
            Self::Organize => "터미널을 직접 볼 때. 지금 창의 일을 정리해 보여 주고, 나쵸는 창에 먼저 끼어들지 않아요.",
            Self::Coordinate => "나쵸에게 맡길 때. 모든 기기의 일을 한 판에, 사람 차례가 맨 위예요.",
        }
    }
}

/// 나쵸가 적은 모드 한 판.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModeState {
    pub(crate) mode: WorkMode,
    pub(crate) rev: String,
    pub(crate) changed_at_ms: u64,
    pub(crate) changed_by: String,
}

/// 나쵸에 닿았는가. 못 닿았으면 왜인지를 화면에 그대로 적는다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    NoKey,
    /// 창구가 없는 옛 나쵸(404).
    OldNacho,
    /// 연결·키·서버 문제 — 나쵸가 준 낱말 또는 연결 실패 문장.
    Failed(String),
}

impl Reach {
    pub(crate) fn sentence(&self) -> String {
        match self {
            Self::NoKey => "이 기기엔 나쵸 연결 키가 없어요 — 나쵸가 도는 기기에서 바꿀 수 있어요".into(),
            Self::OldNacho => "이 나쵸는 아직 작업 모드를 몰라요".into(),
            Self::Failed(why) => format!("나쵸에 확인하지 못했어요 ({why})"),
        }
    }
}

fn parse_state(value: &Value) -> Option<ModeState> {
    Some(ModeState {
        mode: WorkMode::parse(value.get("mode")?.as_str()?)?,
        rev: match value.get("rev")? {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return None,
        },
        changed_at_ms: value.get("changed_at_ms").and_then(Value::as_u64).unwrap_or(0),
        changed_by: value.get("changed_by").and_then(Value::as_str).unwrap_or_default().to_string(),
    })
}

fn error_word(body: &Value, status: u16) -> String {
    body.get("error").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("HTTP {status}"))
}

/// `GET /api/app/work-mode` 의 답. 200 은 몸통 자체가 모드이거나 `work_mode` 칸에 싸여 온다.
pub(crate) fn parse_mode_reply(status: u16, body: &[u8]) -> Result<ModeState, Reach> {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    match status {
        200 => parse_state(value.get("work_mode").unwrap_or(&value))
            .ok_or_else(|| Reach::Failed("모드 응답을 읽지 못함".into())),
        404 if value.get("error").and_then(Value::as_str).is_none_or(|w| w == "not_found") => Err(Reach::OldNacho),
        _ => Err(Reach::Failed(error_word(&value, status))),
    }
}

/// 탭을 누른 한 번의 쓰기 요청 몸통. `nonce` 는 누를 때마다 새로 — 같은 클릭의 재전송만 같은 결과를 받는다.
/// 나쵸의 rev 는 정수다 — 받은 모양 그대로 돌려준다.
pub(crate) fn write_body(mode: WorkMode, rev: &str, nonce: &str) -> Value {
    let rev = rev.parse::<u64>().map(Value::from).unwrap_or_else(|_| Value::from(rev));
    serde_json::json!({"mode": mode.wire(), "rev": rev, "nonce": nonce})
}

pub(crate) fn new_nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 나쵸 승인 창구의 상태.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApprovalCaps {
    pub(crate) enabled: bool,
    pub(crate) http_actions: Vec<String>,
    pub(crate) decide_in_app: bool,
    pub(crate) decide_via: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModeInfo {
    pub(crate) mode: WorkMode,
    pub(crate) label: String,
    pub(crate) summary: String,
    pub(crate) effects: Vec<String>,
}

/// 권한 표의 동작 하나와 그 확인 방식.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TierAction {
    pub(crate) id: String,
    pub(crate) label: String,
    /// `none`·`button`·`card`·`approval`(나쵸 `desk-api.md` 「작업 모드·기능 안내」).
    pub(crate) how: String,
}

impl TierAction {
    pub(crate) fn how_word(&self) -> &'static str {
        match self.how.as_str() {
            "none" => "묻지 않음",
            "button" => "확인 단추",
            "card" => "갈림길 카드",
            "approval" => "승인(범위·해시·만료·1회)",
            _ => "확인 방식 미확인",
        }
    }
}

/// 권한 표 한 칸 — 어떤 동작이 어떤 확인을 거치나.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tier {
    pub(crate) tier: String,
    pub(crate) label: String,
    pub(crate) actions: Vec<TierAction>,
    pub(crate) rule: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Capabilities {
    pub(crate) approvals: Option<ApprovalCaps>,
    pub(crate) modes: Vec<ModeInfo>,
    pub(crate) tiers: Vec<Tier>,
    /// 나쵸가 밝힌 무제한 모드 여부 — `Some(false)` 가 정상이다. 이 앱은 어느 쪽이든 그런 모드를 세우지 않는다.
    pub(crate) unlimited_mode: Option<bool>,
    pub(crate) notes: Vec<String>,
    /// 권한 표 창구가 없는 옛 나쵸 — 승인 창구 상태만 탐침으로 알았다.
    pub(crate) probed_only: bool,
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value.and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// `GET /api/app/capabilities` 의 200 몸통. 모르는 모드 이름은 버린다.
pub(crate) fn parse_capabilities(value: &Value) -> Capabilities {
    let approvals = value.get("approvals").filter(|a| a.is_object()).map(|a| ApprovalCaps {
        enabled: a.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        http_actions: strings(a.get("http_actions")),
        decide_in_app: a.get("decide_in_app").and_then(Value::as_bool).unwrap_or(false),
        decide_via: text(a, "decide_via"),
    });
    let modes = value.pointer("/work_mode/modes").and_then(Value::as_array).into_iter().flatten().filter_map(|m| {
        let (mode, entry) = match m {
            Value::String(word) => (WorkMode::parse(word)?, None),
            Value::Object(_) => (WorkMode::parse(m.get("mode").or_else(|| m.get("name"))?.as_str()?)?, Some(m)),
            _ => return None,
        };
        Some(ModeInfo {
            mode,
            label: entry.map(|e| text(e, "label")).filter(|l| !l.is_empty()).unwrap_or_else(|| mode.label().into()),
            summary: entry.map(|e| text(e, "summary")).unwrap_or_default(),
            effects: entry.map(|e| strings(e.get("effects"))).unwrap_or_default(),
        })
    }).collect();
    let tiers = value.pointer("/policy/tiers").and_then(Value::as_array).into_iter().flatten().filter(|t| t.is_object()).map(|t| Tier {
        tier: text(t, "tier"),
        label: text(t, "label"),
        actions: t.get("actions").and_then(Value::as_array).into_iter().flatten().filter_map(|a| match a {
            Value::String(label) => Some(TierAction { id: String::new(), label: label.clone(), how: String::new() }),
            Value::Object(_) => Some(TierAction { id: text(a, "id"), label: text(a, "label"), how: text(a, "how") }),
            _ => None,
        }).collect(),
        rule: text(t, "rule"),
    }).collect();
    Capabilities {
        approvals,
        modes,
        tiers,
        unlimited_mode: value.pointer("/policy/unlimited_mode").and_then(Value::as_bool),
        notes: strings(value.pointer("/policy/notes")),
        probed_only: false,
    }
}

/// 권한 표 창구가 없는 나쵸에서 승인 창구가 켜졌는지만 안다. 모양은 맞고 없는 id 로 GET 하면 이미 합의된
/// 창구가 404 no_approval(켜짐)·503 approvals_disabled(꺼짐)으로 답한다. 키 문제는 guard 가 먼저 답한다.
pub(crate) const PROBE_APPROVAL: &str = "/api/app/approvals/ap_00000000000000000000000000000000";

pub(crate) fn parse_probe(status: u16, body: &[u8]) -> Result<Capabilities, Reach> {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let enabled = match (status, value.get("error").and_then(Value::as_str)) {
        (404, Some("no_approval")) => true,
        (503, Some("approvals_disabled")) => false,
        (404, _) => return Err(Reach::OldNacho),
        _ => return Err(Reach::Failed(error_word(&value, status))),
    };
    Ok(Capabilities {
        approvals: Some(ApprovalCaps { enabled, http_actions: Vec::new(), decide_in_app: false, decide_via: String::new() }),
        probed_only: true,
        ..Default::default()
    })
}

/// 나쵸 앱 창구 한 번 — 키가 없으면 묻지 않는다.
fn request(method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Vec<u8>), Reach> {
    if let Err(code) = kasa_mcp::nacho_app_target() {
        return Err(if code == "nacho_key_missing" { Reach::NoKey } else { Reach::Failed("나쵸 자리 정보 없음".into()) });
    }
    let bytes = body.map(|b| b.to_string().into_bytes());
    crate::nacho_tasks::app_request(method, path, bytes.as_deref()).map_err(Reach::Failed)
}

pub(crate) fn fetch_mode() -> Result<ModeState, Reach> {
    let (status, body) = request("GET", "/api/app/work-mode", None)?;
    parse_mode_reply(status, &body)
}

pub(crate) fn post_mode(mode: WorkMode, rev: &str, nonce: &str) -> Result<ModeState, Reach> {
    let (status, body) = request("POST", "/api/app/work-mode", Some(&write_body(mode, rev, nonce)))?;
    parse_mode_reply(status, &body)
}

pub(crate) fn fetch_capabilities() -> Result<Capabilities, Reach> {
    let (status, body) = request("GET", "/api/app/capabilities", None)?;
    match status {
        200 => Ok(parse_capabilities(&serde_json::from_slice(&body).unwrap_or(Value::Null))),
        404 => {
            let (status, body) = request("GET", PROBE_APPROVAL, None)?;
            parse_probe(status, &body)
        }
        _ => Err(Reach::Failed(error_word(&serde_json::from_slice(&body).unwrap_or(Value::Null), status))),
    }
}

const CACHE_KEY: &str = "work_mode_cache";

pub(crate) fn read_cache() -> Option<ModeState> {
    parse_state(crate::socket::read_settings().get(CACHE_KEY)?)
}

pub(crate) fn write_cache(state: &ModeState) {
    crate::socket::write_setting(
        CACHE_KEY,
        serde_json::json!({"mode": state.mode.wire(), "rev": state.rev, "changed_at_ms": state.changed_at_ms}),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_modes_exist() {
        assert_eq!(WorkMode::parse("organize"), Some(WorkMode::Organize));
        assert_eq!(WorkMode::parse("coordinate"), Some(WorkMode::Coordinate));
        for word in ["unlimited", "no_confirm", "yolo", "bypass", "", "Organize"] {
            assert_eq!(WorkMode::parse(word), None, "{word}");
        }
        let reply = br#"{"ok":true,"work_mode":{"mode":"unlimited","rev":"3","changed_at_ms":1}}"#;
        assert!(parse_mode_reply(200, reply).is_err(), "나쵸가 모르는 이름을 보내도 모드로 받지 않는다");
    }

    #[test]
    fn mode_replies_keep_rev_and_name_why_they_failed() {
        let bare = br#"{"schema":"nacho-work-mode/1","mode":"organize","rev":7,"changed_at_ms":1790000000000,"changed_by":"app:desktop"}"#;
        let state = parse_mode_reply(200, bare).unwrap();
        assert_eq!((state.mode, state.rev.as_str(), state.changed_by.as_str()), (WorkMode::Organize, "7", "app:desktop"));
        let wrapped = br#"{"ok":true,"work_mode":{"mode":"coordinate","rev":"8","changed_at_ms":2}}"#;
        assert_eq!(parse_mode_reply(200, wrapped).unwrap().mode, WorkMode::Coordinate);
        assert_eq!(parse_mode_reply(409, br#"{"ok":false,"error":"stale_rev"}"#), Err(Reach::Failed("stale_rev".into())));
        assert_eq!(parse_mode_reply(400, br#"{"ok":false,"error":"bad_mode"}"#), Err(Reach::Failed("bad_mode".into())));
        assert_eq!(parse_mode_reply(404, b"404: Not Found"), Err(Reach::OldNacho));
        assert_eq!(parse_mode_reply(403, br#"{"ok":false,"error":"bad_token"}"#), Err(Reach::Failed("bad_token".into())));
    }

    #[test]
    fn a_write_carries_the_seen_rev_and_a_fresh_nonce() {
        let body = write_body(WorkMode::Organize, "7", "n1");
        assert_eq!(body, serde_json::json!({"mode": "organize", "rev": 7, "nonce": "n1"}));
        assert!(new_nonce().len() <= 64, "나쵸는 64자 넘는 nonce 를 bad_nonce 로 거절한다");
        assert_ne!(new_nonce(), new_nonce());
    }

    #[test]
    fn capabilities_drop_unknown_modes_and_keep_the_policy_text() {
        let value = serde_json::json!({
            "schema": "nacho-capabilities/1",
            "approvals": {"enabled": false, "http_actions": ["kasaterm_restart"], "decide_in_app": false, "decide_via": "owner_dm_button"},
            "work_mode": {"modes": [
                {"mode": "organize", "label": "정리", "effects": ["자동 턴이 학생 창에 보내지 않음"]},
                {"mode": "coordinate", "label": "조율"},
                {"mode": "unlimited", "label": "무제한"}
            ], "default": "coordinate", "write": "app"},
            "policy": {"tiers": [
                {"tier": "delegated", "label": "위임", "actions": ["편집", "검사"], "rule": "범위 안에서 다시 묻지 않음"},
                {"tier": "confirm_once", "label": "매번 확인", "actions": ["설치", "재시작"], "rule": "대상·범위·해시·만료 1회"}
            ]}
        });
        let caps = parse_capabilities(&value);
        assert_eq!(caps.modes.iter().map(|m| m.mode).collect::<Vec<_>>(), vec![WorkMode::Organize, WorkMode::Coordinate]);
        assert_eq!(caps.modes[0].effects, vec!["자동 턴이 학생 창에 보내지 않음".to_string()]);
        assert_eq!(caps.modes[1].label, "조율");
        let approvals = caps.approvals.unwrap();
        assert!(!approvals.enabled && !approvals.decide_in_app);
        assert_eq!(approvals.http_actions, vec!["kasaterm_restart".to_string()]);
        assert_eq!(caps.tiers.len(), 2);
        assert_eq!(caps.tiers[1].rule, "대상·범위·해시·만료 1회");
        assert_eq!(caps.tiers[1].actions.iter().map(|a| a.label.as_str()).collect::<Vec<_>>(), vec!["설치", "재시작"]);
        assert!(!caps.probed_only);
    }

    /// 지금 도는 나쵸에 **읽기만** 한다(GET 두 번) — 모드를 바꾸지 않는다. 나쵸 키가 있는 기기에서 `--ignored` 로.
    #[test]
    #[ignore]
    fn live_nacho_reads_without_writing() {
        let mode = fetch_mode().expect("나쵸 작업 모드");
        let caps = fetch_capabilities().expect("나쵸 기능 안내");
        eprintln!("[live] mode={} rev={} approvals={:?} tiers={} unlimited={:?}", mode.mode.wire(), mode.rev,
            caps.approvals.as_ref().map(|a| (a.enabled, a.decide_in_app)), caps.tiers.len(), caps.unlimited_mode);
        assert!(!caps.probed_only, "da6f258 나쵸는 권한 표를 준다");
        assert_eq!(caps.unlimited_mode, Some(false));
        assert!(caps.approvals.is_some_and(|a| !a.decide_in_app));
    }

    /// 나쵸가 내준 고정 자료(나쵸 레포 `docs/development/api/fixtures/{work_mode,capabilities}.implemented.json`)를
    /// 이 파서가 그대로 읽는가. `NACHO_DESK_FIXTURES=<그 폴더>` 로 가리켜 `--ignored` 로 돈다.
    #[test]
    #[ignore]
    fn nacho_work_mode_fixtures_match() {
        let dir = std::path::PathBuf::from(std::env::var("NACHO_DESK_FIXTURES").expect("NACHO_DESK_FIXTURES"));
        let read = |name: &str| -> Value { serde_json::from_str(&std::fs::read_to_string(dir.join(name)).unwrap()).unwrap() };
        let fx = read("work_mode.implemented.json");
        let body = |v: &Value| serde_json::to_vec(v).unwrap();
        let got = parse_mode_reply(200, &body(&fx["get"]["200"])).unwrap();
        assert_eq!((got.mode, got.rev.as_str(), got.changed_by.as_str()), (WorkMode::Organize, "1", "app:desktop"));
        let never = parse_mode_reply(200, &body(&fx["get"]["200_never_set"])).unwrap();
        assert_eq!((never.mode, never.rev.as_str(), never.changed_at_ms), (WorkMode::Coordinate, "0", 0), "바꾼 적 없음 = 나쵸 기본값");
        let request = &fx["post"]["request"]["body"];
        let ours = write_body(WorkMode::parse(request["mode"].as_str().unwrap()).unwrap(), &request["rev"].to_string(), request["nonce"].as_str().unwrap());
        assert_eq!(&ours, request, "요청 몸통이 나쵸가 적은 모양과 같다");
        assert_eq!(parse_mode_reply(200, &body(&fx["post"]["200"])).unwrap().rev, "1");
        assert_eq!(parse_mode_reply(200, &body(&fx["post"]["200_retry_same_nonce"])).unwrap().mode, WorkMode::Organize);
        for (case, status, word) in [("409_stale_rev", 409, "stale_rev"), ("400_bad_mode", 400, "bad_mode"), ("400_rev_required", 400, "rev_required"),
            ("400_bad_nonce", 400, "bad_nonce"), ("403_bad_token", 403, "bad_token"), ("415_json_request_required", 415, "json_request_required")] {
            assert_eq!(parse_mode_reply(status, &body(&fx["post"][case])), Err(Reach::Failed(word.into())), "{case}");
        }
        let desk = parse_state(&fx["desk_tasks_work_mode"]).unwrap();
        assert_eq!(desk.mode, WorkMode::Organize);

        let caps_fx = read("capabilities.implemented.json");
        let caps = parse_capabilities(&caps_fx["200"]);
        assert_eq!(caps.modes.iter().map(|m| m.mode).collect::<Vec<_>>(), vec![WorkMode::Organize, WorkMode::Coordinate]);
        assert!(caps.modes.iter().all(|m| !m.summary.is_empty() && !m.effects.is_empty()), "폰·PC 가 같은 설명을 쓴다");
        assert!(caps.approvals.as_ref().is_some_and(|a| !a.decide_in_app && a.decide_via == "owner_dm_button"));
        assert_eq!(caps.unlimited_mode, Some(false));
        assert!(!caps.notes.is_empty());
        let hows: std::collections::BTreeSet<&str> = caps.tiers.iter().flat_map(|t| t.actions.iter().map(|a| a.how.as_str())).collect();
        assert!(hows.iter().all(|h| ["none", "button", "card", "approval"].contains(h)), "{hows:?}");
        assert!(caps.tiers.iter().flat_map(|t| &t.actions).all(|a| !a.label.is_empty() && a.how_word() != "확인 방식 미확인"));
        let restart = caps.tiers.iter().flat_map(|t| &t.actions).find(|a| a.id == "kasaterm_restart").unwrap();
        assert_eq!(restart.how, "approval");
        for (case, word) in [("403_bad_token", "bad_token"), ("503_app_key_missing", "app_key_missing")] {
            let v = &caps_fx[case];
            assert_eq!(v["error"], word, "{case}");
        }
    }

    #[test]
    fn an_old_nacho_reveals_only_the_approval_switch() {
        let off = parse_probe(503, br#"{"ok":false,"error":"approvals_disabled"}"#).unwrap();
        assert_eq!(off.approvals.as_ref().map(|a| a.enabled), Some(false));
        assert!(off.probed_only && off.tiers.is_empty() && off.modes.is_empty());
        let on = parse_probe(404, br#"{"ok":false,"error":"no_approval"}"#).unwrap();
        assert_eq!(on.approvals.map(|a| a.enabled), Some(true));
        assert_eq!(parse_probe(404, b"not found"), Err(Reach::OldNacho));
        assert_eq!(parse_probe(403, br#"{"ok":false,"error":"bad_token"}"#), Err(Reach::Failed("bad_token".into())));
        assert_eq!(parse_probe(503, br#"{"ok":false,"error":"app_key_missing"}"#), Err(Reach::Failed("app_key_missing".into())));
    }
}
