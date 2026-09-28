use super::*;

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
        if self.generation != 0 && self.target == target { return false; }
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
        remote: target.remote.as_ref().map(|(label, base, _)| (label.clone(), base.clone())),
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
    }.into()
}

pub(crate) fn fetch(target: &Target, generation: u64, commits: usize) -> GitColView {
    if let Some(issue) = &target.issue { return placeholder(target, generation, Some(issue.clone())); }
    let result = if let Some((_, base, machine_id)) = &target.remote {
        let expected = kasa_mcp::git_panel::Source {
            machine_id: machine_id.clone(), pane: target.pane.clone(),
            surface_key: target.surface_key.clone().unwrap_or_default(),
            // Cached board cwd can lag a shell cd; the source resolves it atomically with this read.
            cwd: String::new(),
        };
        let query = format!("/term/gitcol?schema={}&machine_id={}&pane={}&surface_key={}&commits={}",
            kasa_mcp::git_panel::SCHEMA, kasa_mcp::remote::urlencode(machine_id),
            kasa_mcp::remote::urlencode(&expected.pane), kasa_mcp::remote::urlencode(&expected.surface_key), commits.clamp(1, 200));
        kasa_mcp::remote::remote_get_json_bounded(base, &query, 2 * 1024 * 1024)
            .map_err(|error| if error.to_string().contains("404") { "update_needed" } else { "offline" })
            .and_then(|value| kasa_mcp::git_panel::validate_response(&value, &expected))
            .and_then(|value| serde_json::from_value::<GitColView>(value).map_err(|_| "invalid_response"))
    } else {
        target.cwd.as_ref().and_then(|cwd| handler::fetch_git_col_view(cwd, commits)).ok_or("git_unavailable")
    };
    match result {
        Ok(mut view) => {
            view.remote = target.remote.as_ref().map(|(label, base, _)| (label.clone(), base.clone()));
            view.generation = generation;
            view.loading = false;
            view.issue = None;
            view
        }
        Err(code) => placeholder(target, generation, Some(message(code))),
    }
}

impl App {
    pub(crate) fn current_git_target(&self) -> Option<Target> {
        let id = self.ws.lock().ok().and_then(|w| w.active_pane.as_deref().map(|id| w.active_tab_pid(id)))?;
        if let Some(info) = kasa_mcp::remote::remote_info(&id) {
            let label = kasa_mcp::machines::label_for_base(&info.base)
                .or_else(|| (!info.label.is_empty()).then_some(info.label.clone())).unwrap_or_else(|| "원격 기기".into());
            let machine_id = kasa_mcp::machines::find(&label).and_then(|m| m.machine_id).unwrap_or_default();
            let key = kasa_mcp::remote::remote_surface_key(&id);
            // Only the source device's record supplies cwd; a mirror has a local launch directory too.
            let cwd = kasa_mcp::machines::cached_fresh_pane(&label, &info.remote_id)
                .filter(|row| row.get("surface_key").and_then(|v| v.as_str()) == key.as_deref())
                .and_then(|row| row.get("cwd").and_then(|v| v.as_str()).map(str::to_owned))
                .filter(|cwd| kasa_mcp::git_panel::absolute_path(cwd)).map(std::path::PathBuf::from);
            let issue = if key.is_none() { Some(message("update_needed")) }
                else if machine_id.is_empty() { Some("원본 기기의 신원을 확인 중이에요".into()) } else { None };
            return Some(Target { pane: info.remote_id, surface_key: key, cwd,
                remote: Some((label, info.base, machine_id)), issue });
        }
        let cwd = self.git.col_pinned_cwd.clone().or_else(|| self.pane_cwd_cache.get(&id).cloned());
        let issue = cwd.is_none().then(|| "현재 창의 폴더를 확인 중이에요".into());
        Some(Target { pane: id.clone(), surface_key: kasa_mcp::surface_keys::get(&id), cwd, remote: None, issue })
    }
}

pub(crate) fn header(g: &mut gpu::GpuRenderer, git: &mut state::GitState, view: &GitColView,
    cursor: (f32, f32), x: f32, mut y: f32, width: f32) -> f32 {
    let machine: &str = match &view.remote {
        Some((label, _)) => label,
        None => info::cached_local_machine_name().unwrap_or("이 기기"),
    };
    let repo = view.repo_root.as_ref().or(view.cwd.as_ref()).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "폴더 확인 중".into());
    let repo = repo.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).unwrap_or(&repo);
    let source = format!("{machine} · {repo}");
    let rect = native_controls::text_button(g, (x, y, width, native_controls::CONTROL_HEIGHT), cursor, &source,
        native_controls::Style { enabled: view.remote.is_none(), active: git.col_pinned_cwd.is_some() && view.remote.is_none(), ..Default::default() });
    if view.remote.is_none() { git.path_hdr_rect = Some(rect); }
    y += native_controls::CONTROL_HEIGHT + 8.0;
    if !view.no_repo && !view.loading && view.issue.is_none() {
        let current = if view.detached {
            format!("분리된 HEAD · {}", view.head_oid.as_deref().unwrap_or("").chars().take(8).collect::<String>())
        } else if view.unborn { format!("현재 {} · 첫 커밋 전", view.branch) }
        else { format!("현재 {}", view.branch) };
        let label = format!("{current} · 브랜치 {}", view.branch_list.len());
        git.branch_hdr_rect = Some(native_controls::text_button(g, (x, y, width, native_controls::CONTROL_HEIGHT), cursor,
            &label, native_controls::Style { active: git.branch_menu_open, ..Default::default() }));
        y += native_controls::CONTROL_HEIGHT + 8.0;
    }
    y
}

