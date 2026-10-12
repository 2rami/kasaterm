use std::sync::{Mutex, OnceLock};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

const ACTION: &str = "claude-account-sync-v1";

#[derive(Clone, Debug, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountIdentity {
    account_uuid: String,
    organization_uuid: String,
    email: String,
    org: String,
}

impl PartialEq for AccountIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.account_uuid == other.account_uuid && self.organization_uuid == other.organization_uuid
    }
}

impl AccountIdentity {
    fn new(account_uuid: &str, organization_uuid: &str, email: &str, org: &str) -> Option<Self> {
        let account_uuid = uuid::Uuid::parse_str(account_uuid).ok()?;
        let organization_uuid = uuid::Uuid::parse_str(organization_uuid).ok()?;
        let email = email.trim();
        let org = org.trim();
        (!account_uuid.is_nil()
            && !organization_uuid.is_nil()
            && !email.is_empty()
            && !org.is_empty()
            && email.len() <= 320
            && org.len() <= 1024
            && !email.chars().chain(org.chars()).any(char::is_control))
        .then(|| Self {
            account_uuid: account_uuid.to_string(),
            organization_uuid: organization_uuid.to_string(),
            email: email.into(),
            org: org.into(),
        })
    }

    pub(crate) fn from_verified(identity: kasa_mcp::VerifiedClaudeIdentity) -> Option<Self> {
        Self::new(
            &identity.account_uuid,
            &identity.organization_uuid,
            &identity.email,
            &identity.org,
        )
    }

    pub(crate) fn from_profile(value: &Value) -> Option<Self> {
        Self::new(
            value.pointer("/account/accountUuid")?.as_str()?,
            value.pointer("/account/organizationUuid")?.as_str()?,
            value.get("email")?.as_str()?,
            value.get("org")?.as_str()?,
        )
    }

    pub(crate) fn matches_verified(&self, identity: kasa_mcp::VerifiedClaudeIdentity) -> bool {
        Self::from_verified(identity).is_some_and(|identity| &identity == self)
    }

    fn encode(&self) -> String {
        // The settings route rejects path-like ids; URL-safe encoding also preserves org names.
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).unwrap())
    }

    pub(super) fn decode(encoded: &str) -> Result<Self, MatchError> {
        if encoded.len() > 4096 {
            return Err(MatchError::Unverified);
        }
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| MatchError::Unverified)?;
        let value: Self = serde_json::from_slice(&decoded).map_err(|_| MatchError::Unverified)?;
        Self::new(
            &value.account_uuid,
            &value.organization_uuid,
            &value.email,
            &value.org,
        )
        .ok_or(MatchError::Unverified)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MatchError {
    Missing,
    Ambiguous,
    Unverified,
}

impl MatchError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Missing => "account_sync_missing",
            Self::Ambiguous => "account_sync_ambiguous",
            Self::Unverified => "account_sync_identity_unverified",
        }
    }

    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Missing => "같은 이메일·조직으로 로그인된 계정이 없어요. 이 기기에서 그 계정을 등록해 주세요",
            Self::Ambiguous => "같은 신원의 계정이 여러 개예요. 기기 사이 별명을 맞추거나 중복 슬롯을 정리해 주세요",
            Self::Unverified => "계정 신원을 아직 확인하지 못했어요. 계정 목록에서 로그인 상태를 확인한 뒤 다시 선택해 주세요",
        }
    }
}

