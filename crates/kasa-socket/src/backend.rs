//! Behavior delegation. The protocol crate owns framing and dispatch;
//! the embedding host (kasaterm, kasaterm-sugarloaf-cli, etc.) plugs in
//! a `Backend` that translates method calls into actual terminal
//! operations.
//!
//! The trait is intentionally small. Methods that don't have a concrete
//! mapping yet (notifications, sidebar metadata) return a default
//! "unsupported" error from the dispatcher rather than forcing every
//! backend to stub them out — the trait grows as the feature set does.

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Direction passed to `Backend::split_surface`. Mirrors cmux's
/// `surface.split` `direction` parameter exactly so the JSON enum
/// values are stable wire shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    Left,
    Right,
    Up,
    Down,
    /// 방향을 부른 쪽이 안 정한다 — 쪼갤 pane 의 종횡비를 보고 **긴 축**을 쪼갠다.
    /// 결정은 GUI 스레드에서만 가능하다(pane 픽셀 크기를 거기서만 안다).
    Auto,
}

/// A workspace as seen by the protocol — analogous to a tmux session
/// or a cmux workspace. Returned by `workspace.list` /
/// `workspace.current`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: String,
    pub name: String,
}

/// A surface (pane) inside a workspace. Returned by `surface.list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceInfo {
    pub id: String,
    pub workspace_id: String,
    /// Optional pane title. cmux populates this from the OSC 0/2 the
    /// inner shell emits; we forward whatever tmux-bridge captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 그 pane 의 작업 폴더.
    ///
    /// board 에도 있지만 board 는 **transcript 가 바인딩된 pane 만** 싣는다 — codex 나
    /// 셸뿐인 pane 은 아예 줄이 없다. `dismiss` 가 닫기 전에 커밋 안 된 변경을 보는
    /// 근거가 그 cwd 라, board 만 보면 그 pane 들은 **보호 없이 닫힌다**.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// 배정된 학생 이름. 위와 같은 이유로 여기에도 싣는다 — 무엇을 닫는지 사람이
    /// 읽을 수 있어야 한다(board 미스 시 `dismiss` 가 `?` 만 찍었다).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
}

/// serde 기본값용 — 옛 기록(하네스 필드가 없던 시절)은 전부 claude 였다.
fn harness_claude() -> String {
    "claude".to_string()
}

/// A past Claude session discoverable for `claude --resume`, built from the
/// transcript jsonl files under `~/.claude/projects/<encoded-cwd>/`. The
/// arona-ui lists these so the user can pick one to continue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentSession {
    /// 어느 하네스의 세션인가 — `"claude"` | `"codex"` | `"agy"`. 이어가는 명령이
    /// 셋 다 달라서(`claude --resume` / `codex resume` / `agy --conversation`)
    /// 목록을 합칠 때 이 값이 없으면 무엇으로 여는지 알 수가 없다.
    #[serde(default = "harness_claude")]
    pub harness: String,
    /// Claude session uuid (the jsonl file stem) — pass to `claude --resume`.
    pub id: String,
    /// Human-readable label: the session's aiTitle, else its first user
    /// message, else the short id.
    pub label: String,
    /// Last-modified unix seconds — for "n minutes ago" + newest-first sort.
    pub mtime: u64,
    /// Absolute cwd the session ran in.
    pub cwd: String,
    /// 마지막으로 오간 말 한 줄(`나: …` / `에이전트: …`). 제목은 세션이 무엇으로
    /// **시작했나**를 말할 뿐이라, 목록을 훑을 때 "어디서 멈췄지"가 안 보인다 —
    /// 그 칸을 채우는 값이다. 못 뽑으면 빈 문자열이고 그때는 아예 안 실어 보낸다
    /// (웹뷰가 `preview?: string` 으로 받아 없으면 그 줄을 안 그린다).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview: String,
    /// 이 세션을 맡았던 학생 이름. **여기서 채우지 않는다** — 세션→캐릭터 표는
    /// `kasa-mcp` 가 쥐고 있고 그 crate 가 이 crate 를 의존하므로, 여기서 부르면
    /// 순환이 된다. 목록을 받은 쪽(app)이 채워 넣는 칸이고, 매핑이 없는 옛 세션은
    /// 빈 문자열로 남는다 — 짐작해 아무나 붙이면 남의 대화가 남의 얼굴로 보인다.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub student: String,
}

/// Multi-session (tmux-style tab) state for the session panel. `count`
/// is the total number of live sessions; `active` is the index of the
/// currently visible one. Default is a single session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsInfo {
    pub count: usize,
    pub active: usize,
    /// Saved (persisted) sessions from the last shutdown. Surfaced in the
    /// session panel so the user can manually restore them — auto-restore
    /// at launch was retired in favour of "light-launch fresh single pane".
    /// Each entry is a short label (typically the first leaf's cwd basename)
    /// the panel renders as a one-click restore row. Empty when no saved
    /// state is on disk.
    #[serde(default)]
    pub saved: Vec<String>,
    /// Per-session display labels for the LIVE sessions the daemon is
    /// hosting right now — typically each session's active-window
    /// first-pane cwd basename — so the panel shows folder names instead
    /// of "세션 1/2". Parallel to the live session list (index = session
    /// idx); empty falls back to ordinal labels. Distinct from `saved`,
    /// which lists on-disk COLD sessions from a previous shutdown.
    #[serde(default)]
    pub labels: Vec<String>,
}

impl Default for SessionsInfo {
    fn default() -> Self {
        Self {
            count: 1,
            active: 0,
            saved: Vec::new(),
            labels: Vec::new(),
        }
    }
}

