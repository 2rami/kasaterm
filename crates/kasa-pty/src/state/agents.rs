//! 에이전트 감지 — 하네스 표(`AGENT_TABLE`)·런처(node·npm) 아래로 내려가 찾기·argv 판정·
//! `claude agents` 뷰 판정 캐시.

use super::*;
use super::process::effective_shell_pid;

impl PtySession {
    /// 이 pane 이 지금 돌리는 **에이전트 종류**. 셸이거나 다른 프로그램이면 None.
    ///
    /// 게이트를 이 하나로 모으는 이유: 예전엔 11곳이 `active_process_name()` 을 각자
    /// 보며 어떤 곳은 `== "claude"`, 어떤 곳은 `contains` 로 갈렸다. 종류가 둘이 되는
    /// 순간 그 사본들이 제각각 갈라진다 — 오늘만 사본 때문에 세 번 물렸다.
    ///
    /// ⚠️ **codex 는 이름만 봐선 절대 못 잡는다.** npm shim 이라 셸의 직속 자식이
    /// `node` 이고 진짜 바이너리는 **손자**다(실측):
    /// ```text
    /// 32387 ppid=셸    comm=node          args=node …/.npm-global/bin/codex
    /// 32410 ppid=32387 comm=…/bin/codex   ← 이것
    /// ```
    /// 그래서 직속 자식이 런처류(node·npm·sh…)면 한 세대 더 내려간다. 프로세스
    /// 테이블은 300ms 공유 캐시라 `ps` 추가 호출이 없다.
    pub fn active_agent(&self) -> Option<AgentKind> {
        let pid = self.shell_pid?;
        agent_for_shell(&process_table_shared(), pid)
    }

    /// `claude agents`(에이전트 목록 뷰)로 도는 pane 인지 — argv 서브커맨드로 판정.
    /// Cold and expired lookups return the last answer while one shared worker
    /// refreshes argv; a slow process scan must not stall the GUI thread.
    pub fn is_claude_agents(&self) -> bool {
        let Some(pid) = self.shell_pid else {
            return false;
        };
        self.agents_cache.get_or_refresh(Instant::now(), queue_agents_lookup, move || {
            claude_agents_argv(pid)
        })
    }
}

/// Windows 프로세스명은 "claude.exe" — active_process_name 호출자들은 "claude" /
/// "bash" 같은 bare 이름과 정확 일치 비교하므로 여기서 확장자를 벗겨 플랫폼
/// 균질화한다. Unix 는 no-op.
/// 표에 실린 하네스 한 줄. `AgentKind::Other` 가 이 정적 표의 원소를 가리킨다 —
/// 종류가 서른을 넘어 enum 변종으로 세면 `match` 가 호출처마다 폭발하는데,
/// 정작 이들에게 필요한 것은 「이름이 무엇이고 프로세스가 무엇인가」뿐이다.
/// claude·codex·agy 만 변종으로 남긴 이유는 그 셋에만 고유 분기가 실재하기
/// 때문이다(claude=SendMessage·transcript, codex=입력박스 판독, agy=배너).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AgentSpec {
    /// 저장·전송용 id. Orca 의 `TuiAgent` 키와 같은 문자열을 쓴다.
    pub id: &'static str,
    /// 사람에게 보이는 이름(헤더 이름표·info).
    pub label: &'static str,
    /// 이 하네스로 인정할 프로세스 이름들. 첫 원소가 대표.
    pub procs: &'static [&'static str],
    /// comm 이 `node`·`Python` 같은 런처로 **숨을 때** 명령줄에서 찾을 조각.
    ///
    /// 이게 왜 필요한지는 실측이 말해 준다(2026-08-21, 이 컴퓨터에서 직접 띄움):
    /// gemini 는 셸의 자식도 손자도 comm 이 `node` 고, hermes 는 `Python` 이며,
    /// cursor-agent 는 `…/cursor-agent/versions/<판>/node` 라 파일명만 떼면 역시
    /// `node` 다. 이름만 보는 판정으로는 **셋 다 영영 안 잡힌다** — 표를 옮겨
    /// 놓고도 학생이 안 서는 조용한 실패라 알아채기 어렵다.
    pub argv_hints: &'static [&'static str],
}

