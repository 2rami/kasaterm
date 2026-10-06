use super::*;

mod graph;
mod history;
pub(crate) use history::{history, history_height, requested_history_count};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub pane: String,
    pub surface_key: Option<String>,
    pub cwd: Option<std::path::PathBuf>,
    pub remote: Option<(String, String, String)>,
    pub issue: Option<String>,
}

#[derive(Default)]
pub(crate) struct Context {
    pub generation: u64,
    request: u64,
    pub target: Option<Target>,
}

impl Context {
    pub fn select(&mut self, target: Option<Target>) -> bool {
        if self.generation != 0 && self.target == target {
            return false;
        }
        self.generation = self.generation.wrapping_add(1);
        self.target = target;
        true
    }

    pub fn accepts(&self, generation: u64, target: &Target) -> bool {
        self.generation == generation && self.target.as_ref() == Some(target)
    }

    pub fn next_request(&mut self) -> Option<(u64, u64, Target)> {
        let target = self.target.clone()?;
        self.request = self.request.wrapping_add(1);
        Some((self.generation, self.request, target))
    }

    pub fn accepts_request(&self, generation: u64, request: u64, target: &Target) -> bool {
        self.request == request && self.accepts(generation, target)
    }
}

pub(crate) fn placeholder(target: &Target, generation: u64, issue: Option<String>) -> GitColView {
    GitColView {
        cwd: target.cwd.clone(),
        remote: target
            .remote
            .as_ref()
            .map(|(label, base, _)| (label.clone(), base.clone())),
        generation,
        loading: issue.is_none(),
        issue,
        ..Default::default()
    }
}

fn message(code: &str) -> String {
    match code {
        "update_needed" => "원본 기기의 KASA 업데이트가 필요해요",
        "source_changed" => "원본 창이 바뀌었어요. 다시 확인 중이에요",
        "cwd_unavailable" => "원본 창의 폴더를 확인 중이에요",
        "invalid_response" => "원본 기기의 Git 응답을 확인할 수 없어요",
        "offline" => "원본 기기에 연결할 수 없어요. 재시도 중이에요",
        _ => "Git 정보를 읽지 못했어요. 재시도 중이에요",
    }
    .into()
}

pub(crate) fn fetch(target: &Target, generation: u64, commits: usize) -> GitColView {
    if let Some(issue) = &target.issue {
        return placeholder(target, generation, Some(issue.clone()));
    }
    let result = if let Some((_, base, machine_id)) = &target.remote {
        let expected = kasa_mcp::git_panel::Source {
            machine_id: machine_id.clone(),
            pane: target.pane.clone(),
            surface_key: target.surface_key.clone().unwrap_or_default(),
            // Cached board cwd can lag a shell cd; the source resolves it atomically with this read.
            cwd: String::new(),
        };
        let query = format!(
            "/term/gitcol?schema={}&machine_id={}&pane={}&surface_key={}&commits={}",
            kasa_mcp::git_panel::SCHEMA,
            kasa_mcp::remote::urlencode(machine_id),
            kasa_mcp::remote::urlencode(&expected.pane),
            kasa_mcp::remote::urlencode(&expected.surface_key),
            commits.clamp(1, 200)
        );
        kasa_mcp::remote::remote_get_json_bounded(base, &query, 2 * 1024 * 1024)
            .map_err(|error| {
                if error.to_string().contains("404") {
                    "update_needed"
                } else {
                    "offline"
                }
            })
            .and_then(|value| kasa_mcp::git_panel::validate_response(&value, &expected))
            .and_then(|value| {
                serde_json::from_value::<GitColView>(value).map_err(|_| "invalid_response")
            })
    } else {
        target
            .cwd
            .as_ref()
            .and_then(|cwd| handler::fetch_git_col_view(cwd, commits))
            .ok_or("git_unavailable")
    };
    match result {
        Ok(mut view) => {
            view.remote = target
                .remote
                .as_ref()
                .map(|(label, base, _)| (label.clone(), base.clone()));
            view.generation = generation;
            view.loading = false;
            view.issue = None;
            view
        }
        Err(code) => placeholder(target, generation, Some(message(code))),
    }
}