/// One pane's self-reported activity, published to a shared board so
/// sibling panes can coordinate without a human relaying between them:
/// avoid editing the same file, wait out a neighbour's build, or notice
/// two panes are chasing the same problem and join forces. Pure
/// metadata — nothing here touches terminal I/O. Returned by
/// `collab.board`, filled by the transcript watcher.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaneActivity {
    pub surface_id: String,
    /// The pane session's auto-generated title (`ai-title` line in the
    /// transcript), e.g. "Git 패널 stage 기능". One line saying what this pane
    /// is *for*, as a whole — the board's headline label. Empty until claude
    /// names the session.
    #[serde(default)]
    pub title: String,
    /// The latest user prompt (`last-prompt` line), i.e. what this pane was
    /// just told to do. Empty if nothing's been asked yet.
    #[serde(default)]
    pub last_prompt: String,
    /// The pane's most recent assistant reply text (trimmed/clipped), so the
    /// board shows what claude last *said*, not just what tool it ran.
    #[serde(default)]
    pub last_reply: String,
    /// The most recent tool call as a short label ("Edit auth.ts"), derived
    /// from the transcript's last `tool_use`. What the pane is touching right
    /// now; pairs with `files` for conflict detection.
    pub intent: String,
    /// Coarse state for at-a-glance scanning: conventionally one of
    /// "working" | "building" | "blocked" | "idle", but free text is
    /// allowed so a pane can be specific ("running test suite").
    ///
    /// ⚠️ **"free text 허용"을 믿고 값을 늘리지 마라.** 실제 소비부는 **정확 일치**로
    /// 비교한다(2026-08-05 실측 4곳): `auxwin.rs`(busy 바·펄스 둘), `chrome.rs`
    /// (`== "waiting"`), `handler.rs`(창 단위 working 판정). "working 12s" 같은 값을
    /// 넣는 순간 그 표시들이 통째로 죽는다 — 문서와 코드가 어긋난 자리다. 부가 정보는
    /// 이 칸에 얹지 말고 전용 칸을 늘려라(`rate_used_pct` 가 그렇게 생겼다).
    pub status: String,
    /// Files this pane is currently touching. The conflict-detection
    /// signal: a sibling checks the board before editing and backs off
    /// if a path it wants is already claimed here.
    #[serde(default)]
    pub files: Vec<String>,
    /// The pane's visible screen tail as plain text — only filled when a
    /// caller asks (`collab.board {screen_lines: N}`). Lets an orchestrator
    /// pane read what a sibling is showing (a prompt it's stuck on, an
    /// AskUserQuestion menu) straight from the board, without a separate
    /// `surface.peek` per pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
    /// 이 pane 에 **어떻게 말을 걸 수 있나** — `"message"`(cross-session
    /// SendMessage 로 닿는다) · `"tell"`(명부에 없어 입력창 주입뿐) ·
    /// `"stale"`(명부엔 있는데 소켓이나 프로세스가 없다).
    ///
    /// 이 칸이 있는 이유: `SendMessage` 의 성공 응답은 **도달 증명이 아니다**.
    /// 죽은 상대에게 보내도 "Message sent" 가 오고, 이름이 어긋나면 오류 없이
    /// 사라진다. 그래서 보내기 전에 볼 자리가 필요하다 — 2026-08-10 새벽에
    /// 같은 캐릭터 pane 이 둘이라 엉뚱한 쪽에 브리프를 보냈고, 정작 상대는
    /// 명부에 없어 애초에 닿지도 않았다.
    #[serde(default)]
    pub reach: String,
    /// 명부에 등록된 **그 세션의 실제 이름** — `SendMessage` 의 `to` 에 그대로
    /// 넣을 값이다. `agent_name` 과 다를 수 있다(`/rename` 하면 명부 쪽만 바뀐다).
    /// 이름을 규칙으로 짐작하면 어긋나므로 실측값만 싣는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_name: Option<String>,
    /// 이 pane 에 배정된 캐릭터명(아로나 모드 테마) — assign-character 가 박은
    /// `/tmp/kasaterm-collab/<slug>/character-<N>` 마커 내용. arona-ui 가 교실
    /// 도트칩 이름표에 쓴다(title 은 작업 제목이라 캐릭터명이 아니다). 캐릭터
    /// 테마 없는 방은 None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
    /// 이 pane 의 claude 가 팀원으로 떠 있을 때의 하네스 이름·팀(shim 이 pane 셸에
    /// export 하는 `KASATERM_AGENT`/`KASATERM_TEAM`). 둘 다 있으면 그 pane 은
    /// `teams/<team>/inboxes/<agent>.json` 을 폴링하므로 **인박스로 말을 걸 수 있다**
    /// — 입력창에 텍스트를 밀어넣지 않고 SendMessage 와 같은 경로가 열린다. 이름을
    /// 규칙(`<슬러그>-p<번호>`)으로 추측하면 어긋난 순간 고아 인박스가 되므로 실측값만
    /// 싣는다. 트리플 없이 뜬 pane(비-claude·다른 방)은 None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// 이 pane 이 돌리는 하네스(`"claude"` | `"codex"`). 셸이면 None.
    ///
    /// **말 거는 법이 갈리는 자리다.** codex 엔 팀 모드도 인박스도 없어
    /// `agent_name`/`team` 이 영영 None 이고, SendMessage 는 실패하지 않고 **조용히
    /// 사라진다** — 오케스트레이터가 헛되이 쏘고 답을 기다리게 된다. 그래서 board 가
    /// 종류를 밝힌다: `"codex"` 면 `kasaterm-cli tell`(입력창 주입)이 유일한 경로다.
    /// `agent_name` 이 None 인 것만으로는 "트리플 없이 뜬 claude" 와 구별이 안 된다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Why this pane is `status == "waiting"` — the `waitingFor` field from
    /// `claude agents --json` (2.1.162+), e.g. "permission" or "user input".
    /// The transcript watcher can't see this: when claude blocks on a
    /// permission prompt it writes nothing, so the watcher would read the
    /// pane as idle. Only the official `agents --json` poll knows, so this
    /// is always agents-sourced and overrides the watcher's guess. `None`
    /// unless `status == "waiting"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
    /// 무엇을 기다리나 — "permission"(승인) · "question"(질문·선택) · "idle"(답 없이
    /// 60초 넘게 방치). claude 의 Notification 훅이 준 종류, 화면 감지 승인은 permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_kind: Option<String>,
    /// idle 로 들어온 뒤 흐른 초 — 「방금 끝냈다」와 「한참 쉼」을 가른다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<u64>,
    /// P3 — cumulative `message.usage` over the transcript tail window. The orchestrator
    /// reads these to spot an over-budget / runaway pane and steer the fleet.
    #[serde(default)]
    pub tokens_in: u64,
    #[serde(default)]
    pub tokens_out: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_creation: u64,
    /// Estimated USD cost over the tail window (tokens × per-model rate).
    #[serde(default)]
    pub cost_usd: f64,
    /// Tool-use counts over the tail window, e.g. `[("Edit",3),("Bash",5)]`.
    #[serde(default)]
    pub tool_counts: Vec<(String, u32)>,
    /// Every file Edit/Write-touched over the tail window — the orchestrator's
    /// change-set view ("who changed what"). A superset of `files` (the single
    /// most-recent edit, kept for conflict detection).
    #[serde(default)]
    pub changed_files: Vec<String>,
    /// In-flight sub-agents — Task/Agent `tool_use` blocks in the tail window
    /// with no matching `tool_result` yet, as their short descriptions. The
    /// classroom surfaces "서브에이전트 N 실행 중" so a student spawning helpers
    /// is visible. Empty when the pane has launched none (or all completed).
    #[serde(default)]
    pub subagents: Vec<String>,
    /// 최근 완료된 서브에이전트(이름) — tail 윈도 안에서 tool_result 매칭된 Task/Agent.
    /// 끝나면 즉시 사라지던 걸 잠깐 "✓ 완료"로 남겨 흔적을 보인다.
    #[serde(default)]
    pub subagents_done: Vec<String>,
    /// In-flight 백그라운드 셸 — `run_in_background` Bash 중 완료 통보
    /// (`<task-notification>`)가 아직 없는 것들의 설명/명령. 백그라운드 지속 작업이
    /// 최신 tool_use 하나에 덮여 안 보이던 사각지대를 메운다.
    #[serde(default)]
    pub background: Vec<String>,
    /// 최근 도구 사용 흐름(라벨, 최신순) — tail 윈도의 마지막 N개 tool_use. 지금
    /// "현재 도구 하나"만 보이던 걸 타임라인(Bash→Edit→Bash)으로.
    #[serde(default)]
    pub recent_tools: Vec<String>,
    /// 학생(claude) 모델명 — transcript `message.model`(예 "claude-opus-4-8"). 빈값=미상.
    #[serde(default)]
    pub model: String,
    /// 작업 경로 — transcript 의 `cwd` 필드(절대경로). 빈값=tail 에 cwd 줄 없음.
    #[serde(default)]
    pub cwd: String,
    /// statusLine 이 보고한 "현재 보는 경로" — claude 는 셸 위에서 돌아 lsof(cwd)로는
    /// 내부 cd 가 안 보여, statusline.py 가 매 렌더 `report-cwd` 로 직접 보고한다.
    /// cwd(=claude 프로세스 실행 경로, 고정)와 함께 푸터 "실행/현재 보는" 두 경로(거노).
    #[serde(default)]
    pub view_cwd: String,
    /// claude saved default effort(~/.claude/settings.json `effortLevel`) — resume 직후엔 현재
    /// 세션의 /effort stdout 이 jsonl 에 없어 GUI effort 카드가 빈값이 됐다(거노: resume 후 effort
    /// 만 뜸). 그 폴백값. ultracode 는 "this session only"라 여기 안 들어와 잔존하지 않는다.
    #[serde(default)]
    pub effort_default: String,
    /// 모델 컨텍스트 한도(토큰). 200k 또는 1M(fable/mythos·[1m] 변형·관측 초과). 0=모델 미상.
    #[serde(default)]
    pub context_limit: u64,
    /// 컨텍스트 사용량 % — claude TUI 상태바("… ┃ 5% ┃ …")에서 파싱. transcript
    /// 토큰(tokens_in/out)은 claude 가 jsonl 을 라이브로 안 쓰면 0 이라, PTY 화면을
    /// 소유한 우리가 상태바에서 직접 읽는다(robust). 0=상태바에 % 없음/미상.
    #[serde(default)]
    pub context_pct: u8,
    /// 최신 assistant 턴의 컨텍스트 점유 토큰(input+cache_read+cache_creation). context_pct
    /// 의 원자료 — socket.rs 가 상태바 모델로 1M 한도를 확정하면 이 값으로 pct 를 재계산한다.
    #[serde(default)]
    pub context_tokens: u64,
    /// git 브랜치 — pane cwd 에서 rev-parse. None=git repo 아님/미상. collab_board 가 채움.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// 이 pane 이 속한 kasaterm 윈도우(=방) 인덱스. board 가 활성 윈도우뿐 아니라 전
    /// 윈도우 pane 을 실으면서 arona-ui 가 방별로 학생을 그룹핑하게 한다(거노: 좌측 통합).
    /// 0 기본 — board 빌더(socket.rs)가 윈도우별로 채운다.
    #[serde(default)]
    pub window_idx: usize,
    /// 구독 한도 사용률(%) — **codex 전용**, claude pane 은 None.
    ///
    /// 거노 2026-08-05: codex 는 정액제라 비용($)이 무의미하고(그래서 board 비용 칸은
    /// `—`), 실제로 알고 싶은 건 "얼마나 썼나 / 언제 리셋되나"다. 화면 모양:
    /// ```text
    /// claude   $126.02  ctx 50%
    /// codex     —       ctx 7%   주간 62% (3h 뒤 리셋)
    /// ```
    /// 창이 여럿이면 **가장 먼저 터질 것**(사용률 최대) 하나만 싣는다. 실측(2026-08-05,
    /// 78표본)에선 `primary` 주간창(10080분) 하나뿐이고 `secondary` 는 늘 null 이었다.
    #[serde(default)]
    pub rate_used_pct: Option<f32>,
    /// 위 사용률이 어느 창의 것인지(분). 10080 = 주간. 표시 라벨을 여기서 정한다 —
    /// 창 종류가 늘어도 코드를 안 고치게 이름 대신 숫자를 싣는다.
    #[serde(default)]
    pub rate_window_minutes: Option<u32>,
    /// 그 창이 리셋되는 절대 시각(unix 초). **표시는 상대 시간으로** 바꿔라(거노) —
    /// 절대 시각은 읽는 사람이 매번 뺄셈을 해야 한다.
    #[serde(default)]
    pub rate_resets_at: Option<i64>,
    /// 구독 플랜 이름(실측 "plus"). 같은 사용률도 플랜에 따라 뜻이 달라 함께 싣는다.
    #[serde(default)]
    pub plan_type: Option<String>,
    /// 명시적 완료 보고(`pane_done`) — "succeeded" | "failed". None=보고 없음.
    /// 새 브리프를 받아 다시 working 이 되면 board 빌더가 지운다(스테일 방지).
    /// status 칸과 별개인 이유: status 는 "지금 뭘 하나"(순간), 이건 "맡은 일이
    /// 어떻게 끝났나"(결과)라 겹쳐 쓰면 정확 일치 소비부가 죽는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_outcome: Option<String>,
    /// 완료 보고 한 줄 요약 — 뭘 했고 뭐가 남았는지. done_outcome 없이는 안 실린다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_summary: Option<String>,
    /// 보고 후 경과 초 — UI 가 "3분 전 완료" 상대 표시를 하도록 절대시각 대신 나이.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_ago_secs: Option<u64>,
    /// **화면에 없는 pane**(사용자가 닫았거나 숨김 — PTY 는 재부착 대비로 돈다).
    /// 사용자가 닫은 pane 은 화면에서 사라졌는데 board 에는 멀쩡히 떠서, 학생들이
    /// 거기가 닫힌 줄 모르고 새 일을 시켰다(거노 2026-08-15 「내가 pane 닫아도
    /// 너네한텐 안 보이고 살아있어서 거기다가 시킨다」). 참이면 **새 일을 시키지
    /// 말 것** — 이어받으려면 사람이 되살리기로 화면에 꺼낸 뒤에.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub detached: bool,
    /// 이 pane 이 **다른 기계**의 원격 링크일 때 그 기계 이름. 로컬 pane 은 None.
    /// remoteboard 가 원격 board 행에 심는 "machine" 과 같은 키다 — 프론트가
    /// 로컬/원격 행을 한 코드로 읽는다. 키 이름을 status 에 얹지 않는 이유:
    /// status 소비부가 정확 일치 비교라 문자열을 오염시키면 그쪽이 죽는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