/// 하네스 표. 출처는 Orca(`src/shared/tui-agent-config.ts` +
/// `tui-agent-display-names.ts`)이고 2026-08-21 에 기계적으로 옮겼다 — 손으로
/// 늘리면 이름 하나가 어긋나 조용히 안 잡히므로, 갱신할 때도 그 두 파일에서
/// 다시 뽑아라.
///
/// 셋은 일부러 뺐다: `claude`·`codex`·`antigravity` 는 아래 enum 변종이고,
/// `claude-agent-teams` 는 Orca 전용 런치 모드라 우리 쪽에 대응물이 없으며,
/// `kimi` 는 이 컴퓨터에서 **사용자의 자작 런처 이름**이다(claude 를 다른 모델로
/// 띄우는 zsh 스크립트) — 문샷의 Kimi CLI 와 이름이 같아 넣으면 오판한다.
pub static AGENT_TABLE: &[AgentSpec] = &[
    AgentSpec { id: "aider", label: "Aider", procs: &["aider"], argv_hints: &[] },
    AgentSpec { id: "amp", label: "Amp", procs: &["amp"], argv_hints: &["@sourcegraph/amp/"] },
    AgentSpec { id: "ante", label: "Ante", procs: &["ante"], argv_hints: &[] },
    AgentSpec { id: "aug", label: "Auggie", procs: &["auggie"], argv_hints: &[] },
    AgentSpec { id: "autohand", label: "Autohand Code", procs: &["autohand"], argv_hints: &[] },
    AgentSpec { id: "cline", label: "Cline", procs: &["cline"], argv_hints: &[] },
    AgentSpec { id: "codebuff", label: "Codebuff", procs: &["codebuff"], argv_hints: &[] },
    AgentSpec { id: "command-code", label: "Command Code", procs: &["command-code"], argv_hints: &[] },
    AgentSpec { id: "continue", label: "Continue", procs: &["cn"], argv_hints: &[] },
    AgentSpec { id: "copilot", label: "GitHub Copilot", procs: &["copilot"], argv_hints: &[] },
    AgentSpec { id: "crush", label: "Charm", procs: &["crush"], argv_hints: &[] },
    AgentSpec { id: "cursor", label: "Cursor", procs: &["cursor-agent"], argv_hints: &["cursor-agent/versions/"] },
    AgentSpec { id: "devin", label: "Devin", procs: &["devin"], argv_hints: &[] },
    AgentSpec { id: "droid", label: "Droid", procs: &["droid"], argv_hints: &[] },
    // 힌트가 둘인 이유: 설치 방식마다 명령줄이 다르다. npm 전역이면
    // `node …/npm-global/bin/gemini`(이 컴퓨터 실측)이고, 패키지를 직접 가리키면
    // Orca 가 쓰는 `node_modules/@google/gemini-cli/…` 가 된다.
    AgentSpec { id: "gemini", label: "Gemini", procs: &["gemini"], argv_hints: &["node_modules/@google/gemini-cli/", "/bin/gemini"] },
    AgentSpec { id: "goose", label: "Goose", procs: &["goose"], argv_hints: &[] },
    AgentSpec { id: "grok", label: "Grok", procs: &["grok"], argv_hints: &[] },
    AgentSpec { id: "hermes", label: "Hermes", procs: &["hermes"], argv_hints: &[".hermes/hermes-agent/"] },
    AgentSpec { id: "kilo", label: "Kilocode", procs: &["kilo"], argv_hints: &[] },
    AgentSpec { id: "kiro", label: "Kiro", procs: &["kiro-cli"], argv_hints: &[] },
    AgentSpec { id: "mimo-code", label: "MiMo Code", procs: &["mimo"], argv_hints: &[] },
    AgentSpec { id: "mistral-vibe", label: "Mistral Vibe", procs: &["vibe", "mistral-vibe"], argv_hints: &[] },
    AgentSpec { id: "omp", label: "OMP", procs: &["omp"], argv_hints: &[] },
    AgentSpec { id: "openclaude", label: "OpenClaude", procs: &["openclaude"], argv_hints: &[] },
    AgentSpec { id: "openclaw", label: "OpenClaw", procs: &["openclaw"], argv_hints: &[] },
    AgentSpec { id: "opencode", label: "OpenCode", procs: &["opencode"], argv_hints: &[] },
    AgentSpec { id: "pi", label: "Pi", procs: &["pi"], argv_hints: &["pi-coding-agent/dist/cli.js"] },
    AgentSpec { id: "qwen-code", label: "Qwen Code", procs: &["qwen"], argv_hints: &[] },
    AgentSpec { id: "rovo", label: "Rovo Dev", procs: &["rovo"], argv_hints: &[] },
    AgentSpec { id: "trae", label: "Trae", procs: &["traecli"], argv_hints: &[] },
];

/// pane 에서 도는 에이전트 종류. 학생 대접(보더 학생색·타이틀바·얼굴·탭칩)은
/// claude 전용이 아니라 **이 값이 Some 이면** 붙는다(사용자 2026-08-05: codex 도 학생).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentKind {
    Claude,
    Codex,
    Agy,
    /// 표의 한 줄. 얼굴·이름·색만 받는 하네스들이 전부 여기로 온다.
    Other(&'static AgentSpec),
}

