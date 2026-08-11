//! 우측 칼럼의 「MCP·Skill」 탭 — 어느 하네스에 무엇이 붙어 있는지 한 화면에.
//!
//! 붙은 것을 확인하려면 지금까지 `claude mcp list` 와 `codex mcp list` 를 따로 치고,
//! 끄고 켜려면 `~/.claude.json` 과 `~/.codex/config.toml` 을 손으로 열어야 했다. 형식도
//! 서로 다르다(json vs toml). 2026-08-11 에 codex 의 unityMCP 하나가 꺼져 있지 않아
//! MCP 기동 전체가 밀린 일이 있었는데, 그 사실이 어디에도 안 보였다 — 그때 나온 요청이다.
//!
//! 하네스를 탭으로 가르지 않고 **로고를 단 섹션으로 나란히** 둔다(거노: "각 코덱스로고
//! 넣고 어떤거 있고, 클로드로고 넣고 어떤거 있고"). 전환식이면 "저쪽엔 뭐가 있더라"를
//! 늘 기억해야 하는데, 이 목록을 여는 이유의 절반이 그 비교다.
//!
//! 수집은 워커 스레드에서 돈다 — 설정 두 벌을 파싱하고 스킬 폴더를 훑는 일이라
//! (스킬만 29개) 렌더 루프에서 하면 프레임을 떨군다. 세션 기록 탭(`sesscol`)이
//! 저장소를 stat 하는 것과 같은 구조다.
use super::*;
use std::sync::atomic::Ordering::Relaxed;

/// 한 행의 높이. 이름 한 줄 + 상세 한 줄.
const ROW_H: f32 = 40.0;
/// 하네스 구분 머리(로고 + 이름 + 개수).
const SECTION_H: f32 = 30.0;
/// 종류 구분 머리(MCP / Skill).
const GROUP_H: f32 = 22.0;
/// 목록이 비었을 때 안내가 차지하는 높이.
const EMPTY_H: f32 = 44.0;
/// 재수집 간격. 설정 파일은 사람이 고칠 때만 바뀌므로 성기게 본다 — 대신 우리가
/// 직접 고친 직후에는 [`state::McpColState::stale`] 로 즉시 다시 읽는다.
const REFRESH_MS: u64 = 6000;

/// 한 줄이 무엇인가. 순서가 곧 화면 순서다 — 자주 보는 것부터.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum RowKind {
    Mcp,
    Skill,
    /// claude 플러그인. 스킬·MCP·에이전트·커맨드를 한 덩어리로 얹는다.
    Plugin,
    Agent,
    Command,
    Hook,
}

impl RowKind {
    /// 그룹 머리에 적히는 이름.
    fn label(self) -> &'static str {
        match self {
            RowKind::Mcp => "MCP",
            RowKind::Skill => "스킬",
            RowKind::Plugin => "플러그인",
            RowKind::Agent => "에이전트",
            RowKind::Command => "커맨드",
            RowKind::Hook => "훅",
        }
    }
    /// 화면에 나오는 차례. 선언 순서를 그대로 쓰되 숫자로 못 박는다 — 변형을 사이에
    /// 끼워 넣었을 때 화면 순서가 조용히 바뀌지 않도록.
    fn order(self) -> u8 {
        match self {
            RowKind::Mcp => 0,
            RowKind::Skill => 1,
            RowKind::Plugin => 2,
            RowKind::Agent => 3,
            RowKind::Command => 4,
            RowKind::Hook => 5,
        }
    }
    /// 접힘 상태를 저장할 때 쓰는 안정된 키. `Debug` 를 쓰면 변형 이름을 바꾸는
    /// 순간 남이 접어 둔 것이 조용히 풀린다.
    fn key(self) -> &'static str {
        match self {
            RowKind::Mcp => "mcp",
            RowKind::Skill => "skill",
            RowKind::Plugin => "plugin",
            RowKind::Agent => "agent",
            RowKind::Command => "command",
            RowKind::Hook => "hook",
        }
    }
}

/// 스코프가 나오는 차례. 넓은 것부터 좁은 것으로, 남이 준 것은 맨 뒤.
///
/// 「폴더」(`projects[cwd]`, 나만)와 「레포」(`.mcp.json`, 커밋되는 것)는 둘 다 이
/// 프로젝트 몫이지만 레포 쪽을 앞에 둔다 — 팀이 함께 쓰는 것이 먼저 눈에 들어와야 한다.
fn scope_order(scope: &str) -> u8 {
    match scope {
        // kasaterm 이 이 앱의 pane 에 직접 심은 것. 「지금 실제로 도는」 쪽이라 맨 앞이다.
        "kasaterm" => 0,
        "전역" => 1,
        "레포" => 2,
        "폴더" => 3,
        "플러그인" => 4,
        _ => 5,
    }
}

/// 접힘을 저장할 때 쓰는 안정된 스코프 키. 화면 글자를 그대로 키로 쓰면 문구를
/// 다듬는 순간 남이 접어 둔 것이 조용히 풀린다 — [`RowKind::key`] 와 같은 이유다.
fn scope_key(scope: &str) -> &'static str {
    match scope {
        "kasaterm" => "app",
        "전역" => "user",
        "레포" => "repo",
        "폴더" => "local",
        "플러그인" => "plugin",
        _ => "-",
    }
}

/// 목록의 한 줄. 설정 파일에서 읽은 그대로이고, 살았는지(실제 연결)까지는 보지
/// 않는다 — 그건 하네스를 띄워 물어야 알 수 있어 이 주기로 할 일이 아니다.
#[derive(Clone, Debug)]
pub(crate) struct McpRow {
    /// `"claude"` | `"codex"`.
    pub(crate) harness: &'static str,
    pub(crate) kind: RowKind,
    pub(crate) name: String,
    /// 사람이 보고 무엇인지 아는 한 줄 — 실행 커맨드나 URL, 스킬은 설명 앞머리.
    pub(crate) detail: String,
    /// 어디에 적힌 것인가 — `"전역"`/`"레포"`/`"폴더"`/`"플러그인"`.
    ///
    /// 이게 안 보이면 목록이 거짓말을 한다. 같은 이름이 세 곳에 있을 수 있고(실측:
    /// kasaspace 는 전역과 이 레포 `.mcp.json` 양쪽에 있다), 무엇보다 꺼짐이 **폴더마다
    /// 다르다** — 2026-08-11 지시("프로젝트별, 전역도 보이게해야해").
    ///
    /// 그리고 이제 **그룹을 가르는 축**이기도 하다 — 2026-08-11 지시("프로젝트랑 전역
    /// 나눠서 보이게는?"). 배지로만 적어 두면 60줄짜리 목록에서 전역과 레포가 이름순으로
    /// 뒤섞여, 「이 레포가 얹은 것」을 눈으로 못 고른다.
    pub(crate) scope: &'static str,
    /// 파일 하나가 곧 이 줄인 것들(스킬 폴더·에이전트/커맨드 `.md`)의 실제 자리.
    ///
    /// 지울 때 이름과 하네스로 폴더를 되짚지 않으려고 싣는다. 되짚는 방식은 읽는 자리가
    /// 늘 때마다 조용히 어긋난다 — 레포 스킬을 읽기 시작하면 홈 폴더에서 같은 이름을
    /// 찾아 **엉뚱한 것을 지운다**.
    pub(crate) path: Option<std::path::PathBuf>,
    /// 꺼져 있으면 회색으로 눕는다. claude MCP 는 이 창이 보고 있는 폴더 기준이다.
    pub(crate) enabled: bool,
    /// 여기서 껐다 켤 수 있나. codex 스킬만 거짓이다 — 그쪽은 폴더(대개 심볼릭
    /// 링크)가 곧 목록이라, 끄려면 지우는 수밖에 없어 토글과 삭제가 같아진다.
    pub(crate) toggleable: bool,
}

impl McpRow {
    /// 여기서 지울 수 있나. 아니면 `×` 를 아예 안 그린다 — 눌러 봐야 안 된다는 걸
    /// 아는 버튼은 없느니만 못하다.
    ///
    /// 플러그인이 준 것은 전부 제외한다. 파일이 플러그인 캐시 안에 있어 지워도
    /// 다음 갱신에 돌아오고, 그 폴더는 우리 것이 아니다 — 끄려면 플러그인 줄을
    /// 끄는 것이 맞는 자리다. 플러그인 자신도 제외한다: 지우기는 마켓 재설치가
    /// 걸린 일이라 끄기로 충분하다. 훅은 설정 파일 안의 항목이라 파일 단위로
    /// 지울 것이 없다.
    fn deletable(&self) -> bool {
        matches!(self.harness, "claude" | "codex")
            && self.scope != "플러그인"
            && matches!(
                self.kind,
                RowKind::Mcp | RowKind::Skill | RowKind::Agent | RowKind::Command
            )
    }
}