/// One live session from `claude agents --json` (Claude Code 2.1.162+).
/// Only the fields we consume are named; serde ignores the rest (`pid`,
/// `cwd`, `kind`, `startedAt`, `name`). `sessionId` is the join key: it
/// equals the stem of the pane's bound transcript path
/// (`~/.claude/projects/<cwd>/<sessionId>.jsonl`), so the watcher maps a
/// session back to its pane without tracking pids.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentStatus {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// "idle" | "busy" | "waiting".
    pub status: String,
    /// What a `waiting` session is blocked on (e.g. "permission"). 2.1.162+.
    #[serde(rename = "waitingFor", default)]
    pub waiting_for: Option<String>,
}

/// Parse `claude agents --json` stdout into a `sessionId → AgentStatus` map.
/// Pure (string in, map out) so the watcher's subprocess plumbing stays
/// separate from the parse and the parse is unit-testable. Any parse
/// failure or empty output yields an empty map — the caller then leaves the
/// transcript-derived status untouched (fail safe, never worse than today).
pub fn parse_agents_json(stdout: &str) -> std::collections::HashMap<String, AgentStatus> {
    serde_json::from_str::<Vec<AgentStatus>>(stdout)
        .map(|v| v.into_iter().map(|a| (a.session_id.clone(), a)).collect())
        .unwrap_or_default()
}

/// One pane's rectangle in the visible window, as percentages (0..100) of
/// the window's width/height. Percentages rather than cells so a caller
/// (claude deciding where to open a result pane, say) can reason about
/// "right half / top third" without knowing the pixel size. `x,y` is the
/// top-left corner; `w,h` the size. Returned by `window.layout`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaneRect {
    pub surface_id: String,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// Shell cwd of the pane (host-resolved), for the BA GUI to render a
    /// Warp-style status bar on plain (non-claude) terminal tiles. `None` for
    /// callers/backends that don't track per-pane cwd (e.g. ASCII `layout`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// git badge of `cwd` — branch + tree diff summary. `None` outside a repo
    /// or before the badge poller has run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insertions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u32>,
}

/// One shell command block of a pane (Warp-style), delimited by OSC 133 C/D
/// shell-integration marks. The GUI renders a stack of these for plain
/// (non-claude) terminal tiles. Mirrors `kasa_pty::CommandBlock` over the
/// wire. Returned by `/blocks`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaneBlock {
    pub id: u64,
    pub command: String,
    pub output: String,
    /// None while the command is still running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Epoch milliseconds at command start (HISTORY relative time).
    pub started_ms: u64,
    /// Wall-clock command duration; None while running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// The command entered an alt-screen (vim/htop) — GUI falls back to a live
    /// peek instead of the raw block output.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_tui: bool,
}

/// One window in the active session, with its panes and their rects.
/// `surface.list` and `window.layout` only ever expose the *active*
/// window, but the daemon holds every window — this lets an agent inspect
/// a window it isn't currently viewing ("what's in window 1, who's there,
/// how is it split"). `idx` matches the left sidebar's window order.
/// Returned by `window.list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowOverview {
    pub idx: usize,
    pub active: bool,
    pub surfaces: Vec<String>,
    pub panes: Vec<PaneRect>,
    /// pane 영역의 가로÷세로(픽셀). 사각형은 백분율뿐이라 이것이 없으면 폰 미니맵이
    /// 창 모양과 무관한 납작한 상자가 된다. 창이 없으면(헤드리스) 생략.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect: Option<f32>,
}

/// Plug point for terminal operations. Host apps implement this on a
/// type that already owns the tmux session / portable-pty handle and
/// the renderer state.
/// 캐릭터 한 명을 저장하라는 요청. **준 것만** 바꾼다(빠뜨린 필드는 그대로).
///
/// 인자를 늘어놓지 않고 묶은 것은 이 요청이 다섯 자리(웹 핸들러 → trait → GUI
/// 구현 → 이벤트 → 처리부)를 그대로 통과해서다 — 필드를 하나 더할 때마다 다섯
/// 군데의 시그니처를 맞춰야 하고, 하나를 빠뜨리면 그 값만 조용히 안 넘어간다.
#[derive(Debug, Default, Clone)]
pub struct CharacterSave {
    pub name: String,
    pub persona: Option<String>,
    pub new_name: Option<String>,
    /// `--model` 값. 빈 문자열은 "지정 없음"이라 전역 설정으로 떨어진다.
    pub model: Option<String>,
    /// 실행 통로(kimi·glm). 빈 문자열이면 순정 claude.
    pub backend: Option<String>,
    /// 정의 전체를 적은 글 — 원본 뷰 저장. 있으면 위 낱개 필드 대신 이것으로
    /// 통째 갈아 끼운다(원본에서 지운 줄이 사라지려면 통째 교체여야 한다).
    ///
    /// 파싱된 map 이 아니라 **글자 그대로** 받는 이유는 YAML 때문이다. 파싱을
    /// 부르는 쪽에 맡기면 화면마다 다른 파서를 쓰게 되고, 그러면 같은 글이
    /// 화면에 따라 다르게 저장된다.
    pub raw: Option<String>,
    /// `raw` 가 YAML 인가(false = JSON).
    pub raw_yaml: bool,
}