impl AgentKind {
    /// 저장·전송용 이름. `pane_record` 의 `was_agent`, board 의 `harness`, 소켓
    /// 응답이 전부 이 하나를 쓴다 — match 를 사본으로 늘리면 한쪽만 고쳐져 갈린다.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Agy => "agy",
            Self::Other(spec) => spec.id,
        }
    }

    /// 사람에게 보이는 이름. 헤더 이름표·info 가 이걸 그린다 — `as_str` 은
    /// 저장·전송용이라 소문자 id 고, 이쪽은 표기용이라 갈라 둔다.
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Agy => "Antigravity",
            Self::Other(spec) => spec.label,
        }
    }

    /// `as_str` 의 역함수. 저장된 `was_agent`·소켓의 `harness` 를 되읽는 자리가
    /// 각자 하드코딩 match 를 갖고 있었는데, 종류가 서른이 되면 그 사본들이
    /// 곧바로 갈린다.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "agy" => Some(Self::Agy),
            other => AGENT_TABLE.iter().find(|s| s.id == other).map(Self::Other),
        }
    }

    /// 프로세스 comm 으로 판정. comm 은 경로가 붙어 올 수 있어(손자 행은
    /// `…/bin/codex`) 파일명만 떼어 본다.
    fn from_comm(comm: &str) -> Option<Self> {
        let base = comm.rsplit(['/', '\\']).next().unwrap_or(comm);
        let base = strip_exe_suffix(base.to_string());
        match base.as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            // shim 래퍼도 `agy` 라는 이름의 sh 스크립트지만 마지막에 `exec` 로
            // 진짜 바이너리가 그 자리를 차지하므로, 여기 걸리는 건 늘 진짜다.
            "agy" => Some(Self::Agy),
            other => AGENT_TABLE
                .iter()
                .find(|spec| spec.procs.contains(&other))
                .map(Self::Other),
        }
    }
}

/// 셸 pid 아래에서 에이전트를 찾는다 — 테이블만 보는 순수 함수라 실측 트리로 잴 수 있다.
///
/// 같은 부모의 자식 중 **가장 나중에 뜬 것**(pid 큰 쪽)을 고른다 — `active_process_name`
/// 과 같은 규칙. 직속 자식이 런처류면 한 세대 더 내려간다.
/// 셸 pid 하나로 하네스를 묻는다 — `PtySession` 을 못 쥐고 pid 만 아는 호출자
/// (board 조립·소켓 백엔드)용. 프로세스 테이블은 이미 공유 캐시라 `ps` 가 추가로
/// 안 돈다. 판정 본체는 `agent_in_table` 하나뿐이라 `active_agent` 와 결과가 같다.
pub fn agent_for_shell(table: &[(u32, u32, String)], shell_pid: u32) -> Option<AgentKind> {
    agent_pid_for_shell(table, shell_pid).map(|(kind, _)| kind)
}

/// `agent_for_shell` 의 pid 동반판. 계정 실측(그 프로세스의 env 를 `ps` 로 읽어
/// 어느 자격증명 저장소로 떠 있는지 보는 것)이 이 pid 를 집는다 — 종류만 알아서는
/// "어느 계정인가"에 답할 수 없다.
pub fn agent_pid_for_shell(
    table: &[(u32, u32, String)],
    shell_pid: u32,
) -> Option<(AgentKind, u32)> {
    let eff = effective_shell_pid(table, shell_pid);
    if let Some(hit) = agent_pid_in_table(table, eff) {
        return Some(hit);
    }
    agent_pid_by_argv(table, eff)
}

/// 이름으로 못 잡은 pane 을 **명령줄로** 한 번 더 본다 — comm 이 `node`·`Python`
/// 인 하네스들(gemini·cursor·hermes·amp)이 여기서만 잡힌다.
///
/// ⚠️ 순서가 곧 비용이다. 명령줄 읽기는 macOS 에선 커널에 pid 하나를 묻는 일이지만
/// 그 밖에선 `ps` 표 한 번이라, 이름 판정보다 **먼저** 놓으면 학생이 아닌 셸
/// pane 까지 그 문을 연다. 그래서 ①이름 판정이 실패하고 ②그 자식이 실제로
/// 런처류일 때만 여기까지 온다. 결과는 아래 캐시가 1초 잡아 둔다.
fn agent_pid_by_argv(
    table: &[(u32, u32, String)],
    shell_pid: u32,
) -> Option<(AgentKind, u32)> {
    let newest_child = |parent: u32| -> Option<(u32, &str)> {
        let mut best: Option<(u32, &str)> = None;
        for (row_pid, row_ppid, name) in table.iter() {
            if *row_ppid == parent && best.as_ref().is_none_or(|(p, _)| *p < *row_pid) {
                best = Some((*row_pid, name.as_str()));
            }
        }
        best
    };
    let (child_pid, child) = newest_child(shell_pid)?;
    // 런처가 아니면 이름이 이미 진실을 말한 것이다 — 그걸로 못 잡았으면
    // 하네스가 아니라는 뜻이므로 ps 를 부를 이유가 없다.
    if !is_argv_probe_launcher(child) {
        return None;
    }
    // 자식과 손자 둘 다 본다. gemini 는 node 아래 node 로 한 겹 더 들어간다.
    let grandchild = newest_child(child_pid).map(|(p, _)| p);
    for pid in [Some(child_pid), grandchild].into_iter().flatten() {
        if let Some(spec) = agent_spec_by_argv_cached(pid) {
            return Some((AgentKind::Other(spec), pid));
        }
    }
    None
}