/// 신호 없이 다시 읽는 주기. mod 가 말하는 칸은 고친 순간이 신호로 오므로, 바깥(사람 셸·편집기·codex)이 바꾼 것만
/// 늦은 주기로 잡는다.
const POLL: std::time::Duration = std::time::Duration::from_millis(1200);
const POLL_SIGNALLED: std::time::Duration = std::time::Duration::from_secs(5);
/// git 이 아는 상태(HEAD·인덱스·ref·fetch)의 지문이 그대로이면 주기가 와도 이만큼은 읽지 않는다. 한 바퀴가 git 여덟 개
/// (큰 레포에서 CPU 480ms 남짓)라, 열이 열려 있는 것만으로 코어의 30%를 태웠다(2026-10-06 실측). 커밋·스테이지·
/// 체크아웃·fetch 는 지문이 바로 잡고, mod 칸의 편집은 깃 신호가 잡는다 — 이 주기는 바깥 편집기의 작업 트리 편집 몫이다.
const IDLE_READ: std::time::Duration = kasa_mcp::git::BADGE_IDLE_PERIOD;
/// 몰아치는 신호를 한 번의 읽기로 — 마지막 신호 뒤 조용한 틈, 첫 신호부터 미룰 수 있는 한도, 신호로 읽는 사이의 최소
/// 간격. claude 가 파일을 연달아 고치는 동안 git 을 고칠 때마다 돌리지 않는다.
const QUIET: std::time::Duration = std::time::Duration::from_millis(250);
const MAX_DEFER: std::time::Duration = std::time::Duration::from_millis(1000);
const MIN_GAP: std::time::Duration = std::time::Duration::from_millis(800);

fn signal_read_at(first: Instant, last: Instant, last_read: Instant) -> Instant {
    (last + QUIET).min(first + MAX_DEFER).max(last_read + MIN_GAP)
}

/// Git 열 일꾼을 깨우는 자리 — 보는 칸이 바뀌었을 때(바로 읽기)와 깃 신호가 왔을 때.
#[derive(Default)]
pub(crate) struct Wake {
    state: Mutex<WakeState>,
    cond: std::sync::Condvar,
}

#[derive(Default)]
struct WakeState {
    kicks: u64,
    remote_touch: bool,
    /// 다른 기기 원본이 깃 신호를 주고 그 칸이 mod 칸이다 — (base, 원본 칸).
    remote_live: Option<(String, String)>,
}

impl Wake {
    pub fn kick(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.kicks = state.kicks.wrapping_add(1);
        }
        self.cond.notify_all();
    }

    fn touch_remote(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.remote_touch = true;
        }
        self.kick();
    }

    fn take_remote_touch(&self) -> bool {
        self.state.lock().map(|mut state| std::mem::take(&mut state.remote_touch)).unwrap_or(false)
    }

    fn set_remote_live(&self, live: Option<(String, String)>) {
        if let Ok(mut state) = self.state.lock() {
            state.remote_live = live;
        }
    }

    fn remote_live(&self, base: &str, pane: &str) -> bool {
        self.state.lock().is_ok_and(|state| state.remote_live.as_ref().is_some_and(|(b, p)| b == base && p == pane))
    }

    /// `seen` 뒤로 깨우거나 `until` 이 될 때까지 잔다. 지금 번호를 돌려준다.
    fn wait(&self, seen: u64, until: Instant) -> u64 {
        let Ok(mut state) = self.state.lock() else { return seen };
        while state.kicks == seen {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = match self.cond.wait_timeout(state, left) {
                Ok((state, _)) => state,
                Err(_) => return seen,
            };
        }
        state.kicks
    }
}

fn period(target: &Target, wake: &Wake) -> std::time::Duration {
    let signalled = match &target.remote {
        Some((_, base, _)) => wake.remote_live(base, &target.pane),
        None => kasa_mcp::claude_mod::live(&target.pane).is_some(),
    };
    if signalled { POLL_SIGNALLED } else { POLL }
}

/// 이 칸 또는 같은 작업 트리를 건드린 깃 신호가 있었나. 아직 한 번도 못 읽은 칸은 뿌리를 몰라 그 칸의 신호만 본다.
fn touched_by(signals: &[kasa_mcp::claude_mod::GitSignal], target: &Target, root: Option<&std::path::Path>) -> bool {
    signals.iter().any(|signal| match root {
        Some(root) => kasa_mcp::claude_mod::git_signal_touches(signal, &target.pane, root),
        None => signal.surface == target.pane,
    })
}

