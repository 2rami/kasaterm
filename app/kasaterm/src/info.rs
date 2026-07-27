//! Info 패널 — 활성 pane 의 셸 아래에서 도는 프로세스와, 그것들이 listen 중인
//! 포트. 개발 중엔 "dev 서버가 아직 살아 있나, 몇 번 포트를 잡았나"를 확인하려고
//! `lsof -i` 를 치는 일이 잦은데, 그 답이 pane 옆에 상주하면 물어볼 일이 없다.
//!
//! 수집은 GUI 스레드 밖에서 돈다 — `ps` + `lsof` 를 fork 하므로 렌더 루프에서
//! 부르면 프레임을 떨군다. `App::pump_info` 가 패널이 열려 있을 때만 워커를
//! 깨우고, 결과는 `InfoState.rows`(Arc<Mutex>) 로 넘어온다.
//!
//! kasa_pty::process_table 을 쓰지 않는 이유: 그쪽은 (pid, ppid, comm) 만 주는데
//! 여기서는 좀비 판별용 stat 과 표시용 argv 가 함께 필요하고, 한 번의 ps 로 다
//! 받는 편이 fork 를 늘리지 않는다.
use super::*;
use std::collections::HashMap;

/// Info 목록의 한 행. `depth` 는 셸(0)로부터의 거리로, 렌더가 들여쓰기에 쓴다.
#[derive(Clone, Default)]
pub(crate) struct ProcRow {
    pub(crate) pid: u32,
    pub(crate) depth: u8,
    /// 표시용 짧은 이름(`node`, `claude`). argv[0] 의 파일명.
    pub(crate) name: String,
    /// argv 전체에서 이름을 뺀 나머지 — 부제로 흐리게 붙인다.
    pub(crate) rest: String,
    /// 이 pid 가 listen 중인 TCP 포트(오름차순, 중복 제거).
    pub(crate) ports: Vec<u16>,
}

/// `ps` 한 줄에서 뽑은 원시 레코드.
struct Raw {
    pid: u32,
    ppid: u32,
    args: String,
}

/// 셸 pid 를 뿌리로 한 프로세스 목록(자신 포함, 좀비 제외). 순서는 트리 선행
/// 순회 — 부모 바로 밑에 자식이 오도록 정렬해 들여쓰기가 말이 되게 한다.
pub(crate) fn collect(shell_pid: u32) -> Vec<ProcRow> {
    let table = process_snapshot();
    if table.is_empty() {
        return Vec::new();
    }
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let by_pid: HashMap<u32, &Raw> = table.iter().map(|r| (r.pid, r)).collect();
    for r in &table {
        children.entry(r.ppid).or_default().push(r.pid);
    }
    for kids in children.values_mut() {
        kids.sort_unstable();
    }
    let ports = listening_ports();
    let mut out = Vec::new();
    // 명시적 스택 DFS. 재귀를 피하는 건 깊이 때문이 아니라, ppid 가 순환하는
    // 이상 상태(부모가 죽고 pid 가 재사용된 찰나)에서도 멈추게 하려는 것 —
    // `seen` 이 같은 pid 를 두 번 펼치지 않는다.
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![(shell_pid, 0u8)];
    while let Some((pid, depth)) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(raw) = by_pid.get(&pid) {
            let (name, rest) = split_argv(&raw.args);
            out.push(ProcRow {
                pid,
                depth,
                name,
                rest,
                ports: ports.get(&pid).cloned().unwrap_or_default(),
            });
        }
        if let Some(kids) = children.get(&pid) {
            // pop 이 역순으로 꺼내니 뒤집어 넣어야 pid 오름차순으로 나온다.
            for &k in kids.iter().rev() {
                stack.push((k, depth.saturating_add(1)));
            }
        }
    }
    out
}

/// argv 를 (표시 이름, 나머지) 로 가른다. argv[0] 이 절대경로면 파일명만 남겨
/// `/opt/homebrew/bin/node` 가 `node` 로 읽히게 한다.
fn split_argv(args: &str) -> (String, String) {
    let args = args.trim();
    let (head, rest) = match args.split_once(' ') {
        Some((h, r)) => (h, r.trim()),
        None => (args, ""),
    };
    let name = std::path::Path::new(head)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(head)
        .to_string();
    (name, rest.to_string())
}