pub(crate) fn branch_rows(view: &GitColView) -> Vec<String> {
    view.branch_list.iter().map(|branch| format!("{} · {}{}", if branch.remote { "원격" } else { "로컬" },
        branch.name, if branch.current { " · 현재" } else { "" })).collect()
}

fn branch_page_layout(rows: usize, page: usize, room: f32) -> (usize, usize, usize, usize, f32) {
    let row_h = native_controls::CONTROL_HEIGHT;
    let per_page = ((room - row_h - 8.0) / row_h).floor().max(1.0) as usize;
    let pages = rows.max(1).div_ceil(per_page);
    let page = page.min(pages - 1);
    let first = page * per_page;
    let shown = rows.saturating_sub(first).min(per_page).max(1);
    let height = (shown as f32 * row_h + row_h + 8.0).min(room.max(0.0));
    (page, pages, first, shown, height)
}

pub(crate) fn branches(g: &mut gpu::GpuRenderer, git: &mut state::GitState, view: &GitColView,
    cursor: (f32, f32), x: f32, width: f32, bottom: f32) {
    git.branch_page_rects.clear();
    if !git.branch_menu_open { return; }
    let Some((_, y, _, h)) = git.branch_hdr_rect else { return };
    let top = y + h + 8.0;
    let row_h = native_controls::CONTROL_HEIGHT;
    let room = (bottom - top - 8.0).max(0.0);
    let rows = branch_rows(view);
    let (page, pages, first, shown, height) = branch_page_layout(rows.len(), git.branch_page, room);
    git.branch_page = page;
    g.rect(x, top, width, height, theme::panel_bg());
    g.push_clip(x, top, width, height);
    for (i, row) in rows.iter().skip(first).take(shown).enumerate() {
        let label = info::fit_text(g, row, (width - 20.0).max(0.0), 12.0, false);
        let current = view.branch_list[first + i].current;
        g.draw_text(x + 10.0, top + i as f32 * row_h + 7.0, &label,
            gpu::DrawOpts { font_size: 12.0, color: if current { theme::accent() } else { theme::text_dim() }, bold: false, italic: false });
    }
    if rows.is_empty() {
        g.draw_text(x + 10.0, top + 7.0, "브랜치 없음", gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false });
    }
    let y = top + shown as f32 * row_h + 8.0;
    let half = ((width - 8.0) / 2.0).max(0.0);
    for (back, bx, enabled, label) in [(true, x, git.branch_page > 0, "이전".to_string()),
        (false, x + half + 8.0, git.branch_page + 1 < pages, format!("다음 · {}/{}", git.branch_page + 1, pages))] {
        let rect = native_controls::text_button(g, (bx, y, half, row_h), cursor, &label,
            native_controls::Style { enabled, ..Default::default() });
        if enabled {
            if let Some(rect) = g.clip_hit(rect) { git.branch_page_rects.push((back, rect)); }
        }
    }
    g.pop_clip();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(pane: &str, machine: Option<&str>, cwd: &str) -> Target {
        Target { pane: pane.into(), surface_key: Some(format!("key-{pane}")), cwd: Some(cwd.into()),
            remote: machine.map(|m| (m.into(), format!("https://{m}.invalid"), m.into())), issue: None }
    }

    #[test]
    fn old_responses_cannot_cross_pane_device_directory_or_aba_switches() {
        let a = target("%1", None, "/repo");
        let mut context = Context::default();
        context.select(Some(a.clone()));
        let generation = context.generation;
        assert!(context.accepts(generation, &a));
        for next in [target("%2", None, "/repo"), target("%1", Some("other"), "/repo"), target("%1", None, "/new")] {
            context.select(Some(next));
            assert!(!context.accepts(generation, &a));
        }
        context.select(Some(a.clone()));
        assert!(!context.accepts(generation, &a));
    }

    #[test]
    fn placeholder_drops_previous_repository_data_and_preserves_remote_boundary() {
        let view = placeholder(&target("%3", Some("other"), "/repo"), 4, Some(message("offline")));
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
                assert!(count as f32 * native_controls::CONTROL_HEIGHT + 34.0 <= height);
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
            socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
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
        let snapshot = GitColView { cwd: Some("C:\\source\\repo".into()), branch: "main".into(),
            branch_list: vec![kasa_mcp::git::GitBranch { name: "main".into(), remote: false, current: true },
                kasa_mcp::git::GitBranch { name: "origin/main".into(), remote: true, current: false }], ..Default::default() };
        let (base, server) = serve_once(serde_json::json!({"schema":kasa_mcp::git_panel::SCHEMA,"ok":true,
            "source":{"machine_id":"source-device","pane":"%7","surface_key":"key-%7","cwd":"C:\\source\\repo"}, "view": snapshot}));
        let mut target = target("%7", Some("source-device"), "/stale-board-path");
        target.remote.as_mut().unwrap().1 = base;
        let view = fetch(&target, 9, 20);
        assert!(view.issue.is_none() && !view.loading && view.remote.is_some());
        assert_eq!(view.cwd, Some("C:\\source\\repo".into()));
        assert_eq!(view.generation, 9);
        assert_eq!(branch_rows(&view), ["로컬 · main · 현재", "원격 · origin/main"]);
        server.join().unwrap();
    }
}