/// argv 를 들여다볼 가치가 있는 런처 — 이름 판정용 `is_agent_launcher` 보다 넓다.
///
/// python 을 **여기에만** 넣는 이유: 이름으로 한 세대 내려가는 판정에 python 을
/// 넣으면 `python train.py` 처럼 자기가 곧 작업인 흔한 경우에 엉뚱한 자식 이름을
/// 집는다. 반면 argv 폴백은 표의 힌트 문자열과 정확히 맞을 때만 인정하므로 그
/// 위험이 없다. hermes 가 실측에서 comm=`Python`(macOS 프레임워크 번들이라 대문자)
/// 이었고, 이 문이 닫혀 있어 표가 맞는데도 안 잡혔다(2026-08-21).
fn is_argv_probe_launcher(comm: &str) -> bool {
    if is_agent_launcher(comm) {
        return true;
    }
    let base = comm.rsplit(['/', '\\']).next().unwrap_or(comm);
    let base = strip_exe_suffix(base.to_string()).to_ascii_lowercase();
    base == "python" || base.strip_prefix("python").is_some_and(|v| {
        !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')
    })
}

/// pid → argv 판정 결과, 1초 캐시. 명령줄 읽기가 싸도(macOS 는 커널에 바로, 그 밖은
/// `ps` 표 500ms 캐시) 판정(문자열 훑기)까지 매 프레임 되풀이할 이유는 없다.
///
/// pid 는 재사용되지만 TTL 이 1초라 남의 결과를 물려받을 창이 사실상 없다 —
/// 그 사이에 pid 가 한 바퀴 돌려면 초당 수만 개가 떠야 한다.
fn agent_spec_by_argv_cached(pid: u32) -> Option<&'static AgentSpec> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<u32, (Instant, Option<&'static AgentSpec>)>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let now = Instant::now();
    if let Ok(mut map) = cache.lock() {
        if let Some((at, val)) = map.get(&pid) {
            if now.duration_since(*at).as_millis() < 1000 {
                return *val;
            }
        }
        let args = crate::procinfo::process_cmdline(pid).unwrap_or_default();
        let hit = (!args.is_empty())
            .then(|| {
                AGENT_TABLE
                    .iter()
                    .find(|spec| spec.argv_hints.iter().any(|h| args.contains(h)))
            })
            .flatten();
        // 실패도 캐시한다 — 셸 pane 은 늘 실패하는데 그때마다 ps 를 부르면
        // 캐시를 둔 의미가 없다.
        map.retain(|_, (at, _)| now.duration_since(*at).as_secs() < 5);
        map.insert(pid, (now, hit));
        return hit;
    }
    None
}

fn agent_pid_in_table(
    table: &[(u32, u32, String)],
    shell_pid: u32,
) -> Option<(AgentKind, u32)> {
    let newest_child = |parent: u32| -> Option<(u32, &str)> {
        let mut best: Option<(u32, &str)> = None;
        for (row_pid, row_ppid, name) in table.iter() {
            if *row_ppid == parent && best.as_ref().is_none_or(|(p, _)| *p < *row_pid) {
                best = Some((*row_pid, name.as_str()));
            }
        }
        best
    };
    let (child_pid, child) = newest_child(shell_pid)?;
    if let Some(kind) = AgentKind::from_comm(child) {
        return Some((kind, child_pid));
    }
    if is_agent_launcher(child) {
        if let Some((grandchild_pid, grandchild)) = newest_child(child_pid) {
            return AgentKind::from_comm(grandchild).map(|k| (k, grandchild_pid));
        }
    }
    None
}

/// 에이전트를 감싸 띄우는 것들 — 이게 직속 자식이면 진짜 프로세스는 한 세대 아래다.
/// codex 가 npm shim 이라 `node` 를 거치는 게 대표 사례고, `npx`·래퍼 셸도 같다.
fn is_agent_launcher(comm: &str) -> bool {
    let base = comm.rsplit(['/', '\\']).next().unwrap_or(comm);
    let base = strip_exe_suffix(base.to_string());
    matches!(
        base.as_str(),
        "node" | "npm" | "npx" | "bun" | "deno" | "env" | "sh" | "bash" | "zsh" | "fish"
    )
}