/// claude 의 MCP 서버는 세 곳에 나뉘어 적힌다. 전부 읽어야 목록이 실상과 맞는다.
///
/// - **전역**(user) — `~/.claude.json` 의 `mcpServers`. 모든 폴더에서 뜬다.
/// - **폴더**(local) — 같은 파일 `projects["<cwd>"].mcpServers`. 그 폴더에서 나만.
/// - **레포**(project) — `<cwd>/.mcp.json`. 커밋되는 파일이라 팀이 함께 쓴다.
///
/// 그리고 **꺼짐은 폴더마다 다르다**: `projects["<cwd>"].disabledMcpServers` 가 앞의 둘을,
/// `disabledMcpjsonServers` 가 `.mcp.json` 쪽을 끈다(두 배열은 서로 무관하다). 그래서
/// 이 함수는 cwd 없이는 절반만 아는 셈이고, cwd 가 없으면 전역만 켜진 채로 돌려준다.
///
/// 꺼진 목록에만 있고 정의가 어디에도 없는 이름들(claude.ai 커넥터, 플러그인 서버, 예전에
/// 지운 것들 — 실측 tmuxify 에서 14개 중 대부분)은 싣지 않는다. 실행 커맨드도 URL 도 알 수
/// 없어 이름만 남는 줄이 되는데, 그건 「무엇이 붙어 있나」에 답하지 못한다.
fn claude_mcp(home: &std::path::Path, cwd: Option<&std::path::Path>) -> Vec<McpRow> {
    let Ok(text) = std::fs::read_to_string(home.join(".claude.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let proj = cwd
        .and_then(|c| c.to_str())
        .and_then(|c| v.get("projects")?.get(c));
    let names = |key: &str| -> std::collections::HashSet<String> {
        proj.and_then(|p| p.get(key))
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let off = names("disabledMcpServers");
    let off_json = names("disabledMcpjsonServers");

    let mut rows = Vec::new();
    let mut push = |map: Option<&serde_json::Map<String, serde_json::Value>>,
                    scope: &'static str,
                    off: &std::collections::HashSet<String>| {
        for (name, cfg) in map.into_iter().flatten() {
            rows.push(McpRow {
                harness: "claude",
                kind: RowKind::Mcp,
                scope,
                path: None,
                enabled: !off.contains(name),
                name: name.clone(),
                detail: server_detail(cfg),
                toggleable: is_toggleable("claude", RowKind::Mcp, scope),
            });
        }
    };
    push(v.get("mcpServers").and_then(|m| m.as_object()), "전역", &off);
    push(
        proj.and_then(|p| p.get("mcpServers")).and_then(|m| m.as_object()),
        "폴더",
        &off,
    );
    let dot_mcp = cwd
        .map(|c| c.join(".mcp.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    push(
        dot_mcp
            .as_ref()
            .and_then(|d| d.get("mcpServers"))
            .and_then(|m| m.as_object()),
        "레포",
        &off_json,
    );
    // 같은 이름이 여러 스코프에 있을 수 있다(실측: kasaspace 는 전역과 이 레포 양쪽에).
    // 이름 다음에 스코프로 갈라 붙여 둔다 — 어느 쪽이 이겼는지는 하네스가 정하고,
    // 우리가 아는 건 "둘 다 적혀 있다"까지다.
    rows.sort_by(|a, b| a.name.cmp(&b.name).then(a.scope.cmp(b.scope)));
    rows
}

/// `~/.claude/settings.json` 의 `skillOverrides` — 꺼진 스킬 이름들.
///
/// 스킬은 폴더가 곧 목록이라, 껐다는 사실은 폴더가 아니라 이 표에만 있다. 안 읽으면
/// 이미 꺼 둔 것까지 켜진 것처럼 보인다(실측: 2026-08-11 기준 11개가 여기서 꺼져 있다).
fn claude_skills_off(home: &std::path::Path) -> std::collections::HashSet<String> {
    let Ok(text) = std::fs::read_to_string(home.join(".claude/settings.json")) else {
        return Default::default();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Default::default();
    };
    v.get("skillOverrides")
        .and_then(|o| o.as_object())
        .map(|o| {
            o.iter()
                .filter(|(_, v)| v.as_str() == Some("off"))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// `{command, args}` 또는 `{url}` 을 한 줄로. 무엇으로 뜨는지가 보여야 이름만으론
/// 구분 안 되는 것들(npx 로 뜨는 여럿)이 갈린다.
/// URL 키가 셋인 것은 하네스마다 이름을 달리 붙였기 때문이다 — claude/cursor 는 `url`,
/// windsurf/antigravity 는 `serverUrl`, gemini 계열엔 `httpUrl` 도 있다. 한 하네스만
/// 보고 키를 하나로 정하면 나머지는 이름만 뜨고 주소가 빈 줄이 된다.
fn server_detail(cfg: &serde_json::Value) -> String {
    for k in ["url", "serverUrl", "httpUrl"] {
        if let Some(url) = cfg.get(k).and_then(|u| u.as_str()) {
            return url.to_string();
        }
    }
    let cmd = cfg.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let args: Vec<&str> = cfg
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    if args.is_empty() {
        cmd.to_string()
    } else {
        format!("{cmd} {}", args.join(" "))
    }
}

/// codex 쪽 설정 — `~/.codex/config.toml` 의 `[mcp_servers.*]`.
///
/// `enabled = false` 를 읽어 실을 수 있는 건 이쪽뿐이다(claude 엔 그 개념이 없다).
/// 꺼진 것도 목록에 남긴다 — 껐다는 사실이 안 보이면 "왜 안 붙지"를 또 파게 된다.
fn codex_mcp(home: &std::path::Path) -> Vec<McpRow> {
    let Ok(text) = std::fs::read_to_string(home.join(".codex/config.toml")) else {
        return Vec::new();
    };
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return Vec::new();
    };
    let Some(servers) = doc.get("mcp_servers").and_then(|t| t.as_table()) else {
        return Vec::new();
    };
    let mut out: Vec<McpRow> = servers
        .iter()
        .map(|(name, item)| {
            let get = |k: &str| item.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let detail = get("url").unwrap_or_else(|| {
                let cmd = get("command").unwrap_or_default();
                let args: Vec<String> = item
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                if args.is_empty() {
                    cmd
                } else {
                    format!("{cmd} {}", args.join(" "))
                }
            });
            McpRow {
                harness: "codex",
                kind: RowKind::Mcp,
                // codex 는 서버를 한 파일에만 적는다 — 갈릴 자리가 없어 전부 전역이다.
                scope: "전역",
                path: None,
                name: name.to_string(),
                detail,
                enabled: item
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
                toggleable: is_toggleable("codex", RowKind::Mcp, "전역"),
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `SKILL.md` frontmatter 에서 `description` 한 줄을 뽑는다.
///
/// 세 형태가 실제로 섞여 있어 전부 받아야 한다 — 그냥 뒤를 잘라 쓰면 절반이
/// 따옴표째 나오거나 아예 빈다:
/// - `description: 그냥 텍스트`
/// - `description: "따옴표로 감싼 텍스트"` (콜론·특수문자가 들어간 것들)
/// - `description: >-` 다음 줄부터 들여쓴 블록 (긴 영문 설명들)
///
/// 목록의 곁글 한 줄이 필요할 뿐이라 블록은 첫 줄까지만 취한다.
fn skill_description(md: &str) -> String {
    let mut lines = md.lines().take(24);
    let Some(head) = lines
        .find_map(|l| l.strip_prefix("description:"))
        .map(str::trim)
    else {
        return String::new();
    };
    // 블록 스칼라(`>`/`>-`/`|`)면 본문은 다음 줄부터 들여쓰기로 온다.
    let raw = if head.is_empty() || head.starts_with('>') || head.starts_with('|') {
        lines
            .find(|l| !l.trim().is_empty())
            .filter(|l| l.starts_with(' ') || l.starts_with('\t'))
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        head.to_string()
    };
    raw.trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .to_string()
}

/// 스킬 폴더 하나 → 한 줄. `SKILL.md` 앞머리의 `description:` 을 상세로 쓴다.
///
/// `off` 는 꺼진 이름들(claude 만 그 표가 있다), `toggleable` 은 이 하네스에서 끌 수
/// 있는지. `is_dir()` 은 심볼릭 링크를 따라가므로 링크로 걸어 둔 스킬도 실린다.
fn skills_in(
    dir: &std::path::Path,
    harness: &'static str,
    scope: &'static str,
    off: &std::collections::HashSet<String>,
    toggleable: bool,
) -> Vec<McpRow> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<McpRow> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if name.starts_with('.') {
                return None;
            }
            let detail = std::fs::read_to_string(e.path().join("SKILL.md"))
                .map(|s| skill_description(&s))
                .unwrap_or_default();
            Some(McpRow {
                harness,
                kind: RowKind::Skill,
                scope,
                path: Some(e.path()),
                enabled: !off.contains(&name),
                name,
                detail,
                toggleable,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 설치된 claude 플러그인 하나.
struct Plugin {
    /// 마켓을 뗀 이름(`vercel`). 이 플러그인이 주는 것들의 이름 앞에 붙는다 —
    /// `vercel:nextjs`, MCP 는 `plugin:vercel:vercel`.
    name: String,
    /// 어느 마켓에서 왔나. 상세 줄에 적는다.
    market: String,
    /// 실체가 풀린 자리. 여기 아래의 `skills/`·`agents/`·`commands/`·`.mcp.json` 이
    /// 이 플러그인이 주는 전부다.
    path: std::path::PathBuf,
    on: bool,
}

/// 설치된 플러그인 목록. `installed_plugins.json` 이 실체 경로의 정본이다.
///
/// 폴더를 훑으면 안 되는 이유가 있다 — `plugins/cache/` 에는 같은 플러그인의 옛
/// 버전·커밋 폴더가 함께 쌓인다(실측: chrome-devtools-mcp 가 네 벌). 어느 것이 지금
/// 로드되는지는 그 json 의 `installPath` 만 안다.
///
/// 켜짐은 `settings.json` 의 `enabledPlugins` 다. 끈 플러그인도 설치는 남아 있어
/// 키가 `false` 로 적히므로, 키가 없으면 꺼진 것으로 본다.
fn claude_plugins(home: &std::path::Path) -> Vec<Plugin> {
    let read = |p: std::path::PathBuf| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    };
    let Some(inst) = read(home.join(".claude/plugins/installed_plugins.json")) else {
        return Vec::new();
    };
    let on = read(home.join(".claude/settings.json"))
        .and_then(|v| v.get("enabledPlugins").cloned())
        .unwrap_or_else(|| serde_json::json!({}));
    let mut out: Vec<Plugin> = inst
        .get("plugins")
        .and_then(|p| p.as_object())
        .into_iter()
        .flatten()
        .filter_map(|(key, entries)| {
            // 키가 `<이름>@<마켓>` 이다.
            let (name, market) = key.split_once('@').unwrap_or((key.as_str(), ""));
            let path = entries
                .as_array()
                .and_then(|a| a.first())
                .and_then(|e| e.get("installPath"))
                .and_then(|p| p.as_str())?;
            Some(Plugin {
                name: name.to_string(),
                market: market.to_string(),
                path: path.into(),
                on: on.get(key).and_then(|b| b.as_bool()).unwrap_or(false),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 폴더 안의 `*.md` 하나가 곧 한 줄인 것들 — 에이전트·커맨드. 스킬과 달리 폴더가
/// 아니라 파일이다.
///
/// 거르는 둘은 실측에서 나왔다: `*.md.tmpl` 템플릿이 같은 폴더에 함께 있어 그냥 세면
/// 개수가 두 배가 되고(vercel 에이전트 3개가 6개로), `_conventions.md` 같은 밑줄
/// 시작 파일은 등록되지 않는 부속 문서다.
fn md_entries_in(
    dir: &std::path::Path,
    harness: &'static str,
    kind: RowKind,
    prefix: &str,
    scope: &'static str,
    enabled: bool,
) -> Vec<McpRow> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<McpRow> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                return None;
            }
            let stem = p.file_stem().and_then(|s| s.to_str())?;
            if stem.starts_with('_') || stem.starts_with('.') {
                return None;
            }
            let detail = std::fs::read_to_string(&p)
                .map(|s| skill_description(&s))
                .unwrap_or_default();
            Some(McpRow {
                harness,
                kind,
                scope,
                enabled,
                name: format!("{prefix}{stem}"),
                detail,
                toggleable: false,
                path: Some(p),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 플러그인이 주는 MCP 서버. `<실체>/.mcp.json` 인데 **스키마가 두 가지**다 —
/// `{"mcpServers":{…}}` 로 감싼 것(vercel·kasapane)과 감싸지 않은 평면 맵(context7).
/// 둘 다 실제로 로드되므로 양쪽을 받는다.
fn plugin_mcp(p: &Plugin, off: &std::collections::HashSet<String>) -> Vec<McpRow> {
    let Some(v) = std::fs::read_to_string(p.path.join(".mcp.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    else {
        return Vec::new();
    };
    let map = v
        .get("mcpServers")
        .and_then(|m| m.as_object())
        .or_else(|| v.as_object());
    let mut out: Vec<McpRow> = map
        .into_iter()
        .flatten()
        // 감싸지 않은 평면 맵을 읽을 때 값이 객체가 아닌 키(스키마 필드 등)가 섞일 수
        // 있다 — 서버 정의만 남긴다.
        .filter(|(_, cfg)| cfg.is_object())
        .map(|(key, cfg)| {
            let id = format!("plugin:{}:{key}", p.name);
            McpRow {
                harness: "claude",
                kind: RowKind::Mcp,
                scope: "플러그인",
                path: None,
                // 플러그인이 꺼져 있으면 그 서버는 안 붙는다 — 따로 켜 둔 것과 무관하다.
                enabled: p.on && !off.contains(&id),
                detail: server_detail(cfg),
                // 켜진 플러그인의 서버만 여기서 끈다. 꺼진 플러그인 밑에서 끄면 화면은
                // 뒤집히는데 실제로는 아무것도 안 달라져, 플러그인 줄이 진짜 스위치다.
                toggleable: p.on,
                name: id,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 플러그인 자신과 그것이 주는 것 전부.
///
/// 꺼진 플러그인의 제공물도 싣는다 — 회색으로 눕지만 「켜면 무엇이 들어오나」가
/// 보여야 켤지 말지를 정할 수 있다(2026-08-11 지시 "다 뜨게하고").
fn claude_plugin_rows(
    home: &std::path::Path,
    off: &std::collections::HashSet<String>,
) -> Vec<McpRow> {
    let mut rows = Vec::new();
    let mut provided = Vec::new();
    for p in claude_plugins(home) {
        let skills = skills_in(
            &p.path.join("skills"),
            "claude",
            "플러그인",
            &Default::default(),
            false,
        )
        .into_iter()
        .map(|r| McpRow {
            enabled: p.on,
            name: format!("{}:{}", p.name, r.name),
            ..r
        });
        let agents = md_entries_in(
            &p.path.join("agents"),
            "claude",
            RowKind::Agent,
            &format!("{}:", p.name),
            "플러그인",
            p.on,
        );
        let commands = md_entries_in(
            &p.path.join("commands"),
            "claude",
            RowKind::Command,
            &format!("{}:", p.name),
            "플러그인",
            p.on,
        );
        let mcps = plugin_mcp(&p, off);
        // 무엇을 주는 플러그인인지가 이름만으론 안 보인다 — 개수를 상세 줄에 적는다.
        let counts = [
            (skills.len(), "스킬"),
            (mcps.len(), "MCP"),
            (agents.len(), "에이전트"),
            (commands.len(), "커맨드"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, l)| format!("{l} {n}"))
        .collect::<Vec<_>>();
        let detail = if counts.is_empty() {
            // 폴더가 비어 보이는 플러그인이 실제로 있다 — rust-analyzer-lsp 는 실체가
            // 마켓 매니페스트에 인라인으로 적혀 있어 설치 폴더엔 문서뿐이다.
            p.market.clone()
        } else {
            format!("{} · {}", p.market, counts.join(" · "))
        };
        rows.push(McpRow {
            harness: "claude",
            kind: RowKind::Plugin,
            // 플러그인 자체는 `settings.json` 한 곳에서만 켜고 끈다 — 전역이다.
            scope: "전역",
            path: None,
            enabled: p.on,
            name: p.name.clone(),
            detail,
            toggleable: true,
        });
        provided.extend(skills);
        provided.extend(mcps);
        provided.extend(agents);
        provided.extend(commands);
    }
    rows.extend(provided);
    rows
}

/// `settings.json` 의 훅. 무엇이 내 입력·도구 호출에 끼어드는지가 여기 말고는 안 보인다.
///
/// 이벤트 이름을 상세 줄 앞에 적는 건 스코프 배지를 못 쓰기 때문이다(배지는 고정
/// 문자열만 받는데 이벤트 이름은 설정에서 온다). 끄는 키는 없다 — 지우는 것뿐이다.
fn hooks_in(
    settings: &std::path::Path,
    harness: &'static str,
    scope: &'static str,
) -> Vec<McpRow> {
    let Some(v) = std::fs::read_to_string(settings)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (event, groups) in v.get("hooks").and_then(|h| h.as_object()).into_iter().flatten() {
        for g in groups.as_array().into_iter().flatten() {
            let matcher = g.get("matcher").and_then(|m| m.as_str()).unwrap_or("");
            for h in g.get("hooks").and_then(|h| h.as_array()).into_iter().flatten() {
                let Some(cmd) = h.get("command").and_then(|c| c.as_str()) else {
                    continue;
                };
                // 커맨드가 스크립트 한 줄이면 파일 이름이, 인라인 셸이면 앞머리가
                // 그 훅을 알아보는 단서다.
                let short = cmd
                    .split_whitespace()
                    .find(|t| t.contains('/') || t.ends_with(".sh") || t.ends_with(".py"))
                    .and_then(|t| t.rsplit('/').next())
                    .unwrap_or(cmd);
                out.push(McpRow {
                    harness,
                    kind: RowKind::Hook,
                    scope,
                    path: None,
                    enabled: true,
                    name: short.to_string(),
                    detail: if matcher.is_empty() {
                        event.clone()
                    } else {
                        format!("{event}({matcher})")
                    },
                    toggleable: false,
                });
            }
        }
    }
    out.sort_by(|a, b| a.detail.cmp(&b.detail).then(a.name.cmp(&b.name)));
    out
}

/// claude·codex 말고도 이 머신에서 MCP 를 쓰는 도구들. 설정 파일이 **실재하는 것만**
/// 싣는다 — 2026-08-11 지시("orca처럼 있는거 추적해서 다뜨게못하나").
///
/// 글로브로 훑지 않고 표로 적는 이유는 실측이다: 홈 아래 `*mcp*.json` 을 훑으면
/// `~/.gemini/antigravity-backup/`·`antigravity-ide/` 같은 **백업과 옛 사본**이 함께
/// 잡혀, 지금 아무도 안 읽는 파일이 목록에서 살아 있는 척한다. 어느 파일을 실제로
/// 읽는지는 도구마다 정해져 있으므로 그 자리를 적는 편이 정확하다.
///
/// 여긴 **읽기만** 한다. 스키마가 제각각인데다(아래 `key`·꺼짐 표기) 우리가 안 띄우는
/// 도구의 설정을 고치는 것은 되돌릴 자리가 없다. claude·codex 만 우리가 띄우므로 쓰기도
/// 거기까지다.
struct Ext {
    /// 섹션 이름 겸 접힘 키.
    id: &'static str,
    /// 홈 기준 상대 경로.
    rel: &'static str,
    /// 서버 맵이 들어 있는 키. VS Code 는 `servers`, opencode 는 `mcp` 다.
    key: &'static str,
}

/// 전역 설정 자리. macOS 전용 경로는 그 OS 에서만 본다.
fn ext_globals() -> Vec<Ext> {
    let mut v = vec![
        Ext { id: "gemini", rel: ".gemini/settings.json", key: "mcpServers" },
        Ext { id: "antigravity", rel: ".gemini/antigravity/mcp_config.json", key: "mcpServers" },
        Ext { id: "cursor", rel: ".cursor/mcp.json", key: "mcpServers" },
        Ext { id: "windsurf", rel: ".codeium/windsurf/mcp_config.json", key: "mcpServers" },
        Ext { id: "opencode", rel: ".config/opencode/opencode.json", key: "mcp" },
        Ext { id: "qwen", rel: ".qwen/settings.json", key: "mcpServers" },
        Ext { id: "copilot", rel: ".copilot/mcp-config.json", key: "mcpServers" },
    ];
    if cfg!(target_os = "macos") {
        v.push(Ext {
            id: "vscode",
            rel: "Library/Application Support/Code/User/mcp.json",
            key: "servers",
        });
        v.push(Ext {
            id: "claude desktop",
            rel: "Library/Application Support/Claude/claude_desktop_config.json",
            key: "mcpServers",
        });
    }
    v
}

/// 레포에 커밋되는 자리. claude 의 `.mcp.json` 은 [`claude_mcp`] 가 이미 읽는다.
fn ext_repo() -> Vec<Ext> {
    vec![
        Ext { id: "cursor", rel: ".cursor/mcp.json", key: "mcpServers" },
        Ext { id: "vscode", rel: ".vscode/mcp.json", key: "servers" },
        Ext { id: "gemini", rel: ".gemini/settings.json", key: "mcpServers" },
        Ext { id: "opencode", rel: "opencode.json", key: "mcp" },
    ]
}

/// 설정 파일 하나 → 줄들. 꺼짐 표기가 두 갈래라 둘 다 본다: windsurf·antigravity 는
/// `disabled: true`, opencode 는 `enabled: false`. 어느 쪽도 없으면 켜진 것이다.
fn ext_rows(path: &std::path::Path, e: &Ext, scope: &'static str) -> Vec<McpRow> {
    let Some(v) = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    else {
        return Vec::new();
    };
    let Some(map) = v.get(e.key).and_then(|m| m.as_object()) else {
        return Vec::new();
    };
    let mut out: Vec<McpRow> = map
        .iter()
        .filter(|(_, cfg)| cfg.is_object())
        .map(|(name, cfg)| McpRow {
            harness: e.id,
            kind: RowKind::Mcp,
            scope,
            path: None,
            enabled: !cfg.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false)
                && cfg.get("enabled").and_then(|d| d.as_bool()).unwrap_or(true),
            name: name.clone(),
            detail: server_detail(cfg),
            toggleable: false,
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `~/.claude.json` 의 이 폴더 몫 `disabledMcpServers`. 플러그인 서버도 같은 배열에
/// `plugin:<플러그인>:<서버>` 이름으로 들어간다(실측: `plugin:vercel:vercel`).
fn claude_mcp_off(
    home: &std::path::Path,
    cwd: Option<&std::path::Path>,
) -> std::collections::HashSet<String> {
    std::fs::read_to_string(home.join(".claude.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .zip(cwd.and_then(|c| c.to_str()).map(str::to_string))
        .and_then(|(v, c)| {
            v.get("projects")?
                .get(c)?
                .get("disabledMcpServers")?
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect()
                })
        })
        .unwrap_or_default()
}

/// 두 하네스가 로드하는 것 전부를 한 벌로. 워커 스레드에서 부른다.
///
/// `cwd` 는 이 창이 보고 있는 폴더 — claude 쪽 꺼짐과 `.mcp.json` 이 폴더마다 다르다.
///
/// 마지막 정렬이 곧 화면 구조다. 그리기와 높이 계산이 **앞줄과 달라졌나**만 보고
/// 섹션·그룹 머리를 내므로, 같은 (하네스, 종류)가 목록에서 흩어져 있으면 머리가 두 번
/// 그려진다. 모으는 순서에 기대지 않고 여기서 한 번 못 박는다.
pub(crate) fn collect(cwd: Option<&std::path::Path>) -> Vec<McpRow> {
    let Some(home) = kasa_socket::home_dir() else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    rows.extend(claude_mcp(&home, cwd));
    let off = claude_skills_off(&home);
    // 홈과 레포를 같은 규칙으로 두 번 읽는다. 레포 쪽(`<cwd>/.claude/…`)은 지금까지
    // 통째로 안 보였다 — 팀이 커밋해 둔 스킬·에이전트가 실제로 실리는데도.
    for (base, scope) in [
        (home.join(".claude"), "전역"),
        (cwd.map(|c| c.join(".claude")).unwrap_or_default(), "레포"),
    ] {
        if base.as_os_str().is_empty() {
            continue;
        }
        rows.extend(skills_in(
            &base.join("skills"),
            "claude",
            scope,
            &off,
            is_toggleable("claude", RowKind::Skill, scope),
        ));
        rows.extend(md_entries_in(
            &base.join("agents"),
            "claude",
            RowKind::Agent,
            "",
            scope,
            true,
        ));
        rows.extend(md_entries_in(
            &base.join("commands"),
            "claude",
            RowKind::Command,
            "",
            scope,
            true,
        ));
        rows.extend(hooks_in(&base.join("settings.json"), "claude", scope));
    }
    // 이 앱이 자기 pane 에 심은 훅. **여기가 실제로 도는 것들이다** — shim 이 세션 스코프
    // `--settings` 로 주입하므로 사용자 `settings.json` 어디에도 안 적힌다. 그래서 앞의
    // 두 자리만 읽으면 화면엔 하나도 없는데 pane 에선 여덟 개가 돌고 있는 꼴이 된다
    // (2026-08-11 확인). 경로를 짐작하지 않는다 — 이 프로세스가 만든 자리라
    // 부팅 때 자기가 심어 둔 env 를 그대로 본다.
    if let Some(shim) = std::env::var_os("KASATERM_TMUX_SHIM_DIR").map(std::path::PathBuf::from) {
        rows.extend(hooks_in(
            &shim.join("claude-hooks-settings.json"),
            "claude",
            "kasaterm",
        ));
        rows.extend(hooks_in(&shim.join("codex-hooks.json"), "codex", "kasaterm"));
    }
    rows.extend(claude_plugin_rows(&home, &claude_mcp_off(&home, cwd)));
    rows.extend(codex_mcp(&home));
    rows.extend(skills_in(
        &home.join(".codex/skills"),
        "codex",
        "전역",
        &Default::default(),
        is_toggleable("codex", RowKind::Skill, "전역"),
    ));
    for e in ext_globals() {
        rows.extend(ext_rows(&home.join(e.rel), &e, "전역"));
    }
    if let Some(c) = cwd {
        for e in ext_repo() {
            rows.extend(ext_rows(&c.join(e.rel), &e, "레포"));
        }
    }
    // 우리가 띄우는 둘을 먼저, 나머지는 이름순. 그 안에서 종류 → 스코프 → 이름.
    //
    // 스코프가 이름보다 앞이라 **플러그인이 준 것이 저절로 뒤로 몰린다** — 이름순으로만
    // 두면 플러그인 스킬(실측 64개 중 40개)이 앞자리를 차지해 내가 쓴 스킬을 보려면 늘
    // 스크롤해야 했다. 이제 아예 다른 그룹이라 통째로 접을 수도 있다.
    rows.sort_by(|a, b| {
        let h = |x: &str| match x {
            "claude" => 0u8,
            "codex" => 1,
            _ => 2,
        };
        h(a.harness)
            .cmp(&h(b.harness))
            .then(a.harness.cmp(b.harness))
            .then(a.kind.order().cmp(&b.kind.order()))
            .then(scope_order(a.scope).cmp(&scope_order(b.scope)))
            .then(a.name.cmp(&b.name))
    });
    rows
}

/// 설정 파일을 통째로 갈아 끼운다 — 같은 디렉터리의 임시 파일에 쓰고 rename.
///
/// 하네스가 늘 열어 보는 파일이라 반쪽만 쓰인 순간이 있으면 안 된다. 그 순간에 읽히면
/// 파싱이 깨져 MCP 가 통째로 안 붙거나, 더 나쁘게는 설정이 비어 있는 것으로 읽힌다.
/// rename 은 같은 파일시스템 안에서 원자적이므로 그 순간이 아예 없어진다.
///
/// 처음 고칠 때 `.kasaterm.bak` 을 한 벌 남긴다 — 우리가 남의 설정을 고치는 것이라,
/// 뭔가 잘못됐을 때 손으로 되돌릴 자리가 있어야 한다. 매번 덮지 않는 이유는 그 반대다:
/// 두 번째 토글이 첫 번째의 결과를 백업으로 만들어 버리면 원본이 사라진다.
fn write_config_atomic(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    let bak = path.with_extension(format!(
        "{}.kasaterm.bak",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    if !bak.exists() {
        let _ = std::fs::copy(path, &bak);
    }
    let tmp = path.with_extension(format!(
        "{}.kasaterm.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

/// codex MCP 서버를 껐다 켠다 — `[mcp_servers.<이름>]` 의 `enabled`.
///
/// `toml_edit` 로 여는 이유는 주석과 줄 순서를 지키기 위해서다. 이 파일은 사람이 손으로
/// 관리해 온 것이고(주석이 실제로 달려 있다), 통째로 재직렬화하면 그게 다 날아간다.
///
/// 켤 때는 `enabled = true` 를 쓰지 않고 키를 지운다 — 없는 것이 기본값이라, 남겨 두면
/// 손으로 열어 본 사람에게 "왜 이것만 명시돼 있지"라는 질문을 남긴다.
fn set_codex_mcp_enabled(home: &std::path::Path, name: &str, on: bool) -> anyhow::Result<()> {
    let path = home.join(".codex/config.toml");
    let text = std::fs::read_to_string(&path)?;
    let mut doc = text.parse::<toml_edit::DocumentMut>()?;
    let tbl = doc
        .get_mut("mcp_servers")
        .and_then(|t| t.as_table_mut())
        .and_then(|t| t.get_mut(name))
        .ok_or_else(|| anyhow::anyhow!("codex 설정에 {name} 이 없다"))?;
    if on {
        if let Some(t) = tbl.as_table_mut() {
            t.remove("enabled");
        } else if let Some(t) = tbl.as_inline_table_mut() {
            t.remove("enabled");
        }
    } else {
        tbl["enabled"] = toml_edit::value(false);
    }
    write_config_atomic(&path, &doc.to_string())?;
    Ok(())
}

/// claude 스킬을 껐다 켠다 — `~/.claude/settings.json` 의 `skillOverrides`.
///
/// 켤 때 `"on"` 을 넣지 않고 항목을 지우는 것도 위와 같은 이유다. 이 표는 "기본에서
/// 벗어난 것"만 담는 자리라, 켜진 스킬이 늘어설수록 무엇을 건드렸는지가 안 보인다.
fn set_claude_skill_enabled(home: &std::path::Path, name: &str, on: bool) -> anyhow::Result<()> {
    let path = home.join(".claude/settings.json");
    let text = std::fs::read_to_string(&path)?;
    let mut v: serde_json::Value = serde_json::from_str(&text)?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("settings.json 이 객체가 아니다"))?;
    let ov = obj
        .entry("skillOverrides")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("skillOverrides 가 객체가 아니다"))?;
    if on {
        ov.remove(name);
    } else {
        ov.insert(name.to_string(), serde_json::json!("off"));
    }
    // 사람이 열어 보는 파일이라 들여쓰기를 지킨다.
    write_config_atomic(&path, &format!("{}\n", serde_json::to_string_pretty(&v)?))?;
    Ok(())
}

/// 폴더를 휴지통으로 옮긴다. 지우지 않는 이유는 하나다 — 되돌릴 수 있어야 한다.
///
/// 스킬 폴더는 손으로 쓴 것일 수도, 다른 곳을 가리키는 심볼릭 링크일 수도 있다(실측:
/// codex 스킬 5개 중 4개가 링크다). 링크면 링크만 옮겨지므로 원본은 그대로 남는다.
/// 이름이 겹칠 수 있어 뒤에 시각을 붙인다 — 겹치면 rename 이 조용히 덮는다.
///
/// 홈을 인자로 받는 건 테스트 때문이다 — 스스로 찾게 두면 테스트가 돌 때마다 진짜
/// 휴지통에 항목이 하나씩 쌓이고, macOS 에선 그걸 프로그램이 다시 읽어 치울 수도 없다
/// (`~/.Trash` 는 Full Disk Access 없이는 목록조차 못 본다).
#[cfg(target_os = "macos")]
fn trash(home: &std::path::Path, path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let trash = home.join(".Trash");
    std::fs::create_dir_all(&trash)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::other("이름이 이상하다"))?;
    let dest = trash.join(format!("{name}-kasaterm-{stamp}"));
    std::fs::rename(path, &dest)?;
    Ok(dest)
}

/// 휴지통이 없는 곳에선 지우지 않는다 — 되돌릴 수 없는 삭제를 조용히 하느니 실패가 낫다.
#[cfg(not(target_os = "macos"))]
fn trash(_home: &std::path::Path, _path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    Err(std::io::Error::other("이 OS 에선 휴지통으로 못 보낸다"))
}

/// 파일 하나가 곧 그 항목인 것들(스킬 폴더·에이전트/커맨드 `.md`)을 휴지통으로.
///
/// 자리를 이름으로 되짚지 않고 [`McpRow::path`] 를 그대로 쓴다. 되짚는 방식은 읽는
/// 자리가 늘 때마다 조용히 어긋난다 — 레포 스킬을 읽기 시작한 지금, 홈에서 같은 이름을
/// 찾으면 **엉뚱한 것을 지운다**.
///
/// 그래도 확인 둘은 남긴다: 목록이 가리키는 자리가 스킬·에이전트·커맨드 폴더 **안**인지,
/// 그리고 실제로 있는지. 링크 자체를 봐야 하므로 `exists()` 가 아니라
/// `symlink_metadata` 다 — `exists()` 는 링크를 따라가서 끊어진 링크를 없는 것으로 쳐,
/// 목록엔 있는데 지울 수 없는 줄을 만든다.
fn trash_entry(home: &std::path::Path, path: &std::path::Path) -> anyhow::Result<String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("이름을 못 읽었다"))?
        .to_string();
    let parent_ok = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str());
    if !matches!(parent_ok, Some("skills" | "agents" | "commands")) {
        return Err(anyhow::anyhow!("여기 것이 아니다: {}", path.display()));
    }
    if std::fs::symlink_metadata(path).is_err() {
        return Err(anyhow::anyhow!("그 자리에 없다: {}", path.display()));
    }
    let dest = trash(home, path)?;
    Ok(format!(
        "{name} 휴지통으로 ({})",
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("")
    ))
}

/// codex MCP 서버를 설정에서 뺀다. 주석·줄 순서를 지키려 `toml_edit` 으로 지운다.
fn remove_codex_mcp(home: &std::path::Path, name: &str) -> anyhow::Result<String> {
    let path = home.join(".codex/config.toml");
    let text = std::fs::read_to_string(&path)?;
    let mut doc = text.parse::<toml_edit::DocumentMut>()?;
    let servers = doc
        .get_mut("mcp_servers")
        .and_then(|t| t.as_table_mut())
        .ok_or_else(|| anyhow::anyhow!("codex 설정에 mcp_servers 가 없다"))?;
    if servers.remove(name).is_none() {
        return Err(anyhow::anyhow!("codex 설정에 {name} 이 없다"));
    }
    write_config_atomic(&path, &doc.to_string())?;
    Ok(format!("{name} 지웠다 — codex"))
}

/// claude MCP 서버를 뺀다. 우리가 `~/.claude.json` 을 쓰지 않고 CLI 에 맡긴다.
///
/// 그 파일은 수 MB 짜리에 도는 세션들이 함께 쓴다(세션 기록·프로젝트 상태가 다 거기
/// 있다). 우리가 읽고-고쳐-쓰는 사이에 다른 세션이 쓰면 그쪽 기록이 통째로 날아간다.
/// `claude mcp remove` 는 claude 자신이 제 파일을 제 방식으로 고치는 것이라 그 경합을
/// 우리가 만들지 않는다. 대신 프로세스를 띄우는 일이라 워커에서 불러야 한다.
fn remove_claude_mcp(name: &str, scope: &str) -> anyhow::Result<String> {
    // `-s` 는 어느 자리에서 지울지다. 배지에 보이는 스코프가 그대로 여기로 온다.
    let s = match scope {
        "폴더" => "local",
        "레포" => "project",
        _ => "user",
    };
    let out = std::process::Command::new("claude")
        .args(["mcp", "remove", name, "-s", s])
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim().lines().next().unwrap_or("이유 미상");
        return Err(anyhow::anyhow!("{err}"));
    }
    Ok(format!("{name} 지웠다 — claude {scope}"))
}

/// 더하기 전에 이름과 주소를 본다. 통과하면 정리된 (이름, 주소)를 준다.
///
/// 설정 파일에 그대로 적히는 값이라, 여기서 막지 않으면 하네스가 뜰 때 파싱이 깨지거나
/// (이름에 점·따옴표) 조용히 아무 데도 못 붙는다(스킴 없는 주소).
fn validate_add(name: &str, url: &str) -> Result<(String, String), String> {
    let name = name.trim();
    let url = url.trim();
    if name.is_empty() {
        return Err("이름이 비었다".into());
    }
    // codex 는 이름이 toml 테이블 키가 된다 — 점이 들어가면 중첩 테이블로 읽힌다.
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("이름은 영문·숫자·-·_ 만".into());
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("주소는 http:// 나 https:// 로".into());
    }
    Ok((name.to_string(), url.to_string()))
}

/// codex 에 URL 서버를 더한다 — `[mcp_servers.<이름>]` 에 `url` 한 줄.
fn add_codex_mcp(home: &std::path::Path, name: &str, url: &str) -> anyhow::Result<String> {
    let path = home.join(".codex/config.toml");
    let text = std::fs::read_to_string(&path)?;
    let mut doc = text.parse::<toml_edit::DocumentMut>()?;
    if doc.get("mcp_servers").is_none() {
        doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let servers = doc["mcp_servers"]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("mcp_servers 가 테이블이 아니다"))?;
    // 덮어쓰면 원래 있던 서버의 설정이 말없이 사라진다.
    if servers.get(name).is_some() {
        return Err(anyhow::anyhow!("{name} 은 이미 있다"));
    }
    let mut t = toml_edit::Table::new();
    t["url"] = toml_edit::value(url);
    servers.insert(name, toml_edit::Item::Table(t));
    write_config_atomic(&path, &doc.to_string())?;
    Ok(format!("{name} 더했다 — codex"))
}

/// claude 에 URL 서버를 더한다. 지우기와 같은 이유로 CLI 에 맡긴다 —
/// `~/.claude.json` 은 도는 세션들이 함께 쓴다.
fn add_claude_mcp(name: &str, url: &str) -> anyhow::Result<String> {
    let out = std::process::Command::new("claude")
        .args([
            "mcp", "add", "--transport", "http", name, url, "-s", "user",
        ])
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim().lines().next().unwrap_or("이유 미상");
        return Err(anyhow::anyhow!("{err}"));
    }
    Ok(format!("{name} 더했다 — claude 전역"))
}

/// claude MCP 서버를 **이 폴더에서만** 껐다 켠다.
///
/// 꺼짐이 적히는 자리가 스코프에 따라 갈린다: `.mcp.json` 서버(스코프 「레포」)는
/// `disabledMcpjsonServers`, 나머지는 `disabledMcpServers`. 두 배열은 서로를 안 본다 —
/// 틀린 쪽에 적으면 아무 일도 안 일어난 것처럼 보인다.
///
/// **이 파일만 다르게 다뤄야 한다.** `~/.claude.json` 은 지금 도는 claude 세션 전부가
/// 함께 쓰는 파일이고(세션 기록·프로젝트 상태가 다 여기 있다), 우리가 읽고-고쳐-쓰는
/// 사이에 다른 세션이 자기 기록을 쓰면 우리 쓰기가 그것을 통째로 지운다. 그래서 읽은
/// 시각을 들고 있다가 쓰기 직전에 다시 재고, 그 사이 누가 건드렸으면 포기한다 —
/// 덮어쓰는 것보다 "다시 눌러라"가 낫다. 재고 쓰는 사이의 틈은 여전히 남지만(밀리초),
/// 파일 잠금 없이 줄일 수 있는 데까지는 줄인 것이다.
fn set_claude_mcp_enabled(
    home: &std::path::Path,
    cwd: &std::path::Path,
    name: &str,
    scope: &str,
    on: bool,
) -> anyhow::Result<()> {
    let path = home.join(".claude.json");
    let before = std::fs::metadata(&path)?.modified()?;
    write_claude_disabled(&path, before, cwd, name, scope, on)
}

/// 위의 몸통. 읽은 시각을 인자로 받는 건 테스트 때문이다 — 스스로 재게 두면 "그
/// 사이에 남이 고쳤다"를 시간 경합으로만 만들 수 있어, 검사가 도는 날과 안 도는 날이
/// 생긴다.
fn write_claude_disabled(
    path: &std::path::Path,
    before: std::time::SystemTime,
    cwd: &std::path::Path,
    name: &str,
    scope: &str,
    on: bool,
) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut v: serde_json::Value = serde_json::from_str(&text)?;
    let key = if scope == "레포" {
        "disabledMcpjsonServers"
    } else {
        "disabledMcpServers"
    };
    let cwd_s = cwd
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("폴더 경로가 이상하다"))?
        .to_string();
    fn obj(
        val: &mut serde_json::Value,
    ) -> anyhow::Result<&mut serde_json::Map<String, serde_json::Value>> {
        val.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!(".claude.json 의 모양이 예상과 다르다"))
    }
    let projects = obj(&mut v)?
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}));
    let proj = obj(projects)?
        .entry(cwd_s)
        .or_insert_with(|| serde_json::json!({}));
    let arr = obj(proj)?
        .entry(key)
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("{key} 가 배열이 아니다"))?;
    if on {
        arr.retain(|x| x.as_str() != Some(name));
    } else if !arr.iter().any(|x| x.as_str() == Some(name)) {
        arr.push(serde_json::json!(name));
    }
    let body = format!("{}\n", serde_json::to_string_pretty(&v)?);
    if std::fs::metadata(path)?.modified()? != before {
        return Err(anyhow::anyhow!("다른 세션이 방금 설정을 고쳤다 — 다시 눌러라"));
    }
    write_config_atomic(path, &body)?;
    Ok(())
}

/// 여기서 껐다 켤 수 있는 조합인가. 넷 중 셋이다 — codex 스킬만 빠진다: 폴더(대개
/// 심볼릭 링크)가 곧 목록이라 끄기가 삭제와 같아진다.
///
/// claude MCP 는 **이 폴더에서만** 꺼진다(전역으로 끄는 설정이 자체가 없다). 스코프
/// 배지가 붙기 전이라면 전역 줄에 전역인 척 체크박스를 다는 셈이라 거짓이었지만, 이제
/// 「전역」이 화면에 적혀 있고 끈 결과도 그 폴더에서만 반영돼 보인다.
///
/// 목록의 표시와 쓰기 게이트가 이 한 함수를 같이 본다 — 갈라지면 눌리는데 안 써지거나
/// 그 반대가 된다.
fn is_toggleable(harness: &str, kind: RowKind, scope: &str) -> bool {
    match kind {
        // 끄는 키가 **없다**. 추측이 아니라 부재 확인이다 — settings.json 과 157KB
        // 짜리 `.claude.json` 양쪽에서 agentOverrides·disabledCommands·disabledHooks
        // 계열을 정규식으로 훑어 매치가 0건이었다(2026-08-11). 지우는 것뿐이다.
        RowKind::Agent | RowKind::Command | RowKind::Hook => false,
        // codex 스킬은 폴더(대개 심볼릭 링크)가 곧 목록이라 끄기가 삭제와 같아진다.
        RowKind::Skill if harness == "codex" => false,
        // 플러그인이 준 스킬엔 개별 키가 없다 — 플러그인째 꺼야 한다.
        RowKind::Skill => scope != "플러그인",
        RowKind::Plugin => true,
        // 우리가 띄우는 둘만 고친다. 나머지 하네스(cursor·gemini·vscode…)는 스키마도
        // 꺼짐 표기도 제각각이고 우리가 그 도구를 띄우지도 않아, 잘못 써 놓으면 되돌릴
        // 자리가 없다 — 목록에는 올리되 손은 안 댄다.
        RowKind::Mcp => matches!(harness, "claude" | "codex"),
    }
}

/// 플러그인을 껐다 켠다 — `settings.json` 의 `enabledPlugins`. 여긴 **전역**이다
/// (MCP 꺼짐이 폴더별인 것과 다르다).
///
/// 키가 `<이름>@<마켓>` 이라 목록의 이름만으론 모자란다. 설치 목록에서 되찾되, 같은
/// 이름이 두 마켓에 있으면 어느 쪽인지 우리가 정할 일이 아니라 거절한다.
fn set_claude_plugin_enabled(home: &std::path::Path, name: &str, on: bool) -> anyhow::Result<()> {
    let inst: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(
            home.join(".claude/plugins/installed_plugins.json"),
        )?)?;
    let keys: Vec<String> = inst
        .get("plugins")
        .and_then(|p| p.as_object())
        .into_iter()
        .flatten()
        .map(|(k, _)| k.clone())
        .filter(|k| k.split('@').next() == Some(name))
        .collect();
    let [key] = keys.as_slice() else {
        return Err(anyhow::anyhow!(
            "{name} 을 설치 목록에서 하나로 못 좁혔다 ({}건)",
            keys.len()
        ));
    };
    let path = home.join(".claude/settings.json");
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("settings.json 이 객체가 아니다"))?;
    // 스킬과 달리 켤 때도 키를 지우지 않는다 — 이 표는 "설치된 것의 on/off" 라서,
    // 키가 없으면 꺼진 것으로 읽힌다.
    obj.entry("enabledPlugins")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("enabledPlugins 가 객체가 아니다"))?
        .insert(key.clone(), serde_json::json!(on));
    write_config_atomic(&path, &format!("{}\n", serde_json::to_string_pretty(&v)?))?;
    Ok(())
}

/// 한 줄을 뒤집는다. 어느 파일을 고칠지는 (하네스, 종류)가 정한다.
fn toggle_row(row: &McpRow, cwd: Option<&std::path::Path>) -> anyhow::Result<()> {
    // 줄이 들고 있는 판정과 규칙을 함께 본다. 줄 쪽에는 규칙만으로 모르는 사정이
    // 하나 더 실린다 — 꺼진 플러그인이 준 MCP 는 여기서 끄면 화면만 뒤집힌다.
    if !row.toggleable || !is_toggleable(row.harness, row.kind, row.scope) {
        return Err(anyhow::anyhow!("여기선 못 끈다"));
    }
    let home = kasa_socket::home_dir().ok_or_else(|| anyhow::anyhow!("홈 디렉터리를 못 찾았다"))?;
    let on = !row.enabled;
    match (row.harness, row.kind) {
        ("codex", RowKind::Mcp) => set_codex_mcp_enabled(&home, &row.name, on),
        ("claude", RowKind::Skill) => set_claude_skill_enabled(&home, &row.name, on),
        ("claude", RowKind::Plugin) => set_claude_plugin_enabled(&home, &row.name, on),
        ("claude", RowKind::Mcp) => {
            // 꺼짐이 폴더에 적히므로 폴더를 모르면 끌 자리가 없다.
            let cwd = cwd.ok_or_else(|| anyhow::anyhow!("이 pane 의 폴더를 몰라 못 끈다"))?;
            set_claude_mcp_enabled(&home, cwd, &row.name, row.scope, on)
        }
        _ => Err(anyhow::anyhow!("여기선 못 끈다")),
    }
}

/// 화면에 놓이는 것 하나. 머리도 줄도 여기서는 같은 지위다.
#[derive(Debug, PartialEq)]
pub(crate) enum Item {
    /// 하네스 머리(로고 + 이름 + 개수 + 더하기).
    Section {
        harness: &'static str,
        open: bool,
        count: usize,
    },
    /// 종류 머리(MCP / 스킬 / 플러그인 …). 같은 종류라도 **스코프가 다르면 다른
    /// 그룹**이다 — 2026-08-11 지시("프로젝트랑 전역 나눠서 보이게는?").
    Group {
        harness: &'static str,
        kind: RowKind,
        scope: &'static str,
        open: bool,
        count: usize,
    },
    /// `view` 의 몇 번째 줄인가.
    Row(usize),
}

impl Item {
    fn height(&self) -> f32 {
        match self {
            Item::Section { .. } => SECTION_H,
            Item::Group { .. } => GROUP_H,
            Item::Row(_) => ROW_H,
        }
    }
}

fn group_key(harness: &str, kind: RowKind, scope: &str) -> String {
    format!("{harness}/{}/{}", kind.key(), scope_key(scope))
}

/// 목록을 화면 구조로 편다 — 머리와 줄이 나오는 차례 그대로.
///
/// **그리기와 높이 계산이 같은 목록을 본다.** 예전엔 둘이 각각 "앞줄과 달라졌나"를
/// 세었는데, 그러면 규칙이 하나 늘 때마다 두 곳을 똑같이 고쳐야 하고 한쪽만 고치면
/// 스크롤 상한이 어긋난다 — 목록 끝이 잘리거나 바닥에 빈 자리가 남는다. 접힘이
/// 바로 그 "규칙 하나"라서, 붙이기 전에 세는 자리를 하나로 합쳤다.
///
/// 접힌 것은 **목록에서 아예 빠진다**. 화면 밖으로 밀어내는 것이 아니라 자리를 안
/// 차지해야, 스크롤 상한과 클릭 좌표가 저절로 맞는다.
pub(crate) fn layout(rows: &[McpRow], collapsed: &std::collections::HashSet<String>) -> Vec<Item> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let harness = rows[i].harness;
        let hn = rows[i..].iter().take_while(|r| r.harness == harness).count();
        let hopen = !collapsed.contains(harness);
        out.push(Item::Section {
            harness,
            open: hopen,
            count: hn,
        });
        if !hopen {
            i += hn;
            continue;
        }
        let end = i + hn;
        while i < end {
            let (kind, scope) = (rows[i].kind, rows[i].scope);
            let kn = rows[i..end]
                .iter()
                .take_while(|r| r.kind == kind && r.scope == scope)
                .count();
            let kopen = !collapsed.contains(&group_key(harness, kind, scope));
            out.push(Item::Group {
                harness,
                kind,
                scope,
                open: kopen,
                count: kn,
            });
            if kopen {
                out.extend((i..i + kn).map(Item::Row));
            }
            i += kn;
        }
    }
    out
}

/// 하네스 로고 아이콘 이름 — `sesscol::harness_icon` 과 같은 규약.
///
/// 로고가 없는 것은 claude 로고로 때우지 않고 중립 아이콘을 준다. 때우면 cursor·gemini
/// 줄이 claude 인 척해서, 로고가 곧 구분인 이 칼럼에서 제일 나쁜 거짓말이 된다.
fn harness_icon(harness: &str) -> &'static str {
    match harness {
        "claude" | "claude desktop" => "claude",
        "codex" => "codex",
        "antigravity" => "antigravity",
        _ => "terminal",
    }
}

impl App {
    /// 탭이 보일 때만 워커를 깨워 목록을 새로 고친다.
    pub(crate) fn pump_mcp_col(&mut self) {
        if self.info.tab != state::SideTab::Mcp || !self.git.col_visible {
            return;
        }
        let rev = self.mcp_col.rev.load(Relaxed);
        if rev != self.mcp_col.seen_rev {
            if let Ok(g) = self.mcp_col.snap.lock() {
                self.mcp_col.view = g.clone();
            }
            self.mcp_col.seen_rev = rev;
            self.chrome_dirty = true;
        }
        // 워커가 남긴 결과 한 줄(지우기처럼 시간이 걸리는 일)은 여기서 집어 간다.
        if let Some(msg) = self.mcp_col.notice.lock().ok().and_then(|mut g| g.take()) {
            self.set_toast(msg);
        }
        // claude 쪽은 꺼짐도 `.mcp.json` 도 폴더마다 다르다 — pane 을 옮겨 cwd 가 바뀌면
        // 목록의 대상 자체가 달라지므로 주기를 기다리지 않는다.
        let cwd = self.active_pane_cwd();
        let cwd_changed = cwd != self.mcp_col.cwd;
        if cwd_changed {
            self.mcp_col.cwd = cwd.clone();
        }
        let due = self
            .mcp_col
            .last_refresh
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(REFRESH_MS));
        // `stale` 은 우리가 설정을 고친 직후 세운다 — 그때는 주기를 기다리지 않는다.
        if !(due || self.mcp_col.stale || cwd_changed) || self.mcp_col.busy.swap(true, Relaxed) {
            return;
        }
        self.mcp_col.stale = false;
        self.mcp_col.last_refresh = Some(std::time::Instant::now());
        let (snap, rev, busy) = (
            self.mcp_col.snap.clone(),
            self.mcp_col.rev.clone(),
            self.mcp_col.busy.clone(),
        );
        std::thread::spawn(move || {
            let rows = collect(cwd.as_deref());
            if let Ok(mut g) = snap.lock() {
                *g = rows;
            }
            rev.fetch_add(1, Relaxed);
            busy.store(false, Relaxed);
        });
    }

    /// 추가 칸에 글자를 넣는다 — 조합이 끝난 한글도 여기로 온다. 커서 산수는
    /// `lineedit` 한 벌을 쓴다(칸마다 다시 짜면 한글 경계에서 하나씩 어긋난다).
    pub(crate) fn mcp_add_insert(&mut self, text: &str) {
        let Some(f) = self.mcp_col.add.as_mut() else {
            return;
        };
        let (s, c) = if f.on_url {
            (&mut f.url, &mut f.url_cursor)
        } else {
            (&mut f.name, &mut f.name_cursor)
        };
        crate::lineedit::insert(s, c, text);
        f.err = None;
        self.chrome_dirty = true;
    }

    /// 추가 칸의 키 입력 — 한글 조합을 포함한다. `git_commit_input` 과 같은 얼개다:
    /// macOS 는 자모를 `event.text` 로 넘기므로 공용 조합기에 먹이고, 완성된 음절만
    /// 칸에 넣는다. 조합 중인 것은 `self.preedit` 로 오버레이가 그린다.
    pub(crate) fn mcp_add_input(&mut self, event: &winit::event::KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        if crate::input::is_modifier_key(event) {
            return;
        }
        self.ime_retarget(crate::ImeFocus::McpAdd);
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.mcp_add_insert(&commit);
                        }
                        self.preedit = self.hangul.preedit().unwrap_or_default();
                        self.in_preedit = !self.preedit.is_empty();
                        self.chrome_dirty = true;
                        return;
                    }
                }
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.chrome_dirty = true;
            return;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.mcp_add_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        self.mcp_add_key(event);
    }

    /// 추가 칸의 키. Tab 은 칸을 옮기고, Enter 는 더하고, Esc 는 닫는다.
    pub(crate) fn mcp_add_key(&mut self, event: &winit::event::KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        if matches!(&event.logical_key, Key::Named(NamedKey::Tab)) {
            if let Some(f) = self.mcp_col.add.as_mut() {
                f.on_url = !f.on_url;
                self.chrome_dirty = true;
            }
            return;
        }
        let Some(f) = self.mcp_col.add.as_mut() else {
            return;
        };
        let (s, c) = if f.on_url {
            (&mut f.url, &mut f.url_cursor)
        } else {
            (&mut f.name, &mut f.name_cursor)
        };
        match crate::lineedit::key(s, c, &event.logical_key) {
            crate::lineedit::LineEditAction::Submit => self.mcp_add_submit(),
            crate::lineedit::LineEditAction::Cancel => {
                self.mcp_col.add = None;
                self.preedit.clear();
                self.in_preedit = false;
                let _ = self.hangul.flush();
            }
            _ => {
                f.err = None;
            }
        }
        self.chrome_dirty = true;
    }

    /// 칸의 내용을 설정에 더한다. 실패하면 칸을 닫지 않는다 — 닫으면 방금 친 주소를
    /// 다시 쳐야 한다.
    pub(crate) fn mcp_add_submit(&mut self) {
        let Some(f) = self.mcp_col.add.as_ref() else {
            return;
        };
        let (harness, name, url) = match validate_add(&f.name, &f.url) {
            Ok((n, u)) => (f.harness, n, u),
            Err(e) => {
                if let Some(f) = self.mcp_col.add.as_mut() {
                    f.err = Some(e);
                }
                return;
            }
        };
        // claude 는 CLI 라 프로세스를 띄운다 — 워커로.
        if harness == "claude" {
            let (notice, snap, rev, busy, cwd) = (
                self.mcp_col.notice.clone(),
                self.mcp_col.snap.clone(),
                self.mcp_col.rev.clone(),
                self.mcp_col.busy.clone(),
                self.mcp_col.cwd.clone(),
            );
            self.mcp_col.add = None;
            self.set_toast(format!("{name} 더하는 중…"));
            self.mcp_col.busy.store(true, Relaxed);
            std::thread::spawn(move || {
                let msg = match add_claude_mcp(&name, &url) {
                    Ok(m) => m,
                    Err(e) => format!("⚠ {name} 못 더했다: {e}"),
                };
                if let Ok(mut g) = notice.lock() {
                    *g = Some(msg);
                }
                if let Ok(mut g) = snap.lock() {
                    *g = collect(cwd.as_deref());
                }
                rev.fetch_add(1, Relaxed);
                busy.store(false, Relaxed);
            });
            return;
        }
        let res = kasa_socket::home_dir()
            .ok_or_else(|| anyhow::anyhow!("홈 디렉터리를 못 찾았다"))
            .and_then(|h| add_codex_mcp(&h, &name, &url));
        match res {
            Ok(m) => {
                self.mcp_col.add = None;
                self.set_toast(m);
                self.mcp_col.stale = true;
            }
            // 이미 있는 이름처럼 고쳐 쓸 수 있는 실패는 칸 안에 남긴다.
            Err(e) => {
                if let Some(f) = self.mcp_col.add.as_mut() {
                    f.err = Some(e.to_string());
                }
            }
        }
    }

    /// 지우기 — 한 번은 확인, 두 번째에 실행. 되돌릴 수 없는 일이라 두 번 누르게
    /// 한다. 다이얼로그를 띄우지 않는 건 그 순간 목록에서 눈이 떠나기 때문이다.
    fn mcp_col_delete(&mut self, i: usize) {
        let Some(row) = self.mcp_col.view.get(i).cloned() else {
            return;
        };
        // 버튼을 안 그리는 줄이지만 좌표는 남을 수 있다(스크롤·재수집 사이) — 여기서도
        // 같은 규칙을 본다.
        if !row.deletable() {
            return;
        }
        let armed = self
            .mcp_col
            .confirm_delete
            .is_some_and(|(j, t)| j == i && t.elapsed() < std::time::Duration::from_secs(4));
        if !armed {
            self.mcp_col.confirm_delete = Some((i, std::time::Instant::now()));
            self.set_toast(format!("{} 지울까 — 한 번 더", row.name));
            return;
        }
        self.mcp_col.confirm_delete = None;
        // claude 서버만 CLI(=프로세스 기동)라 워커로 보낸다. 나머지는 파일 한 번
        // 고치는 일이라 그 자리에서 끝난다.
        if row.harness == "claude" && row.kind == RowKind::Mcp {
            let (notice, snap, rev, busy, cwd) = (
                self.mcp_col.notice.clone(),
                self.mcp_col.snap.clone(),
                self.mcp_col.rev.clone(),
                self.mcp_col.busy.clone(),
                self.mcp_col.cwd.clone(),
            );
            self.set_toast(format!("{} 지우는 중…", row.name));
            std::thread::spawn(move || {
                let msg = match remove_claude_mcp(&row.name, row.scope) {
                    Ok(m) => m,
                    Err(e) => format!("⚠ {} 못 지웠다: {e}", row.name),
                };
                if let Ok(mut g) = notice.lock() {
                    *g = Some(msg);
                }
                // 지운 뒤의 실상을 바로 싣는다 — 목록이 낡은 채로 남으면 방금 지운 것을
                // 또 지우려 든다.
                if let Ok(mut g) = snap.lock() {
                    *g = collect(cwd.as_deref());
                }
                rev.fetch_add(1, Relaxed);
                busy.store(false, Relaxed);
            });
            // 워커가 스냅샷을 갈아 끼우므로 pump 의 수집과 겹치지 않게 잠근다.
            self.mcp_col.busy.store(true, Relaxed);
            return;
        }
        let home = kasa_socket::home_dir();
        let res = match (home, row.kind) {
            (None, _) => Err(anyhow::anyhow!("홈 디렉터리를 못 찾았다")),
            (Some(h), RowKind::Mcp) => remove_codex_mcp(&h, &row.name),
            // 스킬·에이전트·커맨드는 파일 하나가 곧 그 항목이라 자리를 실어 왔다.
            (Some(h), RowKind::Skill | RowKind::Agent | RowKind::Command) => match &row.path {
                Some(p) => trash_entry(&h, p),
                None => Err(anyhow::anyhow!("자리를 모른다")),
            },
            (Some(_), k) => Err(anyhow::anyhow!("{} 은 여기서 못 지운다", k.label())),
        };
        self.set_toast(match res {
            Ok(m) => m,
            Err(e) => format!("⚠ {} 못 지웠다: {e}", row.name),
        });
        self.mcp_col.stale = true;
    }

    /// 칼럼 안 클릭 — 새로고침, 지우기, 그리고 행을 누르면 켜고 끈다.
    pub(crate) fn mcp_col_click(&mut self, x: f32, y: f32) -> bool {
        if let Some(r) = self.mcp_col.refresh_rect {
            if x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3 {
                self.mcp_col.stale = true;
                self.chrome_dirty = true;
                return true;
            }
        }
        let inside = |r: &(f32, f32, f32, f32)| x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3;
        // 추가 칸이 열려 있으면 그 안이 먼저다 — 칸이 목록 위를 덮고 있다.
        if let Some(f) = self.mcp_col.add.as_mut() {
            if f.name_rect.is_some_and(|r| inside(&r)) {
                f.on_url = false;
                self.chrome_dirty = true;
                return true;
            }
            if f.url_rect.is_some_and(|r| inside(&r)) {
                f.on_url = true;
                self.chrome_dirty = true;
                return true;
            }
            if f.cancel_rect.is_some_and(|r| inside(&r)) {
                self.mcp_col.add = None;
                self.chrome_dirty = true;
                return true;
            }
            if f.ok_rect.is_some_and(|r| inside(&r)) {
                self.mcp_add_submit();
                self.chrome_dirty = true;
                return true;
            }
        }
        if let Some(h) = self
            .mcp_col
            .add_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(h, _)| *h)
        {
            // 같은 + 를 다시 누르면 닫는다.
            let open = self.mcp_col.add.as_ref().is_some_and(|f| f.harness == h);
            self.mcp_col.add = (!open).then(|| state::McpAddForm {
                harness: h,
                ..Default::default()
            });
            self.chrome_dirty = true;
            return true;
        }
        // 머리를 누르면 접힌다. 더하기(+)보다 뒤에 보는 건 그 버튼이 머리 안에
        // 겹쳐 있어서다 — 순서가 바뀌면 + 를 눌러도 접히기만 한다.
        if let Some(key) = self
            .mcp_col
            .head_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(k, _)| k.clone())
        {
            if !self.mcp_col.collapsed.remove(&key) {
                self.mcp_col.collapsed.insert(key);
            }
            // 접으면 목록이 짧아진다 — 스크롤이 끝을 넘어간 채 남으면 빈 화면이 된다.
            // 상한 clamp 는 다음 그리기가 하므로 여기선 되돌릴 자리만 만들어 준다.
            self.chrome_dirty = true;
            return true;
        }
        // 지우기는 행 위에 겹쳐 있어 먼저 본다.
        if let Some(i) = self
            .mcp_col
            .del_rects
            .iter()
            .find(|(_, r)| x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3)
            .map(|(i, _)| *i)
        {
            self.mcp_col_delete(i);
            self.chrome_dirty = true;
            return true;
        }
        // 다른 곳을 누르면 확인 대기를 푼다 — 무장한 채로 남으면 한참 뒤의 클릭이
        // 지우기가 된다.
        self.mcp_col.confirm_delete = None;
        let hit = self
            .mcp_col
            .row_rects
            .iter()
            .find(|(_, r)| x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3)
            .map(|(i, _)| *i);
        let Some(row) = hit.and_then(|i| self.mcp_col.view.get(i)).cloned() else {
            return false;
        };
        if !row.toggleable {
            self.set_toast(format!("{} 스킬은 폴더가 곧 목록이라 못 끈다", row.harness));
            return true;
        }
        let cwd = self.active_pane_cwd();
        match toggle_row(&row, cwd.as_deref()) {
            Ok(()) => {
                // 낙관적으로 먼저 뒤집는다 — 재수집은 워커라 한 박자 늦게 오는데,
                // 그 사이 화면이 안 변하면 눌린 건지 아닌지를 알 수 없다.
                if let Some(r) = hit.and_then(|i| self.mcp_col.view.get_mut(i)) {
                    r.enabled = !r.enabled;
                }
                self.set_toast(format!(
                    "{} {} — {}",
                    row.name,
                    if row.enabled { "껐다" } else { "켰다" },
                    // claude MCP 의 꺼짐은 폴더에 적힌다. 「전역」 배지가 붙은 줄을 껐을 때
                    // 전역이 꺼진 것으로 읽히면 다른 폴더에서 왜 아직 뜨는지를 또 파게 된다.
                    // 나머지는 부팅 때 설정을 읽는다는 사실이 같은 자리에서 할 말이다.
                    if row.harness == "claude" && row.kind == RowKind::Mcp {
                        "이 폴더에서만, 다음 실행부터"
                    } else {
                        "다음 실행부터"
                    }
                ));
            }
            Err(e) => self.set_toast(format!("⚠ {} 실패: {e}", row.name)),
        }
        self.mcp_col.stale = true;
        self.chrome_dirty = true;
        true
    }
}

/// 목록을 섹션(하네스) → 그룹(MCP/Skill) → 행 순으로 그린다.
pub(crate) fn draw_mcp_col(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    mc: &mut state::McpColState,
    x: f32,
    w: f32,
    top: f32,
    bottom: f32,
) {
    let x0 = x + 14.0;
    let right = x + w - 12.0;
    let text_x = x + 31.0;
    let avail = (right - text_x).max(0.0);
    mc.row_rects.clear();
    mc.del_rects.clear();
    mc.refresh_rect = None;
    let mut add_rects: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
    let mut head_rects: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
    // 확인 대기는 스스로 풀린다 — 무장한 줄이 화면에 남아 있으면 한참 뒤의 클릭이
    // 지우기가 된다.
    if mc
        .confirm_delete
        .is_some_and(|(_, t)| t.elapsed() >= std::time::Duration::from_secs(4))
    {
        mc.confirm_delete = None;
    }
    let armed = mc.confirm_delete.map(|(i, _)| i);
    let hit = |r: &(f32, f32, f32, f32)| {
        cursor.0 >= r.0 && cursor.0 <= r.0 + r.2 && cursor.1 >= r.1 && cursor.1 <= r.1 + r.3
    };

    // ── 머리: 개수 + 새로고침 (스크롤 밖 고정) ──
    let head_y = top + 6.0;
    {
        // 종류마다 세야 한다. 「MCP 아닌 것」을 스킬로 묶던 때가 있었는데, 플러그인·
        // 에이전트·커맨드·훅이 들어오자 스킬 26 이 112 로 뛰었다 — 세는 자리가 하나
        // 늘어난 종류를 모르면 숫자는 조용히 거짓말을 한다.
        let n = |k: RowKind| mc.view.iter().filter(|r| r.kind == k).count();
        g.draw_text(
            x0,
            head_y + 4.0,
            &format!(
                "MCP {} · 스킬 {} · 총 {}",
                n(RowKind::Mcp),
                n(RowKind::Skill),
                mc.view.len()
            ),
            gpu::DrawOpts {
                font_size: 11.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        let r = (right - 18.0, head_y + 1.0, 18.0, 18.0);
        let hov = hit(&r);
        g.hover_pointer |= hov;
        g.queue_icon(
            "refresh-cw",
            r.0,
            r.1,
            15.0,
            if hov {
                theme::text()
            } else {
                theme::text_mute()
            },
        );
        mc.refresh_rect = Some(r);
    }

    // ── 추가 칸 (열려 있을 때만, 스크롤 밖 고정) ──
    // 목록 안에 끼워 넣지 않는 건 스크롤 때문이다 — 칸이 흘러 화면 밖으로 나가면
    // 타이핑하는 자리가 안 보인다.
    let mut form_h = 0.0;
    if let Some(f) = mc.add.as_mut() {
        let fy = top + 30.0;
        form_h = if f.err.is_some() { 110.0 } else { 96.0 };
        g.rect(x + 1.0, fy, w - 1.0, form_h, theme::surface_hover());
        // 어느 하네스의 + 를 눌렀는지는 폼이 뜬 뒤엔 화면에 안 남는다 — 두 하네스가
        // 같은 자리에 같은 폼을 띄우므로 여기 적지 않으면 어디로 가는지 알 수 없다.
        g.draw_text(
            x0,
            fy + 6.0,
            &format!("{} 에 URL 서버 추가", f.harness),
            gpu::DrawOpts {
                font_size: 10.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        let field = |g: &mut gpu::GpuRenderer,
                     yy: f32,
                     label: &str,
                     val: &str,
                     focused: bool,
                     ph: &str|
         -> (f32, f32, f32, f32) {
            let r = (x0, yy, right - x0, 22.0);
            round_rect(
                g,
                r.0,
                r.1,
                r.2,
                r.3,
                theme::radius_sm(),
                if focused {
                    theme::surface()
                } else {
                    theme::panel_bg()
                },
            );
            g.draw_text(
                r.0 + 6.0,
                yy + 5.0,
                label,
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: theme::text_dim(),
                    bold: false,
                    italic: false,
                },
            );
            let lw = g.measure_chrome_text(label, 10.0, false);
            let show = if val.is_empty() { ph } else { val };
            g.draw_text(
                r.0 + 12.0 + lw,
                yy + 4.0,
                show,
                gpu::DrawOpts {
                    font_size: 11.0,
                    color: if val.is_empty() {
                        theme::text_dim()
                    } else {
                        theme::text()
                    },
                    bold: false,
                    italic: false,
                },
            );
            r
        };
        f.name_rect = Some(field(g, fy + 24.0, "이름", &f.name, !f.on_url, "my-server"));
        f.url_rect = Some(field(
            g,
            fy + 50.0,
            "주소",
            &f.url,
            f.on_url,
            "https://example.com/mcp",
        ));
        if let Some(e) = &f.err {
            g.draw_text(
                x0,
                fy + 76.0,
                e,
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: crate::render::DIFF_RED,
                    bold: false,
                    italic: false,
                },
            );
        }
        let by = fy + form_h - 20.0;
        let ok = (right - 46.0, by, 46.0, 16.0);
        let cancel = (right - 100.0, by, 40.0, 16.0);
        for (r, label, strong) in [(ok, "더하기", true), (cancel, "취소", false)] {
            let h = hit(&r);
            g.hover_pointer |= h;
            g.draw_text(
                r.0,
                r.1,
                label,
                gpu::DrawOpts {
                    font_size: 11.0,
                    color: if strong || h {
                        theme::text()
                    } else {
                        theme::text_dim()
                    },
                    bold: strong,
                    italic: false,
                },
            );
        }
        f.ok_rect = Some(ok);
        f.cancel_rect = Some(cancel);
    }

    let body_top = top + 30.0 + form_h;
    let vis_h = (bottom - body_top).max(0.0);
    mc.body_rect = (x, body_top, w, vis_h);

    if mc.view.is_empty() {
        mc.content_h = EMPTY_H;
        mc.add_rects = add_rects;
        mc.head_rects = head_rects;
        g.draw_text(
            x0,
            body_top + 12.0,
            "설정을 아직 못 읽었다",
            gpu::DrawOpts {
                font_size: 12.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        return;
    }

    // 접힘까지 반영해 화면 구조를 한 번 편다 — 아래 그리기와 스크롤 상한이 이 하나를
    // 같이 본다.
    let items = layout(&mc.view, &mc.collapsed);
    mc.content_h = content_height(&items);
    let max_scroll = (mc.content_h - vis_h).max(0.0);
    mc.scroll = mc.scroll.clamp(0.0, max_scroll);

    let mut y = body_top - mc.scroll;
    for item in &items {
        let visible = y + item.height() > body_top && y < bottom;
        // 머리는 통째로 누르는 자리다 — 셰브런만 받으면 과녁이 12px 이 된다.
        let head = (x, y, w, item.height());
        let (i, row) = match item {
            Item::Section {
                harness,
                open,
                count,
            } => {
                if visible {
                    let hov = hit(&head);
                    if hov {
                        round_rect(
                            g,
                            x + 4.0,
                            y,
                            w - 8.0,
                            SECTION_H - 2.0,
                            theme::radius_sm(),
                            theme::surface_hover(),
                        );
                    }
                    g.hover_pointer |= hov;
                    g.queue_icon(
                        if *open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        },
                        x + 2.0,
                        y + 9.0,
                        12.0,
                        theme::text_dim(),
                    );
                    g.queue_icon(harness_icon(harness), x + 16.0, y + 7.0, 15.0, theme::text());
                    let lw = g.measure_chrome_text(harness, 12.0, true);
                    g.draw_text(
                        x + 38.0,
                        y + 8.0,
                        harness,
                        gpu::DrawOpts {
                            font_size: 12.0,
                            color: theme::text(),
                            bold: true,
                            italic: false,
                        },
                    );
                    // 개수는 접었을 때 특히 필요하다 — 접힌 머리만 남으면 그 안에
                    // 무엇이 몇 개 있었는지가 화면에서 통째로 사라진다.
                    g.draw_text(
                        x + 44.0 + lw,
                        y + 9.0,
                        &count.to_string(),
                        gpu::DrawOpts {
                            font_size: 10.0,
                            color: theme::text_mute(),
                            bold: false,
                            italic: false,
                        },
                    );
                    // 더하기는 섹션 머리에 둔다 — 어느 하네스에 붙일지가 누르는 자리로
                    // 정해져야, 폼 안에서 하네스를 또 고르지 않아도 된다. 우리가 쓰지
                    // 않는 하네스엔 안 그린다: 눌러도 안 되는 버튼은 없느니만 못하다.
                    if matches!(*harness, "claude" | "codex") {
                        let r = (right - 18.0, y + 5.0, 18.0, 18.0);
                        let ah = hit(&r);
                        g.hover_pointer |= ah;
                        g.queue_icon(
                            "plus",
                            r.0,
                            r.1,
                            15.0,
                            if ah { theme::text() } else { theme::text_dim() },
                        );
                        add_rects.push((harness, r));
                    }
                    head_rects.push((harness.to_string(), head));
                }
                y += SECTION_H;
                continue;
            }
            Item::Group {
                harness,
                kind,
                scope,
                open,
                count,
            } => {
                if visible {
                    let hov = hit(&head);
                    g.hover_pointer |= hov;
                    g.queue_icon(
                        if *open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        },
                        x + 18.0,
                        y + 5.0,
                        11.0,
                        if hov {
                            theme::text_dim()
                        } else {
                            theme::text_mute()
                        },
                    );
                    let label = format!("{} · {scope}", kind.label());
                    g.draw_text(
                        text_x + 2.0,
                        y + 5.0,
                        &label,
                        gpu::DrawOpts {
                            font_size: 10.0,
                            color: theme::text_dim(),
                            bold: false,
                            italic: false,
                        },
                    );
                    let lw = g.measure_chrome_text(&label, 10.0, false);
                    g.draw_text(
                        text_x + 8.0 + lw,
                        y + 5.0,
                        &count.to_string(),
                        gpu::DrawOpts {
                            font_size: 10.0,
                            color: theme::text_mute(),
                            bold: false,
                            italic: false,
                        },
                    );
                    head_rects.push((group_key(harness, *kind, scope), head));
                }
                y += GROUP_H;
                continue;
            }
            Item::Row(i) => match mc.view.get(*i) {
                Some(r) => (*i, r),
                None => {
                    y += ROW_H;
                    continue;
                }
            },
        };

        let r = (x, y, w, ROW_H);
        // 화면 밖 행은 hit rect 도 안 남긴다 — 남기면 스크롤 위쪽 숨은 행이 클릭을 받는다.
        if y + ROW_H > body_top && y < bottom {
            let hov = hit(&r);
            if hov {
                round_rect(
                    g,
                    x + 4.0,
                    y,
                    w - 8.0,
                    ROW_H - 2.0,
                    theme::radius_sm(),
                    theme::surface_hover(),
                );
                // 못 끄는 줄에 손 모양을 띄우면 눌러 보고 나서야 안 된다는 걸 안다.
                g.hover_pointer |= row.toggleable;
            }
            // 켜짐/꺼짐은 점 하나로. 꺼진 줄은 글자까지 흐려 목록을 훑을 때 걸린다.
            let dot = if row.enabled {
                theme::accent()
            } else {
                theme::text_dim()
            };
            round_rect(g, text_x, y + 13.0, 6.0, 6.0, 3.0, dot);
            let name_c = if row.enabled {
                theme::text()
            } else {
                theme::text_mute()
            };
            g.draw_text(
                text_x + 12.0,
                y + 6.0,
                &row.name,
                gpu::DrawOpts {
                    font_size: 12.0,
                    color: name_c,
                    bold: false,
                    italic: false,
                },
            );
            // 스코프 배지는 줄에 안 붙인다. 그룹이 스코프로 갈린 뒤로는 한 그룹의 모든
            // 줄이 같은 값이라, 「전역」이 24줄 연속으로 반복되면서 정작 다른 정보(이름·
            // 상세)를 밀어냈다. 이제 그룹 머리가 한 번만 말한다.
            // 지우기는 hover 했을 때만 — 늘 떠 있으면 목록을 훑는 눈이 매 줄에서
            // 걸리고, 되돌릴 수 없는 버튼이 손끝에 늘 놓인다.
            let is_armed = armed == Some(i);
            let mut detail_w = avail - 12.0;
            if (hov || is_armed) && row.deletable() {
                let d = (right - 20.0, y + 11.0, 16.0, 16.0);
                let dh = hit(&d);
                g.hover_pointer |= dh;
                // 확인 대기는 색으로만 말한다 — 아이콘까지 바뀌면 그 순간 자리가
                // 흔들려, 두 번째 클릭이 방금 있던 자리를 빗나간다.
                g.queue_icon(
                    "x",
                    d.0,
                    d.1,
                    14.0,
                    if is_armed {
                        crate::render::DIFF_RED
                    } else if dh {
                        theme::text()
                    } else {
                        theme::text_dim()
                    },
                );
                mc.del_rects.push((i, d));
                detail_w -= 22.0;
            }
            if !row.detail.is_empty() {
                let d = crate::info::fit_text(g, &row.detail, detail_w.max(0.0), 10.0, false);
                g.draw_text(
                    text_x + 12.0,
                    y + 22.0,
                    &d,
                    gpu::DrawOpts {
                        font_size: 10.0,
                        color: theme::text_dim(),
                        bold: false,
                        italic: false,
                    },
                );
            }
            mc.row_rects.push((i, r));
        }
        y += ROW_H;
    }
    mc.add_rects = add_rects;
    mc.head_rects = head_rects;
}

/// 섹션·그룹 머리를 포함한 목록 전체 높이. 스크롤 상한이 이 값에 걸린다.
fn content_height(items: &[Item]) -> f32 {
    items.iter().map(Item::height).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(h: &'static str, k: RowKind, n: &str) -> McpRow {
        scoped(h, k, "전역", n)
    }

    fn scoped(h: &'static str, k: RowKind, s: &'static str, n: &str) -> McpRow {
        McpRow {
            harness: h,
            kind: k,
            scope: s,
            path: None,
            name: n.into(),
            detail: String::new(),
            enabled: true,
            toggleable: true,
        }
    }

    /// 테스트마다 자기 홈을 판다 — 이 모듈의 쓰기 경로는 전부 진짜 파일을 고치므로,
    /// 한 디렉터리를 나눠 쓰면 병렬 실행에서 서로의 설정을 덮는다.
    fn tmp_home(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "kt-mcpcol-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 높이 계산이 렌더의 배치와 어긋나면 스크롤이 목록 끝에 못 닿거나 빈 공간이
    /// 남는다 — 둘이 같은 목록을 보므로 여기서 확인하는 건 그 목록의 모양이다.
    #[test]
    fn content_height_counts_each_header_once() {
        let rows = vec![
            row("claude", RowKind::Mcp, "exa"),
            row("claude", RowKind::Mcp, "playwright"),
            row("claude", RowKind::Skill, "comfyui"),
            row("codex", RowKind::Mcp, "context7"),
        ];
        let open = Default::default();
        // 섹션 2 + 그룹 3 + 행 4
        let want = SECTION_H * 2.0 + GROUP_H * 3.0 + ROW_H * 4.0;
        assert_eq!(content_height(&layout(&rows, &open)), want);
        assert_eq!(content_height(&layout(&[], &open)), 0.0);
    }

    /// 접힌 것은 자리를 안 차지해야 한다. 화면 밖으로 밀어내는 식이면 스크롤 상한이
    /// 접기 전 그대로라 바닥에 빈 화면이 생긴다.
    #[test]
    fn collapsing_takes_the_rows_out_of_the_layout() {
        let rows = vec![
            row("claude", RowKind::Mcp, "exa"),
            row("claude", RowKind::Skill, "comfyui"),
            row("claude", RowKind::Skill, "notebooklm"),
            row("codex", RowKind::Mcp, "context7"),
        ];
        // 종류 하나만 접으면 그 머리는 남고 줄만 빠진다 — 남아야 다시 펼 수 있다.
        let mut c = std::collections::HashSet::new();
        c.insert("claude/skill/user".to_string());
        let items = layout(&rows, &c);
        assert_eq!(
            content_height(&items),
            SECTION_H * 2.0 + GROUP_H * 3.0 + ROW_H * 2.0
        );
        assert!(
            items.contains(&Item::Group {
                harness: "claude",
                kind: RowKind::Skill,
                scope: "전역",
                open: false,
                count: 2,
            }),
            "접힌 종류 머리가 개수를 들고 남아야 한다: {items:?}"
        );

        // 하네스를 접으면 그 안의 종류 머리까지 통째로 빠진다.
        let mut c = std::collections::HashSet::new();
        c.insert("claude".to_string());
        let items = layout(&rows, &c);
        assert_eq!(
            content_height(&items),
            SECTION_H * 2.0 + GROUP_H + ROW_H,
            "claude 는 머리 한 줄만: {items:?}"
        );
        // 접힌 머리도 개수는 들고 있어야 한다 — 안 보이는 것이 몇 개였는지가
        // 화면에서 통째로 사라지면 접은 쪽을 잊는다.
        assert!(
            items.contains(&Item::Section {
                harness: "claude",
                open: false,
                count: 3,
            }),
            "접힌 하네스 머리가 개수를 들고 남아야 한다: {items:?}"
        );
    }

    /// url 서버와 command 서버가 한 줄로 같은 모양이 되게.
    #[test]
    fn server_detail_reads_both_shapes() {
        let url = serde_json::json!({"url": "https://mcp.figma.com/mcp", "type": "http"});
        assert_eq!(server_detail(&url), "https://mcp.figma.com/mcp");
        let cmd = serde_json::json!({"command": "npx", "args": ["-y", "exa-mcp-server"]});
        assert_eq!(server_detail(&cmd), "npx -y exa-mcp-server");
        // args 가 없으면 커맨드만 — 빈 칸이 붙어 정렬이 밀리지 않게.
        let bare = serde_json::json!({"command": "uv"});
        assert_eq!(server_detail(&bare), "uv");
        assert_eq!(server_detail(&serde_json::json!({})), "");
    }

    /// codex 의 `enabled = false` 를 읽어야 「껐는데 왜 목록에 없지」가 안 생긴다.
    #[test]
    fn codex_reads_enabled_flag_and_url() {
        let dir = tmp_home("codex");
        let cx = dir.join(".codex");
        std::fs::create_dir_all(&cx).unwrap();
        std::fs::write(
            cx.join("config.toml"),
            r#"
model = "gpt-5.5"

[mcp_servers.context7]
command = "npx"
args = ["-y", "@upstash/context7-mcp@latest"]

[mcp_servers.unityMCP]
url = "http://127.0.0.1:8080/mcp"
enabled = false
"#,
        )
        .unwrap();
        let rows = codex_mcp(&dir);
        assert_eq!(rows.len(), 2, "두 서버 다 실려야 한다: {rows:?}");
        let c = rows.iter().find(|r| r.name == "context7").unwrap();
        assert!(c.enabled, "enabled 를 안 쓴 서버는 켜진 것으로 본다");
        assert_eq!(c.detail, "npx -y @upstash/context7-mcp@latest");
        let u = rows.iter().find(|r| r.name == "unityMCP").unwrap();
        assert!(!u.enabled, "enabled=false 가 목록에 반영돼야 한다");
        assert_eq!(u.detail, "http://127.0.0.1:8080/mcp");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// frontmatter 의 세 형태가 실제 스킬 폴더에 섞여 있다. 하나라도 놓치면 목록의
    /// 그 줄만 빈칸이거나 따옴표째 나온다(실측: gws-* 는 따옴표, computer-use 는 `>-`).
    #[test]
    fn skill_description_handles_every_frontmatter_shape() {
        let plain = "---\nname: a\ndescription: 그냥 텍스트\n---\n";
        assert_eq!(skill_description(plain), "그냥 텍스트");
        let quoted = "---\ndescription: \"콜론: 이 든 텍스트\"\nmetadata:\n---\n";
        assert_eq!(skill_description(quoted), "콜론: 이 든 텍스트");
        let block = "---\nname: b\ndescription: >-\n  접힌 첫 줄\n  둘째 줄\n---\n";
        assert_eq!(skill_description(block), "접힌 첫 줄", "블록은 첫 줄까지만");
        let pipe = "---\ndescription: |\n  파이프 블록\n---\n";
        assert_eq!(skill_description(pipe), "파이프 블록");
        // description 이 없거나 블록 뒤가 비면 빈 문자열 — 목록은 이름만 보여준다.
        assert_eq!(skill_description("---\nname: c\n---\n"), "");
        assert_eq!(skill_description("---\ndescription: >-\n---\n"), "");
    }

    /// 하네스마다 키 이름이 다르다 — 하나만 보고 짜면 나머지는 통째로 안 뜬다. 실측한
    /// 세 형태(`mcpServers`/`servers`/`mcp`)와 꺼짐 두 표기를 다 받는지 확인한다.
    #[test]
    fn other_harness_configs_speak_three_dialects() {
        let d = tmp_home("ext");
        std::fs::write(
            d.join("a.json"),
            r#"{"mcpServers":{"u":{"serverUrl":"http://x/mcp","disabled":true},
                              "v":{"command":"npx","args":["-y","p"]}}}"#,
        )
        .unwrap();
        let e = Ext { id: "windsurf", rel: "", key: "mcpServers" };
        let rows = ext_rows(&d.join("a.json"), &e, "전역");
        assert_eq!(rows.len(), 2);
        let u = rows.iter().find(|r| r.name == "u").unwrap();
        assert_eq!(u.detail, "http://x/mcp", "serverUrl 도 주소로 읽어야 한다");
        assert!(!u.enabled, "disabled:true 는 꺼진 것이다");
        assert_eq!(rows.iter().find(|r| r.name == "v").unwrap().detail, "npx -y p");
        assert!(
            rows.iter().all(|r| !r.toggleable && !r.deletable()),
            "우리가 안 띄우는 하네스는 손대지 않는다: {rows:?}"
        );

        // opencode 는 맵 키가 `mcp` 고 꺼짐을 `enabled:false` 로 적는다.
        std::fs::write(
            d.join("b.json"),
            r#"{"$schema":"x","mcp":{"u":{"type":"remote","url":"http://y/mcp","enabled":false}}}"#,
        )
        .unwrap();
        let e = Ext { id: "opencode", rel: "", key: "mcp" };
        let rows = ext_rows(&d.join("b.json"), &e, "전역");
        assert_eq!(rows.len(), 1, "$schema 같은 곁키는 서버가 아니다: {rows:?}");
        assert!(!rows[0].enabled);

        // 파일이 없으면 빈 목록 — 안 깔린 도구가 빈 섹션으로 뜨면 안 된다.
        assert!(ext_rows(&d.join("nope.json"), &e, "전역").is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 스코프가 다르면 다른 그룹이다. 배지로만 적어 두면 전역과 레포가 이름순으로
    /// 뒤섞여, 「이 레포가 얹은 것」을 눈으로 못 고른다.
    #[test]
    fn scope_splits_a_kind_into_its_own_groups() {
        let rows = vec![
            scoped("claude", RowKind::Skill, "전역", "comfyui"),
            scoped("claude", RowKind::Skill, "레포", "run"),
            scoped("claude", RowKind::Skill, "레포", "verify"),
        ];
        let items = layout(&rows, &Default::default());
        assert_eq!(
            content_height(&items),
            SECTION_H + GROUP_H * 2.0 + ROW_H * 3.0,
            "머리가 스코프마다 하나씩: {items:?}"
        );

        // 접힘 키도 스코프까지 간다 — 안 그러면 전역을 접을 때 레포까지 딸려 접힌다.
        let mut c = std::collections::HashSet::new();
        c.insert(group_key("claude", RowKind::Skill, "레포"));
        let items = layout(&rows, &c);
        assert_eq!(content_height(&items), SECTION_H + GROUP_H * 2.0 + ROW_H);
        assert!(items.contains(&Item::Row(0)), "전역 줄은 남아야 한다: {items:?}");
    }

    /// 스킬은 폴더가 곧 목록이고, 설명은 SKILL.md 앞머리에서 온다.
    #[test]
    fn skills_read_folder_and_description() {
        let dir = tmp_home("sk");
        let s = dir.join("skills");
        std::fs::create_dir_all(s.join("comfyui")).unwrap();
        std::fs::create_dir_all(s.join(".hidden")).unwrap();
        std::fs::create_dir_all(s.join("bare")).unwrap();
        std::fs::write(
            s.join("comfyui/SKILL.md"),
            "---\nname: comfyui\ndescription: 노드 그래프를 짠다\n---\n본문\n",
        )
        .unwrap();
        let off = std::collections::HashSet::from(["bare".to_string()]);
        let rows = skills_in(&s, "claude", "전역", &off, true);
        assert_eq!(
            rows.len(),
            2,
            "숨김 폴더는 빼고 나머지는 다 실린다: {rows:?}"
        );
        let c = rows.iter().find(|r| r.name == "comfyui").unwrap();
        assert_eq!(c.detail, "노드 그래프를 짠다");
        assert!(c.enabled);
        // SKILL.md 가 없어도 목록에는 남는다 — 폴더가 있으면 하네스는 그걸 읽는다.
        let b = rows.iter().find(|r| r.name == "bare").unwrap();
        assert_eq!(b.detail, "");
        assert!(!b.enabled, "skillOverrides 로 꺼진 것은 꺼진 채로 보여야 한다");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 끈 스킬이 켜진 것처럼 보이면 목록을 믿을 수 없게 된다 — 폴더엔 그 흔적이
    /// 없으므로 `skillOverrides` 를 읽는 것만이 유일한 근거다.
    #[test]
    fn claude_skill_overrides_are_read_as_off() {
        let home = tmp_home("ovr");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"skillOverrides": {"critique": "off", "impeccable": "name-only"}}"#,
        )
        .unwrap();
        let off = claude_skills_off(&home);
        assert!(off.contains("critique"));
        // "off" 가 아닌 값은 꺼진 게 아니다 — name-only 는 보이기만 줄인 것이라
        // 스킬 자체는 살아 있다(호출된다).
        assert!(!off.contains("impeccable"), "off 만 꺼진 것으로 센다");
        assert!(claude_skills_off(&tmp_home("ovr-none")).is_empty());
    }

    /// codex 토글은 켤 때 키를 지운다 — 남겨 두면 설정에 `enabled = true` 만 잔뜩
    /// 쌓여, 손으로 열어 본 사람이 "왜 이것만 명시돼 있지"를 묻게 된다.
    #[test]
    fn codex_toggle_writes_then_clears_the_flag() {
        let home = tmp_home("cx-toggle");
        let cx = home.join(".codex");
        std::fs::create_dir_all(&cx).unwrap();
        let src = "# 손으로 관리하는 파일이다\n[mcp_servers.exa]\ncommand = \"npx\"\n";
        std::fs::write(cx.join("config.toml"), src).unwrap();

        set_codex_mcp_enabled(&home, "exa", false).unwrap();
        let after = std::fs::read_to_string(cx.join("config.toml")).unwrap();
        assert!(after.contains("enabled = false"), "꺼짐이 파일에 남아야: {after}");
        assert!(
            after.contains("# 손으로 관리하는 파일이다"),
            "주석이 살아야 한다 — 통째 재직렬화하면 날아간다: {after}"
        );
        assert!(!codex_mcp(&home)[0].enabled);
        // 백업은 첫 수정 때의 원본이어야 한다.
        let bak = std::fs::read_to_string(cx.join("config.toml.kasaterm.bak")).unwrap();
        assert_eq!(bak, src);

        set_codex_mcp_enabled(&home, "exa", true).unwrap();
        let back = std::fs::read_to_string(cx.join("config.toml")).unwrap();
        assert!(!back.contains("enabled"), "켜면 키가 사라져야: {back}");
        assert!(codex_mcp(&home)[0].enabled);
        // 두 번째 토글이 백업을 갈아치우면 원본이 사라진다.
        assert_eq!(
            std::fs::read_to_string(cx.join("config.toml.kasaterm.bak")).unwrap(),
            src
        );

        assert!(
            set_codex_mcp_enabled(&home, "없는서버", false).is_err(),
            "없는 이름은 조용히 새 항목을 만들지 말고 실패해야 한다"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 스킬 토글은 `skillOverrides` 만 건드리고 나머지 설정을 보존해야 한다 — 이
    /// 파일엔 권한·훅·모델이 함께 살아서, 한 번 잃으면 복구가 크다.
    #[test]
    fn claude_skill_toggle_keeps_the_rest_of_settings() {
        let home = tmp_home("sk-toggle");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let p = home.join(".claude/settings.json");
        std::fs::write(&p, r#"{"model": "fable", "permissions": {"allow": ["Bash"]}}"#).unwrap();

        set_claude_skill_enabled(&home, "critique", false).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["skillOverrides"]["critique"], "off");
        assert_eq!(v["model"], "fable", "다른 설정이 살아 있어야 한다");
        assert_eq!(v["permissions"]["allow"][0], "Bash");
        assert!(claude_skills_off(&home).contains("critique"));

        set_claude_skill_enabled(&home, "critique", true).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert!(
            v["skillOverrides"].as_object().unwrap().is_empty(),
            "켜면 항목을 지운다 — 이 표는 기본에서 벗어난 것만 담는 자리다"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// claude 는 서버를 세 곳에 나눠 적고 꺼짐은 폴더마다 다르다. 전역만 읽으면
    /// 목록이 실상의 일부만 보여준다 — 2026-08-11 지시("프로젝트별, 전역도 보이게").
    #[test]
    fn claude_mcp_reads_all_three_scopes_and_per_folder_off() {
        let home = tmp_home("scopes");
        let repo = home.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join(".mcp.json"),
            r#"{"mcpServers": {"kasaspace": {"url": "http://127.0.0.1:8765/mcp"},
                               "teamonly": {"command": "npx"}}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude.json"),
            format!(
                r#"{{
                  "mcpServers": {{"exa": {{"command": "npx"}}, "kasaspace": {{"url": "u"}}}},
                  "projects": {{
                    "{}": {{
                      "mcpServers": {{"myonly": {{"command": "uv"}}}},
                      "disabledMcpServers": ["exa"],
                      "disabledMcpjsonServers": ["teamonly"]
                    }}
                  }}
                }}"#,
                repo.display()
            ),
        )
        .unwrap();

        let rows = claude_mcp(&home, Some(&repo));
        let find = |n: &str, s: &str| {
            rows.iter()
                .find(|r| r.name == n && r.scope == s)
                .unwrap_or_else(|| panic!("{n}({s}) 이 없다: {rows:?}"))
        };
        // 전역 exa·kasaspace + 폴더 myonly + 레포 kasaspace·teamonly.
        assert_eq!(rows.len(), 5, "세 스코프가 다 실려야 한다: {rows:?}");
        assert_eq!(find("myonly", "폴더").detail, "uv");
        assert_eq!(find("teamonly", "레포").detail, "npx");
        // 같은 이름이 두 곳에 있으면 둘 다 남긴다 — 어느 쪽이 이겼는지는 하네스가 정한다.
        assert!(rows.iter().filter(|r| r.name == "kasaspace").count() == 2);
        // 꺼짐은 폴더 기준. 두 배열은 서로 무관해서 각자 제 스코프만 끈다.
        assert!(!find("exa", "전역").enabled, "disabledMcpServers 가 전역을 끈다");
        assert!(!find("teamonly", "레포").enabled, "disabledMcpjsonServers");
        assert!(find("kasaspace", "레포").enabled);

        // cwd 를 모르면 폴더 것들은 아예 안 보이고, 꺼짐도 알 수 없다.
        let bare = claude_mcp(&home, None);
        assert_eq!(bare.len(), 2, "전역만: {bare:?}");
        assert!(bare.iter().all(|r| r.enabled && r.scope == "전역"));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 이 값들은 설정 파일에 그대로 적힌다. 여기서 안 막으면 하네스가 뜰 때 파싱이
    /// 깨지거나(이름의 점), 조용히 아무 데도 안 붙는다(스킴 없는 주소).
    #[test]
    fn add_form_rejects_what_would_break_the_config() {
        assert!(validate_add("", "https://a").is_err(), "이름이 비면 안 된다");
        // codex 는 이름이 toml 테이블 키가 된다 — 점이 들어가면 중첩 테이블로 읽힌다.
        assert!(validate_add("my.server", "https://a").is_err());
        assert!(validate_add("my server", "https://a").is_err());
        assert!(validate_add("a\"b", "https://a").is_err());
        assert!(validate_add("ok", "example.com/mcp").is_err(), "스킴이 있어야");
        assert!(validate_add("ok", "").is_err());
        assert_eq!(
            validate_add("  my-server_2 ", " https://x.dev/mcp ").unwrap(),
            ("my-server_2".into(), "https://x.dev/mcp".into()),
            "앞뒤 공백은 털어 넣는다 — 붙여 넣으면 딸려 온다"
        );
    }

    /// 더할 때 이미 있는 이름을 덮으면 원래 서버의 설정이 말없이 사라진다.
    #[test]
    fn codex_add_writes_url_and_refuses_to_overwrite() {
        let home = tmp_home("cx-add");
        let cx = home.join(".codex");
        std::fs::create_dir_all(&cx).unwrap();
        std::fs::write(cx.join("config.toml"), "model = \"gpt-5.5\"\n").unwrap();

        add_codex_mcp(&home, "figma", "https://mcp.figma.com/mcp").unwrap();
        let rows = codex_mcp(&home);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "figma");
        assert_eq!(rows[0].detail, "https://mcp.figma.com/mcp");
        assert!(rows[0].enabled, "더한 것은 켜진 채로 시작한다");
        assert!(
            std::fs::read_to_string(cx.join("config.toml"))
                .unwrap()
                .contains("model = \"gpt-5.5\""),
            "원래 설정이 살아 있어야 한다"
        );

        assert!(
            add_codex_mcp(&home, "figma", "https://other").is_err(),
            "같은 이름을 덮으면 원래 서버가 말없이 사라진다"
        );
        assert_eq!(codex_mcp(&home)[0].detail, "https://mcp.figma.com/mcp");

        // mcp_servers 테이블이 아예 없던 설정에도 더할 수 있어야 한다.
        let fresh = tmp_home("cx-add2");
        std::fs::create_dir_all(fresh.join(".codex")).unwrap();
        std::fs::write(fresh.join(".codex/config.toml"), "").unwrap();
        add_codex_mcp(&fresh, "first", "https://a.dev/mcp").unwrap();
        assert_eq!(codex_mcp(&fresh).len(), 1);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&fresh);
    }

    /// 지우기는 설정에서 그 항목만 빠지고 나머지·주석은 그대로여야 한다.
    #[test]
    fn codex_remove_takes_only_that_server() {
        let home = tmp_home("cx-rm");
        let cx = home.join(".codex");
        std::fs::create_dir_all(&cx).unwrap();
        std::fs::write(
            cx.join("config.toml"),
            "# 손으로 관리하는 파일\nmodel = \"gpt-5.5\"\n\n[mcp_servers.exa]\ncommand = \"npx\"\n\n[mcp_servers.keep]\ncommand = \"uv\"\n",
        )
        .unwrap();
        let msg = remove_codex_mcp(&home, "exa").unwrap();
        assert!(msg.contains("exa"), "무엇을 지웠는지 알려야: {msg}");
        let after = std::fs::read_to_string(cx.join("config.toml")).unwrap();
        assert!(!after.contains("mcp_servers.exa"), "{after}");
        assert!(after.contains("mcp_servers.keep"), "남은 것은 그대로: {after}");
        assert!(after.contains("# 손으로 관리하는 파일"), "주석이 살아야: {after}");
        assert!(after.contains("model = \"gpt-5.5\""));
        let rows = codex_mcp(&home);
        assert_eq!(rows.len(), 1);
        // 없는 것을 지우라면 조용히 성공하면 안 된다 — 목록이 낡았다는 뜻이다.
        assert!(remove_codex_mcp(&home, "exa").is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 스킬은 지우지 않고 휴지통으로 옮긴다 — 되돌릴 수 있어야 한다. 심볼릭 링크면
    /// 링크만 옮겨지므로 원본은 그대로 남는다(codex 스킬 5개 중 4개가 링크다).
    #[test]
    #[cfg(target_os = "macos")]
    fn skill_delete_moves_to_trash_and_spares_the_link_target() {
        let home = tmp_home("sk-rm");
        let skills = home.join(".claude/skills");
        std::fs::create_dir_all(skills.join("plain")).unwrap();
        let origin = home.join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        std::os::unix::fs::symlink(&origin, skills.join("linked")).unwrap();

        // 목록이 실어 온 자리라도 스킬·에이전트·커맨드 폴더 밖이면 안 지운다. 줄과
        // 경로가 어긋나는 상황(낡은 스냅샷·엉뚱한 수집기)에서 이게 마지막 방벽이다.
        assert!(
            trash_entry(&home, &origin).is_err(),
            "스킬 폴더 밖은 통과하면 안 된다"
        );
        assert!(
            trash_entry(&home, &skills.join("없는것")).is_err(),
            "없는 것을 지웠다고 하면 목록이 낡은 걸 못 알아챈다"
        );

        let moved = trash_entry(&home, &skills.join("linked")).unwrap();
        assert!(moved.contains("linked"), "{moved}");
        assert!(
            std::fs::symlink_metadata(skills.join("linked")).is_err(),
            "링크가 자리에서 빠져야 한다"
        );
        assert!(origin.is_dir(), "링크가 가리키던 원본은 살아 있어야 한다");
        // 지운 게 아니라 옮긴 것이다 — 그 자리에 있어야 되돌릴 수 있다.
        let in_trash: Vec<_> = std::fs::read_dir(home.join(".Trash"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            in_trash.iter().any(|n| n.starts_with("linked-kasaterm-")),
            "휴지통에 있어야 한다: {in_trash:?}"
        );

        trash_entry(&home, &skills.join("plain")).unwrap();
        assert!(!skills.join("plain").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 못 끄는 줄에 토글이 걸리면 눌러도 아무 일이 없어 고장으로 읽힌다. 반대로
    /// 끌 수 있는 줄이 못 끄는 것으로 표시되면 안내만 뜨고 설정은 그대로다.
    #[test]
    fn only_codex_skills_are_not_toggleable() {
        assert!(is_toggleable("codex", RowKind::Mcp, ""));
        assert!(is_toggleable("claude", RowKind::Skill, ""));
        assert!(is_toggleable("claude", RowKind::Mcp, "전역"));
        // codex 스킬은 폴더가 곧 목록이라 끄기가 삭제와 같아진다.
        assert!(!is_toggleable("codex", RowKind::Skill, ""));
        // 목록이 만든 행의 표시와 실제 쓰기 게이트가 같은 함수를 봐야 어긋나지 않는다.
        let mut r = row("codex", RowKind::Skill, "orchestration");
        r.toggleable = true;
        assert!(
            toggle_row(&r, None).is_err(),
            "표시가 거짓이면 쓰기도 막혀야 한다"
        );
        // 폴더를 모르면 claude MCP 는 끌 자리가 없다 — 조용히 성공하면 안 된다.
        assert!(toggle_row(&row("claude", RowKind::Mcp, "exa"), None).is_err());
    }

    /// 스코프가 어느 배열로 가는지가 이 기능의 전부다. 틀린 쪽에 적으면 파일은
    /// 바뀌는데 하네스는 그대로 서버를 띄운다 — 화면만 꺼진 것처럼 보인다.
    #[test]
    fn claude_mcp_off_goes_to_the_array_its_scope_belongs_to() {
        let home = tmp_home("claude-mcp-off");
        let cwd = home.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            home.join(".claude.json"),
            r#"{
  "numStartups": 7,
  "mcpServers": { "exa": { "command": "npx" } },
  "projects": { "keep-me": { "history": [1, 2] } }
}"#,
        )
        .unwrap();

        set_claude_mcp_enabled(&home, &cwd, "exa", "전역", false).unwrap();
        set_claude_mcp_enabled(&home, &cwd, "teamonly", "레포", false).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
                .unwrap();
        let p = &v["projects"][cwd.to_str().unwrap()];
        assert_eq!(p["disabledMcpServers"], serde_json::json!(["exa"]));
        assert_eq!(p["disabledMcpjsonServers"], serde_json::json!(["teamonly"]));
        // 남의 기록은 그대로여야 한다 — 이 파일은 우리 것이 아니다.
        assert_eq!(v["projects"]["keep-me"]["history"], serde_json::json!([1, 2]));
        assert_eq!(v["numStartups"], 7);
        // 키 순서까지 지켜야 4000줄짜리 남의 파일이 통째로 흔들리지 않는다.
        let body = std::fs::read_to_string(home.join(".claude.json")).unwrap();
        assert!(
            body.find("numStartups").unwrap() < body.find("mcpServers").unwrap(),
            "원본 키 순서가 뒤집혔다: {}",
            &body[..80]
        );

        // 다시 켜면 배열에서 빠진다. 두 번 켜도 탈이 없어야 한다(중복 제거 아님).
        set_claude_mcp_enabled(&home, &cwd, "exa", "전역", true).unwrap();
        set_claude_mcp_enabled(&home, &cwd, "exa", "전역", true).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(
            v["projects"][cwd.to_str().unwrap()]["disabledMcpServers"],
            serde_json::json!([])
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 이 파일은 도는 세션들이 함께 쓴다. 우리가 읽은 뒤 남이 고쳤으면 우리 쓰기가
    /// 그 기록을 지우므로, 포기하고 다시 누르게 하는 것이 맞다.
    #[test]
    fn claude_json_write_backs_off_when_someone_else_touched_it() {
        let home = tmp_home("claude-mcp-race");
        let cwd = home.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let path = home.join(".claude.json");
        let body = r#"{"mcpServers":{"exa":{"command":"npx"}}}"#;
        std::fs::write(&path, body).unwrap();

        // 읽은 시각이 파일의 현재 mtime 과 다르다 = 그 사이 누가 썼다.
        let stale = std::time::SystemTime::UNIX_EPOCH;
        let err = write_claude_disabled(&path, stale, &cwd, "exa", "전역", false).unwrap_err();
        assert!(
            err.to_string().contains("다시 눌러라"),
            "다른 세션의 수정을 알려야 한다: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            body,
            "포기했으면 파일은 그대로여야 한다"
        );

        // 같은 시각이면 통과한다 — 가드가 늘 막기만 하면 기능이 죽은 것과 같다.
        let now = std::fs::metadata(&path).unwrap().modified().unwrap();
        write_claude_disabled(&path, now, &cwd, "exa", "전역", false).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("exa"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
