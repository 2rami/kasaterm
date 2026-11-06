//! `session/update` 알림과 응답 결과를 카사텀이 쓰는 꼴로 편다.
//!
//! 어댑터마다 같은 사실을 다른 자리에 싣는다. 도구 출력은 codex 가 `_meta.terminal_output_delta`
//! 에 조각으로, claude 가 `rawOutput` 에 통째로 보낸다. 그 차이는 여기서만 흡수하고, 모르는
//! 갱신은 `Event::Other` 로 원문째 넘겨 새 판의 어댑터가 와도 연결이 끊기지 않게 한다.

use serde_json::Value;

use crate::rpc::RequestId;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Message { session_id: String, text: String },
    Thought { session_id: String, text: String },
    /// `session/load` 가 지난 대화를 다시 보낼 때의 사람 몫.
    UserMessage { session_id: String, text: String },
    ToolCall(ToolCall),
    ToolCallUpdate(ToolCallUpdate),
    Plan { session_id: String, entries: Vec<PlanEntry> },
    /// 컨텍스트 창 사용량. `used / size` 가 화면 파싱 없이 얻는 컨텍스트 비율이다.
    Usage { session_id: String, used: u64, size: u64, cost_usd: Option<f64> },
    Title { session_id: String, title: String },
    Mode { session_id: String, mode_id: String },
    /// 답할 때까지 에이전트가 그 도구 호출에서 멈춰 있다. `Connection::answer_permission` 으로 답한다.
    Permission(PermissionRequest),
    Other { session_id: String, kind: String, raw: Value },
    /// 에이전트 stdout 이 닫혔다. 이 뒤로는 이벤트가 오지 않는다.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl ToolStatus {
    fn parse(v: &Value) -> Option<Self> {
        Some(match v.as_str()? {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub session_id: String,
    pub id: String,
    /// 하네스 쪽 도구 이름(`Bash`·`exec_command`). ACP 표준 필드가 아니라 어댑터가 덧붙인 것이다.
    pub name: Option<String>,
    pub title: String,
    pub kind: Option<String>,
    pub status: ToolStatus,
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallUpdate {
    pub session_id: String,
    pub id: String,
    pub status: Option<ToolStatus>,
    pub title: Option<String>,
    pub command: Option<String>,
    pub output: Option<ToolOutput>,
    pub exit_code: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutput {
    /// 앞서 받은 출력 뒤에 이어 붙일 조각(codex).
    Delta(String),
    /// 지금까지의 출력 전체(claude).
    Full(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    pub status: String,
    pub priority: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    pub kind: PermissionKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    pub id: RequestId,
    pub session_id: String,
    pub tool_call_id: Option<String>,
    pub title: Option<String>,
    pub command: Option<String>,
    pub options: Vec<PermissionOption>,
}

impl PermissionRequest {
    pub fn option(&self, kind: PermissionKind) -> Option<&PermissionOption> {
        self.options.iter().find(|o| o.kind == kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    pub current: Option<String>,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub session_id: String,
    pub current_mode: Option<String>,
    pub modes: Vec<String>,
    /// 모델·추론 강도·모드 같은 세션 설정. `Connection::set_config_option` 의 `config_id` 가 여기 `id` 다.
    pub config: Vec<ConfigOption>,
}

impl NewSession {
    pub(crate) fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            session_id: v["sessionId"].as_str()?.to_owned(),
            current_mode: str_of(&v["modes"]["currentModeId"]),
            modes: v["modes"]["availableModes"]
                .as_array()
                .map(|a| a.iter().filter_map(|m| str_of(&m["id"])).collect())
                .unwrap_or_default(),
            config: v["configOptions"]
                .as_array()
                .map(|a| a.iter().filter_map(parse_config_option).collect())
                .unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    Other(String),
}

impl StopReason {
    pub fn as_str(&self) -> &str {
        match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::MaxTurnRequests => "max_turn_requests",
            Self::Refusal => "refusal",
            Self::Cancelled => "cancelled",
            Self::Other(s) => s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptOutcome {
    pub stop: StopReason,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_read_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

impl PromptOutcome {
    pub(crate) fn from_value(v: &Value) -> Option<Self> {
        let stop = match v["stopReason"].as_str()? {
            "end_turn" => StopReason::EndTurn,
            "max_tokens" => StopReason::MaxTokens,
            "max_turn_requests" => StopReason::MaxTurnRequests,
            "refusal" => StopReason::Refusal,
            "cancelled" => StopReason::Cancelled,
            other => StopReason::Other(other.to_owned()),
        };
        let usage = &v["usage"];
        Some(Self {
            stop,
            input_tokens: usage["inputTokens"].as_u64(),
            output_tokens: usage["outputTokens"].as_u64(),
            cached_read_tokens: usage["cachedReadTokens"].as_u64(),
            total_tokens: usage["totalTokens"].as_u64(),
        })
    }
}

pub(crate) fn parse_update(params: &Value) -> Option<Event> {
    let session_id = params["sessionId"].as_str()?.to_owned();
    let u = &params["update"];
    let kind = u["sessionUpdate"].as_str()?;
    Some(match kind {
        "agent_message_chunk" => Event::Message { session_id, text: block_text(&u["content"]) },
        "agent_thought_chunk" => Event::Thought { session_id, text: block_text(&u["content"]) },
        "user_message_chunk" => Event::UserMessage { session_id, text: block_text(&u["content"]) },
        "tool_call" => Event::ToolCall(ToolCall {
            session_id,
            id: u["toolCallId"].as_str()?.to_owned(),
            name: tool_name(u),
            title: str_of(&u["title"]).unwrap_or_default(),
            kind: str_of(&u["kind"]),
            status: ToolStatus::parse(&u["status"]).unwrap_or(ToolStatus::Pending),
            command: str_of(&u["rawInput"]["command"]),
        }),
        "tool_call_update" => Event::ToolCallUpdate(ToolCallUpdate {
            session_id,
            id: u["toolCallId"].as_str()?.to_owned(),
            status: ToolStatus::parse(&u["status"]),
            title: str_of(&u["title"]),
            command: str_of(&u["rawInput"]["command"]),
            output: tool_output(u),
            exit_code: u["rawOutput"]["exit_code"].as_i64(),
        }),
        "plan" => Event::Plan {
            session_id,
            entries: u["entries"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|e| PlanEntry {
                            content: str_of(&e["content"]).unwrap_or_default(),
                            status: str_of(&e["status"]).unwrap_or_default(),
                            priority: str_of(&e["priority"]).unwrap_or_default(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "usage_update" => Event::Usage {
            session_id,
            used: u["used"].as_u64()?,
            size: u["size"].as_u64()?,
            cost_usd: (u["cost"]["currency"].as_str() == Some("USD"))
                .then(|| u["cost"]["amount"].as_f64())
                .flatten(),
        },
        "session_info_update" if u["title"].is_string() => {
            Event::Title { session_id, title: str_of(&u["title"]).unwrap_or_default() }
        }
        "current_mode_update" => Event::Mode { session_id, mode_id: u["currentModeId"].as_str()?.to_owned() },
        _ => Event::Other { session_id, kind: kind.to_owned(), raw: u.clone() },
    })
}

pub(crate) fn parse_permission(id: RequestId, params: &Value) -> Option<PermissionRequest> {
    let tool = &params["toolCall"];
    Some(PermissionRequest {
        id,
        session_id: params["sessionId"].as_str()?.to_owned(),
        tool_call_id: str_of(&tool["toolCallId"]),
        title: str_of(&tool["title"]),
        command: str_of(&tool["rawInput"]["command"]),
        options: params["options"]
            .as_array()?
            .iter()
            .filter_map(|o| {
                Some(PermissionOption {
                    id: o["optionId"].as_str()?.to_owned(),
                    name: str_of(&o["name"]).unwrap_or_default(),
                    kind: match o["kind"].as_str() {
                        Some("allow_once") => PermissionKind::AllowOnce,
                        Some("allow_always") => PermissionKind::AllowAlways,
                        Some("reject_once") => PermissionKind::RejectOnce,
                        Some("reject_always") => PermissionKind::RejectAlways,
                        _ => PermissionKind::Other,
                    },
                })
            })
            .collect(),
    })
}

fn str_of(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

fn block_text(content: &Value) -> String {
    content["text"].as_str().unwrap_or_default().to_owned()
}

fn tool_name(u: &Value) -> Option<String> {
    str_of(&u["name"]).or_else(|| str_of(&u["_meta"]["claudeCode"]["toolName"]))
}

fn tool_output(u: &Value) -> Option<ToolOutput> {
    if let Some(full) = u["rawOutput"].as_str() {
        return Some(ToolOutput::Full(full.to_owned()));
    }
    if let Some(delta) = u["_meta"]["terminal_output_delta"]["data"].as_str() {
        return Some(ToolOutput::Delta(delta.to_owned()));
    }
    let texts: Vec<&str> = u["content"]
        .as_array()?
        .iter()
        .filter_map(|c| c["content"]["text"].as_str())
        .collect();
    (!texts.is_empty()).then(|| ToolOutput::Full(texts.join("\n")))
}

fn parse_config_option(o: &Value) -> Option<ConfigOption> {
    let mut values = Vec::new();
    for item in o["options"].as_array().into_iter().flatten() {
        match item["options"].as_array() {
            Some(group) => values.extend(group.iter().filter_map(|g| str_of(&g["value"]))),
            None => values.extend(str_of(&item["value"])),
        }
    }
    Some(ConfigOption {
        id: o["id"].as_str()?.to_owned(),
        name: str_of(&o["name"]).unwrap_or_default(),
        current: match &o["currentValue"] {
            Value::String(s) => Some(s.clone()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        },
        values,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn update(u: Value) -> Event {
        parse_update(&json!({ "sessionId": "s1", "update": u })).expect("parsed")
    }

    #[test]
    fn codex_tool_call_and_delta_output() {
        let call = update(json!({
            "sessionUpdate": "tool_call", "toolCallId": "exec-1", "name": "exec_command",
            "kind": "execute", "title": "ls -la", "status": "in_progress",
            "rawInput": { "command": "ls -la", "cwd": "/w" }
        }));
        assert_eq!(
            call,
            Event::ToolCall(ToolCall {
                session_id: "s1".into(),
                id: "exec-1".into(),
                name: Some("exec_command".into()),
                title: "ls -la".into(),
                kind: Some("execute".into()),
                status: ToolStatus::InProgress,
                command: Some("ls -la".into()),
            })
        );
        let Event::ToolCallUpdate(done) = update(json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "exec-1", "status": "completed",
            "rawOutput": { "exit_code": 0 },
            "_meta": { "terminal_output_delta": { "data": "a\nb\n", "terminal_id": "exec-1" } }
        })) else {
            panic!("not an update");
        };
        assert_eq!(done.status, Some(ToolStatus::Completed));
        assert_eq!(done.exit_code, Some(0));
        assert_eq!(done.output, Some(ToolOutput::Delta("a\nb\n".into())));
    }

    #[test]
    fn claude_tool_name_from_meta_and_full_output() {
        let Event::ToolCall(call) = update(json!({
            "sessionUpdate": "tool_call", "toolCallId": "toolu_1", "rawInput": {},
            "status": "pending", "title": "Terminal", "kind": "execute", "content": [],
            "_meta": { "claudeCode": { "toolName": "Bash" } }
        })) else {
            panic!("not a call");
        };
        assert_eq!(call.name.as_deref(), Some("Bash"));
        assert_eq!(call.command, None);

        let Event::ToolCallUpdate(done) = update(json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "toolu_1", "status": "completed",
            "rawOutput": "total 16",
            "content": [{ "type": "content", "content": { "type": "text", "text": "```console\ntotal 16\n```" } }]
        })) else {
            panic!("not an update");
        };
        assert_eq!(done.output, Some(ToolOutput::Full("total 16".into())));
        assert_eq!(done.exit_code, None);
    }

    #[test]
    fn usage_title_mode_and_unknown_kinds() {
        assert_eq!(
            update(json!({ "sessionUpdate": "usage_update", "used": 48405, "size": 1000000,
                           "cost": { "amount": 0.41, "currency": "USD" } })),
            Event::Usage { session_id: "s1".into(), used: 48405, size: 1000000, cost_usd: Some(0.41) }
        );
        assert_eq!(
            update(json!({ "sessionUpdate": "usage_update", "used": 1, "size": 2 })),
            Event::Usage { session_id: "s1".into(), used: 1, size: 2, cost_usd: None }
        );
        assert_eq!(
            update(json!({ "sessionUpdate": "session_info_update", "title": "Fix tests" })),
            Event::Title { session_id: "s1".into(), title: "Fix tests".into() }
        );
        assert!(matches!(
            update(json!({ "sessionUpdate": "session_info_update" })),
            Event::Other { ref kind, .. } if kind == "session_info_update"
        ));
        assert_eq!(
            update(json!({ "sessionUpdate": "current_mode_update", "currentModeId": "plan" })),
            Event::Mode { session_id: "s1".into(), mode_id: "plan".into() }
        );
        assert!(matches!(
            update(json!({ "sessionUpdate": "available_commands_update", "availableCommands": [] })),
            Event::Other { ref kind, .. } if kind == "available_commands_update"
        ));
        assert_eq!(
            update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hi" } })),
            Event::Message { session_id: "s1".into(), text: "hi".into() }
        );
    }

    #[test]
    fn permission_request_options() {
        let req = parse_permission(
            RequestId::for_test(json!("p-1")),
            &json!({
                "sessionId": "s1",
                "toolCall": { "toolCallId": "t1", "title": "rm -rf build", "rawInput": { "command": "rm -rf build" } },
                "options": [
                    { "optionId": "yes", "name": "Allow", "kind": "allow_once" },
                    { "optionId": "always", "name": "Always", "kind": "allow_always" },
                    { "optionId": "no", "name": "Reject", "kind": "reject_once" }
                ]
            }),
        )
        .expect("parsed");
        assert_eq!(req.command.as_deref(), Some("rm -rf build"));
        assert_eq!(req.option(PermissionKind::AllowOnce).map(|o| o.id.as_str()), Some("yes"));
        assert_eq!(req.option(PermissionKind::RejectOnce).map(|o| o.id.as_str()), Some("no"));
        assert_eq!(req.option(PermissionKind::RejectAlways), None);
    }

    #[test]
    fn new_session_reads_modes_and_flat_or_grouped_config() {
        let s = NewSession::from_value(&json!({
            "sessionId": "abc",
            "modes": { "currentModeId": "agent", "availableModes": [{ "id": "read-only" }, { "id": "agent" }] },
            "configOptions": [
                { "id": "effort", "name": "Effort", "currentValue": "high",
                  "options": [{ "value": "low" }, { "value": "high" }] },
                { "id": "model", "name": "Model", "currentValue": "m1",
                  "options": [{ "group": "g", "name": "G", "options": [{ "value": "m1" }, { "value": "m2" }] }] },
                { "id": "fast", "name": "Fast", "type": "boolean", "currentValue": false }
            ]
        }))
        .expect("parsed");
        assert_eq!(s.session_id, "abc");
        assert_eq!(s.current_mode.as_deref(), Some("agent"));
        assert_eq!(s.modes, ["read-only", "agent"]);
        assert_eq!(s.config[0].values, ["low", "high"]);
        assert_eq!(s.config[1].values, ["m1", "m2"]);
        assert_eq!(s.config[2].current.as_deref(), Some("false"));
    }

    #[test]
    fn prompt_outcome_stop_reasons() {
        let o = PromptOutcome::from_value(&json!({
            "stopReason": "end_turn",
            "usage": { "inputTokens": 6, "outputTokens": 317, "cachedReadTokens": 86459, "totalTokens": 135110 }
        }))
        .expect("parsed");
        assert_eq!(o.stop, StopReason::EndTurn);
        assert_eq!(o.output_tokens, Some(317));
        assert_eq!(
            PromptOutcome::from_value(&json!({ "stopReason": "paused" })).map(|o| o.stop),
            Some(StopReason::Other("paused".into()))
        );
        assert_eq!(PromptOutcome::from_value(&json!({})), None);
    }
}