#[cfg(unix)]
fn process_snapshot() -> Vec<Raw> {
    let Ok(out) = proc::command("ps")
        .args(["-A", "-o", "pid=,ppid=,stat=,args="])
        .output()
    else {
        return Vec::new();
    };
    let s = String::from_utf8_lossy(&out.stdout);
    let mut rows = Vec::new();
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(stat)) = (
            it.next().and_then(|x| x.parse::<u32>().ok()),
            it.next().and_then(|x| x.parse::<u32>().ok()),
            it.next(),
        ) else {
            continue;
        };
        // 좀비(stat 앞머리 'Z')는 뺀다 — 출력을 볼 수도, kill 할 수도 없는
        // 껍데기라 목록에 있어봐야 노이즈다(kero 가 0.1.26 에서 같은 걸 고쳤다).
        if stat.starts_with('Z') {
            continue;
        }
        let args = it.collect::<Vec<_>>().join(" ");
        if args.is_empty() {
            continue;
        }
        rows.push(Raw { pid, ppid, args });
    }
    rows
}

#[cfg(windows)]
fn process_snapshot() -> Vec<Raw> {
    // Windows 엔 ps 가 없다. 프로세스 트리는 kasa_pty 의 Toolhelp 스냅샷을 그대로
    // 쓰고(같은 (pid, ppid, exe) 형태), argv 는 못 읽으면 exe 이름으로 떨어진다.
    // 좀비는 Toolhelp 스냅샷에 애초에 안 잡히므로 unix 쪽 stat 필터가 불필요하다.
    kasa_pty::process_table()
        .into_iter()
        .map(|(pid, ppid, name)| Raw {
            pid,
            ppid,
            args: kasa_pty::process_cmdline(pid).unwrap_or(name),
        })
        .collect()
}

/// pid → listen 중인 TCP 포트. 실패하면 빈 맵이라 패널은 포트 칩 없이 뜬다.
#[cfg(unix)]
fn listening_ports() -> HashMap<u32, Vec<u16>> {
    // `-F pn` 은 프로세스 레코드(p<pid>)와 이름 레코드(n<addr>)만 내보내는 lsof
    // 의 기계 판독 모드다. 사람이 읽는 표를 파싱하면 명령 이름에 공백이 든
    // 프로세스에서 열이 밀린다.
    let Ok(out) = proc::command("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pn"])
        .output()
    else {
        return HashMap::new();
    };
    let s = String::from_utf8_lossy(&out.stdout);
    let mut map: HashMap<u32, Vec<u16>> = HashMap::new();
    let mut cur: Option<u32> = None;
    for line in s.lines() {
        let (tag, val) = line.split_at(line.char_indices().nth(1).map_or(0, |(i, _)| i));
        match tag {
            "p" => cur = val.parse::<u32>().ok(),
            "n" => {
                let Some(pid) = cur else { continue };
                // `*:3000` · `127.0.0.1:8080` · `[::1]:5173` — 어느 쪽이든 포트는
                // 마지막 ':' 뒤다.
                if let Some(port) = val.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                    map.entry(pid).or_default().push(port);
                }
            }
            _ => {}
        }
    }
    for v in map.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    map
}

#[cfg(windows)]
fn listening_ports() -> HashMap<u32, Vec<u16>> {
    let Ok(out) = proc::command("netstat").args(["-ano", "-p", "TCP"]).output() else {
        return HashMap::new();
    };
    let s = String::from_utf8_lossy(&out.stdout);
    let mut map: HashMap<u32, Vec<u16>> = HashMap::new();
    for line in s.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        // TCP  <local>  <remote>  LISTENING  <pid>
        if f.len() < 5 || !f[0].eq_ignore_ascii_case("TCP") || f[3] != "LISTENING" {
            continue;
        }
        let (Some(port), Some(pid)) = (
            f[1].rsplit(':').next().and_then(|p| p.parse::<u16>().ok()),
            f[4].parse::<u32>().ok(),
        ) else {
            continue;
        };
        map.entry(pid).or_default().push(port);
    }
    for v in map.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    map
}