pub(super) fn match_slot<I: PartialEq>(
    slots: &[(String, String)],
    identity_of: impl Fn(&str) -> Option<I>,
    display_of: impl Fn(&str) -> String,
    identity: &I,
    label: &str,
) -> Result<String, MatchError> {
    let mut unknown = false;
    let matches: Vec<_> = slots
        .iter()
        .filter(|(id, _)| !id.is_empty())
        .filter(|(id, _)| match identity_of(id) {
            Some(found) => &found == identity,
            None => {
                unknown = true;
                false
            }
        })
        .collect();
    match matches.as_slice() {
        [] => Err(if unknown {
            MatchError::Unverified
        } else {
            MatchError::Missing
        }),
        [(id, _)] => Ok(id.clone()),
        _ => {
            // Labels can choose among proven aliases, never authorize another identity.
            let label = label.trim();
            let mut named = matches.iter().filter(|(id, name)| {
                !label.is_empty() && (name.trim() == label || display_of(id).trim() == label)
            });
            let first = named.next().map(|(id, _)| id.clone());
            match (first, named.next()) {
                (Some(id), None) => Ok(id),
                _ => Err(MatchError::Ambiguous),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Pending,
    Applied,
    Missing,
    Ambiguous,
    Unverified,
    PreservationPending,
    WorkbenchChanged,
    StorageFailed,
    UpdateNeeded,
    LoginNeeded,
    Unconfirmed,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "반영 중",
            Self::Applied => "동기화 완료",
            Self::Missing => "같은 계정 없음",
            Self::Ambiguous => "중복 계정 · 별명 확인 필요",
            Self::Unverified => "계정 신원 확인 필요",
            Self::PreservationPending => "현재 로그인 보관 확인 필요 · 잠시 뒤 재시도",
            Self::WorkbenchChanged => "현재 로그인 계정 등록·선택 필요",
            Self::StorageFailed => "계정 저장소 접근·저장 확인 필요",
            Self::UpdateNeeded => "카사텀 업데이트 필요",
            Self::LoginNeeded => "선택 계정 로그인 확인 필요",
            Self::Unconfirmed => "응답 확인 실패",
        }
    }
}

impl Outcome {
    fn failed(self) -> bool {
        !matches!(self, Self::Applied | Self::Pending)
    }
}

fn outcome(reply: Result<Value, ()>, expected: &str) -> Outcome {
    let Ok(reply) = reply else {
        return Outcome::Unconfirmed;
    };
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
        return if reply.get("account_sync_identity").and_then(Value::as_str) == Some(expected) {
            Outcome::Applied
        } else {
            Outcome::Unconfirmed
        };
    }
    match reply.get("error_code").and_then(Value::as_str) {
        Some("account_sync_missing") => Outcome::Missing,
        Some("account_sync_ambiguous") => Outcome::Ambiguous,
        Some("account_sync_identity_unverified") => Outcome::Unverified,
        Some("account_sync_preservation_pending") => Outcome::PreservationPending,
        Some("account_sync_workbench_changed") => Outcome::WorkbenchChanged,
        Some("account_sync_storage_failed") => Outcome::StorageFailed,
        Some("action_unknown" | "account_sync_update_needed") => Outcome::UpdateNeeded,
        Some("account_switch_failed") => Outcome::LoginNeeded,
        _ => Outcome::Unconfirmed,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AccountSyncReport {
    serial: u64,
    device_epoch: u64,
    slot: String,
    label: String,
    peers: Vec<(String, Outcome)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SyncNotice {
    pub(crate) summary: String,
    pub(crate) details: String,
    pub(crate) failed: bool,
    pub(crate) pending: bool,
}

impl AccountSyncReport {
    fn visible_for(&self, selected: &str, serial: u64, device_epoch: u64) -> bool {
        self.slot == selected && self.serial == serial && self.device_epoch == device_epoch
    }

    fn notice(&self) -> SyncNotice {
        let failed = self
            .peers
            .iter()
            .filter(|(_, result)| result.failed())
            .count();
        let pending = self
            .peers
            .iter()
            .filter(|(_, result)| *result == Outcome::Pending)
            .count();
        let summary = if failed > 0 {
            format!("다른 기기 {failed}대 동기화 확인 필요")
        } else if pending > 0 {
            format!("다른 기기 {pending}대 반영 중")
        } else {
            format!("다른 기기 {}대 동기화 완료", self.peers.len())
        };
        let mut peers: Vec<_> = self.peers.iter().collect();
        peers.sort_by_key(|(_, result)| {
            if result.failed() {
                0
            } else if *result == Outcome::Pending {
                1
            } else {
                2
            }
        });
        let mut details = format!("이 기기: {} 적용", self.label);
        for (machine, result) in peers {
            details.push_str(&format!("\n{machine}: {}", result.label()));
        }
        if failed > 0 {
            details.push_str("\n확인 후 현재 계정을 다시 선택하면 다른 기기에 다시 보내요");
        }
        SyncNotice {
            summary,
            details,
            failed: failed > 0,
            pending: pending > 0,
        }
    }

    fn message(&self) -> String {
        let notice = self.notice();
        if let Some((machine, result)) = self.peers.iter().find(|(_, result)| result.failed()) {
            let mut short: String = machine.chars().take(16).collect();
            if machine.chars().count() > 16 {
                short.push('…');
            }
            format!(
                "{} — {short}: {}. 이 기기 적용 · 계정 메뉴에서 확인 후 다시 선택",
                notice.summary,
                result.label()
            )
        } else {
            format!("{} — 이 기기: {} 적용", notice.summary, self.label)
        }
    }
}

fn last_report() -> &'static Mutex<Option<AccountSyncReport>> {
    static LAST: Mutex<Option<AccountSyncReport>> = Mutex::new(None);
    &LAST
}

pub(crate) fn account_sync_notice(selected: &str) -> Option<SyncNotice> {
    let report = last_report().lock().ok()?.clone()?;
    let serial = queue().lock().ok()?.serial;
    report
        .visible_for(selected, serial, kasa_mcp::device_auth::change_epoch())
        .then(|| report.notice())
}

pub(crate) fn seed_account_sync_notice_for_verify(selected: &str) -> bool {
    if selected.is_empty()
        || !crate::verification_run()
        || environment_allowed(|key| std::env::var_os(key).is_some())
    {
        return false;
    }
    let mut queue = queue().lock().unwrap_or_else(|error| error.into_inner());
    queue.cancel();
    *last_report()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(AccountSyncReport {
        serial: queue.serial,
        device_epoch: kasa_mcp::device_auth::change_epoch(),
        slot: selected.into(),
        label: "검증 계정".into(),
        peers: vec![
            ("검증 기기 1".into(), Outcome::Applied),
            ("검증 기기 2".into(), Outcome::Ambiguous),
        ],
    });
    true
}

#[derive(Clone)]
struct Peer {
    label: String,
    base: String,
}

struct Job {
    report: AccountSyncReport,
    identity: Option<AccountIdentity>,
    peers: Vec<Peer>,
    proxy: EventLoopProxy<UserEvent>,
}

struct Queue<T> {
    serial: u64,
    running: bool,
    pending: Option<T>,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self {
            serial: 0,
            running: false,
            pending: None,
        }
    }
}

impl<T> Queue<T> {
    fn cancel(&mut self) {
        self.serial = self.serial.wrapping_add(1);
        self.pending = None;
    }

    fn submit(&mut self, job: impl FnOnce(u64) -> T) -> bool {
        self.cancel();
        self.pending = Some(job(self.serial));
        let start = !self.running;
        self.running = true;
        start
    }

    fn next(&mut self) -> Option<T> {
        let job = self.pending.take();
        if job.is_none() {
            self.running = false;
        }
        job
    }
}

fn queue() -> &'static Mutex<Queue<Job>> {
    static QUEUE: OnceLock<Mutex<Queue<Job>>> = OnceLock::new();
    QUEUE.get_or_init(Default::default)
}