/// Git 열 일꾼. 보는 칸이 바뀌면 바로, 깃 신호가 오면 몰아친 것을 합쳐 한 번, 그 밖에는 주기마다 읽는다.
pub(crate) fn spawn_poller(
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    context: Arc<Mutex<Context>>,
    data: Arc<Mutex<GitColView>>,
    want: Arc<std::sync::atomic::AtomicUsize>,
    wake: Arc<Wake>,
) {
    std::thread::spawn(move || {
        let mut kicks = 0;
        let mut seen = kasa_mcp::claude_mod::git_seq();
        let mut read_generation = None;
        let mut due = Instant::now();
        let mut pending: Option<(Instant, Instant)> = None;
        let mut last_read = Instant::now().checked_sub(MIN_GAP).unwrap_or_else(Instant::now);
        let mut repo: Option<(std::path::PathBuf, Option<kasa_mcp::git::RepoPaths>)> = None;
        let mut read_print: Option<u64> = None;
        let mut read_want = 0;
        loop {
            let until = pending.map_or(due, |(first, last)| signal_read_at(first, last, last_read).min(due));
            kicks = wake.wait(kicks, until);
            let now = Instant::now();
            let (generation, target) = match context.lock() {
                Ok(context) => (context.generation, context.target.clone()),
                Err(_) => break,
            };
            let (seq, signals, lost) = kasa_mcp::claude_mod::git_signals_since(seen);
            seen = seq;
            let Some(target) = target else {
                pending = None;
                due = now + POLL;
                continue;
            };
            let local_root = if target.remote.is_some() {
                None
            } else {
                data.lock()
                    .ok()
                    .filter(|view| view.generation == generation)
                    .and_then(|view| view.repo_root.clone().or(view.cwd.clone()))
                    .or_else(|| target.cwd.clone())
            };
            let touched = if target.remote.is_some() {
                wake.take_remote_touch()
            } else {
                lost || touched_by(&signals, &target, local_root.as_deref())
            };
            if touched {
                pending = Some(pending.map_or((now, now), |(first, _)| (first, now)));
            }
            let fresh = read_generation != Some(generation);
            let signal_due = pending.is_some_and(|(first, last)| now >= signal_read_at(first, last, last_read));
            if !(fresh || signal_due || now >= due) {
                continue;
            }
            let print = local_root.as_deref().and_then(|root| {
                if repo.as_ref().is_none_or(|(at, _)| at != root) {
                    repo = Some((root.to_path_buf(), kasa_mcp::git::repo_paths(root)));
                }
                repo.as_ref().and_then(|(_, paths)| paths.as_ref()).and_then(kasa_mcp::git::repo_fingerprint)
            });
            let want_now = want.load(std::sync::atomic::Ordering::Relaxed);
            if !(fresh || signal_due)
                && print.is_some()
                && (print, want_now) == (read_print, read_want)
                && now.duration_since(last_read) < IDLE_READ
            {
                due = now + period(&target, &wake);
                continue;
            }
            pending = None;
            let request = match context.lock() {
                Ok(mut context) => context.next_request(),
                Err(_) => break,
            };
            let Some((generation, request, target)) = request else { continue };
            let view = fetch(&target, generation, want_now);
            last_read = Instant::now();
            (read_print, read_want) = (print, want_now);
            read_generation = Some(generation);
            due = last_read + period(&target, &wake);
            let Ok(context) = context.lock() else { break };
            if !context.accepts_request(generation, request, &target) {
                continue;
            }
            let Ok(mut data) = data.lock() else { break };
            if *data != view {
                *data = view;
                drop(data);
                drop(context);
                if proxy.send_event(UserEvent::Redraw).is_err() {
                    break;
                }
            }
        }
    });
}

/// 다른 기기 칸의 Git 열 — 원본 기기의 `/term/gitcol/wait` 에 매달려, 원본이 깃 신호를 받으면 일꾼을 깨운다.
/// 옛 원본(그 길이 없음)이면 물러나고 일꾼은 옛 주기로 읽는다.
pub(crate) fn spawn_remote_watcher(context: Arc<Mutex<Context>>, wake: Arc<Wake>) {
    std::thread::spawn(move || {
        let mut kicks = 0;
        let mut since: Option<(String, String, u64)> = None;
        // 보는 칸이 바뀌거나 `back` 이 지날 때까지 쉰다 — 깃 신호마다 깨는 kick 에 물러남이 끊기지 않게.
        let rest = |kicks: &mut u64, generation: u64, back: std::time::Duration| {
            let until = Instant::now() + back;
            while Instant::now() < until {
                *kicks = wake.wait(*kicks, until);
                if context.lock().map_or(true, |c| c.generation != generation) {
                    return;
                }
            }
        };
        loop {
            let Ok((generation, target)) = context.lock().map(|c| (c.generation, c.target.clone())) else { break };
            let remote = target.filter(|t| t.issue.is_none()).and_then(|t| t.remote.clone().map(|r| (t, r)));
            let Some((target, (_, base, machine_id))) = remote else {
                wake.set_remote_live(None);
                since = None;
                rest(&mut kicks, generation, std::time::Duration::from_secs(30));
                continue;
            };
            let previous = since.as_ref().filter(|(b, p, _)| *b == base && *p == target.pane).map(|(_, _, seq)| *seq);
            let query = format!(
                "/term/gitcol/wait?schema={}&machine_id={}&pane={}&surface_key={}&wait_ms={}{}",
                kasa_mcp::git_panel::SCHEMA,
                kasa_mcp::remote::urlencode(&machine_id),
                kasa_mcp::remote::urlencode(&target.pane),
                kasa_mcp::remote::urlencode(target.surface_key.as_deref().unwrap_or_default()),
                kasa_mcp::git_panel::WAIT_CAP_MS,
                previous.map(|seq| format!("&since={seq}")).unwrap_or_default(),
            );
            match kasa_mcp::remote::remote_get_json_bounded(&base, &query, 4096) {
                Ok(answer) if answer.get("ok").and_then(serde_json::Value::as_bool) == Some(true) => {
                    let seq = answer.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(0);
                    let live = answer.get("live").and_then(serde_json::Value::as_bool) == Some(true);
                    wake.set_remote_live(live.then(|| (base.clone(), target.pane.clone())));
                    let same = context.lock().is_ok_and(|c| c.generation == generation);
                    if same && answer.get("changed").and_then(serde_json::Value::as_bool) == Some(true) {
                        wake.touch_remote();
                    }
                    since = Some((base, target.pane, seq));
                }
                Ok(_) => {
                    wake.set_remote_live(None);
                    since = None;
                    rest(&mut kicks, generation, std::time::Duration::from_secs(10));
                }
                Err(error) => {
                    wake.set_remote_live(None);
                    since = None;
                    let old_source = error.to_string().contains("404");
                    rest(&mut kicks, generation, std::time::Duration::from_secs(if old_source { 120 } else { 5 }));
                }
            }
        }
    });
}