impl App {
    /// 패널이 열려 있는 동안 워커를 주기적으로 깨운다. 렌더 루프에서 불리므로
    /// 여기서 직접 ps/lsof 를 돌리면 안 된다 — 스레드를 띄우고 즉시 반환한다.
    /// 워커가 하나 도는 동안 다시 띄우지 않도록 `busy` 로 막는다.
    pub(crate) fn pump_info(&mut self) {
        if self.info.tab != state::SideTab::Info || !self.git.col_visible {
            return;
        }
        let Some(pid) = self.active_pty().and_then(|p| p.shell_pid()) else {
            return;
        };
        // pane 을 옮기면 대상이 바뀐 것이므로 캐시를 버리고 즉시 다시 뜬다.
        if self.info.shell_pid != Some(pid) {
            self.info.shell_pid = Some(pid);
            self.info.last_refresh = None;
            self.info.scroll = 0.0;
            if let Ok(mut g) = self.info.rows.lock() {
                g.clear();
            }
        }
        let fresh = self
            .info
            .last_refresh
            .is_some_and(|t: Instant| t.elapsed() < std::time::Duration::from_millis(1500));
        if fresh || self.info.busy.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        self.info.last_refresh = Some(Instant::now());
        self.info.busy.store(true, std::sync::atomic::Ordering::Relaxed);
        let rows = self.info.rows.clone();
        let busy = self.info.busy.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let fresh = collect(pid);
            if let Ok(mut g) = rows.lock() {
                *g = fresh;
            }
            busy.store(false, std::sync::atomic::Ordering::Relaxed);
            // 새 목록이 붙었으니 한 프레임 그려달라고 깨운다.
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }
}

/// 우측 칼럼 두 탭이 공유하는 머리 — 탭 이름 둘 + 확장/닫기 버튼. 활성 탭은
/// 밑줄로 표시한다(배경 pill 은 칼럼이 좁을 때 글자를 먹는다). 반환값은 본문이
/// 시작할 y.
///
/// `App` 메서드가 아니라 자유 함수인 건 빌림 때문이다 — 호출부는 이미
/// `self.gpu.as_mut()` 로 gpu 필드를 빌린 상태라 `self.method(g)` 는 self 를
/// 통째로 다시 빌려 E0499 가 난다. 쓰는 필드만 따로 받으면 서로 겹치지 않는다.
pub(crate) fn draw_side_tabs(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    info: &mut state::InfoState,
    git: &mut state::GitState,
    x: f32,
    w: f32,
    top: f32,
) -> f32 {
    let y = top + 10.0;
    let bi = 15.0_f32;
    let close_x = x + w - 12.0 - bi;
    let expand_x = close_x - bi - 8.0;
    let bhov = |cx: f32| {
        cursor.0 >= cx - 3.0
            && cursor.0 <= cx + bi + 3.0
            && cursor.1 >= y - 3.0
            && cursor.1 <= y + bi + 3.0
    };
    g.queue_icon(
        "maximize",
        expand_x,
        y,
        bi,
        if bhov(expand_x) { theme::text() } else { theme::text_mute() },
    );
    g.queue_icon(
        "x",
        close_x,
        y,
        bi,
        if bhov(close_x) { theme::text() } else { theme::text_mute() },
    );
    git.col_expand_rect = Some((expand_x - 3.0, y - 3.0, bi + 6.0, bi + 6.0));
    git.col_close_rect = Some((close_x - 3.0, y - 3.0, bi + 6.0, bi + 6.0));
    info.tab_rects.clear();
    let mut tx = x + 14.0;
    for (tab, label) in [(state::SideTab::Git, "Git"), (state::SideTab::Info, "Info")] {
        let active = info.tab == tab;
        let tw = g.measure_chrome_text(label, 12.0, active);
        let hot = (tx - 4.0, y - 4.0, tw + 8.0, 21.0);
        let hovered = cursor.0 >= hot.0
            && cursor.0 <= hot.0 + hot.2
            && cursor.1 >= hot.1
            && cursor.1 <= hot.1 + hot.3;
        let col = if active {
            theme::text()
        } else if hovered {
            theme::text_dim()
        } else {
            theme::text_mute()
        };
        g.draw_text(
            tx,
            y,
            label,
            gpu::DrawOpts { font_size: 12.0, color: col, bold: active, italic: false },
        );
        if active {
            g.rect(tx, y + 17.0, tw, 1.5, theme::accent());
        }
        info.tab_rects.push((tab, hot));
        tx += tw + 16.0;
    }
    y + 27.0
}