pub trait Backend: Send + Sync {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>>;
    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>>;
    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>>;
    /// List recent Claude sessions under `cwd` (or the backend's current cwd
    /// when None) so the user can pick one to `claude --resume`. Newest first.
    /// Default: none (backends without transcript access).
    fn recent_sessions(&self, _cwd: Option<&str>) -> Result<Vec<RecentSession>> {
        Ok(Vec::new())
    }
    /// Open a pane and resume session `id` in it: `newroom=true` opens a fresh
    /// window, otherwise it splits the active one; the pane is spawned in `cwd`
    /// when given. The resume command is injected once the new pane's shell
    /// prompt is up. Default: unsupported.
    ///
    /// `harness` 는 그 세션을 만든 코딩 프로그램(`claude`/`codex`/`agy`). 빈 문자열이면
    /// claude 로 본다. 주입 문자열은 [`crate::sessions::resume_command`] 한 곳에서만
    /// 만든다 — 예전에 CLI 와 GUI 가 각자 `claude --resume` 을 조립하다 한쪽만 고쳐진
    /// 적이 있어 일부러 한 벌로 합쳤다.
    fn resume_session(
        &self,
        _id: &str,
        _cwd: Option<&str>,
        _newroom: bool,
        _attach: bool,
        _harness: &str,
    ) -> Result<()> {
        anyhow::bail!("resume_session unsupported by this backend")
    }
    /// "대화 저장하기" — foreground claude 를 ←← 주입으로 background daemon 으로 detach.
    /// `surface` 없으면 active pane. Default: unsupported.
    fn save_session(&self, _surface: Option<&str>) -> Result<()> {
        anyhow::bail!("save_session unsupported by this backend")
    }
    fn focus_surface(&self, surface_id: &str) -> Result<()>;
    /// Split a surface. `from` is the pane to split — `None` means "the focused
    /// one", which is only right for a human at the keyboard. An agent calling
    /// from its own pane passes its id, otherwise the split lands wherever the
    /// human happens to be looking.
    ///
    /// `focus` decides whether the *new* pane becomes active: CLI/automation
    /// callers pass `false` so a scripted split doesn't yank the user's focus
    /// (like `tell`, it stays put); the GUI's own keyboard split keeps
    /// focus-follows behavior by going through `layout` directly, not this
    /// method.
    fn split_surface(
        &self,
        direction: SplitDirection,
        focus: bool,
        from: Option<&str>,
    ) -> Result<SurfaceInfo>;
    /// pane 의 PTY 를 로컬 상주 데몬으로 **무중단 이관**(승격). GUI 백엔드 전용.
    fn promote_pane(&self, _pane: &str) -> Result<String> {
        anyhow::bail!("promote: 이 백엔드는 지원하지 않는다")
    }
    /// pane 의 claude 를 다른 기계의 PTY 호스트로 **이사** — 대화 jsonl 운반 +
    /// 같은 자리 원격 스왑 + resume 주입. GUI 백엔드 전용.
    /// `run` 은 태생 스폰(에이전트 없는 셸 pane) 전용 — 저쪽에서 claude 대신
    /// 돌릴 명령(`mini codex`). 이사(대화 운반)에는 뜻이 없어 무시된다.
    fn migrate_pane(
        &self,
        _pane: &str,
        _base: &str,
        _cwd: Option<&str>,
        _force: bool,
        _run: Option<&str>,
    ) -> Result<String> {
        anyhow::bail!("migrate: 이 백엔드는 지원하지 않는다")
    }
    /// 역이사 — 원격 pane 의 claude 를 이 기계로 데려온다. `cwd` 는 **이 기계 기준**
    /// 도착 경로(생략 시 이사 때 저장한 원래 자리 → machines.json roots 매핑 순).
    fn migrate_pane_back(&self, _pane: &str, _cwd: Option<&str>, _force: bool) -> Result<String> {
        anyhow::bail!("migrate back: 이 백엔드는 지원하지 않는다")
    }
    /// 기계 하나의 학생 pane 전부를 이 창의 거울로 **펼친다** — 원격 방(창)마다
    /// 이쪽에도 새 창을 만들어 같은 묶음으로 미러링한다. GUI 백엔드 전용.
    fn unfold_machine(&self, _label: &str) -> Result<String> {
        anyhow::bail!("unfold: 이 백엔드는 지원하지 않는다")
    }
    /// 명부의 본진(home:true) 기계 — `Some((라벨, 지금 닿는가))`, 미설정이면
    /// `None`. 셰임의 순정 `claude` 디스패치가 이걸 물어 태생지를 고른다.
    fn home_machine(&self) -> Result<Option<(String, bool)>> {
        Ok(None)
    }
    /// 명부 기계 목록 — `to`(기계를 cd 처럼 오가는 셰임)의 `ls`. `from` 은 묻는
    /// pane — 그 pane 이 어느 기계의 거울이면 응답의 `here` 가 그 라벨이다.
    /// 응답: `{here, machines:[{label, online, ago_secs, students, waiting, mirrored}]}`.
    fn list_machines(&self, _from: Option<&str>) -> Result<serde_json::Value> {
        anyhow::bail!("machines: 이 백엔드는 지원하지 않는다")
    }
    /// pane → 작업 폴더. `/term/panes` 의 cwd 폴백 — board(claude 바인딩)가 못
    /// 싣는 순수 셸 pane 의 cwd 를 셸 pid 에서 직접 읽는다. GUI 백엔드 전용.
    fn pane_cwds(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// 원격 PTY 호스트(`kasa-serve-web`)의 세션을 이 창의 pane 으로 앉힌다 —
    /// 스폰(`pane` 없음) 또는 이어받기(`pane` = `web-…`). GUI 백엔드만 구현한다.
    /// `here` 면 기준 pane 옆에 새로 쪼개지 않고 **그 pane 자체**를 원격 셸의
    /// 거울로 갈아끼운다(`mini` 한 마디 = 이 자리가 맥미니 터미널).
    fn remote_pane(
        &self,
        _base: &str,
        _cwd: Option<&str>,
        _pane: Option<&str>,
        _from: Option<&str>,
        _here: bool,
    ) -> Result<SurfaceInfo> {
        anyhow::bail!("remote_pane: 이 백엔드는 원격 pane 을 지원하지 않는다")
    }
    /// pane `count` 개를 **한 번의 호출로** 배치한다 — 부른 pane 이 크게 남고 나머지가
    /// 균등하게 선다.
    ///
    /// `split_surface` 를 N 번 부르는 것과 결과가 다르다. 그쪽은 회차마다 대상이
    /// 직전에 만든 pane 으로 이어져 몫이 1/2 → 1/4 → 1/8 로 반감한다 — 넷을 부르면
    /// 마지막이 화면의 1/16 이고, 사람이 매번 드래그로 고쳐 왔다.
    ///
    /// 반환은 **실제로 앉힌** pane 들이다. 쓸 만한 크기의 하한(80칸·16줄)에 걸려
    /// 요청보다 적을 수 있으므로, 부른 쪽은 개수를 대조해 사람에게 알려야 한다.
    /// 디폴트가 unsupported 인 이유는 tmux/원격 백엔드가 트리를 소유하지 않아서다.
    fn split_fleet(
        &self,
        _count: usize,
        _from: Option<&str>,
        _host_ratio: Option<f32>,
    ) -> Result<Vec<SurfaceInfo>> {
        anyhow::bail!("split_fleet unsupported by this backend")
    }
    /// 그 pane 이 claude 를 띄우면 **쓰게 될** teammate 이름과 팀 — `(agent, team)`.
    ///
    /// 예측이 가능한 이유: 학생은 pane 이 생길 때 배정되고(`assign_character_env`),
    /// 셰임은 그 학생 슬러그에 `-p<번호>` 를 붙일 뿐이다. 그래서 **부팅을 기다리지
    /// 않고** split 응답에 실어 보낼 수 있고, 부른 쪽은 곧바로 SendMessage 로 브리프를
    /// 보낼 수 있다(인박스 파일은 셰임이 `[ -f ] ||` 로 만들어 먼저 쓴 걸 안 덮는다).
    /// 이게 없으면 오케스트레이터가 board 를 되짚거나 이름을 짐작하는데, 짐작은
    /// 어긋나도 오류가 안 나고 지시가 조용히 사라진다.
    ///
    /// 팀은 pane 의 cwd 로 계산한다 — 학생을 **다른 레포로 `cd` 시켜** 띄우면 그
    /// 학생의 실제 팀은 달라지므로 이 값이 틀린다. 그 경우는 board 가 정본이다.
    fn pane_agent(&self, _surface_id: &str) -> Option<(String, String)> {
        None
    }
    /// 되살리기 목록을 읽고, `discard` 가 있으면 그 pane 을 **진짜 끈다**.
    ///
    /// 닫은 pane 은 죽지 않는다 — 프로세스를 물고 이 목록에 앉아 있다가 밀려날 때
    /// 비로소 죽는다. 오케스트레이터가 `dismiss` 로 정리한 학생들이 그래서 계속 살아
    /// 있는데, GUI 밖에서는 그 사실을 볼 수도 끌 수도 없었다(거노 2026-08-06).
    fn closed_panes(&self, _discard: Option<&str>) -> Result<serde_json::Value> {
        anyhow::bail!("이 백엔드는 되살리기 목록을 모른다")
    }
    fn send_text(&self, surface_id: Option<&str>, text: &str) -> Result<()>;
    fn send_key(&self, surface_id: Option<&str>, key: &str) -> Result<()>;
    /// Send raw bytes straight to a surface's PTY (no symbolic-key mapping).
    /// The GUI client forwards key input to a daemon-hosted pane this way so
    /// escapes/UTF-8/control bytes pass through verbatim. Default unsupported.
    fn send_raw(&self, _surface_id: Option<&str>, _bytes: &[u8]) -> Result<()> {
        anyhow::bail!("send_raw unsupported by this backend")
    }
    /// Resize a surface's PTY grid to cols×rows (drives SIGWINCH). Default
    /// unsupported.
    fn resize_surface(&self, _surface_id: &str, _cols: u16, _rows: u16) -> Result<()> {
        anyhow::bail!("resize_surface unsupported by this backend")
    }
    /// Scroll a surface's scrollback by `lines` (negative = toward older
    /// history). Default unsupported.
    fn scroll_surface(&self, _surface_id: &str, _lines: i32) -> Result<()> {
        anyhow::bail!("scroll_surface unsupported by this backend")
    }
    /// Close (kill) a surface by id. Removes its leaf from the layout.
    /// Default: unsupported — layout-managing backends (PTY) override it.
    fn close_surface(&self, _surface_id: &str) -> Result<()> {
        anyhow::bail!("close_surface unsupported by this backend")
    }
    /// Fold a surface into its session's dock — the layout leaf is removed but
    /// the PTY stays alive (kill-free). Default: unsupported.
    fn dock_surface(&self, _surface_id: &str) -> Result<()> {
        anyhow::bail!("dock_surface unsupported by this backend")
    }
    /// Restore a docked surface back into the active window. Default: unsupported.
    fn undock_surface(&self, _surface_id: &str) -> Result<()> {
        anyhow::bail!("undock_surface unsupported by this backend")
    }
    /// Set a surface's header title (rename). Default: unsupported.
    fn rename_surface(&self, _surface_id: &str, _title: &str) -> Result<()> {
        anyhow::bail!("rename_surface unsupported by this backend")
    }
    /// Add a new student pane to the active room with an explicit character
    /// (members or leaders — 아로나/프라나 included). Returns the new pane's
    /// surface id so a caller can immediately address it (the dispatcher sends
    /// the brief there); an empty string means the backend created no pane.
    /// Default: unsupported.
    fn spawn_student(&self, _character: &str) -> Result<String> {
        anyhow::bail!("spawn_student unsupported by this backend")
    }
    /// Swap a pane's character: respawn its PTY with the new persona (the live
    /// claude conversation resets — persona is fixed at shell spawn). Default:
    /// unsupported.
    fn swap_character(&self, _surface_id: &str, _character: &str) -> Result<()> {
        anyhow::bail!("swap_character unsupported by this backend")
    }
    /// Reassign a pane's character *without* respawning its PTY — updates the
    /// header/board/marker/session binding only. The per-student launcher shims
    /// (`시로코`) call this right before starting claude; the persona itself
    /// travels via the shim's override file. Default: unsupported.
    fn repersona(&self, _surface_id: &str, _character: &str) -> Result<()> {
        anyhow::bail!("repersona unsupported by this backend")
    }
    /// 이사(migrate)가 출발지의 캐릭터 테마 선택을 이 기계에 재현한다 —
    /// `character_theme` 설정 + 고른 명단(`character_picks`, `{테마:[이름…]}` JSON).
    /// 캐시(활성 테마·로스터)까지 함께 비워야 하므로 설정 파일을 밖에서 고치는
    /// 것으로는 안 되고, 도는 앱 프로세스 안에서 설정 화면과 같은 경로를 태운다
    /// (2026-08-31 지적 「이사시켜봤는데 테마 적용이 안 되네」). Default: unsupported.
    fn apply_character_theme(&self, _theme_id: &str, _picks_json: &str) -> Result<()> {
        anyhow::bail!("apply_character_theme unsupported by this backend")
    }
    /// 이사가 실어 온 테마 팩 zip 을 이 기계의 `themes/` 에 푼다 — 설정 창 zip
    /// 드롭과 같은 코드(zip slip 검사·임시 폴더 경유·기존 팩 _trash 보존)를 탄다.
    /// 성공 시 풀린 테마 id. Default: unsupported.
    fn import_theme_pack(&self, _zip_path: &std::path::Path) -> Result<String> {
        anyhow::bail!("import_theme_pack unsupported by this backend")
    }
    /// Rename the *window/session* that `surface_id` belongs to (sidebar
    /// session label), independent of the pane header. The rename override uses this
    /// so the session label holds even when that pane isn't the window's
    /// representative (first-leaf) pane. Default: unsupported.
    fn rename_window(&self, _surface_id: &str, _title: &str) -> Result<()> {
        anyhow::bail!("rename_window unsupported by this backend")
    }
    /// Set a surface's accent color (header band), RGBA 0..255.
    /// Default: unsupported.
    fn set_color(&self, _surface_id: &str, _color: [u8; 4]) -> Result<()> {
        anyhow::bail!("set_color unsupported by this backend")
    }
    /// statusLine 이 매 렌더 보고하는 "현재 보는 경로" + 컨텍스트 창/사용 토큰.
    /// claude 가 셸 위에서 cd 해도 lsof(최상위 셸 cwd)로는 안 보여, statusline.py 가
    /// 직접 push 한다. board 의 `view_cwd` 로 노출(GUI 푸터 "현재 보는 경로").
    ///
    /// `ctx_window`/`ctx_tokens` 는 훅 stdin 의 하네스 정본이다(0 = 미보고). transcript
    /// 의 model 엔 `[1m]` 태그가 안 실려(API 응답이 `claude-opus-5`) 모델명으로는 1M
    /// 세션을 가려낼 수 없어, 이 값이 ctx% 분모의 유일한 확정 소스다. 기본: 무동작.
    ///
    /// `model`/`effort` 도 같은 훅 stdin 에서 온다(빈 문자열 = 미보고). 앱을 재시작해
    /// pane 을 복원할 때 **끄기 직전 쓰던 것으로** 되살리는 데 쓴다. ★`model` 은
    /// `model.id` 원본(`claude-opus-5[1m]`)이라 CLI 에 그대로 되먹일 수 있다 — board 에
    /// 뜨는 모델명은 API 응답 표기라 `[1m]` 이 없고, 그걸 되먹이면 1M 세션이 200k 로
    /// 강등된다. 그래서 board 쪽 값과 **의도적으로 다른 경로**를 쓴다.
    fn report_cwd(
        &self,
        _surface_id: &str,
        _cwd: &str,
        _session_id: &str,
        _ctx_window: u64,
        _ctx_tokens: u64,
        _model: &str,
        _effort: &str,
    ) -> Result<()> {
        Ok(())
    }
    /// Render one pane to a PNG and return the file path.
    ///
    /// `peek` 는 텍스트만 준다 — 에이전트가 제 화면이 실제로 어떻게 보이는지는
    /// 못 본다(2026-08-10 지시). 이 메서드가 그 구멍을 메운다: pane 영역만 잘라
    /// 저장하고, 받는 쪽은 경로를 Read 로 연다.
    ///
    /// GPU 프레임버퍼 리드백이라 **창이 다른 창에 가려져 있어도 찍힌다** —
    /// `screencapture` 와 달리 화면이 아니라 방금 그린 프레임을 읽기 때문이다.
    /// 다만 창이 최소화되면 OS 가 렌더를 멈추므로 그때는 갱신이 서 있다.
    ///
    /// `max_width` = 0 이면 원본 크기. 그 외에는 가로가 그 값을 넘을 때만 비율을
    /// 지켜 줄인다(읽는 쪽 컨텍스트 절약). 기본: 미지원.
    fn capture_surface(
        &self,
        _surface_id: &str,
        _path: Option<&str>,
        _max_width: u32,
    ) -> Result<serde_json::Value> {
        anyhow::bail!("capture_surface unsupported by this backend")
    }
    /// Swap two surfaces' positions in the layout. Default: unsupported.
    fn swap_surfaces(&self, _a: &str, _b: &str) -> Result<()> {
        anyhow::bail!("swap_surfaces unsupported by this backend")
    }
    /// Move `surface_id` beside `target` along `direction` — detach its leaf
    /// and re-insert next to target. The PTY stays alive (pure layout move,
    /// unlike close). Drag-and-drop relocation routes through this so the
    /// daemon stays the layout authority. Default: unsupported.
    fn move_surface(
        &self,
        _surface_id: &str,
        _target: &str,
        _direction: SplitDirection,
    ) -> Result<()> {
        anyhow::bail!("move_surface unsupported by this backend")
    }
    /// `outer` **안에 새 탭**을 연다 — 쪼개지 않으므로 화면이 안 줄어든다. `outer` 가
    /// None 이면 포커스된 pane. 새 탭의 surface 를 돌려준다(부른 쪽이 거기에 명령을
    /// 실어야 한다). Default: unsupported.
    fn new_tab(&self, _outer: Option<&str>, _focus: bool) -> Result<SurfaceInfo> {
        anyhow::bail!("new_tab unsupported by this backend")
    }
    /// Set the split ratio at `path` (the seam the GUI just dragged) so the
    /// daemon — the layout authority — persists it and restores it on restart.
    /// `path` is the tree route to the owning Split node (0 = child a, 1 = b).
    /// Default: unsupported.
    fn resize_divider(&self, _path: &[u8], _ratio: f32) -> Result<()> {
        anyhow::bail!("resize_divider unsupported by this backend")
    }
    /// Make `surface_id` take `ratio` (0..1) of its *immediate* split
    /// container — the pane-addressed cousin of `resize_divider` (which
    /// needs a tree path callers like the CLI don't know). Orchestration
    /// knob: "make the orchestrator pane big" after a fleet regroup. Default:
    /// unsupported.
    fn set_split_ratio(&self, _surface_id: &str, _ratio: f32) -> Result<()> {
        anyhow::bail!("set_split_ratio unsupported by this backend")
    }
    /// Current working directory of the active pane's shell, if the backend
    /// tracks it. Lets the git panel follow the user's terminal directory.
    /// Default `None` (e.g. the tmux backend doesn't track per-pane cwd).
    fn active_cwd(&self) -> Option<std::path::PathBuf> {
        None
    }
    /// Foreground process name of the active pane (e.g. "claude", "zsh").
    /// Lets the AI-commit button decide whether to delegate the commit to a
    /// running claude or fall back. Default `None`.
    fn active_process_name(&self) -> Option<String> {
        None
    }
    /// 활성 pane 이 돌리는 **하네스 종류**(`"claude"` | `"codex"`). 셸이면 None.
    ///
    /// `active_process_name` 으로는 못 갈음한다 — codex 는 npm shim 이라 셸의 직속
    /// 자식이 `node` 라서 이름에 "codex" 가 없다(실측). 판정은 kasa-pty 의
    /// `agent_for_shell` 하나이고 여기선 그 문자열만 건넨다(kasa-socket 은 kasa-pty
    /// 를 안 쓴다). Default 는 None — PTY 를 안 가진 백엔드는 알 길이 없다.
    fn active_agent(&self) -> Option<String> {
        None
    }
    /// Fire a "work complete" notification for a surface. The push half of
    /// the board (which is pull-only): a claude `Stop` hook runs
    /// `kasaterm-cli notify`, the host decides whether to raise a desktop
    /// alert (suppressed when that pane is already focused, cmux-style) and
    /// flashes the pane / sidebar. Default unsupported.
    fn notify(&self, _surface_id: &str, _title: &str, _body: &str) -> Result<()> {
        anyhow::bail!("notify unsupported by this backend")
    }
    /// 클립보드에 글을 넣는다 — **캐릭터가 사람 대신 복사해 주는 문**이다.
    ///
    /// 사람이 직접 끌어서 복사하는 길은 노플리커를 끈 claude 앞에서 막힌다: 그 하네스가
    /// 화면 전체와 마우스를 함께 쥐고 있어 드래그가 터미널까지 오지 않는다(Shift 를
    /// 눌러야 겨우 온다). 그래서 「이 부분 복사해 줘」를 사람이 손으로 하는 대신 캐릭터가
    /// 대신 하도록 길을 낸다(2026-09-05 지시: 「에이전트가 카사텀 조종할 수 있게 해서
    /// 복사하고 클립보드 띄워주는」).
    ///
    /// 넣은 글은 호스트가 화면에 **띄워서** 무엇이 담겼는지 사람이 눈으로 확인하게 한다 —
    /// 클립보드는 보이지 않는 그릇이라, 알리지 않으면 붙여넣기 전까지 맞는지 알 수 없다.
    /// 기본은 미지원.
    fn clipboard_set(&self, _text: &str) -> Result<()> {
        anyhow::bail!("clipboard unsupported by this backend")
    }
    /// 지금 클립보드에 담긴 글. 캐릭터가 「무엇이 들어 있나」를 확인하거나, 사람이 복사해
    /// 둔 것을 받아 이어서 일할 때 쓴다. 기본은 미지원.
    fn clipboard_get(&self) -> Result<String> {
        anyhow::bail!("clipboard unsupported by this backend")
    }
    /// A pane's agent is blocked waiting for the user — a permission prompt or
    /// an idle input prompt — surfaced by claude's `Notification` hook running
    /// `kasaterm-cli attention`. Unlike `notify` (a one-shot push), this marks
    /// the pane as `waiting` so `collab_board` can flag it (the transcript
    /// can't: a blocked claude writes nothing), and raises a desktop alert when
    /// the pane isn't already focused. `reason` is the hook's message (e.g.
    /// "permission" / the prompt text); empty is fine. Default unsupported.
    fn attention(&self, _surface_id: &str, _reason: &str) -> Result<()> {
        anyhow::bail!("attention unsupported by this backend")
    }

    /// `attention` 에 종류를 얹은 것 — "permission"(승인) · "question"(질문·선택) ·
    /// "idle"(답 없이 방치). 기본은 종류를 버리고 `attention` 으로 넘긴다.
    fn attention_kind(&self, surface_id: &str, _kind: &str, reason: &str) -> Result<()> {
        self.attention(surface_id, reason)
    }
    /// 학생의 명시적 완료 보고 — 브리프를 마친 pane 이 `kasaterm-cli done` 으로
    /// 부른다. board 의 완료 판정을 idle 추정에서 자기 보고 정본으로 바꾸는 자리:
    /// transcript 휴리스틱은 "놀고 있음"만 알지 "성공/실패로 끝났음"은 모른다.
    /// `outcome` 은 "succeeded" | "failed" 둘뿐(자유 텍스트 금지 — status 칸과 같은
    /// 정확 일치 소비 함정을 처음부터 막는다). Default unsupported.
    fn pane_done(&self, _surface_id: &str, _outcome: &str, _summary: &str) -> Result<()> {
        anyhow::bail!("pane_done unsupported by this backend")
    }
    /// 이 pane 이 **지금 무엇을 돌리기 시작했고 무엇이 끝났는지** — `PreToolUse`/
    /// `PostToolUse` 훅이 `kasaterm-cli agent-status` 로 부른다.
    ///
    /// 진행 표시(헤더 바·사이드바 펄스)는 원래 transcript 꼬리를 읽어 런치와 회수를
    /// 짝지었다. 그 방식은 세션이 커지면 런치 기록이 꼬리 밖으로 밀려 **오래 걸리는
    /// 작업일수록 표시가 사라진다**(2026-08-11 실측: 3.8MB 7건 / 8.3MB·24MB 0건).
    /// 훅은 일어난 그 순간 한 번 오므로 세션 크기와 무관하다.
    ///
    /// - `phase` — `start` | `end` | `clear`
    /// - `kind` — `subagent`(Task) | `background`(run_in_background Bash)
    /// - `key` — 시작과 종료를 잇는 값. 겹칠 수 있어 소비부는 세어서 관리한다.
    /// - `label` — 화면에 띄울 짧은 설명. `end`/`clear` 엔 없어도 된다.
    ///
    /// `clear` 는 `key` 를 무시하고 그 pane 의 해당 `kind` 를 통째로 비운다 — 턴이
    /// 끝나면(`Stop`) 서브에이전트는 하나도 안 남으므로, `end` 를 놓친 것이 있어도
    /// 거기서 새는 걸 막는다. Default unsupported.
    fn agent_status(
        &self,
        _surface_id: &str,
        _phase: &str,
        _kind: &str,
        _key: &str,
        _label: &str,
    ) -> Result<()> {
        anyhow::bail!("agent_status unsupported by this backend")
    }
    /// Multi-session (tmux-style tab) state for the session panel. Default
    /// is a single session — backends that don't support sessions just
    /// report one.
    fn sessions(&self) -> SessionsInfo {
        SessionsInfo::default()
    }
    /// pane(surface id) → 방(윈도우) 인덱스. 웹텀 `/term/panes` 가 방별 그룹핑에
    /// 쓴다 — board 는 claude 가 바인딩된 pane 만 담아 순수 셸이 빠지므로, 트리
    /// 전체를 아는 이 맵이 따로 필요하다. 기본은 빈 목록(방 개념 없는 백엔드).
    fn pane_windows(&self) -> Vec<(String, usize)> {
        Vec::new()
    }
    /// Switch the visible session to index `idx`. Default unsupported.
    fn switch_session(&self, _idx: usize) -> Result<()> {
        anyhow::bail!("switch_session not supported")
    }
    /// Switch the visible window (tmux-style tab within the current session,
    /// shown in the left sidebar) to index `idx`. Default unsupported.
    fn switch_window(&self, _idx: usize) -> Result<()> {
        anyhow::bail!("switch_window not supported")
    }
    /// Reorder the window at index `from` to index `to` within the active
    /// session's window list (sidebar tab drag-reorder). The daemon owns the
    /// window order, so the GUI routes the drop through this. Default unsupported.
    fn reorder_window(&self, _from: usize, _to: usize) -> Result<()> {
        anyhow::bail!("reorder_window not supported")
    }
    /// Create a fresh session and switch to it. Default unsupported.
    fn new_session(&self) -> Result<()> {
        anyhow::bail!("new_session not supported")
    }
    /// Create a fresh room (window) whose first pane is assigned the given
    /// character. `POST /session-new?character=<name>`. Default unsupported.
    fn new_room(&self, _character: &str) -> Result<()> {
        anyhow::bail!("new_room not supported")
    }
    /// 활성 pane(보이는 방)의 방 식별자 — 방별 collab(모모톡 inbox 등)을 그 방으로
    /// 격리한다(거노: 방끼리 inbox 공유 금지). 기본 방(없음)이면 None.
    fn active_room(&self) -> Option<String> {
        None
    }
    /// 모든 pane 의 `(surface_id, claude session_id)` — claude task store
    /// (~/.claude/tasks) 매핑용. statusline 이 보고한 session_id 를 GUI 즉답으로
    /// 가져온다(`/pane-tasks` 가 소비). 기본 빈 벡터(미지원 백엔드).
    /// pane 별 `(pane, model, effort)` — statusline 이 보고한 값. 원격에서 이 pane 을
    /// 데려갈 때(`migrate … local`) 그 기계가 모르는 모델·effort 를 이 창구로 읽는다.
    /// 기본: 없음(헤드리스 서버).
    fn agent_cfg(&self) -> Vec<(String, String, String)> {
        Vec::new()
    }
    fn pane_session_ids(&self) -> Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }
    /// 아로나 프롬프트 입력창 이미지 드롭(`POST /paste-image`) → 그 pane claude 에 첨부
    /// (시스템 클립보드 비트맵 + Ctrl+V). 기본 미지원.
    fn paste_image(&self, _surface: &str, _bytes: Vec<u8>) -> Result<()> {
        anyhow::bail!("paste_image not supported")
    }
    /// 아로나 타이틀바 버튼(`POST /git-panel`) → 터미널 GUI git 소스컨트롤 패널 토글. 기본 미지원.
    fn toggle_git_panel(&self) -> Result<()> {
        anyhow::bail!("toggle_git_panel not supported")
    }
    /// Create a fresh window in the current session and switch to it. Default
    /// unsupported.
    fn new_window(&self) -> Result<()> {
        anyhow::bail!("new_window not supported")
    }
    /// Close the session at index `idx`. Backends must keep at least one
    /// session alive (closing the last is rejected). Default unsupported.
    fn close_session(&self, _idx: usize) -> Result<()> {
        anyhow::bail!("close_session not supported")
    }
    /// Close the window at index `idx` in the active session, reaping its
    /// panes. Backends must keep at least one window alive (closing the last
    /// is rejected). Authoritative window teardown so a GUI need not fake it
    /// by closing panes individually off a stale local layout. Default
    /// unsupported.
    fn close_window(&self, _idx: usize) -> Result<()> {
        anyhow::bail!("close_window not supported")
    }
    /// Restore a saved (on-disk, not-yet-live) session at index `idx` in the
    /// saved-session list — spawns its panes lazily and switches to it.
    /// Default unsupported.
    fn restore_session(&self, _idx: usize) -> Result<()> {
        anyhow::bail!("restore_session not supported")
    }
    /// Give the session at index `idx` a custom display name (overrides the
    /// auto-derived cwd-basename label). An empty/blank name clears it back to
    /// the auto label. Default unsupported.
    fn rename_session(&self, _idx: usize, _name: &str) -> Result<()> {
        anyhow::bail!("rename_session not supported")
    }
    /// Tear down every session and pane, then leave a single fresh empty
    /// session — the panel's "reset everything" button. Default unsupported.
    fn reset_sessions(&self) -> Result<()> {
        anyhow::bail!("reset_sessions not supported")
    }
    /// Open a preview pane for a file. `kind` is "image" or "markdown";
    /// `path` is an absolute path on the host. `target` is the pane that
    /// requested it (from `$KASATERM_PANE_ID` via imgopen) so the preview
    /// splits beside the *working* pane, not whatever window the sidebar
    /// last focused; None falls back to the active pane. Default unsupported
    /// (e.g. the legacy tmux backend has no window host).
    fn open_preview(&self, _kind: &str, _path: &str, _target: Option<&str>) -> Result<()> {
        anyhow::bail!("open_preview not supported")
    }
    /// URL 을 **사람이 보는 브라우저**로 — 그 pane 을 거울로 보는 기계가 있으면
    /// 거기로 되돌리고, 없으면 이 기계의 기본 브라우저. `target` 은 요청 pane
    /// (`$KASATERM_PANE_ID`). 웹 pane(`open_preview kind=web`)과 달리 화면 안에
    /// 아무것도 안 만든다.
    fn open_url(&self, _url: &str, _target: Option<&str>) -> Result<()> {
        anyhow::bail!("open_url not supported")
    }
    /// 웹(브라우저) pane 조종 — 에이전트가 내장 브라우저를 확인 도구로 쓰는
    /// 손잡이(2026-08-18 「손잡이 뚫어줘」). `op` 는 `eval`(JS 실행, 결과를
    /// JSON 문자열로) · `text`(본문 innerText) · `shot`(PNG 를 `arg` 절대경로에
    /// 저장) · `url`(현재 주소). `surface` 를 안 주면 열린 웹 pane 이 하나일
    /// 때만 그걸 잡는다 — 여럿이면 후보를 나열한 오류가 돌아온다.
    fn web_drive(&self, _op: &str, _arg: &str, _surface: Option<&str>) -> Result<String> {
        anyhow::bail!("web_drive not supported")
    }
    /// Show/hide the *main terminal* window. The arona classroom window calls
    /// this so entering the classroom can take the screen over and its
    /// red-pill button can bring the terminal back. `focus_pane` additionally
    /// focuses that pane on reveal (the classroom jumps the user to a
    /// character's pane). Default unsupported.
    fn reveal_terminal(&self, _show: bool, _focus_pane: Option<&str>) -> Result<()> {
        anyhow::bail!("reveal_terminal not supported")
    }
    /// Close the arona classroom window (no-op when it isn't open) and bring
    /// the main terminal back. The ModePicker's "터미널로" choice calls this —
    /// the web page can't close its own host window. Default unsupported.
    fn close_arona(&self) -> Result<()> {
        anyhow::bail!("close_arona not supported")
    }