impl App {
    pub(crate) fn current_git_target(&self) -> Option<Target> {
        let id = self
            .ws
            .lock()
            .ok()
            .and_then(|w| w.active_pane.as_deref().map(|id| w.active_tab_pid(id)))?;
        if let Some(info) = kasa_mcp::remote::remote_info(&id) {
            let label = kasa_mcp::machines::label_for_base(&info.base)
                .or_else(|| (!info.label.is_empty()).then_some(info.label.clone()))
                .unwrap_or_else(|| "원격 기기".into());
            let machine_id = kasa_mcp::machines::find(&label)
                .and_then(|m| m.machine_id)
                .unwrap_or_default();
            let key = kasa_mcp::remote::remote_surface_key(&id);
            // Only the source device's record supplies cwd; a mirror has a local launch directory too.
            let cwd = kasa_mcp::machines::cached_fresh_pane(&label, &info.remote_id)
                .filter(|row| row.get("surface_key").and_then(|v| v.as_str()) == key.as_deref())
                .and_then(|row| row.get("cwd").and_then(|v| v.as_str()).map(str::to_owned))
                .filter(|cwd| kasa_mcp::git_panel::absolute_path(cwd))
                .map(std::path::PathBuf::from);
            let issue = if key.is_none() {
                Some(message("update_needed"))
            } else if machine_id.is_empty() {
                Some("원본 기기의 신원을 확인 중이에요".into())
            } else {
                None
            };
            return Some(Target {
                pane: info.remote_id,
                surface_key: key,
                cwd,
                remote: Some((label, info.base, machine_id)),
                issue,
            });
        }
        let cwd = self
            .git
            .col_pinned_cwd
            .clone()
            .or_else(|| self.pane_cwd_cache.get(&id).cloned());
        let issue = cwd
            .is_none()
            .then(|| "현재 창의 폴더를 확인 중이에요".into());
        Some(Target {
            pane: id.clone(),
            surface_key: kasa_mcp::surface_keys::get(&id),
            cwd,
            remote: None,
            issue,
        })
    }
}