/// Info 탭 본문. 셸을 뿌리로 한 프로세스 트리를 들여쓰기로 그리고, listen 중인
/// 포트는 행 오른쪽에 칩으로 붙인다(클릭 → 브라우저로 열기).
pub(crate) fn draw_info_col(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    info: &mut state::InfoState,
    x: f32,
    w: f32,
    top: f32,
    bottom: f32,
) {
    let x0 = x + 14.0;
    let right = x + w - 12.0;
    let rows: Vec<ProcRow> = info.rows.lock().map(|r| r.clone()).unwrap_or_default();
    info.port_rects.clear();

    // 머리글: 프로세스 수 · 포트 수. 비어 있을 땐 왜 비었는지를 말해준다 —
    // "아무것도 없음"과 "아직 못 읽음"은 사용자에게 전혀 다른 정보다.
    let nports: usize = rows.iter().map(|r| r.ports.len()).sum();
    let head = if rows.is_empty() {
        if info.last_refresh.is_none() { "읽는 중…".to_string() } else { "프로세스 없음".to_string() }
    } else if nports > 0 {
        format!("프로세스 {} · 포트 {}", rows.len(), nports)
    } else {
        format!("프로세스 {}", rows.len())
    };
    g.draw_text(
        x0,
        top,
        &head,
        gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false },
    );
    let list_top = top + 22.0;
    if rows.is_empty() {
        return;
    }

    const ROW_H: f32 = 22.0;
    // 스크롤은 목록이 넘칠 때만. 남는 높이보다 짧아지면 0 으로 되돌려, pane 을
    // 옮겨 목록이 짧아진 뒤 빈 화면에 갇히는 일이 없게 한다.
    let max_scroll = (rows.len() as f32 * ROW_H - (bottom - list_top)).max(0.0);
    info.scroll = info.scroll.clamp(0.0, max_scroll);
    let mut y = list_top - info.scroll;

    for r in &rows {
        if y + ROW_H < list_top || y > bottom {
            y += ROW_H;
            continue;
        }
        if cursor.0 >= x && cursor.0 <= x + w && cursor.1 >= y && cursor.1 < y + ROW_H {
            g.rect(x, y, w, ROW_H, theme::surface_hover());
        }
        // 오른쪽부터 채운다 — 포트 칩과 pid 가 자리를 먼저 잡아야 이름이 남는
        // 폭을 정확히 알고 잘린다.
        let mut rx = right;
        let pid_s = r.pid.to_string();
        let pid_w = g.measure_chrome_text(&pid_s, 10.0, false);
        g.draw_text(
            rx - pid_w,
            y + 6.0,
            &pid_s,
            gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false },
        );
        rx -= pid_w + 8.0;
        for &port in r.ports.iter().rev() {
            let ps = port.to_string();
            let pw = g.measure_chrome_text(&ps, 10.0, true) + 12.0;
            let cx = rx - pw;
            if cx < x0 + 40.0 {
                break; // 이름 자리를 침범하면 나머지 칩은 접는다.
            }
            let chov = cursor.0 >= cx
                && cursor.0 <= cx + pw
                && cursor.1 >= y + 3.0
                && cursor.1 <= y + 19.0;
            let fill = if chov { theme::accent() } else { theme::with_alpha(theme::accent(), 0x33) };
            round_rect(g, cx, y + 3.0, pw, 16.0, theme::RADIUS_SM, fill);
            g.draw_text(
                cx + 6.0,
                y + 5.0,
                &ps,
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: if chov { theme::bg() } else { theme::text() },
                    bold: true,
                    italic: false,
                },
            );
            info.port_rects.push((port, (cx, y + 3.0, pw, 16.0)));
            rx = cx - 5.0;
        }
        // 이름 + argv 나머지. 남는 폭 안에서만 그린다.
        let nx = x0 + r.depth as f32 * 11.0;
        let avail = (rx - nx - 6.0).max(0.0);
        let name_w = g.measure_chrome_text(&r.name, 12.0, true);
        g.draw_text(
            nx,
            y + 4.0,
            &r.name,
            gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false },
        );
        if !r.rest.is_empty() && avail > name_w + 24.0 {
            let rest = fit_text(g, &r.rest, avail - name_w - 6.0);
            g.draw_text(
                nx + name_w + 6.0,
                y + 5.0,
                &rest,
                gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
            );
        }
        y += ROW_H;
    }
}

/// 주어진 폭에 들어가도록 꼬리를 자르고 말줄임을 붙인다. 폭이 아예 부족하면 빈
/// 문자열 — 잘린 한 글자만 남는 것보다 아무것도 없는 편이 읽기 낫다.
fn fit_text(g: &mut gpu::GpuRenderer, s: &str, avail: f32) -> String {
    if g.measure_chrome_text(s, 11.0, false) <= avail {
        return s.to_string();
    }
    let ell = g.measure_chrome_text("…", 11.0, false);
    if avail <= ell {
        return String::new();
    }
    // char 경계로만 자른다 — 바이트로 자르면 한글/이모지에서 패닉한다.
    let mut cut = 0;
    for (i, _) in s.char_indices() {
        if g.measure_chrome_text(&s[..i], 11.0, false) + ell > avail {
            break;
        }
        cut = i;
    }
    format!("{}…", &s[..cut])
}
