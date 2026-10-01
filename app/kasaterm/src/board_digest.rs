//! 모든 기기 보드 스냅샷(`collab.snapshot`)의 요약 — 사이드바 학생 줄과 펫 현황판이 쓰는 세 수와
//! 사람을 기다리는 학생들. 보드 판은 걷었고(나쵸 대화·작업은 독립 앱), 판정만 여기 남았다.

use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Address {
    machine_id: String,
    surface_key: String,
    surface_id: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Pane {
    id: String,
    address: Address,
    machine_label: String,
    character: Option<String>,
    title: String,
    status: String,
    status_reason: Option<String>,
    /// 기다림의 종류(`permission`·`question`·`idle`). 낱말 `waiting` 하나로는 사람을
    /// 부르는 기다림과 방치를 못 가른다.
    attention_kind: Option<String>,
    done_outcome: Option<String>,
    observed_at_ms: u64,
    freshness: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Source {
    machine_id: String,
    is_local: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Snapshot {
    schema_version: u32,
    sources: Vec<Source>,
    panes: Vec<Pane>,
}

fn snapshot_from_value(value: serde_json::Value) -> Result<Snapshot, String> {
    let mut data: Snapshot = serde_json::from_value(value).map_err(|_| "보드 응답을 읽지 못했어요".to_string())?;
    if data.schema_version != 1 { return Err("보드 형식이 달라 갱신하지 못했어요".into()); }
    data.panes.retain(|row| !row.id.is_empty() && !row.address.machine_id.is_empty() && !row.address.surface_key.is_empty() && !row.address.surface_id.is_empty());
    data.panes.truncate(2000);
    data.sources.truncate(100);
    Ok(data)
}

/// 거울 줄은 원본 기기 줄과 같은 학생이라 빼지 않으면 두 번 선다.
pub(crate) fn pulse_digest(value: serde_json::Value) -> Result<crate::sidebar_pulse::PulseDigest, String> {
    Ok(digest(&snapshot_from_value(value)?))
}

fn digest(data: &Snapshot) -> crate::sidebar_pulse::PulseDigest {
    use crate::sidebar_pulse::{PulseDigest, WaitingStudent};
    let mirror = |row: &&Pane| row.status_reason.as_deref() == Some(crate::socket::REMOTE_MIRROR_REASON);
    let local = |machine_id: &str| data.sources.iter().any(|source| source.is_local && source.machine_id == machine_id);
    let mut digest = PulseDigest::default();
    for row in data.panes.iter().filter(|row| !mirror(row)) {
        match status_rank(row) {
            0 => {
                digest.counts.yours += 1;
                digest.waiting.push(WaitingStudent {
                    name: pane_name(row).to_string(),
                    character: row.character.clone().filter(|name| !name.is_empty()),
                    line: waiting_line(row),
                    machine_label: row.machine_label.clone(),
                    surface_id: row.address.surface_id.clone(),
                    local: local(&row.address.machine_id),
                    mirror: None,
                    since_ms: row.observed_at_ms,
                });
            }
            3 => digest.counts.working += 1,
            5 => digest.counts.done += 1,
            _ => {}
        }
    }
    // 다른 기기 학생은 이 기기의 거울 창이 있으면 그리로 간다 — 원본 pane id 는 여기서 못 연다.
    let mirrors: Vec<&str> = data.panes.iter().filter(mirror).map(|row| row.address.surface_id.as_str()).collect();
    for student in digest.waiting.iter_mut().filter(|student| !student.local) {
        student.mirror = mirrors.iter().find(|pane| {
            kasa_mcp::remote::remote_info(pane).is_some_and(|info| info.label == student.machine_label && info.remote_id == student.surface_id)
        }).map(|pane| pane.to_string());
    }
    digest.waiting.sort_by(|a, b| a.since_ms.cmp(&b.since_ms).then_with(|| a.name.cmp(&b.name)));
    digest
}

/// 기다리는 까닭 한 줄 — 종류가 앞, 무슨 일감인지가 뒤.
fn waiting_line(row: &Pane) -> String {
    let kind = match (row.done_outcome.as_deref(), row.attention_kind.as_deref()) {
        (Some("failed"), _) => "실패 보고",
        (_, Some("permission")) => "승인 기다림",
        (_, Some("question")) => "답 기다림",
        _ => "확인 필요",
    };
    let task = plain(&row.title, 60);
    if task.is_empty() { kind.to_string() } else { format!("{kind} · {task}") }
}

fn plain(value: &str, max_chars: usize) -> String {
    value.chars().filter(|ch| !ch.is_control() || ch.is_whitespace()).take(max_chars)
        .collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn pane_name(row: &Pane) -> &str {
    row.character.as_deref().filter(|name| !name.is_empty())
        .unwrap_or_else(|| if row.title.is_empty() { &row.address.surface_id } else { &row.title })
}

/// 0 = 사람 차례, 3 = 하는 중, 5 = 끝(완료 보고). 나머지는 세지 않는다.
fn status_rank(row: &Pane) -> u8 {
    if row.freshness != "fresh" { return 2; }
    // 방치(60초 조용)는 「답을 마치고 다음 지시를 기다림」이라 사람을 부르지 않는다 —
    // 세어 올리면 「확인 필요 N」이 실제 손댈 칸보다 부풀고, 정작 급한 칸이 묻힌다.
    if matches!(row.status.as_str(), "waiting" | "attention" | "blocked")
        && row.attention_kind.as_deref().and_then(crate::agent_state::WaitKind::parse)
            .is_some_and(crate::agent_state::WaitKind::needs_you) {
        return 0;
    }
    match row.done_outcome.as_deref() {
        Some("succeeded") => return 5,
        Some("failed") => return 0,
        _ => {}
    }
    match row.status.as_str() {
        "working" | "running" | "building" | "thinking" | "compacting" => 3,
        "idle" => 4,
        "waiting" if idle_wait(row) => 4,
        _ => 2,
    }
}

/// 그 기다림이 방치인가 — 칸이 먼저, 없으면 판정 이유(`wait_kind_of_row` 와 같은 규칙).
fn idle_wait(row: &Pane) -> bool {
    match row.attention_kind.as_deref() {
        Some(kind) => kind == crate::agent_state::WaitKind::Idle.as_str(),
        None => row.status_reason.as_deref() == Some(crate::agent_state::IDLE_PROMPT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe() -> serde_json::Value {
        let at = 1_000_000_u64;
        let panes: Vec<serde_json::Value> = [
            ("device-a", "아로나", "working", "fresh", None, None),
            ("device-a", "모모이", "waiting", "fresh", Some("question"), None),
            ("device-a", "미도리", "idle", "fresh", None, None),
            ("device-b", "아로나", "idle", "fresh", None, Some("succeeded")),
            ("device-b", "", "unknown", "fresh", None, None),
            ("device-c", "유즈", "working", "stale", None, None),
        ].into_iter().enumerate().map(|(index, (machine, character, status, freshness, kind, done))| serde_json::json!({
            "id": format!("{machine}/surface-{index}"),
            "address": {"machine_id": machine, "surface_key": format!("surface-{index}"), "surface_id": format!("%{}", index % 3 + 1)},
            "machine_label": machine, "character": character, "title": "주문 내역 화면 점검",
            "status": status, "attention_kind": kind, "done_outcome": done,
            "observed_at_ms": if freshness == "fresh" { at - 4_000 } else { at - 180_000 }, "freshness": freshness,
        })).collect();
        serde_json::json!({"schema_version": 1, "panes": panes,
            "sources": [{"machine_id": "device-a"}, {"machine_id": "device-b"}, {"machine_id": "device-c"}]})
    }

    /// 「확인 필요」는 사람이 손대야 풀리는 칸만 센다. 60초 방치는 답을 마치고 다음
    /// 지시를 기다리는 것이라 대기 중이다 — 세어 올리면 요약이 부풀어 급한 칸이 묻힌다.
    #[test]
    fn an_idle_wait_is_not_a_call_for_you() {
        let row = |kind: Option<&str>, reason: Option<&str>| Pane {
            status: "waiting".into(),
            freshness: "fresh".into(),
            attention_kind: kind.map(str::to_string),
            status_reason: reason.map(str::to_string),
            ..Default::default()
        };
        assert_eq!(status_rank(&row(Some("idle"), Some("idle prompt"))), 4);
        assert_eq!(status_rank(&row(Some("permission"), None)), 0);
        assert_eq!(status_rank(&row(Some("question"), None)), 0);
        assert_eq!(status_rank(&row(None, Some("idle prompt"))), 4, "종류 칸이 없는 옛 판 기계는 판정 이유로 가른다");
        assert_eq!(status_rank(&row(None, Some("hook attention"))), 2, "모르는 이유에서 승인 대기를 추측하지 않는다");
        let mut stale = row(Some("permission"), None);
        stale.freshness = "stale".into();
        assert_eq!(status_rank(&stale), 2);
    }

    /// 거울 줄은 상태가 무엇이든 안 센다. 거울 필터를 빼면 둘째 단언이 깨진다.
    #[test]
    fn pulse_counts_skip_remote_mirrors() {
        let value = probe();
        let counts = pulse_digest(value.clone()).unwrap().counts;
        assert_eq!((counts.yours, counts.working, counts.done), (1, 1, 1));

        let mut mirror = value["panes"][0].clone();
        mirror["id"] = "mirror-of-first".into();
        mirror["address"]["surface_key"] = "mirror-key".into();
        mirror["status_reason"] = crate::socket::REMOTE_MIRROR_REASON.into();
        let mut with_mirror = value.clone();
        with_mirror["panes"].as_array_mut().unwrap().push(mirror.clone());
        assert_eq!(pulse_digest(with_mirror).unwrap().counts, counts, "거울 줄을 세면 같은 학생이 두 번 선다");

        mirror["status_reason"] = "hook turn open".into();
        let mut plain = value;
        plain["panes"].as_array_mut().unwrap().push(mirror);
        assert_eq!(pulse_digest(plain).unwrap().counts.working, counts.working + 1, "거울이 아닌 줄은 센다");
    }

    /// 기다리는 학생은 「확인 필요」 칸 그대로다 — 수와 목록 길이가 같고, 문구는 종류·일감 순,
    /// 이 기기인지는 보드의 `is_local` 로 가른다.
    #[test]
    fn digest_lists_exactly_the_students_waiting_for_you() {
        let mut value = probe();
        value["sources"][0]["is_local"] = true.into();
        let digest = pulse_digest(value).unwrap();
        assert_eq!(digest.waiting.len(), digest.counts.yours);
        let first = &digest.waiting[0];
        assert_eq!(first.name, "모모이");
        assert_eq!(first.line, "답 기다림 · 주문 내역 화면 점검");
        assert!(first.local, "device-a 는 이 기기다");
    }
}