pub(crate) fn header(
    g: &mut gpu::GpuRenderer,
    git: &mut state::GitState,
    view: &GitColView,
    cursor: (f32, f32),
    x: f32,
    mut y: f32,
    width: f32,
) -> f32 {
    let machine = match &view.remote {
        Some((label, _)) => crate::render::pane_identity::device_name(label),
        None => crate::render::pane_identity::local_device_name().unwrap_or_else(|| "이 기기".to_string()),
    };
    let repo = view
        .repo_root
        .as_ref()
        .or(view.cwd.as_ref())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "폴더 확인 중".into());
    let repo = repo
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&repo);
    let source = format!("{machine} / {repo}");
    let rect = icon_label_button(
        g,
        (x, y, width, native_controls::CONTROL_HEIGHT),
        cursor,
        if view.remote.is_some() {
            "server"
        } else {
            "folder"
        },
        &source,
        None,
        native_controls::Style {
            enabled: view.remote.is_none(),
            active: git.col_pinned_cwd.is_some() && view.remote.is_none(),
            ..Default::default()
        },
    );
    if view.remote.is_none() {
        git.path_hdr_rect = Some(rect);
    }
    y += native_controls::CONTROL_HEIGHT + 8.0;
    if !view.no_repo && !view.loading && view.issue.is_none() {
        let current = if view.detached {
            "분리된 HEAD"
        } else {
            &view.branch
        };
        git.branch_hdr_rect = Some(icon_label_button(
            g,
            (x, y, width, native_controls::CONTROL_HEIGHT),
            cursor,
            "git-branch",
            current,
            Some(if git.branch_menu_open {
                "chevron-up"
            } else {
                "chevron-down"
            }),
            native_controls::Style {
                active: true,
                ..Default::default()
            },
        ));
        y += native_controls::CONTROL_HEIGHT + 6.0;
        let current_branch = view
            .branch_list
            .iter()
            .find(|branch| branch.current && !branch.remote);
        let tracking = if view.detached {
            format!(
                "{}  ·  브랜치 {}개",
                short_oid(view.head_oid.as_deref().unwrap_or("")),
                view.branch_list.len()
            )
        } else if view.unborn {
            "첫 커밋 전".into()
        } else if let Some(upstream) = current_branch.and_then(|branch| branch.upstream.as_deref())
        {
            let counts = tracking_counts(
                current_branch.and_then(|b| b.ahead),
                current_branch.and_then(|b| b.behind),
            );
            format!("{}  {counts}", upstream_label(upstream))
        } else {
            "추적 브랜치 없음".into()
        };
        label(
            g,
            x + 10.0,
            y,
            &tracking,
            width - 20.0,
            10.5,
            theme::text_dim(),
            false,
        );
        y += 18.0 + 8.0;
    }
    y
}

fn short_oid(oid: &str) -> String {
    oid.chars().take(8).collect()
}

/// 화면에는 `origin/main` — 데이터는 전체 refname 을 지켜 같은 이름의 로컬·원격이 안 섞이게 한다.
fn upstream_label(upstream: &str) -> &str {
    upstream.strip_prefix("refs/remotes/").unwrap_or(upstream)
}

fn tracking_counts(ahead: Option<u32>, behind: Option<u32>) -> String {
    match (ahead, behind) {
        (Some(0), Some(0)) => "최신".into(),
        (Some(ahead), Some(behind)) => format!("앞섬 {ahead} · 뒤처짐 {behind}"),
        _ => "비교 미확인".into(),
    }
}

fn label(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    text: &str,
    width: f32,
    size: f32,
    color: [u8; 4],
    bold: bool,
) {
    let shown = info::fit_text(g, text, width.max(0.0), size, bold);
    let color = theme::enforce_contrast_at(color, theme::panel_bg(), 4.5);
    g.draw_text(
        x,
        y,
        &shown,
        gpu::DrawOpts {
            font_size: size,
            color,
            bold,
            italic: false,
        },
    );
}

fn icon_label_button(
    g: &mut gpu::GpuRenderer,
    rect: (f32, f32, f32, f32),
    cursor: (f32, f32),
    icon: &str,
    text: &str,
    trailing: Option<&str>,
    style: native_controls::Style,
) -> (f32, f32, f32, f32) {
    let rect = native_controls::text_button(g, rect, cursor, "", style);
    let ink = if !style.enabled {
        theme::text_dim()
    } else if style.active {
        theme::enforce_contrast_at(theme::accent(), theme::panel_bg(), 4.5)
    } else {
        theme::text()
    };
    g.queue_icon(icon, rect.0 + 10.0, rect.1 + 6.0, 14.0, ink);
    let reserve = if trailing.is_some() { 28.0 } else { 10.0 };
    label(
        g,
        rect.0 + 32.0,
        rect.1 + 6.0,
        text,
        rect.2 - 32.0 - reserve,
        12.0,
        ink,
        false,
    );
    if let Some(icon) = trailing {
        g.queue_icon(icon, rect.0 + rect.2 - 24.0, rect.1 + 6.0, 14.0, ink);
    }
    rect
}

#[cfg(test)]
fn branch_rows(view: &GitColView) -> Vec<String> {
    view.branch_list
        .iter()
        .map(|branch| {
            format!(
                "{} · {}{}",
                if branch.remote { "원격" } else { "로컬" },
                branch.name,
                if branch.current { " · 현재" } else { "" }
            )
        })
        .collect()
}