    /// 지금 화면에 쓰이는 디자인 토큰(색 팔레트·실루엣). 설정 화면의 웹뷰 판이
    /// 이걸 CSS 변수로 심어 네이티브와 같은 색으로 그린다.
    ///
    /// 트레이트를 거치는 이유는 의존 방향이다 — 토큰의 정본은 GUI crate 의
    /// `theme` 모듈이고 HTTP 서버는 그 아래 crate 라 직접 못 본다. 기본값은
    /// `null`: 테마 개념이 없는 백엔드(레거시 tmux)는 오류가 아니라 "없음"을
    /// 답해야 호출부가 항상 조회할 수 있다.
    fn design_tokens(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// 설정 화면의 캐릭터 탭이 그릴 것 전부 — 테마 카드 목록과 활성 로스터.
    /// 기본 `null`(캐릭터 개념이 없는 백엔드는 오류가 아니라 "없음"을 답한다).
    fn settings_characters(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// 설정 화면의 나머지 탭이 그릴 값 전부 — 카테고리마다 하위 객체 하나씩
    /// (`general` · `appearance` · `shell` · `claude` · `feedback`).
    ///
    /// 탭마다 라우트를 파지 않는 것은 `settings_action` 과 같은 이유다: 값의 정본이
    /// GUI 한 곳이라 조회도 한 번이면 되고, 탭이 늘어도 여기 손댈 게 없다. 기본
    /// `null` — 설정 개념이 없는 백엔드는 오류가 아니라 "없음" 을 답해야 호출부가
    /// 항상 물어볼 수 있다.
    fn settings_values(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// 테마 캐릭터 생성 화면이 2초마다 묻는 상태 — 엔진 목록·선택값·마스킹한 키·
    /// 활성 테마·돌고 있는 잡.
    ///
    /// 값 조회(`settings_values`)와 달리 **GUI 왕복을 안 태운다**. 폴링이라 왕복을
    /// 태우면 굽는 동안 프레임마다 한 번씩 GUI 를 물게 되고, 하필 그때가 화면이
    /// 가장 바쁜 시점이다. 기본 `null` — 생성 개념이 없는 백엔드의 답.
    fn themegen_state(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// 한 캐릭터의 생성 참조 그림(원본 그대로). 기본 None = 404.
    ///
    /// `slug` 는 경로 조각이 되므로 구현이 탈출을 막아야 한다.
    fn themegen_ref(&self, _slug: &str) -> Option<Vec<u8>> {
        None
    }

    /// 참조 그림을 놓는다. `slug` 가 비어 있으면 `name`(원본 파일명)에서 이름을
    /// 유도해 **새 캐릭터로 등록**하고, 그때 정해진 slug 를 돌려준다 — 파일명이
    /// 어떻게 다듬어졌는지 화면이 알아야 그 캐릭터를 이어서 열 수 있다.
    fn themegen_put_ref(&self, _slug: &str, _name: &str, _bytes: &[u8]) -> Result<String, String> {
        Err("이 인스턴스는 캐릭터 생성을 몰라요".to_string())
    }

    /// 캐릭터 프사 PNG 바이트. `theme` 이 있으면 **그 테마 폴더의** 그림을(카드
    /// 미리보기), 없으면 활성 스프라이트 폴더 → 번들 순으로 찾는다.
    ///
    /// `slug`·`theme` 은 경로 조각이 되므로 구현이 탈출을 막아야 한다. 기본 None
    /// = 404.
    fn character_face(&self, _slug: &str, _theme: Option<&str>) -> Option<Vec<u8>> {
        None
    }

    /// 한 캐릭터·모션의 프레임 이미지 바이트. 사용자가 넣은 그림이 있으면 그것,
    /// 없으면 번들 — **화면이 지금 쓰는 것과 같은 순서**여야 미리보기가 실제와
    /// 어긋나지 않는다.
    ///
    /// `slug`·`motion` 은 경로 조각이 되므로 구현이 탈출을 막아야 한다. 기본
    /// None = 404.
    fn character_sprite(&self, _slug: &str, _motion: &str, _frame: usize) -> Option<Vec<u8>> {
        None
    }

    /// 모션별 상태 — 프레임 수와 지금 쓰는 그림의 출처(`user`/`bundled`/`none`).
    ///
    /// 프레임 수를 화면이 아니라 여기서 주는 이유는 그 수가 로더의 규약이기
    /// 때문이다(walk 만 6, 나머지 4). 웹이 자기 상수로 그리면 로더가 바뀔 때
    /// 업로드 칸 수만 옛 값으로 남아, 사용자는 다 넣었는데 앱은 한 장이 없다고
    /// 판단하는 상태가 된다.
    fn character_sprite_status(&self, _slug: &str) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// 사용자 그림을 **벌 단위로** 저장한다. `frames` 는 그 모션의 프레임 수와
    /// 같아야 하며, 하나라도 어긋나면 아무것도 쓰지 않는다.
    ///
    /// 한 장씩 받지 않는 이유는 로더가 all-or-nothing 이라서다 — 절반만 남은
    /// 폴더는 조용히 번들로 폴백해, 사용자에겐 "업로드가 먹지 않는" 것으로 보인다.
    fn save_character_sprite(
        &self,
        _slug: &str,
        _motion: &str,
        _frames: &[Vec<u8>],
    ) -> Result<serde_json::Value> {
        anyhow::bail!("save_character_sprite unsupported")
    }

    /// 사용자 그림을 지워 번들 기본으로 되돌린다. 옛 평면 이름으로 넣어 둔 것도
    /// 함께 지운다 — 새 구조만 지우면 로더가 옛 파일로 폴백해 "되돌렸는데 그대로"가 된다.
    fn clear_character_sprite(&self, _slug: &str, _motion: &str) -> Result<serde_json::Value> {
        anyhow::bail!("clear_character_sprite unsupported")
    }

    /// 캐릭터 한 명의 성격·이름을 굳힌다. `persona`·`new_name` 은 각각 준 것만
    /// 바꾼다(None = 그대로).
    ///
    /// 이 크레이트가 직접 파일을 쓰지 못하는 이유는 저장이 파일 쓰기로 끝나지
    /// 않기 때문이다 — 성격은 shim 재생성이, 이름은 로스터 캐시 무효화가 짝으로
    /// 따라붙고 그 둘은 GUI 가 쥐고 있다. 그래서 구현이 GUI 로 위임하고 결과를
    /// 되받는다.
    ///
    /// 반환값은 `{ok, name}` — 이름 변경이 거부되면 `ok: false` 에 **저장된**
    /// 이름이 실린다. 부른 쪽이 화면을 요청값으로 그려 두면 파일과 어긋나므로,
    /// 회신의 이름을 써야 한다.
    fn save_character(&self, _req: CharacterSave) -> Result<serde_json::Value> {
        anyhow::bail!("save_character unsupported")
    }

    /// 설정 화면의 버튼 하나를 누른 것과 같은 일을 한다 — 테마 고르기·만들기·
    /// 이름 바꾸기·치우기, 폴더 열기, 그림 다시 읽기, 페르소나 스위치.
    ///
    /// 액션마다 라우트를 따로 파지 않고 이름으로 가르는 이유는 **네이티브가 이미
    /// 그 모양**이기 때문이다(액션 enum 하나에 전부 모여 있다). 1:1 로 두면 구현이
    /// 둘로 갈릴 수가 없고, 네이티브에 액션이 늘어도 여기 손댈 게 없다.
    ///
    /// `id` 는 테마 폴더 이름(`select-theme` 에선 빈 문자열 = 번들), `label` 은
    /// `rename-theme` 의 새 이름. 반환값은 `{ok, message}` — `message` 는 네이티브가
    /// 토스트로 띄우려던 문구다. 웹뷰는 그 토스트를 못 보므로 그대로 실어 보낸다.
    fn settings_action(
        &self,
        _action: &str,
        _id: Option<&str>,
        _label: Option<&str>,
    ) -> Result<serde_json::Value> {
        anyhow::bail!("settings_action unsupported")
    }

    /// Read every pane's activity. Default: empty board — a backend that
    /// doesn't track activity reports nothing rather than erroring, so
    /// callers can always scan.
    fn collab_board(&self) -> Result<Vec<PaneActivity>> {
        Ok(Vec::new())
    }

    /// Geometry of the panes in the visible window, as window-relative
    /// percentages — so a caller can see who sits where (right half, top
    /// third) and pick a spot to split. Default: empty (backends that don't
    /// track a layout report nothing rather than erroring).
    fn window_layout(&self) -> Result<Vec<PaneRect>> {
        Ok(Vec::new())
    }

    /// Every window in the active session, each with its panes and rects —
    /// unlike `window_layout`/`list_surfaces`, which only expose the active
    /// window. Lets an agent inspect a window it isn't viewing ("what's in
    /// window 1"). Default: empty (single-window backends report nothing
    /// beyond what `window_layout` already gives).
    fn windows_overview(&self) -> Result<Vec<WindowOverview>> {
        Ok(Vec::new())
    }

    /// Read the visible screen text (last `lines` rows) of a pane so a
    /// sibling can check on a build or long-running job without focusing
    /// it. Default unsupported.
    fn peek(&self, _surface_id: &str, _lines: usize) -> Result<String> {
        anyhow::bail!("peek not supported")
    }

    /// Same as `peek` but returns the text with ANSI SGR color escape sequences
    /// so a viewer can reproduce the pane's colors. Default unsupported.
    fn peek_ansi(&self, _surface_id: &str, _lines: usize) -> Result<String> {
        anyhow::bail!("peek_ansi not supported")
    }

    /// The accumulated OSC 133 command blocks of a pane (newest last, capped at
    /// `limit`), so the GUI can render a Warp-style block stack. Default: none.
    fn pane_blocks(&self, _surface_id: &str, _limit: usize) -> Result<Vec<PaneBlock>> {
        Ok(Vec::new())
    }

    /// Register a pane's claude-code transcript file (the
    /// `~/.claude/projects/<cwd>/<session>.jsonl` it streams to) so the
    /// host can tail it and auto-fill that pane's board activity from the
    /// tool_use calls inside — no manual `announce` needed. Called by a
    /// SessionStart/PreToolUse hook that knows both the `transcript_path`
    /// (from its stdin) and the pane id (from `$KASATERM_PANE_ID`).
    /// Default unsupported.
    fn bind_transcript(&self, _surface_id: &str, _path: &str) -> Result<()> {
        anyhow::bail!("bind_transcript not supported")
    }

    /// Read the last `turns` conversation turns (user prompts + assistant
    /// replies) from a pane's bound transcript. Where `peek` shows the raw
    /// screen (whatever's currently rendered), this gives the structured
    /// dialogue — what a sibling claude was *asked* and what it *answered* —
    /// including turns that have already scrolled off-screen. An orchestrator
    /// pane reads this to monitor what its workers are actually doing.
    /// Default: empty (a backend that tracks no transcripts reports nothing).
    /// 이 pane 이 한 일을 도구 단위로 시간순 — `transcript_tail` 이 버리는
    /// tool_use/tool_result 를 인자·결과까지 담아 돌려준다. `limit` 은 최신
    /// 몇 건까지(0=기본). Default: unsupported.
    fn pane_activity_log(&self, _surface_id: &str, _limit: usize) -> Result<Vec<ActivityEvent>> {
        anyhow::bail!("pane_activity_log not supported")
    }

    fn transcript_tail(&self, _surface_id: &str, _turns: usize) -> Result<Vec<ConversationTurn>> {
        Ok(Vec::new())
    }

    /// Read a pane's bound transcript jsonl *incrementally*. `offset` is the byte
    /// position the client already holds: `0` (first load) returns the **tail**
    /// window (`reset: true`, first partial line dropped); `>0` returns only the
    /// whole lines appended since, `reset: false`. A still-being-written trailing
    /// line is held back until the next call completes it, so the returned
    /// `offset` always lands on a line boundary. Lets the BA GUI append new lines
    /// instead of re-reading & re-parsing the whole (multi-MB) jsonl every poll.
    /// Default: empty.
    fn transcript_raw(&self, _surface_id: &str, _offset: u64) -> Result<TranscriptChunk> {
        Ok(TranscriptChunk::default())
    }

    /// Read a *past* (offline) Claude session's transcript jsonl as raw text,
    /// addressed by its session uuid + the cwd it ran in — no live pane needed.
    /// Where `transcript_raw` reads the jsonl bound to a running surface, this
    /// resolves `~/.claude/projects/<encoded-cwd>/<id>.jsonl` directly so the BA
    /// GUI can preview a recent session read-only before deciding to resume it.
    /// Default: unsupported.
    fn session_transcript_raw(&self, _id: &str, _cwd: Option<&str>) -> Result<String> {
        anyhow::bail!("session_transcript_raw unsupported by this backend")
    }

    /// List the subagents (Task/Agent) a pane's claude has spawned, newest first.
    /// Claude Code writes each subagent's full conversation to a sidecar file
    /// `<session-dir>/subagents/agent-<id>.jsonl` (same jsonl format as the main
    /// transcript) plus an `agent-<id>.meta.json` carrying agentType/description.
    /// The BA GUI lists these to let the user drill into a subagent's dialogue.
    /// Default: empty.
    fn subagents(&self, _surface_id: &str) -> Result<Vec<SubagentInfo>> {
        Ok(Vec::new())
    }

    /// Read one subagent's transcript jsonl as raw text (every line, unparsed) —
    /// `<session-dir>/subagents/agent-<agent_id>.jsonl`. Same shape as
    /// `transcript_raw`; the BA GUI renders it with the same per-tool path.
    /// Default: empty.
    fn subagent_transcript_raw(&self, _surface_id: &str, _agent_id: &str) -> Result<String> {
        Ok(String::new())
    }
}

/// One subagent (Task/Agent) spawned by a pane's claude, surfaced from its
/// `subagents/agent-<id>.meta.json` sidecar. `agent_id` is the file stem (used
/// to fetch its transcript), `mtime` is the transcript's last-modified unix secs
/// (recency / activity ordering). Returned by `subagents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentInfo {
    pub agent_id: String,
    pub agent_type: String,
    pub description: String,
    pub mtime: u64,
}

/// An incremental slice of a transcript jsonl, returned by `transcript_raw`.
/// `raw` is zero or more whole jsonl lines (empty when nothing changed since the
/// client's `offset`), `offset` is the byte position the client should send on
/// its next poll, and `reset` means replace the buffer (a tail (re)load) rather
/// than append.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranscriptChunk {
    pub raw: String,
    pub offset: u64,
    pub reset: bool,
}

/// One turn of a pane's claude conversation, extracted from its transcript
/// jsonl. `role` is "user" (a typed prompt — tool_results are skipped as
/// noise) or "assistant" (the reply text, tool_use blocks dropped). Returned
/// by `transcript_tail` / `collab.transcript`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: String,
    pub text: String,
}

/// 한 pane 이 실제로 **무엇을 했는지** 시간순으로 — 도구 이름·인자·결과까지.
/// `surface.activity` 가 돌려준다.
///
/// `ConversationTurn`(=`transcript_tail`)과 정반대 방향이다. 그쪽은 사람이 읽을
/// 대화만 남기려고 tool_use 와 tool_result 를 버리는데, 이쪽은 그것만 남긴다.
///
/// 왜 board 로 부족한가: `PaneActivity::recent_tools` 는 라벨 여덟 개뿐이라
/// ①인자가 잘리고 ②성패를 모르고 ③여덟 개를 넘어간 것은 사라진다. 그래서
/// 「같은 명령이 세 번 넘게 같은 오류로 끝났다」 같은 판정은 board 만으로는
/// **원리적으로 불가능하다** — 재료 자체가 없다. 이 타입이 그 재료다.
///
/// 관찰자(observer) 에이전트가 하네스에게서 받는 활동 요약과 같은 재료이기도
/// 하다(2026-08-30 실측: 그쪽은 `<tool-call>`/`<tool-result>` 태그로 온다).
/// 차이는 배달 방식뿐 — 관찰자는 턴마다 밀어 주고, 이쪽은 물을 때 뜬다.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEvent {
    /// `"prompt"`(사람이 시킨 것) | `"say"`(학생이 말한 것) | `"tool"` | `"result"`.
    pub kind: String,
    /// 도구 이름 — `kind` 가 `tool`/`result` 일 때만, 그 밖엔 빈 문자열.
    #[serde(default)]
    pub name: String,
    /// `tool` 이면 인자, `result` 면 결과, 그 밖엔 본문. 둘 다 상한을 넘으면 `…`.
    pub text: String,
    /// `result` 가 오류로 끝났나. `tool`/`prompt`/`say` 는 None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agents_keys_by_session_and_reads_waiting_for() {
        // Real `claude agents --json` shape: extra fields (pid/cwd/…) ignored,
        // a waiting session carries waitingFor, others omit it.
        let json = r#"[
            {"pid":284,"cwd":"/a","kind":"interactive","startedAt":1,"sessionId":"sess-idle","status":"idle"},
            {"pid":99,"cwd":"/b","kind":"interactive","startedAt":2,"sessionId":"sess-busy","name":"그림","status":"busy"},
            {"pid":12,"cwd":"/c","kind":"interactive","startedAt":3,"sessionId":"sess-wait","status":"waiting","waitingFor":"permission"}
        ]"#;
        let map = parse_agents_json(json);
        assert_eq!(map.len(), 3);
        assert_eq!(map["sess-idle"].status, "idle");
        assert_eq!(map["sess-idle"].waiting_for, None);
        assert_eq!(map["sess-busy"].status, "busy");
        assert_eq!(map["sess-wait"].status, "waiting");
        assert_eq!(map["sess-wait"].waiting_for.as_deref(), Some("permission"));
    }

    #[test]
    fn parse_agents_empty_or_garbage_is_empty_map() {
        assert!(parse_agents_json("").is_empty());
        assert!(parse_agents_json("not json").is_empty());
        assert!(parse_agents_json("[]").is_empty());
    }
}