/// 자기 이름으로는 아무것도 말해 주지 않는, 남을 띄우기만 하는 것들.
/// 진짜 프로그램은 이들의 자식으로 뜨므로 이름 해석은 여기서 멈추면 안 된다.
/// python 류는 넣지 않았다 — `python train.py` 처럼 자기가 곧 작업인 경우가
/// 흔해서, 내려갔다가 엉뚱한 자식 이름을 집을 수 있다.
fn is_launcher_name(name: &str) -> bool {
    matches!(
        strip_exe_suffix(name.to_string()).as_str(),
        "node" | "npx" | "npm" | "bun" | "deno"
    )
}

/// 런처를 만나면 그 아래 최신 자식으로 계속 내려간다.
///
/// 여기서 멈추면 pane 을 닫을 때 "node 실행 중"이라고 물어 무엇을 닫는 건지 알
/// 수가 없다 — codex 는 npm shim 을 거쳐 진짜 바이너리가 손자로 뜨고, agy 를
/// 게이트웨이 모델(kimi·glm)로 돌릴 때도 free-antigravity-cli(node)를 지난다.
/// 사슬이 길어질 수 있으니 몇 걸음만 내려가고, 더 못 내려가면 그 자리를 답으로 쓴다.
pub(super) fn descend_launchers(
    table: &[(u32, u32, String)],
    start: Option<(u32, String)>,
) -> Option<(u32, String)> {
    let mut cur = start;
    for _ in 0..3 {
        let (cpid, cname) = cur.clone()?;
        if !is_launcher_name(&cname) {
            break;
        }
        let mut grandchild: Option<(u32, String)> = None;
        for (row_pid, row_ppid, name) in table.iter() {
            if *row_ppid == cpid && grandchild.as_ref().is_none_or(|(p, _)| *p < *row_pid) {
                grandchild = Some((*row_pid, name.clone()));
            }
        }
        match grandchild {
            Some(g) => cur = Some(g),
            None => break,
        }
    }
    cur
}

pub(super) fn strip_exe_suffix(name: String) -> String {
    #[cfg(not(windows))]
    {
        name
    }
    #[cfg(windows)]
    {
        if name.to_ascii_lowercase().ends_with(".exe") {
            name[..name.len() - 4].to_string()
        } else {
            name
        }
    }
}

/// shell 의 직계 claude 자식이 `claude agents`(에이전트 목록 뷰) 서브커맨드로
/// 도는지. agents 뷰는 shell→claude 직계라 부모 체인 walk 불필요(background
/// --resume 은 실제 대화라 여기 해당 없음). argv 에 독립 토큰 `agents` 가 있으면
/// true — 일반 대화 argv 엔 없다.
fn claude_agents_argv(shell_pid: u32) -> bool {
    let table = process_table_shared();
    let claude_pid = table
        .iter()
        .filter(|(_, ppid, name)| *ppid == shell_pid && name.contains("claude"))
        .map(|(pid, _, _)| *pid)
        .max();
    let Some(pid) = claude_pid else {
        return false;
    };
    let Some(argv) = crate::procinfo::process_cmdline(pid) else {
        return false;
    };
    // attach 도 뷰 — agents 목록과 마찬가지로 "남의 세션을 보는 pane"이라, 학생 표시를
    // 파싱 결과로만 하는 게이트(display_pane_char)가 같은 판정을 공유한다. 일반 세션
    // 부팅은 --session-id/--resume/persona 가 붙어 이 토큰이 나올 일이 없다.
    argv_is_agents_view(&argv)
}

fn argv_is_agents_view(argv: &str) -> bool {
    argv.split_whitespace().any(|t| t == "agents" || t == "attach")
}

type AgentsLookup = Box<dyn FnOnce() + Send>;

fn queue_agents_lookup(job: AgentsLookup) -> bool {
    static WORKER: Mutex<Option<Sender<AgentsLookup>>> = Mutex::new(None);
    let Ok(mut worker) = WORKER.try_lock() else { return false };
    if worker.is_none() {
        let (tx, rx) = crossbeam_channel::unbounded::<AgentsLookup>();
        // One sleeping worker avoids a thread per pane per refresh and lets the
        // existing global argv cache serve a whole batch of panes from one scan.
        if std::thread::Builder::new().name("agents-view-cache".into()).spawn(move || {
            for job in rx {
                job();
            }
        }).is_err() {
            return false;
        }
        *worker = Some(tx);
    }
    if worker.as_ref().is_some_and(|tx| tx.send(job).is_ok()) {
        true
    } else {
        *worker = None;
        false
    }
}

#[derive(Default)]
pub(super) struct AgentsViewCache {
    value: std::sync::atomic::AtomicBool,
    refresh: Mutex<(Option<Instant>, bool)>,
}

