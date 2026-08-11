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

/// 한 줄이 무엇인가.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RowKind {
    Mcp,
    Skill,
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
    /// 꺼져 있으면 회색으로 눕는다.
    pub(crate) enabled: bool,
    /// 여기서 껐다 켤 수 있나. codex 스킬만 거짓이다 — 그쪽은 폴더(대개 심볼릭
    /// 링크)가 곧 목록이라, 끄려면 지우는 수밖에 없어 토글과 삭제가 같아진다.
    pub(crate) toggleable: bool,
}

/// claude 쪽 설정 — `~/.claude.json` 의 `mcpServers`.
fn claude_mcp(home: &std::path::Path) -> Vec<McpRow> {
    let Ok(text) = std::fs::read_to_string(home.join(".claude.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(map) = v.get("mcpServers").and_then(|m| m.as_object()) else {
        return Vec::new();
    };
    let mut out: Vec<McpRow> = map
        .iter()
        .map(|(name, cfg)| McpRow {
            harness: "claude",
            kind: RowKind::Mcp,
            name: name.clone(),
            detail: server_detail(cfg),
            // claude 쪽엔 서버를 끄는 전역 플래그가 없다 — 목록에 있으면 붙는다.
            enabled: true,
            toggleable: is_toggleable("claude", RowKind::Mcp),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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
fn server_detail(cfg: &serde_json::Value) -> String {
    if let Some(url) = cfg.get("url").and_then(|u| u.as_str()) {
        return url.to_string();
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
                name: name.to_string(),
                detail,
                enabled: item
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
                toggleable: is_toggleable("codex", RowKind::Mcp),
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

/// 두 하네스의 MCP·스킬을 한 벌로. 워커 스레드에서 부른다.
pub(crate) fn collect() -> Vec<McpRow> {
    let Some(home) = kasa_socket::home_dir() else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    rows.extend(claude_mcp(&home));
    let off = claude_skills_off(&home);
    rows.extend(skills_in(
        &home.join(".claude/skills"),
        "claude",
        &off,
        is_toggleable("claude", RowKind::Skill),
    ));
    rows.extend(codex_mcp(&home));
    rows.extend(skills_in(
        &home.join(".codex/skills"),
        "codex",
        &Default::default(),
        is_toggleable("codex", RowKind::Skill),
    ));
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

/// 여기서 껐다 켤 수 있는 조합인가. 넷 중 둘뿐이다:
///
/// - claude MCP — 전역으로 끄는 설정이 없다. 프로젝트별 `disabledMcpServers` 는 있지만
///   그건 "이 폴더에서만"이라, 전역 목록에 전역인 척 체크박스를 다는 것이 거짓이 된다.
/// - codex 스킬 — 폴더(대개 심볼릭 링크)가 곧 목록이라 끄기가 삭제와 같아진다.
///
/// 목록의 표시와 쓰기 게이트가 이 한 함수를 같이 본다 — 갈라지면 눌리는데 안 써지거나
/// 그 반대가 된다.
fn is_toggleable(harness: &str, kind: RowKind) -> bool {
    matches!(
        (harness, kind),
        ("codex", RowKind::Mcp) | ("claude", RowKind::Skill)
    )
}

/// 한 줄을 뒤집는다. 어느 파일을 고칠지는 (하네스, 종류)가 정한다.
fn toggle_row(row: &McpRow) -> anyhow::Result<()> {
    if !is_toggleable(row.harness, row.kind) {
        return Err(anyhow::anyhow!("여기선 못 끈다"));
    }
    let home = kasa_socket::home_dir().ok_or_else(|| anyhow::anyhow!("홈 디렉터리를 못 찾았다"))?;
    let on = !row.enabled;
    match (row.harness, row.kind) {
        ("codex", RowKind::Mcp) => set_codex_mcp_enabled(&home, &row.name, on),
        ("claude", RowKind::Skill) => set_claude_skill_enabled(&home, &row.name, on),
        _ => Err(anyhow::anyhow!("여기선 못 끈다")),
    }
}

/// 하네스 로고 아이콘 이름 — `sesscol::harness_icon` 과 같은 규약.
fn harness_icon(harness: &str) -> &'static str {
    match harness {
        "codex" => "codex",
        _ => "claude",
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
        let due = self
            .mcp_col
            .last_refresh
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(REFRESH_MS));
        // `stale` 은 우리가 설정을 고친 직후 세운다 — 그때는 주기를 기다리지 않는다.
        if !(due || self.mcp_col.stale) || self.mcp_col.busy.swap(true, Relaxed) {
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
            let rows = collect();
            if let Ok(mut g) = snap.lock() {
                *g = rows;
            }
            rev.fetch_add(1, Relaxed);
            busy.store(false, Relaxed);
        });
    }

    /// 칼럼 안 클릭 — 새로고침, 그리고 행을 누르면 켜고 끈다.
    pub(crate) fn mcp_col_click(&mut self, x: f32, y: f32) -> bool {
        if let Some(r) = self.mcp_col.refresh_rect {
            if x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3 {
                self.mcp_col.stale = true;
                self.chrome_dirty = true;
                return true;
            }
        }
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
            self.set_toast(match (row.harness, row.kind) {
                ("claude", RowKind::Mcp) => "claude 는 서버를 끄는 설정이 없다 — 지우는 것뿐".into(),
                _ => format!("{} 스킬은 폴더가 곧 목록이라 못 끈다", row.harness),
            });
            return true;
        }
        match toggle_row(&row) {
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
                    // 이미 도는 하네스는 설정을 부팅 때 읽는다. 그 사실을 안 알리면
                    // "껐는데 왜 아직 뜨지"를 여기서 또 파게 된다.
                    "다음 실행부터"
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
    mc.refresh_rect = None;
    let hit = |r: &(f32, f32, f32, f32)| {
        cursor.0 >= r.0 && cursor.0 <= r.0 + r.2 && cursor.1 >= r.1 && cursor.1 <= r.1 + r.3
    };

    // ── 머리: 개수 + 새로고침 (스크롤 밖 고정) ──
    let head_y = top + 6.0;
    {
        let mcps = mc.view.iter().filter(|r| r.kind == RowKind::Mcp).count();
        let skills = mc.view.len() - mcps;
        g.draw_text(
            x0,
            head_y + 4.0,
            &format!("MCP {mcps} · 스킬 {skills}"),
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

    let body_top = top + 30.0;
    let vis_h = (bottom - body_top).max(0.0);
    mc.body_rect = (x, body_top, w, vis_h);

    if mc.view.is_empty() {
        mc.content_h = EMPTY_H;
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

    // 섹션·그룹 머리가 몇 줄 들어가는지 미리 세어 스크롤 상한을 잡는다.
    mc.content_h = content_height(&mc.view);
    let max_scroll = (mc.content_h - vis_h).max(0.0);
    mc.scroll = mc.scroll.clamp(0.0, max_scroll);

    let mut y = body_top - mc.scroll;
    let mut cur_harness = "";
    let mut cur_kind: Option<RowKind> = None;
    for (i, row) in mc.view.iter().enumerate() {
        if row.harness != cur_harness {
            cur_harness = row.harness;
            cur_kind = None;
            if y + SECTION_H > body_top && y < bottom {
                g.queue_icon(
                    harness_icon(row.harness),
                    x + 9.0,
                    y + 7.0,
                    15.0,
                    theme::text(),
                );
                g.draw_text(
                    text_x,
                    y + 8.0,
                    row.harness,
                    gpu::DrawOpts {
                        font_size: 12.0,
                        color: theme::text(),
                        bold: true,
                        italic: false,
                    },
                );
            }
            y += SECTION_H;
        }
        if cur_kind != Some(row.kind) {
            cur_kind = Some(row.kind);
            if y + GROUP_H > body_top && y < bottom {
                g.draw_text(
                    text_x,
                    y + 5.0,
                    if row.kind == RowKind::Mcp {
                        "MCP"
                    } else {
                        "스킬"
                    },
                    gpu::DrawOpts {
                        font_size: 10.0,
                        color: theme::text_dim(),
                        bold: false,
                        italic: false,
                    },
                );
            }
            y += GROUP_H;
        }

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
            if !row.detail.is_empty() {
                let d = crate::info::fit_text(g, &row.detail, avail - 12.0, 10.0, false);
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
}

/// 섹션·그룹 머리를 포함한 목록 전체 높이. 스크롤 상한이 이 값에 걸린다.
fn content_height(rows: &[McpRow]) -> f32 {
    let mut h = 0.0;
    let mut cur_harness = "";
    let mut cur_kind: Option<RowKind> = None;
    for r in rows {
        if r.harness != cur_harness {
            cur_harness = r.harness;
            cur_kind = None;
            h += SECTION_H;
        }
        if cur_kind != Some(r.kind) {
            cur_kind = Some(r.kind);
            h += GROUP_H;
        }
        h += ROW_H;
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(h: &'static str, k: RowKind, n: &str) -> McpRow {
        McpRow {
            harness: h,
            kind: k,
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
    /// 남는다 — 둘은 같은 규칙(섹션이 바뀌면 머리, 종류가 바뀌면 그룹)을 따라야 한다.
    #[test]
    fn content_height_counts_each_header_once() {
        let rows = vec![
            row("claude", RowKind::Mcp, "exa"),
            row("claude", RowKind::Mcp, "playwright"),
            row("claude", RowKind::Skill, "comfyui"),
            row("codex", RowKind::Mcp, "context7"),
        ];
        // 섹션 2 + 그룹 3 + 행 4
        let want = SECTION_H * 2.0 + GROUP_H * 3.0 + ROW_H * 4.0;
        assert_eq!(content_height(&rows), want);
        assert_eq!(content_height(&[]), 0.0);
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
        let rows = skills_in(&s, "claude", &off, true);
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

    /// 못 끄는 줄에 토글이 걸리면 눌러도 아무 일이 없어 고장으로 읽힌다. 반대로
    /// 끌 수 있는 줄이 못 끄는 것으로 표시되면 안내만 뜨고 설정은 그대로다.
    #[test]
    fn only_the_two_writable_kinds_are_toggleable() {
        assert!(is_toggleable("codex", RowKind::Mcp));
        assert!(is_toggleable("claude", RowKind::Skill));
        // claude 는 서버를 끄는 전역 설정이 없고(프로젝트별 `disabledMcpServers` 뿐),
        // codex 스킬은 폴더가 곧 목록이라 끄기가 삭제와 같아진다.
        assert!(!is_toggleable("claude", RowKind::Mcp));
        assert!(!is_toggleable("codex", RowKind::Skill));
        // 목록이 만든 행의 표시와 실제 쓰기 게이트가 같은 함수를 봐야 어긋나지 않는다.
        let mut r = row("claude", RowKind::Mcp, "exa");
        r.toggleable = false;
        assert!(toggle_row(&r).is_err(), "표시가 거짓이면 쓰기도 막혀야 한다");
    }
}