pub(super) fn cancel() {
    let mut queue = queue().lock().unwrap_or_else(|error| error.into_inner());
    queue.cancel();
    *last_report()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
}

fn current(report: &AccountSyncReport) -> bool {
    queue()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .serial
        == report.serial
        && kasa_mcp::device_auth::change_epoch() == report.device_epoch
}

pub(super) fn accept_report(report: &AccountSyncReport, selected: &str) -> Option<String> {
    if report.slot != selected || !current(report) {
        return None;
    }
    *last_report()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(report.clone());
    Some(report.message())
}

fn environment_allowed(has: impl Fn(&str) -> bool) -> bool {
    !crate::version::isolated_environment(&has)
        && ![
            "KASATERM_DEVICE_FILE",
            "KASATERM_MACHINES_FILE",
            "KASATERM_WINDOW_SIZE",
            "KASATERM_WINDOW_POS",
        ]
        .iter()
        .any(|key| has(key))
}

pub(super) fn start(
    slot: String,
    identity: Option<AccountIdentity>,
    label: String,
    proxy: EventLoopProxy<UserEvent>,
) {
    if cfg!(test) || !environment_allowed(|key| std::env::var_os(key).is_some()) {
        return;
    }
    let own = kasa_mcp::mobile::machine_identity();
    let mut seen = std::collections::HashSet::new();
    let peers: Vec<_> = kasa_mcp::machines::machines()
        .into_iter()
        .filter(|machine| {
            !machine.base.trim().is_empty() && !(own.is_some() && machine.machine_id == own)
        })
        .filter(|machine| {
            seen.insert(
                machine
                    .machine_id
                    .clone()
                    .unwrap_or_else(|| machine.base.clone()),
            )
        })
        .map(|machine| Peer {
            label: machine.label,
            base: machine.base,
        })
        .collect();
    if peers.is_empty() {
        return;
    }
    let start = queue()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .submit(|serial| {
            let report = AccountSyncReport {
                serial,
                device_epoch: kasa_mcp::device_auth::change_epoch(),
                slot,
                label,
                peers: peers
                    .iter()
                    .map(|peer| (peer.label.clone(), Outcome::Pending))
                    .collect(),
            };
            *last_report()
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(report.clone());
            Job {
                report,
                identity,
                peers,
                proxy,
            }
        });
    if !start {
        return;
    }
    std::thread::spawn(|| loop {
        let Some(mut job) = queue()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .next()
        else {
            break;
        };
        let encoded = job.identity.as_ref().map(AccountIdentity::encode);
        if let Some(results) = send_batch(
            &job.peers,
            encoded.as_deref(),
            &job.report.label,
            || current(&job.report),
            |peer, identity, label| {
                let actual = crate::claude_auth::verified_workbench_identity()
                    .and_then(AccountIdentity::from_verified);
                if actual.as_ref() != job.identity.as_ref() {
                    return Ok(
                        serde_json::json!({"ok":false,"error_code":"account_sync_identity_unverified"}),
                    );
                }
                kasa_mcp::remote::settings_action_reply(
                    &peer.base,
                    ACTION,
                    Some(identity),
                    Some(label),
                    None,
                )
                .map_err(|_| ())
            },
        ) {
            job.report.peers = results;
            if current(&job.report) {
                let _ = job
                    .proxy
                    .send_event(UserEvent::AccountSyncResult(job.report));
            }
        }
    });
}