impl AgentsViewCache {
    fn get_or_refresh(
        self: &Arc<Self>,
        now: Instant,
        schedule: impl FnOnce(AgentsLookup) -> bool,
        lookup: impl FnOnce() -> bool + Send + 'static,
    ) -> bool {
        use std::sync::atomic::Ordering;
        let previous = self.value.load(Ordering::Acquire);
        let Ok(mut state) = self.refresh.try_lock() else { return previous };
        if state.1 || state.0.is_some_and(|at| now.saturating_duration_since(at).as_millis() < 500) {
            return previous;
        }
        state.1 = true;
        drop(state);
        let weak = Arc::downgrade(self);
        if !schedule(Box::new(move || {
            let Some(cache) = weak.upgrade() else { return };
            // A failed scan must not strand the in-flight flag or kill the shared worker.
            let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(lookup));
            if let Ok(mut state) = cache.refresh.lock() {
                if let Ok(value) = value {
                    cache.value.store(value, Ordering::Release);
                }
                *state = (Some(Instant::now()), false);
            };
        })) {
            if let Ok(mut state) = self.refresh.lock() {
                *state = (Some(now), false);
            }
        }
        previous
    }
}

/// 판정 본체의 종류-만 어댑터 — 이제 prod 는 pid 동반판을 쓰고, 트리 판정
/// 테스트들이 이 얇은 이름으로 남아 있다.
#[cfg(test)]
fn agent_in_table(table: &[(u32, u32, String)], shell_pid: u32) -> Option<AgentKind> {
    agent_pid_in_table(table, shell_pid).map(|(kind, _)| kind)
}