fn branch_page_layout(rows: usize, page: usize, room: f32) -> (usize, usize, usize, usize, f32) {
    let (row_h, group_h) = branch_row_sizes(room);
    let reserve = native_controls::CONTROL_HEIGHT + 2.0 * group_h + 8.0;
    let per_page = ((room - reserve) / row_h).floor().max(1.0) as usize;
    let pages = rows.max(1).div_ceil(per_page);
    let page = page.min(pages - 1);
    let first = page * per_page;
    let shown = rows.saturating_sub(first).min(per_page).max(1);
    let height = (shown as f32 * row_h + reserve).min(room.max(0.0));
    (page, pages, first, shown, height)
}

fn branch_row_sizes(room: f32) -> (f32, f32) {
    if room >= 126.0 {
        (40.0, native_controls::CONTROL_HEIGHT)
    } else {
        (native_controls::CONTROL_HEIGHT, 0.0)
    }
}

pub(crate) fn branches(
    g: &mut gpu::GpuRenderer,
    git: &mut state::GitState,
    view: &GitColView,
    cursor: (f32, f32),
    x: f32,
    width: f32,
    bottom: f32,
) {
    git.branch_page_rects.clear();
    if !git.branch_menu_open {
        return;
    }
    let Some((_, y, _, h)) = git.branch_hdr_rect else {
        return;
    };
    let top = y + h + 6.0 + 18.0 + 8.0;
    let room = (bottom - top - 8.0).max(0.0);
    let (row_h, group_h) = branch_row_sizes(room);
    let mut rows = view.branch_list.iter().collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        a.remote
            .cmp(&b.remote)
            .then_with(|| b.current.cmp(&a.current))
            .then_with(|| a.name.cmp(&b.name))
    });
    let (page, pages, first, shown, height) = branch_page_layout(rows.len(), git.branch_page, room);
    git.branch_page = page;
    g.rect(x, top, width, height, theme::panel_bg());
    g.push_clip(x, top, width, height);
    let mut row_y = top;
    let mut group = None;
    for branch in rows.iter().skip(first).take(shown) {
        if group_h > 0.0 && group != Some(branch.remote) {
            group = Some(branch.remote);
            let name = if branch.remote {
                "원격 브랜치"
            } else {
                "로컬 브랜치"
            };
            let count = rows.iter().filter(|b| b.remote == branch.remote).count();
            g.queue_icon(
                if branch.remote { "server" } else { "folder" },
                x + 10.0,
                row_y + 6.0,
                14.0,
                theme::text_dim(),
            );
            label(
                g,
                x + 32.0,
                row_y + 7.0,
                &format!("{name}  {count}"),
                width - 42.0,
                11.0,
                theme::text_dim(),
                true,
            );
            row_y += native_controls::CONTROL_HEIGHT;
        }
        let ink = if branch.current {
            theme::enforce_contrast_at(theme::accent(), theme::panel_bg(), 4.5)
        } else {
            theme::text()
        };
        g.queue_icon(
            if branch.current {
                "check"
            } else if branch.remote {
                "server"
            } else {
                "git-branch"
            },
            x + 10.0,
            row_y + 4.0,
            14.0,
            ink,
        );
        let badge_w = if branch.current { 40.0 } else { 0.0 };
        label(
            g,
            x + 32.0,
            row_y + 4.0,
            &branch.name,
            width - 42.0 - badge_w,
            12.0,
            ink,
            false,
        );
        if branch.current {
            label(
                g,
                x + width - 38.0,
                row_y + 5.0,
                "현재",
                28.0,
                10.5,
                ink,
                false,
            );
        }
        let detail = if let Some(upstream) = &branch.upstream {
            format!(
                "{}  {}",
                upstream_label(upstream),
                tracking_counts(branch.ahead, branch.behind)
            )
        } else {
            let subject = view
                .commit_graph
                .iter()
                .find(|commit| commit.oid == branch.oid)
                .map(|commit| commit.subject.as_str())
                .unwrap_or("");
            format!("{}  {subject}", short_oid(&branch.oid))
        };
        if row_h > native_controls::CONTROL_HEIGHT {
            label(
                g,
                x + 32.0,
                row_y + 22.0,
                &detail,
                width - 42.0,
                10.5,
                theme::text_dim(),
                false,
            );
        }
        row_y += row_h;
    }
    if rows.is_empty() {
        g.draw_text(
            x + 10.0,
            top + 7.0,
            "브랜치 없음",
            gpu::DrawOpts {
                font_size: 12.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
    }
    let y = top + height - native_controls::CONTROL_HEIGHT;
    let half = ((width - 8.0) / 2.0).max(0.0);
    for (back, bx, enabled, label) in [
        (true, x, git.branch_page > 0, "이전".to_string()),
        (
            false,
            x + half + 8.0,
            git.branch_page + 1 < pages,
            format!("다음 · {}/{}", git.branch_page + 1, pages),
        ),
    ] {
        let rect = icon_label_button(
            g,
            (bx, y, half, native_controls::CONTROL_HEIGHT),
            cursor,
            if back {
                "chevron-left"
            } else {
                "chevron-right"
            },
            &label,
            None,
            native_controls::Style {
                enabled,
                ..Default::default()
            },
        );
        if enabled {
            if let Some(rect) = g.clip_hit(rect) {
                git.branch_page_rects.push((back, rect));
            }
        }
    }
    g.pop_clip();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(pane: &str, machine: Option<&str>, cwd: &str) -> Target {
        Target {
            pane: pane.into(),
            surface_key: Some(format!("key-{pane}")),
            cwd: Some(cwd.into()),
            remote: machine.map(|m| (m.into(), format!("https://{m}.invalid"), m.into())),
            issue: None,
        }
    }

    #[test]
    fn a_burst_of_git_signals_becomes_one_read_after_a_quiet_gap_or_the_defer_cap() {
        let t0 = Instant::now();
        let long_ago = t0.checked_sub(std::time::Duration::from_secs(10)).unwrap();
        assert_eq!(signal_read_at(t0, t0, long_ago), t0 + QUIET, "a lone edit is read after the quiet gap");
        let busy = t0 + std::time::Duration::from_millis(900);
        assert_eq!(signal_read_at(t0, busy, long_ago), t0 + MAX_DEFER, "a steady stream still reads by the cap");
        assert_eq!(signal_read_at(t0, t0, t0), t0 + MIN_GAP, "signal reads keep a minimum gap");
    }

    #[test]
    fn a_kick_wakes_the_worker_before_its_deadline() {
        let wake = Arc::new(Wake::default());
        let started = Instant::now();
        let kicker = wake.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            kicker.kick();
        });
        assert_eq!(wake.wait(0, started + std::time::Duration::from_secs(5)), 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        thread.join().unwrap();
        assert_eq!(wake.wait(1, Instant::now() + std::time::Duration::from_millis(20)), 1, "no kick: sleeps to the deadline");
        wake.touch_remote();
        assert!(wake.take_remote_touch());
        assert!(!wake.take_remote_touch());
    }

    #[test]
    fn signals_from_another_pane_count_only_inside_the_same_tree() {
        let signal = |surface: &str, path: &str| kasa_mcp::claude_mod::GitSignal {
            seq: 1,
            surface: surface.into(),
            cwd: "/work".into(),
            paths: vec![path.into()],
        };
        let local = target("%7", None, "/work/repo");
        let root = std::path::Path::new("/work/repo");
        assert!(touched_by(&[signal("%7", "/elsewhere/a")], &local, Some(root)));
        assert!(touched_by(&[signal("%8", "/work/repo/a.rs")], &local, Some(root)));
        assert!(!touched_by(&[signal("%8", "/work/other/a.rs")], &local, Some(root)));
        assert!(!touched_by(&[signal("%8", "/work/repo/a.rs")], &local, None), "before the first read only the pane's own claude counts");
    }

    #[test]
    fn a_source_signal_wakes_the_viewer_column_and_marks_the_pane_as_signalled() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        // 첫 번은 답하고 둘째(다음 기다림)는 쥐고 있는다 — 원본이 다음 신호를 기다리는 모양.
        let server = std::thread::spawn(move || {
            let mut first = String::new();
            for round in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let n = socket.read(&mut buffer).unwrap();
                    request.extend_from_slice(&buffer[..n]);
                }
                if round == 0 {
                    first = String::from_utf8(request).unwrap();
                    let body = serde_json::json!({"schema": kasa_mcp::git_panel::SCHEMA, "ok": true, "seq": 7, "changed": true, "live": true}).to_string();
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                } else {
                    assert!(String::from_utf8(request).unwrap().contains("&since=7"), "the next hold waits past the number it learned");
                    return (first, socket);
                }
            }
            unreachable!()
        });
        let mut remote = target("%7", Some("source-device"), "/stale");
        remote.remote.as_mut().unwrap().1 = base.clone();
        let mut context = Context::default();
        context.select(Some(remote));
        let context = Arc::new(Mutex::new(context));
        let wake = Arc::new(Wake::default());
        spawn_remote_watcher(context, wake.clone());
        let (request, _held) = server.join().unwrap();
        assert!(request.starts_with("GET /term/gitcol/wait?schema=kasa.git-panel.v2&machine_id=source-device&pane=%257&surface_key=key-%257&"));
        assert!(!request.contains("since="), "the first hold only learns the source's number");
        assert!(wake.remote_live(&base, "%7"));
        assert!(wake.take_remote_touch());
    }

    #[test]
    fn old_responses_cannot_cross_pane_device_directory_or_aba_switches() {
        let a = target("%1", None, "/repo");
        let mut context = Context::default();
        context.select(Some(a.clone()));
        let generation = context.generation;
        assert!(context.accepts(generation, &a));
        for next in [
            target("%2", None, "/repo"),
            target("%1", Some("other"), "/repo"),
            target("%1", None, "/new"),
        ] {
            context.select(Some(next));
            assert!(!context.accepts(generation, &a));
        }
        context.select(Some(a.clone()));
        assert!(!context.accepts(generation, &a));
    }

    #[test]
    fn placeholder_drops_previous_repository_data_and_preserves_remote_boundary() {
        let view = placeholder(
            &target("%3", Some("other"), "/repo"),
            4,
            Some(message("offline")),
        );
        assert!(view.remote.is_some() && view.issue.is_some());
        assert!(view.branch.is_empty() && view.branch_list.is_empty() && view.staged.is_empty());
        assert!(!view.no_repo && !view.loading);
    }

    #[test]
    fn newer_reads_win_when_two_requests_for_the_same_source_finish_out_of_order() {
        let mut context = Context::default();
        context.select(Some(target("%1", Some("source"), "/repo")));
        let (generation, old, target) = context.next_request().unwrap();
        let (_, new, _) = context.next_request().unwrap();
        assert!(!context.accepts_request(generation, old, &target));
        assert!(context.accepts_request(generation, new, &target));
    }

    #[test]
    fn branch_pages_fit_the_available_height_and_reach_every_reference() {
        for room in [86.0, 164.0, 320.0] {
            let mut reached = Vec::new();
            let (_, pages, _, _, _) = branch_page_layout(53, 0, room);
            for page in 0..pages {
                let (actual, _, first, count, height) = branch_page_layout(53, page, room);
                assert_eq!(actual, page);
                assert!(height <= room);
                let (row_h, group_h) = branch_row_sizes(room);
                assert!(count as f32 * row_h + group_h * 2.0 + 34.0 <= height);
                reached.extend(first..first + count);
            }
            assert_eq!(reached, (0..53).collect::<Vec<_>>());
        }
        let (page, pages, first, count, _) = branch_page_layout(0, 999, 86.0);
        assert_eq!((page, pages, first, count), (0, 1, 0, 1));
    }

    fn serve_once(value: serde_json::Value) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let n = socket.read(&mut buffer).unwrap();
                assert!(n > 0 && request.len() < 16384);
                request.extend_from_slice(&buffer[..n]);
            }
            let body = value.to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            String::from_utf8(request).unwrap()
        });
        (base, handle)
    }

    #[test]
    fn old_remote_http_reply_is_an_update_notice_without_local_fallback() {
        let (base, server) = serve_once(serde_json::json!({"ok":false,"error":"path required"}));
        let mut target = target("%7", Some("source-device"), "/same-path");
        target.remote.as_mut().unwrap().1 = base;
        let view = fetch(&target, 5, 20);
        assert!(view.issue.as_deref().unwrap().contains("업데이트"));
        assert!(view.branch_list.is_empty() && !view.no_repo && view.remote.is_some());
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /term/gitcol?schema=kasa.git-panel.v2&"));
        assert!(request.contains("pane=%257") && request.contains("surface_key=key-%257"));
        assert!(!request.contains("path=") && !request.contains("cwd="));
    }

    #[test]
    fn remote_http_snapshot_uses_source_directory_and_branch_metadata() {
        let snapshot = GitColView {
            cwd: Some("C:\\source\\repo".into()),
            branch: "main".into(),
            branch_list: vec![
                kasa_mcp::git::GitBranch {
                    name: "main".into(),
                    remote: false,
                    current: true,
                    ..Default::default()
                },
                kasa_mcp::git::GitBranch {
                    name: "origin/main".into(),
                    remote: true,
                    current: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let (base, server) = serve_once(
            serde_json::json!({"schema":kasa_mcp::git_panel::SCHEMA,"ok":true,
            "source":{"machine_id":"source-device","pane":"%7","surface_key":"key-%7","cwd":"C:\\source\\repo"}, "view": snapshot}),
        );
        let mut target = target("%7", Some("source-device"), "/stale-board-path");
        target.remote.as_mut().unwrap().1 = base;
        let view = fetch(&target, 9, 20);
        assert!(view.issue.is_none() && !view.loading && view.remote.is_some());
        assert_eq!(view.cwd, Some("C:\\source\\repo".into()));
        assert_eq!(view.generation, 9);
        assert_eq!(
            branch_rows(&view),
            ["로컬 · main · 현재", "원격 · origin/main"]
        );
        server.join().unwrap();
    }
}