fn send_batch(
    peers: &[Peer],
    identity: Option<&str>,
    label: &str,
    is_current: impl Fn() -> bool,
    mut send: impl FnMut(&Peer, &str, &str) -> Result<Value, ()>,
) -> Option<Vec<(String, Outcome)>> {
    let mut results = Vec::new();
    for peer in peers {
        if !is_current() {
            return None;
        }
        let result = match identity {
            Some(identity) => outcome(send(peer, identity, label), identity),
            None => Outcome::Unverified,
        };
        if !is_current() {
            return None;
        }
        results.push((peer.label.clone(), result));
    }
    Some(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(email: &str, org: &str) -> AccountIdentity {
        use std::hash::{Hash, Hasher};
        let id = |value: &str| {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            value.hash(&mut hash);
            uuid::Uuid::from_u128(hash.finish() as u128 + 1).to_string()
        };
        AccountIdentity::new(&id(email), &id(org), email, org).unwrap()
    }

    #[test]
    fn duplicate_verified_identity_uses_only_a_unique_matching_label() {
        let slots = vec![
            ("one".into(), "gmail".into()),
            ("two".into(), "old-work".into()),
            ("three".into(), "gmail".into()),
        ];
        let want = identity("person@example.test", "Personal");
        let other = identity("person@example.test", "Company");
        let get = |id: &str| {
            Some(if id == "three" {
                other.clone()
            } else {
                want.clone()
            })
        };
        assert_eq!(
            match_slot(&slots, get, str::to_string, &want, "gmail"),
            Ok("one".into())
        );
        assert_eq!(
            match_slot(&slots, get, str::to_string, &want, "old-work"),
            Ok("two".into())
        );
        assert_eq!(
            match_slot(&slots, get, str::to_string, &want, "missing"),
            Err(MatchError::Ambiguous)
        );
        assert_eq!(
            match_slot(&slots, get, str::to_string, &want, ""),
            Err(MatchError::Ambiguous)
        );
    }

    #[test]
    fn labels_never_cross_email_or_organization_and_unknown_is_explicit() {
        let slots = vec![("one".into(), "gmail".into())];
        let actual = identity("person@example.test", "Personal");
        for want in [
            identity("someone@example.test", "Personal"),
            identity("person@example.test", "Company"),
        ] {
            assert_eq!(
                match_slot(
                    &slots,
                    |_| Some(actual.clone()),
                    str::to_string,
                    &want,
                    "gmail"
                ),
                Err(MatchError::Missing)
            );
        }
        assert_eq!(
            match_slot(&slots, |_| None, str::to_string, &actual, "gmail"),
            Err(MatchError::Unverified)
        );
    }

    #[test]
    fn identity_encoding_preserves_names_without_path_like_ids() {
        let want = identity("person@example.test", "R&D / 팀..이름");
        let encoded = want.encode();
        assert!(!encoded.contains('/') && !encoded.contains(".."));
        assert_eq!(AccountIdentity::decode(&encoded), Ok(want));
        assert_eq!(AccountIdentity::decode("bad"), Err(MatchError::Unverified));
        assert!(AccountIdentity::new("invalid", "invalid", "a@b", "org").is_none());
        assert!(AccountIdentity::new(
            &uuid::Uuid::nil().to_string(),
            &uuid::Uuid::nil().to_string(),
            "a@b",
            "org"
        )
        .is_none());
    }

    #[test]
    fn provider_uuids_not_display_names_authorize_the_choice() {
        let original = identity("person@example.test", "Original team");
        let mut renamed = original.clone();
        renamed.email = "renamed@example.test".into();
        renamed.org = "Renamed team".into();
        assert_eq!(original, renamed);
        let mut other_account = original.clone();
        other_account.account_uuid = "00000000-0000-0000-0000-000000000001".into();
        assert_ne!(original, other_account);
        let mut other_org = original.clone();
        other_org.organization_uuid = "00000000-0000-0000-0000-000000000002".into();
        assert_ne!(original, other_org);
    }

    #[test]
    fn a_new_selection_replaces_unsent_work_and_same_slot_can_retry() {
        let mut queue = Queue::default();
        assert!(queue.submit(|serial| (serial, "a")));
        let old = queue.next().unwrap();
        assert!(!queue.submit(|serial| (serial, "b")));
        assert!(!queue.submit(|serial| (serial, "a")));
        let latest = queue.next().unwrap();
        assert_eq!(latest.1, "a");
        assert_ne!(old.0, latest.0);
        assert!(queue.next().is_none());
        assert!(queue.submit(|serial| (serial, "a")));
        queue.cancel();
        assert!(
            queue.next().is_none(),
            "a peer switch discards pending local work"
        );
    }

    #[test]
    fn fake_delivery_reports_each_peer_and_requires_an_exact_ack() {
        let peers: Vec<_> = (0..4)
            .map(|n| Peer {
                label: format!("device-{n}"),
                base: String::new(),
            })
            .collect();
        let mut calls = 0;
        let result = send_batch(
            &peers,
            Some("identity"),
            "label",
            || true,
            |_, _, _| {
                calls += 1;
                match calls {
                    1 => Ok(serde_json::json!({"ok":true,"account_sync_identity":"identity"})),
                    2 => Ok(serde_json::json!({"ok":false,"error_code":"account_sync_ambiguous"})),
                    3 => Ok(serde_json::json!({"ok":true,"account_sync_identity":"other"})),
                    _ => Err(()),
                }
            },
        )
        .unwrap();
        assert_eq!(calls, 4);
        assert_eq!(
            result.iter().map(|(_, result)| *result).collect::<Vec<_>>(),
            vec![
                Outcome::Applied,
                Outcome::Ambiguous,
                Outcome::Unconfirmed,
                Outcome::Unconfirmed
            ]
        );
        assert_eq!(
            outcome(
                Ok(serde_json::json!({"ok":false,"error_code":"action_unknown"})),
                "x"
            ),
            Outcome::UpdateNeeded
        );
    }

    #[test]
    fn a_superseded_batch_never_sends_to_the_next_device() {
        let peers = vec![
            Peer {
                label: "one".into(),
                base: String::new(),
            },
            Peer {
                label: "two".into(),
                base: String::new(),
            },
        ];
        let current = std::cell::Cell::new(true);
        let mut calls = 0;
        assert!(send_batch(
            &peers,
            Some("identity"),
            "label",
            || current.get(),
            |_, _, _| {
                calls += 1;
                current.set(false);
                Ok(serde_json::json!({"ok":true,"account_sync_identity":"identity"}))
            }
        )
        .is_none());
        assert_eq!(calls, 1);
    }

    #[test]
    fn unavailable_identity_never_contacts_a_peer() {
        let peers = vec![Peer {
            label: "one".into(),
            base: String::new(),
        }];
        let result = send_batch(
            &peers,
            None,
            "label",
            || true,
            |_, _, _| panic!("unverified identity sent"),
        );
        assert_eq!(result.unwrap(), vec![("one".into(), Outcome::Unverified)]);
    }

    #[test]
    fn partial_success_cannot_be_read_as_all_devices_switched() {
        let report = AccountSyncReport {
            serial: 0,
            device_epoch: 0,
            slot: "one".into(),
            label: "Chosen".into(),
            peers: vec![
                ("Mini".into(), Outcome::Ambiguous),
                ("Desktop".into(), Outcome::Applied),
            ],
        };
        let message = report.message();
        assert!(message.starts_with("다른 기기 1대 동기화 확인 필요 —"));
        assert!(message.contains("이 기기 적용"));
        assert!(message.contains("Mini: 중복 계정"));
        assert!(message.contains("다시 선택"));
        let notice = report.notice();
        assert!(notice.failed && !notice.pending);
        assert!(notice.details.contains("Desktop: 동기화 완료"));
        assert!(notice.details.contains("이 기기: Chosen 적용"));
    }

    #[test]
    fn menu_keeps_all_peer_results_but_toast_prioritizes_failure() {
        let mut report = AccountSyncReport {
            serial: 4,
            device_epoch: 2,
            slot: "one".into(),
            label: "Chosen".into(),
            peers: Vec::new(),
        };
        for i in 0..8 {
            report
                .peers
                .push((format!("Desktop-{i}"), Outcome::Applied));
        }
        report.peers.push(("Mini".into(), Outcome::Ambiguous));
        report.peers.push(("Laptop".into(), Outcome::Missing));
        let notice = report.notice();
        assert_eq!(notice.summary, "다른 기기 2대 동기화 확인 필요");
        assert!(notice.details.find("Mini").unwrap() < notice.details.find("Desktop-0").unwrap());
        assert!(notice.details.contains("Laptop: 같은 계정 없음"));
        assert!(report.message().contains("Mini: 중복 계정"));
        assert!(!report.message().contains("Desktop-0"));
        assert!(report.visible_for("one", 4, 2));
        assert!(!report.visible_for("other", 4, 2));
        assert!(!report.visible_for("one", 5, 2));
        assert!(!report.visible_for("one", 4, 3));
    }

    #[test]
    fn pending_notice_cannot_claim_success() {
        let report = AccountSyncReport {
            serial: 1,
            device_epoch: 0,
            slot: "one".into(),
            label: "Chosen".into(),
            peers: vec![("Mini".into(), Outcome::Pending)],
        };
        let notice = report.notice();
        assert!(notice.pending && !notice.failed);
        assert_eq!(notice.summary, "다른 기기 1대 반영 중");
        assert!(!notice.details.contains("완료"));
    }

    #[test]
    fn isolated_runs_cannot_propagate_to_real_machines() {
        assert!(environment_allowed(|_| false));
        for key in [
            "KASATERM_SETTINGS_FILE",
            "KASATERM_SESSION_FILE",
            "KASATERM_WINDOW_FILE",
            "KASATERM_DEVICE_FILE",
            "KASATERM_MACHINES_FILE",
            "KASATERM_WINDOW_SIZE",
            "KASATERM_AUTOQUIT_MS",
        ] {
            assert!(!environment_allowed(|candidate| candidate == key));
        }
    }
}