#[cfg(test)]
mod agents_view_cache_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn cold_and_expired_reads_return_without_running_lookup_and_deduplicate() {
        let cache = Arc::new(AgentsViewCache::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut jobs = VecDeque::new();
        let now = Instant::now();
        for _ in 0..20 {
            let calls = calls.clone();
            assert!(!cache.get_or_refresh(now, |job| { jobs.push_back(job); true }, move || {
                calls.fetch_add(1, Ordering::SeqCst);
                true
            }));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(jobs.len(), 1);
        jobs.pop_front().unwrap()();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(cache.get_or_refresh(Instant::now(), |_| panic!("fresh cache scheduled work"), || false));

        let expired = Instant::now() + std::time::Duration::from_secs(1);
        for _ in 0..20 {
            let calls = calls.clone();
            assert!(cache.get_or_refresh(expired, |job| { jobs.push_back(job); true }, move || {
                calls.fetch_add(1, Ordering::SeqCst);
                false
            }));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(jobs.len(), 1);
        jobs.pop_front().unwrap()();
        assert!(!cache.get_or_refresh(Instant::now(), |_| panic!("fresh cache scheduled work"), || true));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn busy_publication_lock_returns_previous_value() {
        let cache = Arc::new(AgentsViewCache::default());
        cache.value.store(true, Ordering::Release);
        let _guard = cache.refresh.lock().unwrap();
        assert!(cache.get_or_refresh(Instant::now(), |_| panic!("lock held"), || false));
    }

    #[test]
    fn shared_worker_can_block_without_blocking_reads() {
        let cache = Arc::new(AgentsViewCache::default());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        assert!(!cache.get_or_refresh(Instant::now(), queue_agents_lookup, move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            true
        }));
        started_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        for _ in 0..100 {
            assert!(!cache.get_or_refresh(Instant::now(), |_| panic!("duplicate job"), || false));
        }
        release_tx.send(()).unwrap();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        assert!(queue_agents_lookup(Box::new(move || { finished_tx.send(()).unwrap(); })));
        finished_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(cache.value.load(Ordering::Acquire));
    }

    #[test]
    fn dropped_pane_skips_queued_scan() {
        let cache = Arc::new(AgentsViewCache::default());
        let mut pending = None;
        cache.get_or_refresh(Instant::now(), |job| { pending = Some(job); true }, || panic!("pane gone"));
        drop(cache);
        pending.unwrap()();
    }

    #[test]
    fn schedule_failure_can_retry_after_ttl() {
        let cache = Arc::new(AgentsViewCache::default());
        let now = Instant::now();
        assert!(!cache.get_or_refresh(now, |_| false, || true));
        assert!(!cache.refresh.lock().unwrap().1);
        let mut pending = None;
        cache.get_or_refresh(now + std::time::Duration::from_secs(1), |job| { pending = Some(job); true }, || true);
        pending.unwrap()();
        assert!(cache.value.load(Ordering::Acquire));
    }

    #[test]
    fn failed_lookup_keeps_previous_answer_and_clears_inflight() {
        let cache = Arc::new(AgentsViewCache::default());
        cache.value.store(true, Ordering::Release);
        let mut pending = None;
        assert!(cache.get_or_refresh(Instant::now(), |job| { pending = Some(job); true }, || panic!("scan failed")));
        pending.unwrap()();
        assert!(cache.value.load(Ordering::Acquire));
        assert!(!cache.refresh.lock().unwrap().1);
    }

    #[test]
    fn agents_and_attach_keep_existing_token_classification() {
        for argv in ["claude agents", "/bin/claude attach abc", "claude --verbose agents"] {
            assert!(argv_is_agents_view(argv));
        }
        for argv in ["claude", "claude --resume abc", "claude --session-id abc", "claude agents-extra", "claude --agents"] {
            assert!(!argv_is_agents_view(argv));
        }
    }
}

#[cfg(test)]
mod agent_kind_tests {
    use super::*;

    /// 실측 트리(2026-08-05, 사용자 머신). codex 는 npm shim 이라 진짜 바이너리가
    /// **손자**다 — 이름만 보는 판정은 여기서 반드시 실패한다.
    fn codex_tree() -> Vec<(u32, u32, String)> {
        vec![
            (60536, 1, "zsh".into()),
            (60973, 60536, "node".into()),
            (60992, 60973, "codex".into()),
        ]
    }

    #[test]
    fn codex_는_node_아래_손자로_잡힌다() {
        assert_eq!(agent_in_table(&codex_tree(), 60536), Some(AgentKind::Codex));
    }

    #[test]
    fn claude_는_직속_자식으로_잡힌다() {
        let t = vec![(71388, 1, "zsh".into()), (71391, 71388, "claude".into())];
        assert_eq!(agent_in_table(&t, 71388), Some(AgentKind::Claude));
    }

    #[test]
    fn 그냥_셸은_아무것도_아니다() {
        // 음성 대조군이 없으면 "늘 Some" 을 내는 판정도 통과한다.
        let t = vec![(100, 1, "zsh".into()), (101, 100, "vim".into())];
        assert_eq!(agent_in_table(&t, 100), None);
    }

    #[test]
    fn 런처만_있고_손자가_없으면_아무것도_아니다() {
        // node 를 띄웠지만 codex 가 아닌 경우 — 런처를 봤다고 에이전트로 치면 안 된다.
        let t = vec![(200, 1, "zsh".into()), (201, 200, "node".into())];
        assert_eq!(agent_in_table(&t, 200), None);
    }

    #[test]
    fn 자식이_여럿이면_가장_나중_것() {
        // active_process_name 과 같은 규칙(pid 큰 쪽) — 옛 자식이 남아 있어도
        // 지금 화면에 보이는 것을 고른다.
        let t = vec![
            (300, 1, "zsh".into()),
            (301, 300, "vim".into()),
            (302, 300, "claude".into()),
        ];
        assert_eq!(agent_in_table(&t, 300), Some(AgentKind::Claude));
    }

    #[test]
    fn 경로가_붙어_와도_파일명으로_본다() {
        // 테이블은 basename 으로 정규화하지만, 그 전제가 깨져도 판정은 서야 한다.
        let t = vec![
            (400, 1, "zsh".into()),
            (401, 400, "/usr/local/bin/node".into()),
            (402, 401, "/opt/homebrew/bin/codex".into()),
        ];
        assert_eq!(agent_in_table(&t, 400), Some(AgentKind::Codex));
    }
}

#[cfg(test)]
mod 하네스_표_tests {
    use super::*;

    fn row(pid: u32, ppid: u32, name: &str) -> (u32, u32, String) {
        (pid, ppid, name.to_string())
    }

    /// 표에 실린 하네스는 이름만으로 잡혀야 한다 — opencode 는 실측에서 comm 이
    /// `/Users/…/.opencode/bin/opencode` 라 경로가 붙어 온다(2026-08-21).
    #[test]
    fn 표에_실린_하네스는_이름으로_잡힌다() {
        let t = vec![
            row(100, 1, "zsh"),
            row(200, 100, "/Users/kasa/.opencode/bin/opencode"),
        ];
        let got = agent_in_table(&t, 100).expect("opencode 를 못 잡았다");
        assert_eq!(got.as_str(), "opencode");
        assert_eq!(got.label(), "OpenCode");
    }

    /// ⚠️ 표의 프로세스 이름이 enum 변종 셋과 겹치면, 겹친 쪽이 `Other` 로
    /// 잡혀 claude 전용 분기(SendMessage·transcript·입력 판독)가 통째로 빠진다.
    /// match 가 먼저라 실제로는 변종이 이기지만, 표에 그 이름이 있다는 것
    /// 자체가 "표를 고치면 하네스가 바뀐다"는 함정이므로 아예 금지한다.
    #[test]
    fn 표가_내장_변종을_가리지_않는다() {
        for spec in AGENT_TABLE {
            for p in spec.procs {
                assert!(
                    !matches!(*p, "claude" | "codex" | "agy"),
                    "{}: 프로세스 이름 {p:?} 가 내장 변종과 겹친다",
                    spec.id
                );
            }
            assert!(
                !matches!(spec.id, "claude" | "codex" | "agy"),
                "{}: id 가 내장 변종과 겹친다",
                spec.id
            );
        }
    }

    /// 같은 프로세스 이름이 두 줄에 있으면 먼저 쓰인 쪽이 늘 이겨, 뒤엣것은
    /// 영영 안 잡히면서도 컴파일은 통과한다.
    #[test]
    fn 표에_같은_프로세스_이름이_두_번_없다() {
        let mut seen = std::collections::HashMap::new();
        for spec in AGENT_TABLE {
            for p in spec.procs {
                if let Some(prev) = seen.insert(*p, spec.id) {
                    panic!("프로세스 이름 {p:?} 가 {prev} 와 {} 에 겹친다", spec.id);
                }
            }
        }
    }

    /// `as_str` → `from_id` 왕복. 저장된 `was_agent` 를 되읽는 경로가 이걸 탄다 —
    /// 깨지면 재시작 때 그 pane 이 셸로 되살아난다.
    #[test]
    fn id_는_왕복한다() {
        for spec in AGENT_TABLE {
            let k = AgentKind::from_id(spec.id).expect("표에 있는 id 를 못 되읽었다");
            assert_eq!(k.as_str(), spec.id);
            assert_eq!(k, AgentKind::Other(spec));
        }
        for k in [AgentKind::Claude, AgentKind::Codex, AgentKind::Agy] {
            assert_eq!(AgentKind::from_id(k.as_str()), Some(k));
        }
        assert_eq!(AgentKind::from_id("없는하네스"), None);
    }

    /// comm 이 런처가 아니면 argv 를 볼 이유가 없다 — 그 문을 열어 두면 셸
    /// pane 마다 명령줄 표를 들추게 된다.
    #[test]
    fn 런처가_아니면_명령줄을_안_읽는다() {
        let t = vec![row(100, 1, "zsh"), row(200, 100, "vim")];
        assert_eq!(agent_pid_by_argv(&t, 100), None);
    }

    /// argv 힌트를 단 하네스는 이름 판정으로는 못 잡히는 것들이다(실측). 힌트가
    /// 빈 채로 남으면 그 줄은 표에 있어도 영영 안 선다.
    #[test]
    fn 이름에_숨는_하네스는_명령줄_힌트를_갖는다() {
        for id in ["gemini", "cursor", "hermes", "amp"] {
            let spec = AGENT_TABLE.iter().find(|s| s.id == id).expect("표에 없다");
            assert!(!spec.argv_hints.is_empty(), "{id}: argv 힌트가 비었다");
        }
    }
}

#[cfg(test)]
mod launcher_descend_tests {
    use super::descend_launchers;

    fn row(pid: u32, ppid: u32, name: &str) -> (u32, u32, String) {
        (pid, ppid, name.to_string())
    }

    #[test]
    fn stops_at_a_real_program() {
        let t = vec![row(100, 1, "zsh"), row(200, 100, "vim")];
        let got = descend_launchers(&t, Some((200, "vim".into())));
        assert_eq!(got.unwrap().1, "vim");
    }

    #[test]
    fn descends_past_node_to_the_real_binary() {
        // 실측 트리: 셸 → node(free-antigravity-cli) → agy
        let t = vec![
            row(100, 1, "zsh"),
            row(200, 100, "node"),
            row(300, 200, "agy"),
        ];
        let got = descend_launchers(&t, Some((200, "node".into())));
        assert_eq!(got.unwrap().1, "agy", "node 에서 멈추면 pane 닫기가 'node' 라고 묻는다");
    }

    #[test]
    fn descends_two_hops_for_npm_shim() {
        // codex 처럼 npm shim 을 한 번 더 지나는 경우
        let t = vec![
            row(100, 1, "zsh"),
            row(200, 100, "npm"),
            row(300, 200, "node"),
            row(400, 300, "codex"),
        ];
        let got = descend_launchers(&t, Some((200, "npm".into())));
        assert_eq!(got.unwrap().1, "codex");
    }

    #[test]
    fn keeps_launcher_when_it_has_no_child() {
        // `node` 를 맨손으로 띄운 REPL — 내려갈 곳이 없으면 그대로 둔다
        let t = vec![row(100, 1, "zsh"), row(200, 100, "node")];
        let got = descend_launchers(&t, Some((200, "node".into())));
        assert_eq!(got.unwrap().1, "node");
    }

    #[test]
    fn picks_the_newest_child() {
        let t = vec![
            row(100, 1, "zsh"),
            row(200, 100, "node"),
            row(300, 200, "old"),
            row(400, 200, "new"),
        ];
        let got = descend_launchers(&t, Some((200, "node".into())));
        assert_eq!(got.unwrap().1, "new");
    }
}
