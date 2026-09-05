//! 자동 테스트 하네스 — env 기반 auto-split/window/toggle/drag/tabs + schedule 타이머.
use super::*;

/// md 스크립트에 아직 실행할 단계가 남았는지. `about_to_wait` 이 이걸 보고
/// 프레임을 펌프한다 — 자세한 사정은 `run_pending_automdscript` 참고.
static MDSCRIPT_LEFT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `autoboxlabel` 이 심는 가짜 transcript 자리. `/tmp/...` 로 골라 프로젝트 슬러그가
/// 거노 실제 폴더와 안 겹치게 한다 — 그 폴더를 잘못 건드린 사고를 한 번 냈다.
const BOXLABEL_CWD: &str = "/tmp/kasaterm-boxlabel";
const BOXLABEL_SID: &str = "boxlabel-probe";

/// `autoimgtip` 이 심는 가짜 transcript 자리 — `boxlabel` 과 같은 이유로 `/tmp/`
/// 슬러그를 써서 거노 실제 프로젝트 폴더와 안 겹치게 한다.
const IMGTIP_CWD: &str = "/tmp/kasaterm-imgtip";
const IMGTIP_SID: &str = "imgtip-probe";
/// 화면에 찍을 참조 번호 — 심는 jsonl 의 `imagePasteIds` 와 짝이다.
const IMGTIP_N: u32 = 7;

/// `autotitlesync` 가 심는 가짜 transcript 자리 — 위 둘과 같은 이유로 `/tmp/` 슬러그.
const TITLESYNC_CWD: &str = "/tmp/kasaterm-titlesync";
const TITLESYNC_SID: &str = "titlesync-probe";

/// `autoultrascan` 이 심는 가짜 transcript 자리.
const ULTRASCAN_CWD: &str = "/tmp/kasaterm-ultrascan";
const ULTRASCAN_SID: &str = "ultrascan-probe";
/// `/effort` 가 화면에 뱉는 줄이 transcript 에 남는 모양 — 실제 레코드에서 떴다.
const ULTRASCAN_ENTER: &str = concat!(
    r#"{"attachment":{"type":"ultra_effort_enter","reminderType":"full"},"#,
    r#""type":"attachment"}"#
);

/// `KASATERM_AUTOPANEMERGE` 예약 슬롯 — (발사 시각, 대상 leaf).
static AUTO_MERGE: std::sync::OnceLock<std::sync::Mutex<Option<(Instant, String)>>> =
    std::sync::OnceLock::new();

fn auto_merge_slot() -> &'static std::sync::Mutex<Option<(Instant, String)>> {
    AUTO_MERGE.get_or_init(|| std::sync::Mutex::new(None))
}

pub(crate) fn mdscript_pending() -> bool {
    MDSCRIPT_LEFT.load(std::sync::atomic::Ordering::Relaxed)
}

/// 모니터 이동 프로브의 "정착 후" 재측정 예약 시각. 검증 전용이라
/// `struct App` 을 늘리지 않는다(병렬 작업 충돌 핫스팟).
static LAYERGEOM_DUE: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// `autostudent`·`autotabcycle` 이 함께 쓰는 가짜 claude 한 벌. 셸의 직속 자식
/// 이름이 `claude` 여야 `runs_claude` 관문이 열리므로 **그 이름의 실제 바이너리를
/// rustc 로 굽고**, 그 앞에 claude 입력박스(앵커 빈 줄·테두리·statusline)를 찍어
/// 프사와 전신이 설 자리를 만든다. 규칙은 `run_pending_autostudent` 주석에 있다.
///
/// 두 하네스가 각자 한 벌씩 갖고 있으면 한쪽만 고쳐져 「여기선 뜨는데 저기선
/// 안 뜬다」가 되므로 한 자리에 둔다.
pub(crate) const FAKE_CLAUDE_SCRIPT: &str = concat!(
        "d=\"$TMPDIR/kasaterm-student-probe\"; mkdir -p \"$d\"; ",
        "[ -x \"$d/claude\" ] || { ",
        "printf 'fn main(){std::thread::sleep(std::time::Duration::from_secs(600));}' > \"$d/c.rs\"; ",
        "rustc -o \"$d/claude\" \"$d/c.rs\" >/dev/null 2>&1; }; ",
        "R(){ printf \"\\342\\224\\200%.0s\" $(seq 1 \"$1\"); }; ",
        "echo; R 40; printf ' 대시보드 '; R 2; echo; ",
        "printf \"\\342\\235\\257 \\n\"; ",
        "R 60; echo; ",
        "printf \"\\357\\277\\274\\357\\277\\274\\357\\277\\274\\357\\277\\274 ctx 42%% \\n\"; ",
        "\"$d/claude\"\n",
);

impl App {
    /// Headless verification: arm a clean exit after KASATERM_AUTOQUIT_MS so a
    /// background run exercises the save-on-exit path (and thus the next
    /// launch's restore). No-op when the env var is unset.
    pub(crate) fn schedule_autoquit(&mut self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOQUIT_MS") else { return; };
        let Ok(ms) = ms_str.parse::<u64>() else { return; };
        eprintln!("[autoquit] clean exit in {ms}ms");
        self.autoquit_at = Some(std::time::Instant::now() + std::time::Duration::from_millis(ms));
    }
    /// `KASATERM_AUTOCURSOR="x,y"` (+ `_MS`) — 커서를 그 논리 좌표에 놓는다.
    ///
    /// hover 는 정적 캡처로 볼 방법이 없다. 들림도 손가락 커서도 커서가 그 위에
    /// 있을 때만 생기는데, 헤드리스는 마우스를 못 움직여 "hover 를 넣었다" 는
    /// 주장이 눈으로 확인되지 않은 채 남는다. 캡처 직전에 커서만 옮겨 두면
    /// 그 프레임이 곧 hover 스크린샷이 된다.
    pub(crate) fn run_pending_autocursor(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<(Instant, f32, f32)>> = OnceLock::new();
        static MOVED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let spec = std::env::var("KASATERM_AUTOCURSOR").ok()?;
            let (xs, ys) = spec.split_once(',')?;
            let ms: u64 = std::env::var("KASATERM_AUTOCURSOR_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(6000);
            Some((
                Instant::now() + std::time::Duration::from_millis(ms),
                xs.trim().parse().ok()?,
                ys.trim().parse().ok()?,
            ))
        });
        let Some((due, x, y)) = *due else { return };
        if Instant::now() < due || MOVED.swap(true, Ordering::Relaxed) {
            return;
        }
        self.cursor_px = (x, y);
        self.chrome_dirty = true;
        eprintln!("[autocursor] ({x:.0},{y:.0})");
    }
    /// `KASATERM_AUTONOTIFY_MS` — 그 시각에 데스크톱 알림을 한 발 쏜다.
    ///
    /// 실물 알림은 학생 턴 완료·승인 대기 같은 실제 사건에서만 나가, 「알림이
    /// 어느 경로(native/osascript)로 갔고 아이콘이 무엇으로 떴나」를 헤드리스로
    /// 재현할 길이 없었다(2026-08-17 「os알림 아직도 기본아이콘인데」 조사).
    /// 격리 인스턴스에서 이 훅으로 쏘고 stderr 의 `[notify]` 줄과 화면을 본다.
    pub(crate) fn run_pending_autonotify(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let ms: u64 = std::env::var("KASATERM_AUTONOTIFY_MS").ok()?.parse().ok()?;
            Some(Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(at) = *due else { return };
        if Instant::now() < at || FIRED.swap(true, Ordering::Relaxed) {
            return;
        }
        eprintln!("[autonotify] 데스크톱 알림 발사");
        crate::chrome::notify_desktop("아리스 프로브", "알림 아이콘 경로 확인", None, None, None);
    }
    /// `KASATERM_AUTOEXPANDCLICK="<방idx>"` (+ `_MS`) — 그 방의 **펼치기 버튼**을
    /// 진짜로 누른다. `"2:body"` 는 같은 카드의 이름줄, `"2:dots"` 는 버튼 바로
    /// 오른쪽 상태 점 자리 — 둘 다 방 전환으로 흘러야 하는 곳이다.
    ///
    /// 상태를 직접 세우는 `AUTOEXPAND` 와 갈리는 건 좌표 판정을 지난다는 점이다.
    /// 버튼과 전환이 한 카드 안에서 갈리므로, 정작 검증해야 할 것이 그 갈림
    /// 자체다 — 예전엔 클릭 쪽이 "아랫줄 오른쪽 100px" 라는 자기 공식을 갖고 있어
    /// 눈에 보이는 삼각형보다 훨씬 넓은 구역이 전환을 삼켰다.
    pub(crate) fn run_pending_autoexpandclick(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<(Instant, usize, u8)>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let spec = std::env::var("KASATERM_AUTOEXPANDCLICK").ok()?;
            let (idx, rest) = spec.split_once(':').unwrap_or((spec.as_str(), ""));
            let ms: u64 = std::env::var("KASATERM_AUTOEXPANDCLICK_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5000);
            Some((
                Instant::now() + std::time::Duration::from_millis(ms),
                idx.trim().parse().ok()?,
                match rest.trim() {
                    "body" => 1,
                    "dots" => 2,
                    _ => 0,
                },
            ))
        });
        let Some((due, idx, spot)) = *due else { return };
        if Instant::now() < due || FIRED.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(tab) = self.window_tab_rects.iter().find(|(i, _)| *i == idx).map(|(_, r)| *r)
        else {
            eprintln!("[autoexpandclick] 방 {idx} 없음");
            return;
        };
        let btn = self.window_expand_rect(idx, tab);
        let (x, y) = match (spot, btn) {
            (1, _) => (tab.0 + 40.0, tab.1 + 14.0),
            // 배지 **왼쪽** 여백 — 아랫줄에서 드래그를 시작할 수 있는 자리다.
            // 오른쪽은 배지가 카드 끝에 붙어 있어 카드 밖으로 나간다(실측 handled=false).
            (2, Some(r)) => (r.0 - 10.0, r.1 + r.3 / 2.0),
            (_, Some(r)) => (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0),
            _ => {
                eprintln!("[autoexpandclick] 방 {idx} 는 pane 이 하나라 버튼이 없음");
                return;
            }
        };
        let before = self.active_window;
        let handled = self.window_strip_click(x, y);
        // 드래그 장전 여부까지 찍는다 — 버튼을 카드에서 도려내는 변경은 그 자리의
        // tear-off 를 조용히 죽일 수 있고(빌드도 클릭도 멀쩡하다), 신호가 여기뿐이다.
        eprintln!(
            "[autoexpandclick] ({x:.0},{y:.0}) handled={handled} 활성 {before}->{} 펼침={:?} 드래그장전={}",
            self.active_window,
            self.expanded_windows,
            self.win_tab_drag.is_some()
        );
        // 펼침 모션 프레임은 **클릭 기준**으로 잡아야 한다. 시작 기준
        // `AUTOCAPTURE_MS` 로는 못 잡는다 — 이 클릭 자체가 이벤트 루프가 깨어날 때
        // 나가서, 예약보다 한참 늦게 발화한다(실측: 캡처가 먼저 찍혀 네 장 모두
        // 접힌 그림이 나왔다). `_CAP` 에 경로, `_CAP_MS` 에 클릭 후 ms 를 콤마로.
        //
        // ⚠️ 여러 장을 걸 때는 **간격을 readback 보다 넓게**. 캡처는 `capture_next`
        // 한 칸을 거쳐 다음 렌더에 찍히는데 그 전에 다음 만기가 오면 앞엣것을
        // 덮어써 파일이 조용히 빈다(실측: 30·70·120·300 중 1·4 번만 남았다).
        // 0.16초짜리 이 모션은 오프셋을 바꿔 가며 한 실행에 한 장이 확실하다.
        let Ok(path) = std::env::var("KASATERM_AUTOEXPANDCLICK_CAP") else { return };
        let offs = std::env::var("KASATERM_AUTOEXPANDCLICK_CAP_MS")
            .unwrap_or_else(|_| "40,90,140,260".into());
        let now = Instant::now();
        for (i, ms) in offs.split(',').filter_map(|s| s.trim().parse::<u64>().ok()).enumerate() {
            let p = match path.rsplit_once('.') {
                Some((stem, ext)) => format!("{stem}-{}.{ext}", i + 1),
                None => format!("{path}-{}", i + 1),
            };
            self.pending_capture.push((now + std::time::Duration::from_millis(ms), p));
        }
    }
    /// `KASATERM_AUTOROWDRAG="<src줄>:<dst줄>[:before]"` (+ `_MS`) — 사이드바
    /// 목록의 src 번째 줄을 잡아 dst 번째 줄 위(`before`)나 아래에 떨어뜨린다.
    ///
    /// 누르기는 진짜 클릭 판정(`window_strip_click`)을 지나고, 떨어질 자리는
    /// handler 와 같은 규칙(대상 줄의 위/아래 절반)으로 잡는다. 확인할 건 "옮겼다"가
    /// 아니라 **아무것도 잃지 않았나**다 — pane 이동은 트리에서 leaf 를 떼어 다른
    /// 트리에 붙이는 일이라, 어긋나면 캡처는 멀쩡한데 pane 하나가 조용히 사라진다.
    pub(crate) fn run_pending_autorowdrag(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<(Instant, usize, usize, bool)>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let spec = std::env::var("KASATERM_AUTOROWDRAG").ok()?;
            let mut it = spec.split(':');
            let src: usize = it.next()?.trim().parse().ok()?;
            let dst: usize = it.next()?.trim().parse().ok()?;
            let before = it.next().map(|s| s.trim() == "before").unwrap_or(false);
            let ms: u64 = std::env::var("KASATERM_AUTOROWDRAG_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5000);
            Some((Instant::now() + std::time::Duration::from_millis(ms), src, dst, before))
        });
        let Some((due, src, dst, before)) = *due else { return };
        if Instant::now() < due || FIRED.swap(true, Ordering::Relaxed) {
            return;
        }
        let rows = self.sidebar_row_rects.clone();
        let Some((_, sid, sr)) = rows.get(src) else {
            eprintln!("[autorowdrag] 줄 {src} 없음 (총 {})", rows.len());
            return;
        };
        // dst 가 줄 범위를 넘으면 **방 카드**에 떨어뜨린 것으로 본다(넘긴 만큼이 방
        // 인덱스) — pane 하나짜리 방은 목록에 줄이 없어 이 경로로만 닿는다.
        let did = match rows.get(dst) {
            Some((_, id, _)) => id.clone(),
            None => {
                let wi = dst - rows.len();
                match self.window_leaves(wi).into_iter().last() {
                    Some(id) => id,
                    None => {
                        eprintln!("[autorowdrag] 방 {wi} 가 비었음");
                        return;
                    }
                }
            }
        };
        let did = &did;
        let all = |s: &Self| -> Vec<String> {
            (0..s.windows.len()).flat_map(|i| s.window_leaves(i)).collect()
        };
        let before_leaves = all(self);
        self.window_strip_click(sr.0 + sr.2 / 2.0, sr.1 + sr.3 / 2.0);
        let armed = self.sidebar_row_drag.is_some();
        if let Some(d) = self.sidebar_row_drag.as_mut() {
            d.active = true;
            d.target = Some((did.clone(), before));
        }
        let zone = if before { crate::DropZone::Up } else { crate::DropZone::Down };
        let (sid, did) = (sid.clone(), did.clone());
        self.move_pane(&sid, &did, zone);
        self.sidebar_row_drag = None;
        self.render_frame();
        let after = all(self);
        eprintln!(
            "[autorowdrag] {sid} → {did} ({}) 장전={armed} leaves {}개→{}개 {:?}",
            if before { "위" } else { "아래" },
            before_leaves.len(),
            after.len(),
            after
        );
        eprintln!("[autorowdrag] 기대: 장전=true · leaves 수 그대로 · 모든 pane 살아 있음");
    }
    /// `KASATERM_AUTOTHEME="<키>"` (+ `_MS`) — 그 시각에 테마를 갈아 끼운다.
    ///
    /// 전환 디졸브는 0.4초짜리라 손으로는 중간을 못 잡는다. 바뀌는 시각을 못박아
    /// 두면 `AUTOCAPTURE_MS` 를 그 뒤 몇십 ms 에 붙여 원하는 진행도의 한 장을
    /// 정확히 찍을 수 있다(콤마로 여러 시각을 주면 연속 프레임).
    pub(crate) fn run_pending_autotheme(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<(Instant, String)>> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let key = std::env::var("KASATERM_AUTOTHEME").ok()?;
            let ms: u64 = std::env::var("KASATERM_AUTOTHEME_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3000);
            Some((Instant::now() + std::time::Duration::from_millis(ms), key))
        });
        let Some((due, key)) = due.as_ref() else { return };
        if Instant::now() < *due || DONE.swap(true, Ordering::Relaxed) {
            return;
        }
        self.begin_theme_fx();
        crate::theme::set_theme(key);
        self.repaint_all();
        eprintln!("[autotheme] → {key}");
    }
    pub(crate) fn schedule_autocapture(&mut self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOCAPTURE_MS") else { return; };
        // 콤마로 여러 시각 지정 가능("14000,14300") — 애니메이션처럼 시간에
        // 따라 그림이 바뀌는 기능을 프레임 비교로 검증할 때 쓴다.
        let deadlines: Vec<u64> = ms_str
            .split(',')
            .filter_map(|s| s.trim().parse::<u64>().ok())
            .collect();
        if deadlines.is_empty() {
            return;
        }
        let path = std::env::var("KASATERM_AUTOCAPTURE_PATH").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("kasaterm.png")
                .to_string_lossy()
                .into_owned()
        });
        // Optional git-panel demo before the capture: expand the first changed
        // file's inline diff ("diff") or open the commit modal ("modal").
        if let Ok(action) = std::env::var("KASATERM_AUTOGIT") {
            let gms: u64 = std::env::var("KASATERM_AUTOGIT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(deadlines[0].saturating_sub(1500));
            self.pending_autogit = Some((
                std::time::Instant::now() + std::time::Duration::from_millis(gms),
                action,
            ));
        }
        // GPU frame readback (gpu::render → save_rgba_png) needs no OS
        // screen-record permission, so it works headless on every platform —
        // replacing the old screencapture (macOS, permission-blocked) and
        // PrintWindow (Windows, can't grab the Vulkan/Metal surface) paths.
        let multi = deadlines.len() > 1;
        for (i, ms) in deadlines.into_iter().enumerate() {
            // 단일 캡처는 기존 경로 그대로, 다중이면 "-1", "-2" suffix.
            let p = if multi {
                match path.rsplit_once('.') {
                    Some((stem, ext)) => format!("{stem}-{}.{ext}", i + 1),
                    None => format!("{path}-{}", i + 1),
                }
            } else {
                path.clone()
            };
            eprintln!("[autocapture] in {ms}ms → {p} (gpu readback)");
            self.pending_capture.push((
                std::time::Instant::now() + std::time::Duration::from_millis(ms),
                p,
            ));
        }
    }
    /// Run a queued git-panel demo action (KASATERM_AUTOGIT) so headless capture
    /// can show the inline diff / commit modal without a real click.
    pub(crate) fn run_autogit(&mut self, action: &str) {
        // The demo actions assume the column is up; open it for headless capture.
        if !self.git.col_visible {
            self.toggle_git_col();
        }
        match action {
            "diff" => {
                let pick = self.git.col_data.lock().ok().and_then(|g| {
                    g.unstaged
                        .first()
                        .map(|(_, p)| (false, p.clone()))
                        .or_else(|| g.staged.first().map(|(_, p)| (true, p.clone())))
                });
                if let Some((staged, path)) = pick {
                    self.toggle_git_diff(staged, path);
                }
            }
            "modal" => self.open_commit_modal(),
            // 커밋 메시지 칸의 **커서 산수**를 화면으로 확인한다. 조작을 `lineedit`
            // 한 벌로 합친 뒤(2026-08-07) 캐럿이 한글 경계에서 어긋나지 않는지가
            // 눈으로 보여야 한다 — 단위테스트는 문자열만 보고 캐럿 픽셀은 못 본다.
            // 키는 `logical_key` 만 있으면 되므로 KeyEvent 를 짓지 않는다.
            "commitedit" => {
                use winit::keyboard::{Key, NamedKey};
                self.open_commit_modal();
                self.git.commit_focused = true;
                self.git.commit_msg.clear();
                self.git.commit_cursor = 0;
                self.git_commit_insert("한글커밋메시지");
                let (msg, cur) = (&mut self.git.commit_msg, &mut self.git.commit_cursor);
                // 맨 앞으로 갔다가 두 칸 오른쪽 → "한글" 뒤에 캐럿.
                crate::lineedit::key(msg, cur, &Key::Named(NamedKey::Home));
                crate::lineedit::key(msg, cur, &Key::Named(NamedKey::ArrowRight));
                crate::lineedit::key(msg, cur, &Key::Named(NamedKey::ArrowRight));
                crate::lineedit::key(msg, cur, &Key::Character("X".into()));
                eprintln!(
                    "[autogit] commitedit msg={:?} cursor={} (기대: '한글X커밋메시지' / 3)",
                    self.git.commit_msg, self.git.commit_cursor
                );
            }
            "menu" => self.git.commit_menu_open = true,
            "spin" => self.git.op = Some("Pushing"),
            "hover" => {
                // Park the cursor over the first file row so its action cluster
                // (open / discard / stage) renders for a headless capture.
                let gx = self.git_col_x();
                let gw = self.git_col_w();
                self.cursor_px = (gx + gw - 30.0, TITLE_HEIGHT + 150.0);
            }
            _ => {}
        }
    }
    pub(crate) fn schedule_autosend(&self) {
        let Ok(text) = std::env::var("KASATERM_AUTOSEND") else { return; };
        let ms: u64 = std::env::var("KASATERM_AUTOSEND_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000);
        eprintln!("[autosend] in {ms}ms: {text:?}");
        // Capture whichever backend is wired so we don't need access
        // to self inside the timer thread.
        let tmux = self.tmux.clone();
        // Autosend always targets the currently-focused pane. In tmux
        // mode we leave pane targeting to the daemon; in pty mode we
        // grab the active session here so the closure doesn't need
        // self access.
        let pty = self.active_pty().cloned();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let mut payload = text.clone();
            // Enter is CR on Windows' ConPTY — PowerShell reads a bare LF as
            // "line unfinished" and parks on its `>>` continuation prompt, so
            // the autosent command never runs. POSIX shells want LF.
            let eol = if cfg!(windows) { '\r' } else { '\n' };
            if !payload.ends_with(eol) {
                payload.push(eol);
            }
            if let Some(t) = tmux.as_ref() {
                let hex: String = payload
                    .bytes()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let _ = t.send_keys_hex(None, &hex);
            } else if let Some(p) = pty.as_ref() {
                let _ = p.send_bytes(payload.as_bytes());
            }
        });
    }
    /// `KASATERM_AUTOTURNSCROLL="<줄수>"` (+ `_MS`) — 그 시각에 활성 pane 을 그만큼
    /// **과거로** 민다(휠 위와 같은 경로: alacritty display_offset).
    ///
    /// 대화 턴 헤더는 스크롤백을 올려다볼 때만 뜨는데, claude pane 에서 휠은
    /// mouse-tracking 으로 전부 TUI 쪽에 넘어가 헤드리스로는 그 상태를 만들 길이
    /// 없었다. 줄 수는 **화면 높이의 배수가 아닌 값**을 주는 게 좋다 — 딱 떨어지면
    /// 경계에 걸치는 경우를 못 본다.
    pub(crate) fn schedule_autoturnscroll(&self) {
        let Ok(spec) = std::env::var("KASATERM_AUTOTURNSCROLL") else { return };
        let Ok(lines) = spec.parse::<i32>() else { return };
        let ms: u64 = std::env::var("KASATERM_AUTOTURNSCROLL_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4000);
        eprintln!("[turnscroll] in {ms}ms: {lines}줄 위로");
        let pty = self.active_pty().cloned();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let Some(p) = pty.as_ref() else {
                eprintln!("[turnscroll] 활성 pty 없음 — NO-OP");
                return;
            };
            let off = p.scroll(lines);
            let (_, hist) = p.view_state();
            eprintln!("[turnscroll] display_offset={off} history={hist}");
            for a in p.prompt_anchors() {
                eprintln!("[turnscroll] 앵커 abs={} {:?}", a.abs_line, a.text);
            }
        });
    }
    /// Headless confirm-modal repro: after `KASATERM_TEST_CONFIRM_MS` fire the
    /// window-close confirm path, so a background run can screenshot the modal
    /// (pair with AUTOSEND="sleep 300" to give a pane a real foreground job).
    pub(crate) fn arm_autoconfirm(&mut self) {
        let Ok(ms) = std::env::var("KASATERM_TEST_CONFIRM_MS") else { return };
        let Ok(ms) = ms.parse::<u64>() else { return };
        self.autoconfirm_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    pub(crate) fn run_pending_autoconfirm(&mut self) {
        let Some(due) = self.autoconfirm_at else { return };
        if Instant::now() < due {
            return;
        }
        self.autoconfirm_at = None;
        let raised = self.confirm_or_close_window();
        eprintln!("[autoconfirm] confirm_or_close_window -> raised={raised}");
    }
    /// Headless 학생 교체 확인 repro: `KASATERM_AUTOSWAPCONFIRM_MS` 뒤에 카드를
    /// 띄운다. `_TO=<학생>`(기본 은랑), `_FRESH=1` 이면 이어붙일 대화가 없는 판.
    ///
    /// 실제 경로(`ask_or_repersona`)를 태울 수 없는 이유는 계정 카드와 같다 —
    /// 헤드리스에는 claude 가 도는 pane 이 없어 늘 「묻지 않고 바로」로 갈린다.
    /// 버튼이 셋이라 카드 폭에 들어가는지를 눈으로 확인할 길이 이것뿐이다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_swap_confirm(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = *DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOSWAPCONFIRM_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if Instant::now() < due || FIRED.swap(true, Ordering::SeqCst) {
            return;
        }
        let to = std::env::var("KASATERM_AUTOSWAPCONFIRM_TO").unwrap_or_else(|_| "은랑".into());
        let resumable = std::env::var_os("KASATERM_AUTOSWAPCONFIRM_FRESH").is_none();
        eprintln!("[autoswapconfirm] to={to} resumable={resumable}");
        self.character_swap_confirm = Some(crate::session::PendingCharacterSwap {
            pane: "%0".to_string(),
            to,
            resumable,
            rects: Vec::new(),
        });
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Headless 계정 전환 확인 repro: `KASATERM_AUTOACCOUNTCONFIRM_MS` 뒤에 확인
    /// 카드를 띄운다. `_TO=<계정id>` 를 주면 **실제 경로**(`ask_or_switch_claude_account`)
    /// 를 태우고, 안 주면 가짜 영향으로 카드만 세운다 — 헤드리스에는 claude pane 이
    /// 없어 영향이 늘 0이라 실제 경로만으로는 카드를 볼 수 없다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_account_confirm(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = *DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOACCOUNTCONFIRM_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if Instant::now() < due || FIRED.swap(true, Ordering::SeqCst) {
            return;
        }
        // `_FLASH=1` 이면 카드 대신 반짝임만 켠다 — 헤드리스에는 전환할 계정이 없어
        // 실제 경로로는 그 효과를 볼 수 없다.
        if std::env::var_os("KASATERM_AUTOACCOUNTCONFIRM_FLASH").is_some() {
            self.account_flash = Some(Instant::now());
            eprintln!("[autoacctconfirm] flash");
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        if let Ok(to) = std::env::var("KASATERM_AUTOACCOUNTCONFIRM_TO") {
            self.ask_or_switch_claude_account(&to, crate::session::ConfirmSurface::Main);
            let shown = self.account_switch_confirm.is_some();
            eprintln!("[autoacctconfirm] real to={to} shown={shown}");
            return;
        }
        let impact = crate::session::AccountSwitchImpact {
            restart_when_quiet: 2,
            restart_after_turn: 1,
            chip_focused: 1,
            fresh: 1,
            ..Default::default()
        };
        eprintln!("[autoacctconfirm] fake torn_down={}", impact.torn_down());
        self.account_switch_confirm = Some(crate::session::PendingAccountSwitch {
            provider: crate::session::AccountSwitchProvider::Claude,
            nonce: "testkit".to_string(),
            to: "acct-1".to_string(),
            to_label: "지메일".to_string(),
            impact,
            surface: crate::session::ConfirmSurface::Main,
            rects: Vec::new(),
        });
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Headless 사이드바 창-닫기 repro: `KASATERM_AUTOWINCLOSE_MS` 뒤에 사이드바
    /// 창 탭의 ×를 **실제 hit-test 경로**(`window_strip_click`)로 누른다.
    ///
    /// 그 ×는 한 번 확인 모달을 잃은 적이 있다 — 사이드바 strip 과 상단 탭 strip 을
    /// 한 함수로 합치면서 `close_window` 를 직접 부르게 돼, 돌고 있는 claude 가
    /// 말없이 죽었다. `confirm_or_close_session` 을 직접 부르는 테스트로는 그 회귀를
    /// 못 잡으므로(끊긴 건 라우팅이지 모달이 아니다) 좌표 클릭으로 재현한다.
    /// AUTOSEND="sleep 300" 과 같이 써서 pane 에 진짜 작업을 물려둘 것.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autowinclose(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOWINCLOSE_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        // close rect 는 ①사이드바가 보이고 ②창이 2개 이상일 때만 그려진다
        // (sidebar_layout 의 `n > 1` — 마지막 창은 닫을 수 없으니 ×가 없다).
        // 좌표는 렌더가 채우므로 조건을 맞춘 뒤 한 프레임 그린다.
        if !self.sidebar_visible {
            self.toggle_sidebar();
        }
        if self.windows.len() < 2 {
            self.new_window();
        }
        self.render_frame();
        let Some((_, r)) = self.window_tab_close_rects.first().copied() else {
            eprintln!("[autowinclose] close rect 없음 — 사이드바 창 탭이 안 그려졌다");
            return;
        };
        let handled = self.window_strip_click(r.0 + r.2 * 0.5, r.1 + r.3 * 0.5);
        eprintln!(
            "[autowinclose] handled={handled} confirm_raised={} why={:?}",
            self.confirm_close.is_some(),
            self.confirm_close.as_ref().map(|c| match &c.why {
                CloseWhy::Busy(p) => format!("busy:{p}"),
                CloseWhy::Dirty(d) =>
                    format!("dirty:{}", d.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(",")),
                CloseWhy::LastPane => "lastpane".to_string(),
            }),
        );
    }
    /// Headless 회귀: **split 으로 만든 pane 에서 바쁜 프로세스가 도는데 닫으면
    /// 확인이 뜨는가.** `KASATERM_AUTOBUSYCLOSE_MS` 뒤에 split → `sleep` 실행 →
    /// 2초 뒤 그 pane 닫기를 시도하고 `confirm_raised` 를 찍는다.
    ///
    /// 이 자리가 하네스 없이는 **안 보인다**: 조용히 닫히는 것과 정상 동작이
    /// 화면상 구분이 안 되고, 잃는 것은 도는 claude 세션이라 알아챘을 땐 이미
    /// 닫힌 뒤다(2026-08-18 "pane닫기도 클로드켜져있는데 그냥닫혀버려").
    ///
    /// ⚠️ split 으로 만든 pane 이어야 한다 — 그게 `ws.panes` 에 없는 경로이고
    /// 버그가 살던 자리다. 이미 출력이 있는 pane 으로 재면 통과해 버린다.
    pub(crate) fn run_pending_autobusyclose(&mut self) {
        use std::sync::atomic::{AtomicU8, Ordering};
        static STAGE: AtomicU8 = AtomicU8::new(0);
        static DUE: std::sync::OnceLock<Option<Instant>> = std::sync::OnceLock::new();
        static CLOSE_AT: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOBUSYCLOSE_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        let now = Instant::now();
        match STAGE.load(Ordering::Relaxed) {
            0 => {
                if now < *due {
                    return;
                }
                STAGE.store(1, Ordering::Relaxed);
                let target = match self.split_active_pane(kasa_pty::SplitDir::Horizontal) {
                    Ok(id) => id,
                    Err(e) => {
                        eprintln!("[autobusyclose] split 실패: {e}");
                        STAGE.store(9, Ordering::Relaxed);
                        return;
                    }
                };
                // 셸이 아닌 전경 프로세스 — `is_shell_name` 이 걸러내지 않는 것이면
                // 무엇이든 된다. 프로세스 테이블에 뜨기까지 시간이 걸리므로 2초 준다.
                // split 직후 새 pane 이 활성이라 `send_bytes` 가 그리로 간다.
                self.send_bytes(b"sleep 300\n");
                *CLOSE_AT.lock().unwrap() =
                    Some(now + std::time::Duration::from_millis(2000));
                eprintln!("[autobusyclose] split={target} 에 sleep 실행, 2초 뒤 닫기");
            }
            1 => {
                let at = *CLOSE_AT.lock().unwrap();
                let Some(at) = at else { return };
                if now < at {
                    return;
                }
                STAGE.store(2, Ordering::Relaxed);
                let target = self.ws.lock().unwrap().active_pane.clone();
                let Some(target) = target else { return };
                // 이 pane 이 `ws.panes` 에 있는지 함께 찍는다 — 없는 상태로 통과해야
                // 진짜 회귀 검사다(있으면 옛 코드도 통과한다).
                let in_panes = self.ws.lock().unwrap().panes.contains_key(&target);
                let busy = self.pid_busy(&target);
                // `KASATERM_AUTOBUSYCLOSE_VIA=pane` 이면 ⋮ 메뉴의 ×(pane 통째)를,
                // 아니면 ⌘W(탭 단위)를 태운다. 두 경로가 갈라져 있어서 한쪽만
                // 재면 다른 쪽 구멍이 안 보인다 — 실제로 ⌘W 는 물어보는데 ⋮ × 만
                // 조용히 닫히던 것이 2026-08-20 지적이었다.
                let via_pane =
                    std::env::var("KASATERM_AUTOBUSYCLOSE_VIA").as_deref() == Ok("pane");
                if via_pane {
                    self.confirm_or_close_pane(&target);
                } else {
                    self.confirm_or_close_tab(&target, 0);
                }
                eprintln!(
                    "[autobusyclose] via={} pane={target} in_ws_panes={in_panes} busy={busy:?} confirm_raised={}",
                    if via_pane { "pane" } else { "tab" },
                    self.confirm_close.is_some()
                );
            }
            _ => {}
        }
    }

    /// Headless Cmd+W repro: `KASATERM_AUTOLASTCLOSE_MS` 뒤에 방을 둘로 만들고
    /// **pane 이 하나인 상태에서** `close_active_tab`(Cmd+W 가 부르는 그 함수)을 친다.
    ///
    /// 이 경로는 아무 일도 안 하던 자리다 — 마지막 pane 이면 `confirm_or_close_tab`
    /// 이 조용히 return 해서 키가 죽은 것처럼 보였다(거노). 확인 모달이 뜨는지를
    /// 로그로 못박는다. 모달만 띄우고 실제로 닫지는 않으므로 캡처도 그대로 남는다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autolastclose(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOLASTCLOSE_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        if self.windows.len() < 2 {
            self.new_window();
        }
        let leaves = self.pty_layout.as_ref().map_or(0, |t| t.leaves().len());
        // 갈림길이 셋(탭 여러 개 / 마지막 pane / active_pane 없음)이라 어디로 갔는지
        // 못 보면 "안 뜬다"까지만 알고 왜인지는 모른다.
        let (active, tabs) = {
            let ws = self.ws.lock().unwrap();
            let a = ws.active_pane.clone();
            let t = a.as_ref().and_then(|id| ws.panes.get(id)).map(|p| p.tabs.len());
            (a, t)
        };
        self.close_active_tab();
        eprintln!(
            "[autolastclose] active={active:?} tabs={tabs:?} windows={} leaves={leaves} confirm_raised={} why={:?} action={:?}",
            self.windows.len(),
            self.confirm_close.is_some(),
            self.confirm_close.as_ref().map(|c| match &c.why {
                CloseWhy::Busy(p) => format!("busy:{p}"),
                CloseWhy::Dirty(_) => "dirty".to_string(),
                CloseWhy::LastPane => "lastpane".to_string(),
            }),
            self.confirm_close.as_ref().map(|c| match &c.action {
                crate::PendingClose::Session(i) => format!("session:{i}"),
                crate::PendingClose::Window => "window".to_string(),
                crate::PendingClose::Pane { pane } => format!("pane:{pane}"),
                _ => "other".to_string(),
            }),
        );
    }
    /// Headless 방 재배치 repro: `KASATERM_AUTOWINREORDER_MS` 뒤에 방 셋을 만들어
    /// 이름·알림을 심고, 가운데 탭을 **실제 hit-test 경로**(`window_strip_click`)로
    /// 잡아 맨 뒤로 끌어 놓는다.
    ///
    /// 여기서 깨지기 쉬운 건 순서가 아니라 신원이다 — 활성 방의 트리는 슬롯이 아니라
    /// `pty_layout` 에 얹혀 있고, 이름·알림은 인덱스가 키다. 그래서 옮기기 전후로 각
    /// 방의 leaf 수와 이름을 같이 찍는다. leaf 가 0 이면 그 방의 내용이 증발한
    /// 것이고, 이름이 어긋나면 남의 이름을 단 것이다 — 캡처로는 둘 다 멀쩡해 보인다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autowinreorder(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOWINREORDER_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let dump = |app: &App, tag: &str| {
            let rows: Vec<String> = (0..app.windows.len())
                .map(|i| {
                    let layout = if i == app.active_window {
                        app.pty_layout.as_ref()
                    } else {
                        app.windows[i].as_ref()
                    };
                    format!(
                        "{i}{}:{} leaves={}{}",
                        if i == app.active_window { "*" } else { "" },
                        app.window_name_override
                            .get(&i)
                            .map(|s| s.as_str())
                            .unwrap_or("-"),
                        layout.map(|l| l.leaves().len()).unwrap_or(0),
                        if app.window_alert.contains(&i) {
                            " alert"
                        } else {
                            ""
                        },
                    )
                })
                .collect();
            eprintln!("[autowinreorder] {tag}: {}", rows.join(" | "));
        };
        if !self.sidebar_visible {
            self.toggle_sidebar();
        }
        while self.windows.len() < 3 {
            self.new_window();
        }
        for (i, name) in ["A", "B", "C"].iter().enumerate() {
            self.window_name_override.insert(i, name.to_string());
        }
        // 알림은 안 잡을 방(2번)에 건다 — 잡은 방은 press 가 전환하면서 지운다.
        self.window_alert.insert(2);
        self.window_labels_at = None;
        self.render_frame();
        dump(self, "before");
        let Some((_, r)) = self.window_tab_rects.get(1).copied() else {
            eprintln!("[autowinreorder] 탭 rect 없음 — 사이드바 창 탭이 안 그려졌다");
            return;
        };
        let handled = self.window_strip_click(r.0 + r.2 * 0.5, r.1 + r.3 * 0.5);
        let armed = self.win_tab_drag.as_ref().map(|d| d.from);
        // 마지막 탭 아래까지 끌었다고 친다(문턱 통과 + 삽입 슬롯 = 방 개수).
        let end = self.windows.len();
        if let Some(d) = self.win_tab_drag.as_mut() {
            d.active = true;
            d.target = end;
        }
        // HOLD 면 놓지 않고 잡은 채로 둔다 — `AUTOCAPTURE_MS` 를 뒤에 붙여 삽입선이
        // 그려지는 프레임을 잡기 위한 것(놓고 나면 선은 사라져 캡처할 수 없다).
        if std::env::var("KASATERM_AUTOWINREORDER_HOLD").is_ok() {
            self.chrome_dirty = true;
            eprintln!("[autowinreorder] hold — 잡은 채로 유지, 삽입선 프레임 대기");
            return;
        }
        if let Some(d) = self.win_tab_drag.take() {
            self.reorder_window(d.from, d.target);
        }
        self.refresh_window_labels();
        eprintln!("[autowinreorder] press handled={handled} armed_from={armed:?} target={end}");
        dump(self, "after ");
        eprintln!(
            "[autowinreorder] 기대: A,C,B / 잡은 B 가 활성인 채 맨 뒤 / alert 는 C 를 따라 1번 / 모든 leaves>0"
        );
    }
    /// Headless 내부 방 재배치 repro: `KASATERM_AUTOINTERNALREORDER_MS` 뒤에 사용자
    /// 방을 둘 만들고 설정 방을 연 다음, 사이드바에서 **설정 탭을 실제 hit-test
    /// 경로**(`window_strip_click`)로 잡아 맨 앞으로 끌어 놓는다.
    ///
    /// 내부 방은 셸도 저장 기록도 없어 눈으로는 그냥 탭 하나로 보이지만, 활성 방의
    /// 트리는 슬롯이 아니라 `pty_layout` 에 얹혀 있다. 그래서 옮기기 전후로 각 방이
    /// 무슨 방인지와 leaf 수를 같이 찍는다 — leaf 가 0 이면 그 방의 내용이 증발한
    /// 것이고, 자리가 그대로면 재배치가 조용히 무시된 것이다(옛 가드의 증상).
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autointernalreorder(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOINTERNALREORDER_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let dump = |app: &App, tag: &str| {
            let rows: Vec<String> = (0..app.windows.len())
                .map(|i| {
                    let layout = if i == app.active_window {
                        app.pty_layout.as_ref()
                    } else {
                        app.windows[i].as_ref()
                    };
                    let kind = app
                        .internal_room_kind_at(i)
                        .map(crate::internal_room::InternalRoomKind::label)
                        .unwrap_or("사용자");
                    format!(
                        "{i}{}:{kind} leaves={}",
                        if i == app.active_window { "*" } else { "" },
                        layout.map(|l| l.leaves().len()).unwrap_or(0),
                    )
                })
                .collect();
            eprintln!("[autointernalreorder] {tag}: {}", rows.join(" | "));
        };
        if !self.sidebar_visible {
            self.toggle_sidebar();
        }
        while self.windows.len() < 2 {
            self.new_window();
        }
        if !self.open_settings_room(None) {
            eprintln!("[autointernalreorder] 설정 방을 못 열었다");
            return;
        }
        self.window_labels_at = None;
        self.render_frame();
        dump(self, "before");
        let Some(from) = self.settings_room_index() else {
            eprintln!("[autointernalreorder] 설정 방 인덱스가 없다");
            return;
        };
        let Some((_, r)) = self
            .window_tab_rects
            .iter()
            .find(|(idx, _)| *idx == from)
            .copied()
        else {
            eprintln!("[autointernalreorder] 설정 탭 rect 없음 — 사이드바에 안 그려졌다");
            return;
        };
        let handled = self.window_strip_click(r.0 + r.2 * 0.5, r.1 + r.3 * 0.5);
        // 맨 앞 슬롯으로 끌어 놓는다(문턱 통과 + 삽입 슬롯 0).
        if let Some(d) = self.win_tab_drag.as_mut() {
            d.active = true;
            d.target = 0;
        }
        if let Some(d) = self.win_tab_drag.take() {
            self.reorder_window(d.from, d.target);
        }
        self.refresh_window_labels();
        eprintln!("[autointernalreorder] press handled={handled} from={from} target=0");
        dump(self, "after ");
        eprintln!("[autointernalreorder] 기대: 설정이 0 번에 활성인 채로 · 모든 leaves>0");
    }

    /// Headless 방 이름 편집 repro: `KASATERM_AUTOROOMRENAME_MS` 뒤에 방 탭을 **실제
    /// hit-test 경로**(`window_strip_click`)로 두 번 눌러 편집에 들어가고, 글자를 넣은
    /// 직후의 라벨을 찍는다.
    ///
    /// 여기서 봐야 할 건 두 가지다. ①편집에 들어간 순간의 버퍼 — 빈칸이면 사람이
    /// 이름을 통째로 다시 쳐야 한다. ②글자를 넣은 **그 프레임**의 라벨 — 라벨은
    /// `refresh_window_labels` 의 1초 캐시를 타므로, 합성이 캐시 안에 있으면 타이핑이
    /// 1초씩 뭉쳐 나온다. 그래서 캐시를 일부러 fresh 로 만들어 둔 채 확인한다.
    /// (한글 조합은 OS 키 경로라 여기서 재현 못 한다 — 사람이 직접 쳐야 한다.)
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autoroomrename(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOROOMRENAME_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        if !self.sidebar_visible {
            self.toggle_sidebar();
        }
        self.render_frame();
        let Some((_, r)) = self.window_tab_rects.first().copied() else {
            eprintln!("[autoroomrename] 탭 rect 없음 — 사이드바 창 탭이 안 그려졌다");
            return;
        };
        let (cx, cy) = (r.0 + r.2 * 0.5, r.1 + r.3 * 0.5);
        let before = self.window_labels.first().map(|(n, _)| n.clone()).unwrap_or_default();
        // 첫 클릭은 전환·장전만. 두 번째가 편집을 연다(Finder 의 느린 재클릭).
        self.window_strip_click(cx, cy);
        std::thread::sleep(std::time::Duration::from_millis(
            crate::chrome::ROOM_RENAME_DOUBLE_CLICK_MS as u64 + 60,
        ));
        self.window_strip_click(cx, cy);
        let opened = self.room_rename.editing.clone();
        eprintln!("[autoroomrename] 라벨={before:?} 편집진입={opened:?}");
        // 라벨 캐시를 일부러 갓 만든 상태로 둔다 — 캐시가 살아 있는데도 방금 넣은
        // 글자가 라벨에 보여야 통과다.
        self.refresh_window_labels();
        self.room_rename_insert("X");
        self.refresh_window_labels();
        let after = self.window_labels.first().map(|(n, _)| n.clone()).unwrap_or_default();
        eprintln!("[autoroomrename] 한 글자 넣은 뒤 라벨={after:?}");
        // 커서를 앞으로 옮겨 가운데에 넣는다 — 캐럿이 늘 끝에 붙던 시절엔 여기서
        // 글자와 캐럿 자리가 갈렸다.
        self.room_rename.cursor = 0;
        self.room_rename_insert("A");
        self.refresh_window_labels();
        let mid = self.window_labels.first().map(|(n, _)| n.clone()).unwrap_or_default();
        eprintln!("[autoroomrename] 맨 앞에 넣은 뒤 라벨={mid:?} 커서={}", self.room_rename.cursor);
        eprintln!(
            "[autoroomrename] 기대: 편집진입 버퍼 = 원래 라벨 / 'X▌' / 맨 앞 삽입은 'A▌' 뒤에 원래 이름"
        );
        self.render_frame();
    }

    /// Headless 파일트리 이름변경 repro: `KASATERM_AUTOFTRENAME_MS` 뒤에 트리를 켜고
    /// 첫 파일의 인라인 이름변경을 열어, 커서가 어디에 서는지와 가운데 삽입이 그
    /// 자리에 들어가는지를 찍는다.
    ///
    /// 봐야 할 건 둘이다. ①편집을 연 순간의 커서 — 0 이면 이름 맨 앞에 서서, 확장자
    /// 하나 고치려는 사람이 커서를 끝까지 몰고 가야 한다. ②커서를 옮긴 뒤의 삽입
    /// 자리 — 끝에만 붙던 시절엔 여기서 글자가 엉뚱한 데로 갔다. 캐럿이 그 자리에
    /// 그려지는지는 같은 프레임의 캡처로 눈으로 본다.
    /// (한글 조합은 OS 키 경로라 여기서 재현 못 한다 — 사람이 직접 쳐야 한다.)
    pub(crate) fn run_pending_autoftrename(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOFTRENAME_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        if !self.file_tree.visible {
            self.toggle_file_tree();
        }
        self.refresh_file_tree();
        self.render_frame();
        // 같은 칼럼의 검색칸도 한 화면에 담아 둔다 — 커서를 옮겨 넣은 글자가
        // 제자리에 그려지는지 본다(이 칸의 캐럿은 rename 이 열리며 꺼진다).
        self.file_tree_search_insert("sc");
        self.file_tree.search_cursor = 0;
        self.file_tree_search_insert("A");
        eprintln!(
            "[autoftrename] 검색칸={:?} 커서={} (기대: \"Asc\" / 1)",
            self.file_tree.search_query, self.file_tree.search_cursor
        );
        let Some(target) = self.file_tree.nodes.iter().find(|n| !n.is_dir).map(|n| n.path.clone())
        else {
            eprintln!("[autoftrename] 트리에 파일이 없다 — cwd 를 확인할 것");
            return;
        };
        self.file_tree.selected = Some(target.clone());
        self.run_ft_menu_action(crate::FtMenuAction::Rename);
        let opened = self.file_tree.rename.clone();
        eprintln!("[autoftrename] 대상={target:?} 편집진입={opened:?} 커서={}", self.file_tree.edit_cursor);
        // 커서를 이름 한가운데로 옮겨 넣어 본다 — 끝에만 붙던 시절의 회귀 감시.
        let mid = self.file_tree.rename.as_ref().map_or(0, |(_, n)| n.chars().count() / 2);
        self.file_tree.edit_cursor = mid;
        self.ft_edit_insert("Z");
        eprintln!(
            "[autoftrename] {mid} 번째에 넣은 뒤 버퍼={:?} 커서={}",
            self.file_tree.rename.as_ref().map(|(_, n)| n.clone()),
            self.file_tree.edit_cursor
        );
        eprintln!("[autoftrename] 기대: 진입 커서 = 이름 길이 / 'Z' 가 이름 한가운데 / 캐럿이 그 뒤");
        self.render_frame();
    }

    /// Headless 경로 검색칸 repro: `KASATERM_AUTOPATHSEARCH_MS` 뒤에 활성 pane 의 경로
    /// 드롭다운을 열고, 검색어를 친 뒤 커서를 앞으로 옮겨 한 글자를 더 넣는다.
    ///
    /// 이 칸엔 원래 캐럿이 없었다 — 끝에만 붙는 칸이었기 때문이다. 그래서 여기서는
    /// 커서가 가운데 있을 때 **캐럿이 그 자리에 서는지**를 캡처로 본다.
    pub(crate) fn run_pending_autopathsearch(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOPATHSEARCH_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let Some(id) = self.ws.lock().unwrap().active_pane.clone() else {
            eprintln!("[autopathsearch] 활성 pane 이 없다");
            return;
        };
        // 드롭다운은 footer 의 경로 칩에 앵커링된다 — 바를 안 켜면 칩 rect 가 없어
        // 메뉴가 통째로 안 그려진다(첫 시도에서 빈 화면만 찍혔다).
        self.set_footer_default = true;
        self.render_frame();
        self.open_statusbar_menu(&id, crate::StatusbarMenu::Path);
        self.statusbar_search_insert("sc");
        // 커서를 맨 앞으로 몰고 한 글자 — 끝에만 붙던 시절엔 "sc" 뒤에 붙었다.
        self.statusbar.menu_search_cursor = 0;
        self.statusbar_search_insert("A");
        eprintln!(
            "[autopathsearch] 검색어={:?} 커서={} (기대: \"Asc\" / 1, 캐럿은 A 뒤)",
            self.statusbar.menu_search, self.statusbar.menu_search_cursor
        );
        self.render_frame();
    }

    /// Headless 닫기→되살리기 repro: `KASATERM_AUTOCLOSEREOPEN_MS` 뒤에 pane 을 쪼갠 뒤
    /// 하나를 닫고, 되살리기 스택에 남았는지 찍고, 다시 되살린다.
    ///
    /// "새 셸이 하나 뜬다"로는 되살렸는지 알 수 없다 — 원래 자리·cwd·대화를 되찾아야
    /// 되살린 것이다. 그래서 닫기 전후 leaf 목록과 스택 내용(어느 pane·어느 폴더)을 같이
    /// 찍는다. `_HOLD=1` 이면 되살리지 않고 멈춘다(인포의 대기 줄을 캡처하기 위한 것).
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autoclosereopen(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOCLOSEREOPEN_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let leaves = |app: &App| -> Vec<String> {
            app.pty_layout
                .as_ref()
                .map(|l| l.leaves().iter().map(|s| s.to_string()).collect())
                .unwrap_or_default()
        };
        let stack = |app: &App| -> Vec<String> {
            app.closed_panes
                .iter()
                .map(|c| {
                    format!(
                        "{}({}{})",
                        c.pane_id,
                        c.folder,
                        if c.alive { ",살아있음" } else { ",죽음" }
                    )
                })
                .collect()
        };
        let _ = self.split_active_pane(kasa_pty::SplitDir::Horizontal);
        self.render_frame();
        let before = leaves(self);
        let Some(victim) = before.last().cloned() else { return };
        eprintln!("[autoclosereopen] before: leaves={before:?}");
        self.close_pane(&victim);
        self.render_frame();
        eprintln!(
            "[autoclosereopen] {victim} 닫음 → leaves={:?} 스택={:?} PTY생존={}",
            leaves(self),
            stack(self),
            self.pty.contains_key(&victim)
        );
        if std::env::var("KASATERM_AUTOCLOSEREOPEN_HOLD").is_ok() {
            // 되살리기는 Info 섹션이 맡으므로 hold 는 그 화면에서 멈춘다 — 하단바가
            // 0 이라는 것만 찍고 끝내면 "되살릴 길이 사라진 것"과 구분이 안 된다.
            if !self.git.col_visible {
                self.toggle_git_col();
            }
            self.info.tab = crate::state::SideTab::Info;
            self.render_frame();
            eprintln!(
                "[autoclosereopen] hold — 하단바 칩={:?} 예약={} · Info 되살리기 줄={:?}",
                self.dock_chip_rects,
                self.bottom_reserve_h(),
                self.info.closed_rects
            );
            return;
        }
        self.reopen_closed_pane();
        self.render_frame();
        let after = leaves(self);
        eprintln!(
            "[autoclosereopen] 되살린 뒤: leaves={:?} 스택={:?} 같은id복귀={}",
            after,
            stack(self),
            after.iter().any(|l| *l == victim)
        );
        // × 경로 — 다시 닫고 이번엔 되살리는 대신 끈다. 여기서만 프로세스가 죽어야
        // 한다. "끄기전=true, 끈뒤=false" 가 아니면 × 가 목록만 지우고 셸을 남긴
        // 것이고, 그건 닫을수록 프로세스가 쌓인다는 뜻이다.
        self.close_pane(&victim);
        self.render_frame();
        let before_kill = self.pty.contains_key(&victim);
        let last = self.closed_panes.len().saturating_sub(1);
        self.discard_closed_pane_at(last);
        self.render_frame();
        eprintln!(
            "[autoclosereopen] × 로 끔 → 끄기전PTY={before_kill} 끈뒤PTY={} 스택={:?}",
            self.pty.contains_key(&victim),
            stack(self)
        );
        eprintln!(
            "[autoclosereopen] 기대: 닫아도 PTY생존=true(죽이지 않는다) · 되살리면 같은id복귀=true(새로 띄우는 게 아니라 다시 붙인다) · × 만 끄기전true→끈뒤false"
        );
    }

    /// Headless 미리보기 탭 닫기→되살리기 repro: `KASATERM_AUTOPREVIEWREOPEN` 에
    /// 파일 경로를 주면 `_MS`(기본 4000) 뒤에 그 파일을 활성 pane 의 보조 탭으로
    /// 열고, 닫고, 닫힘 스택을 찍고, ⌘⇧T 경로(`reopen_closed_pane`)로 되살린다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autopreviewreopen(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<(Instant, std::path::PathBuf)>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let path = std::env::var("KASATERM_AUTOPREVIEWREOPEN").ok()?;
            let ms = std::env::var("KASATERM_AUTOPREVIEWREOPEN_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(4000);
            Some((
                Instant::now() + std::time::Duration::from_millis(ms),
                std::path::PathBuf::from(path),
            ))
        });
        let Some((due, path)) = due.clone() else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        // (pane id, 탭 인덱스, 그 pane 의 활성 탭) — 미리보기 탭이 어디 붙었는지.
        let find = |app: &App| -> Option<(String, usize, usize)> {
            let ws = app.ws.lock().unwrap();
            ws.panes.iter().find_map(|(id, p)| {
                p.tabs
                    .iter()
                    .position(|t| t.preview_path.as_deref() == Some(path.as_path()))
                    .map(|i| (id.clone(), i, p.active_tab))
            })
        };
        let stack = |app: &App| -> Vec<String> {
            app.closed_panes
                .iter()
                .map(|c| {
                    format!(
                        "{}({})",
                        c.pane_id,
                        if c.preview.is_some() { "미리보기" } else { "pane" }
                    )
                })
                .collect()
        };
        let active = self.ws.lock().unwrap().active_pane.clone();
        self.open_file(path.clone(), active, true);
        self.render_frame();
        let Some((outer, idx, _)) = find(self) else {
            eprintln!("[autopreviewreopen] 실패 — 미리보기 탭이 안 생겼다");
            return;
        };
        eprintln!("[autopreviewreopen] 열림: pane={outer} 탭={idx}");
        self.close_tab(&outer, idx);
        self.render_frame();
        eprintln!(
            "[autopreviewreopen] 닫음 → 탭남음={:?} 스택={:?}",
            find(self),
            stack(self)
        );
        self.reopen_closed_pane();
        self.render_frame();
        let back = find(self);
        let fronted = back.as_ref().is_some_and(|(id, i, at)| {
            i == at && self.ws.lock().unwrap().active_pane.as_deref() == Some(id.as_str())
        });
        eprintln!(
            "[autopreviewreopen] 되살린 뒤: 탭={back:?} 앞탭됨={fronted} 스택={:?}",
            stack(self)
        );
        eprintln!(
            "[autopreviewreopen] 기대: 닫으면 스택에 (미리보기) 항목 · 되살리면 같은 파일 탭이 다시 생기고 앞탭됨=true · 스택 비움"
        );
    }

    /// Headless 유령 pane repro: `KASATERM_AUTOGHOST_MS` 뒤에, 숨긴 pane 의 셸이
    /// 스스로 끝난 상황을 만들어 **낡은 되살리기 레코드가 산 pane 을 죽이는지**와
    /// **자원 없는 leaf 가 검은 사각형으로 남는지**를 잰다.
    ///
    /// 2026-08-24 에 거노가 두 번 목격한 사고다. pane 을 숨기면 레코드가 `alive`
    /// 로 스택에 남는데, 그 셸이 그 뒤에 죽으면 플래그가 낡는다. 그때 같은 번호를
    /// 새 pane 이 물려받으면 레코드 정리(개수 상한·15분 idle·인포의 ×)가 **남의
    /// 살아 있는 셸**을 끄고, 트리는 안 걷어 클릭도 안 되는 빈 칸이 남았다.
    ///
    /// 화면으로는 영영 안 보이는 종류라 세 관문을 로그로 못 박는다:
    ///   ① 새 pane 이 낡은 레코드의 번호를 물려받지 않는다(`used_pane_ids`)
    ///   ② 낡은 레코드를 정리해도 트리에 있는 pane 은 안 죽는다(`kill_hidden_pane`)
    ///   ③ 그래도 자원이 놓인 leaf 가 생기면 그 자리는 즉시 접힌다
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autoghost(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOGHOST_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let leaves = |app: &App| -> Vec<String> {
            app.pty_layout
                .as_ref()
                .map(|l| l.leaves().iter().map(|s| s.to_string()).collect())
                .unwrap_or_default()
        };
        let _ = self.split_active_pane(kasa_pty::SplitDir::Horizontal);
        self.render_frame();
        let Some(victim) = leaves(self).last().cloned() else {
            eprintln!("[autoghost] split 실패 — 잴 것이 없다");
            return;
        };
        // ① 숨긴다 → 레코드가 `alive` 로 스택에 남는다.
        self.hide_pane(&victim);
        self.render_frame();
        let hidden_ok = self
            .closed_panes
            .iter()
            .any(|c| c.pane_id == victim && c.alive);
        // 그 셸이 스스로 끝난다 — 숨긴 pane 의 EOF 는 `reap_dead_panes` 를 거쳐
        // 자원을 놓으므로(PTY 도 그리드도), 실제 사고와 같은 자리를 만들려면 그
        // 한 줄을 그대로 쓴다. 레코드의 `alive` 는 여기서 낡는다.
        self.drop_pane_resources(&victim);
        self.render_frame();
        // ② 새 pane 이 그 번호를 물려받으면 안 된다.
        let _ = self.split_active_pane(kasa_pty::SplitDir::Vertical);
        self.render_frame();
        let fresh = leaves(self).last().cloned().unwrap_or_default();
        let reused = fresh == victim;
        // ③ 낡은 레코드를 정리해도 트리에 있는 pane 은 안 죽는다. 번호가 갈렸으니
        // 이미 무해하지만, 가드가 살아 있는지는 **일부러 그 번호를 겨눠** 잰다.
        let live_before = self.pty.contains_key(&fresh);
        self.kill_hidden_pane(&fresh);
        let live_after = self.pty.contains_key(&fresh);
        // ④ 그래도 자원이 놓인 leaf 가 생기면 즉시 접혀야 한다. 트리에 있는 pane 의
        // 자원을 직접 놓아(사고와 같은 상태) 그 자리가 남는지 본다.
        self.drop_pane_resources(&fresh);
        self.render_frame();
        let ghost_left = leaves(self).iter().any(|l| *l == fresh);
        eprintln!(
            "[autoghost] 숨김레코드={hidden_ok} · 숨긴={victim} 새pane={fresh} 번호재사용={reused} \
             · 가드전PTY={live_before} 가드후PTY={live_after} · 유령leaf={ghost_left} \
             · leaves={:?}",
            leaves(self)
        );
        eprintln!(
            "[autoghost] 기대: 숨김레코드=true · 번호재사용=false · 가드전PTY=true 가드후PTY=true · 유령leaf=false"
        );
        let pass = hidden_ok && !reused && live_before && live_after && !ghost_left;
        eprintln!("[autoghost] {}", if pass { "PASS" } else { "FAIL" });
    }

    /// Headless 클릭 영역 감사: `KASATERM_AUTOHITAUDIT_MS` 뒤에 설정 화면의 모든
    /// 카테고리를 차례로 열어 **눌릴 수 없는 영역**을 센다.
    ///
    /// 「눌렀는데 아무 일도 안 난다」는 사람이 우연히 그 자리를 눌러야 발견되고,
    /// 발견해도 어느 버튼인지 말로 옮기기 어렵다(2026-09-05 「자잘한 버그는 내가
    /// 직접 써 보면서 계속 말해야 하나」). 그런데 그 원인 둘은 화면을 안 보고도
    /// 셀 수 있다:
    ///
    /// - **크기가 0 이하** — 어느 좌표로도 안 잡힌다.
    /// - **뒤에 등록된 영역에 완전히 덮임** — `hit_at` 은 `.rev()` 라 나중 것이
    ///   먼저 잡히므로, 완전히 덮인 앞 영역은 영영 차례가 안 온다.
    ///
    /// 겹침 자체는 정상이다(카드 위의 버튼). **완전히 덮여 자기 몫이 한 점도 안
    /// 남은 것**만 센다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autohitaudit(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOHITAUDIT_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let cats = [
            ("일반", SettingsCat::General),
            ("모양", SettingsCat::Appearance),
            ("셸", SettingsCat::Shell),
            ("에이전트", SettingsCat::Claude),
            ("테마", SettingsCat::Theme),
            ("캐릭터", SettingsCat::Students),
            ("피드백", SettingsCat::Feedback),
        ];
        let mut total_bad = 0usize;
        for (label, cat) in cats {
            if !self.open_settings_room(Some(cat)) {
                eprintln!("[hitaudit] {label}: 설정 방을 못 열었다");
                continue;
            }
            // 화면을 바꾼 **직후**에 무엇이 눌리는가. 클릭 영역은 그리면서 채우므로,
            // 첫 프레임 전에는 앞 화면의 영역이 그대로 남아 있다 — 그 사이에 누르면
            // 지금 화면에 없는 버튼이 눌린다(2026-09-05 「설정창 열릴 때 뭔가 안
            // 눌리는 것」의 후보).
            let stale = self.settings_scene.hits().len();
            // 첫 프레임까지 걸리는 시간. 이 틈이 사람의 두 번째 클릭보다 길면
            // 「열자마자 눌렀는데 안 먹었다」가 실제로 일어난다.
            let t0 = Instant::now();
            self.render_frame();
            let first_frame_ms = t0.elapsed().as_secs_f32() * 1000.0;
            let after_one = self.settings_scene.hits().len();
            for _ in 0..2 {
                self.render_frame();
            }
            let hits = self.settings_scene.hits();
            eprintln!(
                "[hitaudit] {label}: 열자마자 영역={stale} · 첫 프레임 {first_frame_ms:.0}ms 뒤={after_one} · 안정={}",
                hits.len()
            );
            let mut zero = Vec::new();
            let mut buried = Vec::new();
            for (i, hit) in hits.iter().enumerate() {
                let (x, y, w, h) = hit.rect;
                if w <= 0.0 || h <= 0.0 {
                    zero.push(format!("{:?}", hit.target));
                    continue;
                }
                // 나보다 **뒤에** 등록된 것 하나가 나를 통째로 덮으면 끝이다.
                let covered = hits[i + 1..].iter().any(|later| {
                    let (lx, ly, lw, lh) = later.rect;
                    lw > 0.0
                        && lh > 0.0
                        && lx <= x
                        && ly <= y
                        && lx + lw >= x + w
                        && ly + lh >= y + h
                });
                if covered {
                    buried.push(format!("{:?}", hit.target));
                }
            }
            // 여는 즉시 영역이 하나도 없으면, 그 사이 클릭은 통째로 사라진다.
            // 좌표 겹침이 아니라 타이밍이라 화면으로는 영영 안 보인다.
            let cold = stale == 0 && !hits.is_empty();
            if cold {
                eprintln!("[hitaudit] {label}: ⚠ 여는 순간 클릭 영역 0 — 그때 누르면 사라진다");
            }
            total_bad += zero.len() + buried.len() + usize::from(cold);
            eprintln!(
                "[hitaudit] {label}: 영역 {}개 · 크기0 {} · 완전히덮임 {}{}{}",
                hits.len(),
                zero.len(),
                buried.len(),
                if zero.is_empty() {
                    String::new()
                } else {
                    format!("\n  크기0: {}", zero.join(", "))
                },
                if buried.is_empty() {
                    String::new()
                } else {
                    format!("\n  덮임: {}", buried.join(", "))
                },
            );
        }
        eprintln!(
            "[hitaudit] {}",
            if total_bad == 0 {
                "PASS — 눌릴 수 없는 영역 없음".to_string()
            } else {
                format!("FAIL — 눌릴 수 없는 영역 {total_bad}개")
            }
        );
    }

    /// Headless 「마지막 pane 을 숨기면 그 방은 어떻게 되나」 repro:
    /// `KASATERM_AUTOLONESTASH_MS` 뒤에 방을 하나 더 만들고, pane 이 하나뿐인 그 방의
    /// pane 을 숨긴 다음 방 목록과 라벨을 찍는다.
    ///
    /// 숨기기는 트리에서 leaf 를 빼는데, 마지막 하나면 뺄 자리가 없어 트리를 통째로
    /// 버린다(`pty_layout = None`). 그 뒤 그 방이 목록에서 사라지는지, 빈 채로 남는지,
    /// 남는다면 이름이 무엇이 되는지가 화면으로는 잘 안 갈린다 — 비어 있으면 사라진
    /// 것처럼 보이기 때문이다. 그래서 개수와 라벨을 같이 찍는다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autolonestash(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOLONESTASH_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        if !self.sidebar_visible {
            self.toggle_sidebar();
        }
        let dump = |app: &mut App, tag: &str| {
            app.window_labels_at = None;
            app.refresh_window_labels();
            let rows: Vec<String> = (0..app.windows.len())
                .map(|i| {
                    let leaves = if i == app.active_window {
                        app.pty_layout.as_ref()
                    } else {
                        app.windows[i].as_ref()
                    }
                    .map(|l| l.leaves().len())
                    .unwrap_or(0);
                    let label = app
                        .window_labels
                        .get(i)
                        .map(|(n, _)| n.clone())
                        .unwrap_or_else(|| "?".to_string());
                    format!(
                        "{i}{}:{label} leaves={leaves}",
                        if i == app.active_window { "*" } else { "" }
                    )
                })
                .collect();
            eprintln!("[lonestash] {tag}: 방={} | {}", app.windows.len(), rows.join(" | "));
        };
        // 방을 하나 더 만든다 — 방이 하나뿐이면 「사라졌다」와 「원래 하나뿐」이
        // 안 갈린다.
        self.new_window();
        self.render_frame();
        dump(self, "before");
        let Some(lone) = self
            .pty_layout
            .as_ref()
            .and_then(|t| t.leaves().first().map(|s| s.to_string()))
        else {
            eprintln!("[lonestash] 숨길 pane 이 없다");
            return;
        };
        let leaves = self
            .pty_layout
            .as_ref()
            .map(|t| t.leaves().len())
            .unwrap_or(0);
        eprintln!("[lonestash] 이 방의 pane 수={leaves} · 숨길 것={lone}");
        self.stash_pane(&lone);
        self.render_frame();
        dump(self, "after ");
        eprintln!(
            "[lonestash] 되살리기 목록에 남았나={}",
            self.closed_panes.iter().any(|c| c.pane_id == lone)
        );
    }

    /// Headless pane 숨기기 repro: `KASATERM_AUTOSTASH_MS` 뒤에 사이드바를 켜고 pane 을
    /// 셋으로 쪼갠 다음, 한 줄을 **진짜로 우클릭**해 「숨기기」를 고른다.
    ///
    /// 확인할 건 "목록에서 사라졌다"가 아니라 **살아 있느냐**다. 숨기기는 닫기와 같은
    /// 스택에 들어가는데 그 스택에는 프로세스를 놓는 손이 둘 있다(개수 상한·idle reap).
    /// 조용히 죽는 종류라 화면으로는 영영 안 보인다 — 그래서 대조군을 같이 둔다:
    /// 하나는 숨기고 하나는 그냥 닫은 뒤 **같은 정리 한 번**을 돌린다. 닫은 것만 죽고
    /// 숨긴 것이 남아야 통과다(둘 다 살면 정리가 안 돈 것이라 증명이 아니다).
    /// `KASATERM_CLOSED_IDLE_SECS=1` 과 함께 쓴다 — 안 주면 15분을 기다려야 한다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autostash(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOSTASH_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        if !self.sidebar_visible {
            self.toggle_sidebar();
        }
        // 넷으로 쪼갠다 — 배치도는 칸이 둘일 때와 넷일 때 읽히는 게 다르고, 하나를
        // 숨기고 하나를 대조군으로 닫아도 목록에 볼 것이 남는다.
        let _ = self.split_active_pane(kasa_pty::SplitDir::Horizontal);
        let _ = self.split_active_pane(kasa_pty::SplitDir::Vertical);
        let _ = self.split_active_pane(kasa_pty::SplitDir::Horizontal);
        let _ = self.split_active_pane(kasa_pty::SplitDir::Vertical);
        // pane 줄과 배치도는 **펼친 방**에만 그려진다 — 접힌 카드에는 rect 가 하나도
        // 안 실려, 펼치지 않으면 목록이 빈 채로 "못 찾음"이 된다.
        if !self.expanded_windows.contains(&self.active_window) {
            self.toggle_window_expand(self.active_window);
        }
        // 펼침은 애니메이션이라 첫 프레임엔 카드가 아직 납작하고 줄 rect 가 하나도 안
        // 실린다(실측: 목록=[]). 다 펴질 때까지 프레임을 돌린다.
        for _ in 0..40 {
            self.render_frame();
            if !self.sidebar_row_rects.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let leaves = |app: &App| -> Vec<String> {
            app.pty_layout
                .as_ref()
                .map(|l| l.leaves().iter().map(|s| s.to_string()).collect())
                .unwrap_or_default()
        };
        let before = leaves(self);
        let Some(victim) = before.last().cloned() else { return };
        let control = before.first().cloned().unwrap_or_default();
        if std::env::var("KASATERM_AUTOSTASH_HOLD").is_ok() {
            // 배치도를 눈으로 볼 때 — 숨기기 전에 멈춘다. 숨긴 뒤에는 칸이 줄어
            // 정작 확인하려던 "분할 모양이 맞나"를 못 본다.
            // `_INFO=1` 이면 방을 하나 더 만들고 Info 를 켠다 — 방 머리 밴드는
            // 경계를 보이려는 것이라 **방이 둘 이상일 때만** 확인이 된다.
            if std::env::var("KASATERM_AUTOSTASH_INFO").is_ok() {
                self.new_window();
                if !self.git.col_visible {
                    self.toggle_git_col();
                }
                self.info.tab = crate::state::SideTab::Info;
            }
            self.render_frame();
            // 복원이 되살리기 목록을 채웠는지도 같은 자리에서 밝힌다 — 재시작으로
            // 잃던 것이 정확히 이 목록이라(2026-08-27), 눈으로만 보면 카드가 접혀
            // 있을 때 「없다」와 「안 그렸다」가 구별되지 않는다.
            let stashed: Vec<String> = self
                .closed_panes
                .iter()
                .filter(|c| c.stashed)
                .map(|c| format!("{}({})", c.pane_id, c.folder))
                .collect();
            eprintln!(
                "[autostash] hold — leaves={before:?} 배치도칸={} 되살리기목록={stashed:?}",
                self.sidebar_row_rects.len()
            );
            return;
        }
        // 그 pane 의 줄 한가운데. 미니맵 칸도 같은 벡터에 있어 `find` 는 먼저 오는
        // 칸을 집을 수 있는데, 우클릭은 어느 쪽이든 같은 pane 을 가리키므로 무해하다.
        let Some(row) = self
            .sidebar_row_rects
            .iter()
            .find(|(_, p, _)| *p == victim)
            .map(|(_, _, r)| *r)
        else {
            eprintln!("[autostash] {victim} 줄을 사이드바에서 못 찾음 — 목록={:?}", self.sidebar_row_rects);
            return;
        };
        let armed = self.sidebar_row_right_click(row.0 + row.2 / 2.0, row.1 + row.3 / 2.0);
        self.render_frame();
        let items: Vec<String> =
            self.sidebar_menu_rects.iter().map(|(a, _)| format!("{a:?}")).collect();
        let Some(hide) = self
            .sidebar_menu_rects
            .iter()
            .find(|(a, _)| matches!(a, crate::SidebarMenuAction::Hide))
            .map(|(_, r)| *r)
        else {
            eprintln!("[autostash] 메뉴 장전={armed} 인데 숨기기 항목이 없음 — 항목={items:?}");
            return;
        };
        self.sidebar_menu_click(hide.0 + hide.2 / 2.0, hide.1 + hide.3 / 2.0);
        self.render_frame();
        let stack = |app: &App| -> Vec<String> {
            app.closed_panes
                .iter()
                .map(|c| {
                    format!(
                        "{}({}{})",
                        c.pane_id,
                        if c.stashed { "숨김" } else { "닫힘" },
                        if c.alive { ",살아있음" } else { ",죽음" }
                    )
                })
                .collect()
        };
        eprintln!(
            "[autostash] 메뉴 장전={armed} 항목={items:?} · {victim} 숨김 → leaves={:?} 스택={:?}",
            leaves(self),
            stack(self)
        );
        // 대조군 — 같은 스택에 평범하게 닫은 것을 하나 넣는다.
        self.close_pane(&control);
        self.render_frame();
        // idle_since 는 첫 정리에서 찍힌다. 한 번 돌리고 상한을 넘긴 뒤 다시 돌려야
        // 실제로 놓는 자리까지 간다 — 헤드리스라 루프를 잠깐 세워도 된다.
        self.reap_idle_closed_panes();
        std::thread::sleep(std::time::Duration::from_millis(
            crate::closed_pane_idle_reap().as_millis() as u64 + 300,
        ));
        self.reap_idle_closed_panes();
        self.render_frame();
        eprintln!(
            "[autostash] 정리 뒤 스택={:?} · 숨긴 {victim} PTY={} · 닫은 {control} PTY={}",
            stack(self),
            self.pty.contains_key(&victim),
            self.pty.contains_key(&control)
        );
        eprintln!(
            "[autostash] 기대: 숨긴 PTY=true · 닫은 PTY=false (둘 다 true 면 정리가 안 돈 것이라 증명이 아니다)"
        );
        // 되돌리기 — 숨긴 줄을 다시 우클릭하면 이번엔 항목이 「보이기」여야 하고,
        // 누르면 **같은 id 가** 트리로 돌아와야 한다. 새 셸이 하나 뜨는 것과는 다르다.
        let Some(hrow) = self
            .sidebar_row_rects
            .iter()
            .find(|(_, p, _)| *p == victim)
            .map(|(_, _, r)| *r)
        else {
            eprintln!("[autostash] 숨긴 {victim} 줄이 목록에 없다 — 되돌릴 길이 없음");
            return;
        };
        self.sidebar_row_right_click(hrow.0 + hrow.2 / 2.0, hrow.1 + hrow.3 / 2.0);
        self.render_frame();
        let hitems: Vec<String> =
            self.sidebar_menu_rects.iter().map(|(a, _)| format!("{a:?}")).collect();
        let Some(un) = self
            .sidebar_menu_rects
            .iter()
            .find(|(a, _)| matches!(a, crate::SidebarMenuAction::Unhide))
            .map(|(_, r)| *r)
        else {
            eprintln!("[autostash] 숨긴 줄 메뉴에 보이기 항목이 없음 — 항목={hitems:?}");
            return;
        };
        self.sidebar_menu_click(un.0 + un.2 / 2.0, un.1 + un.3 / 2.0);
        self.render_frame();
        let back = leaves(self);
        eprintln!(
            "[autostash] 되돌림 항목={hitems:?} → leaves={back:?} 같은id복귀={} 스택={:?}",
            back.iter().any(|l| *l == victim),
            stack(self)
        );
        // `_INFO=1` — 검증을 다 지난 뒤 **그림용 상태**를 세운다(넷을 한 캡처에).
        // 되돌리기가 방금 숨긴 것을 되살렸으므로 흐린 줄을 보려면 다시 치워야 하고,
        // 방 머리 밴드는 방이 둘 이상일 때만 확인이 된다.
        if std::env::var("KASATERM_AUTOSTASH_INFO").is_ok() {
            self.stash_pane(&victim);
            self.new_window();
            if !self.git.col_visible {
                self.toggle_git_col();
            }
            self.info.tab = crate::state::SideTab::Info;
            // 방을 하나 더 만들면 활성이 그쪽으로 옮겨간다 — 배치도가 가리키는 방과
            // 화면에 뜬 방이 갈리면 그림이 오히려 헷갈리므로 원래 방으로 돌아온다.
            self.switch_window(0);
            self.render_frame();
            eprintln!(
                "[autostash] 그림용 — 활성방={} 방수={} 방슬롯={:?} 그 트리={:?} 스택={:?} 살아있는PTY={:?}",
                self.active_window,
                self.windows.len(),
                self.windows.iter().map(|w| w.as_ref().map(|t| t.leaves().len())).collect::<Vec<_>>(),
                leaves(self),
                stack(self),
                {
                    let mut k: Vec<&String> = self.pty.keys().collect();
                    k.sort();
                    k
                }
            );
        }
    }

    /// Headless 펼친 방 검증: `KASATERM_AUTOVIEW_MS` 뒤에 방을 pane 넷으로 쪼개
    /// 펼치고, 배치도 칸이 leaf 수만큼 섰는지 센다.
    ///
    /// 세는 건 `sidebar_row_rects` 가 아니다 — 거기엔 칸과 줄이 한 벡터에 섞여 있어
    /// 넷이 넷으로 바뀌면 아무것도 안 바뀐 것과 구별이 안 된다. 레이아웃을 직접 불러
    /// 칸과 꼬리 줄을 갈라 센다. 통과 모양은 `칸N/줄0` 이다(꼬리 줄은 숨긴 pane 몫).
    ///
    /// 배치도↔목록 왕복은 뺐다 — 목록 뷰 자체가 없어졌다(2026-08-24 지시:
    /// "목록표시는 info에서 보면되고").
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autoview(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOVIEW_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        if !self.sidebar_visible {
            self.toggle_sidebar();
        }
        let _ = self.split_active_pane(kasa_pty::SplitDir::Horizontal);
        let _ = self.split_active_pane(kasa_pty::SplitDir::Vertical);
        let _ = self.split_active_pane(kasa_pty::SplitDir::Horizontal);
        let wi0 = self.active_window;
        if !self.expanded_windows.contains(&wi0) {
            self.toggle_window_expand(wi0);
        }
        // ★ **다 펴질 때까지** 기다린다 — rect 가 생기자마자 세면 안 된다. 목록은
        // 카드가 자라는 동안 아래에서 한 줄씩 드러나는 설계라, 애니메이션 중간에
        // 세면 마지막 줄이 아직 카드 밖이어서 「줄이 하나 모자란다」로 읽힌다
        // (실측: h=139 일 때 4번째 줄이 잘렸고, 다 펴진 150 에서는 들어온다).
        for _ in 0..40 {
            self.render_frame();
            if self.expand_progress(wi0) >= 1.0 && !self.sidebar_row_rects.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let counts = |app: &App| -> (usize, usize) {
            let win_h = app
                .window
                .as_ref()
                .map(|w| w.inner_size().height as f32 / app.effective_scale())
                .unwrap_or(800.0);
            let (_, _, _, rows, mini) = app.sidebar_layout(win_h);
            (mini.len(), rows.len())
        };
        let wi = self.active_window;
        // `_WAIT=1` — 배치도 칸이 **신호를 받았을 때** 어떻게 보이나. 판정이 아니라
        // 그림을 보려는 것이라, 트랜스크립트 감시기가 쓰는 자리(`pane_activity` ·
        // `unread_panes`)에 같은 값을 직접 심는다. 진짜 claude 를 띄워 대기 상태를
        // 만들려면 승인 프롬프트가 뜰 때까지 기다려야 하는데 그건 재현이 안 된다.
        let error_capture = std::env::var("KASATERM_AUTOVIEW_ERROR").ok();
        if let Some((path, show_error)) = error_capture
            .map(|path| (path, true))
            .or_else(|| std::env::var("KASATERM_AUTOVIEW_WAIT").ok().map(|path| (path, false)))
        {
            let ls = self.window_leaves(wi);
            // ★ 심고서 이벤트 루프로 돌아가면 안 된다 — `refresh_pane_activity` 가 매
            // 틱 `pane_activity` 를 통째로 다시 만들어 심은 값을 지운다(실측: 3초 뒤
            // 예약 캡처에는 주황이 없고 파랑 둘만 찍혔다). 심은 **그 프레임에서**
            // 찍는다. `capture_next` 는 handler 가 쓰는 것과 같은 한 칸이다.
            if let Some(id) = ls.get(1) {
                self.pane_activity.insert(
                    id.clone(),
                    crate::stream::PaneStatusView {
                        status: if show_error { "idle" } else { "waiting" }.into(),
                        waiting_for: (!show_error).then(|| "선택지".into()),
                        has_error: show_error,
                        ..Default::default()
                    },
                );
            }
            if let Some(id) = ls.get(2) {
                self.unread_panes.insert(id.clone());
            }
            // 깜빡임의 **밝은 쪽**에서 찍는다. 아무 프레임이나 잡으면 절반은 알파가
            // 바닥이라 "글로우가 안 나온다"로 읽힌다 — 그림이 위상에 좌우되면 안 된다.
            for _ in 0..60 {
                if crate::render::blink_phase(0.9) > 0.95 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
            if let Some(g) = self.gpu.as_mut() {
                g.capture_next = Some(path.clone());
            }
            self.render_frame();
            eprintln!(
                "[autoview] 신호 심음 — 오류={} 대기={:?} 못본완료={:?} → {path}",
                show_error,
                ls.get(1),
                ls.get(2)
            );
            return;
        }
        let n_leaf = self.window_leaves(wi).len();
        let (cells, rows) = counts(self);
        eprintln!("[autoview] leaf={n_leaf} · 배치도(칸{cells}/꼬리줄{rows})");
        eprintln!("[autoview] 기대: 칸=leaf 수 · 꼬리줄=숨긴 pane 수(없으면 0)");
    }

    /// 상단바 토글 프로브. `KASATERM_AUTOHEADER_MS` 뒤에 활성 pane 의 헤더 띠를
    /// 켜고, PTY 행 수가 실제로 줄었는지 찍는다.
    ///
    /// 띠를 켜면 셀 그리드가 그만큼 밀리므로 render·hit-test·PTY 가 같은 값을 봐야
    /// 한다. 행 수가 그대로면 PTY 만 옛 크기로 남아 클릭이 한 행씩 어긋나는데,
    /// 캡처로는 멀쩡해 보인다 — 그래서 숫자로 찍는다.
    pub(crate) fn run_pending_autoheader(&mut self) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static STEP: AtomicUsize = AtomicUsize::new(0);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOHEADER_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        // 단계마다 1.5초 — SIGWINCH → 셸 재그림 → apply_screen_update 가 비동기라
        // 토글 직후 읽으면 옛 격자가 그대로 보인다.
        let step = STEP.load(Ordering::Relaxed);
        if step > 4
            || Instant::now() < *due + std::time::Duration::from_millis(1500 * step as u64)
        {
            return;
        }
        STEP.store(step + 1, Ordering::Relaxed);
        let Some(target) = self.ws.lock().unwrap().active_pane.clone() else { return };
        let snap = |app: &Self| {
            let ws = app.ws.lock().unwrap();
            ws.panes.get(&target).map(|p| {
                (p.has_header(), p.term().map_or((0, 0), |t| (t.cols, t.rows)))
            })
        };
        // 0: 그대로 읽기 → 1: resize_backend 만(토글 없이) → 2: 읽기 → 3: 헤더 켜기
        // → 4: 읽기. 1번이 있어야 "격자가 변한 게 헤더 때문인지, 그냥 리사이즈가
        // 처음 밀린 것인지" 를 가른다 — 이걸 안 갈라서 한 번 오판했다.
        match step {
            0 => eprintln!("[autoheader] 0 초기 {:?}", snap(self)),
            1 => {
                let (c, r) = self.window_cells();
                self.resize_backend(c, r);
                eprintln!("[autoheader] 1 헤더 없이 resize_backend 만 호출");
            }
            2 => eprintln!("[autoheader] 2 리사이즈 후 {:?}", snap(self)),
            3 => {
                self.toggle_pane_header(&target);
                eprintln!("[autoheader] 3 헤더 켬");
            }
            _ => eprintln!("[autoheader] 4 헤더 켠 뒤 {:?}", snap(self)),
        }
    }
    /// surface 크기 어긋남 재현. `KASATERM_FORCE_SURFACE_HALF_MS` 뒤에 스왑체인만
    /// 창의 절반 크기로 다시 잡는다 — 모니터를 옮길 때 Resized/ScaleFactorChanged
    /// 가 코얼레스되며 실제로 벌어지는 상태를 인위적으로 만든 것이다.
    /// 거노 스크린샷 실측(창 1510x950 안에 콘텐츠 754x472, 빈 영역은 우리
    /// 배경색이 아닌 NSWindow 기본색)이 바로 이 상태다.
    pub(crate) fn run_pending_forcesurfacehalf(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_FORCE_SURFACE_HALF_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if Instant::now() < *due || DONE.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(size) = self.window.as_ref().map(|w| w.inner_size()) else { return };
        // `view` = 뷰 자체를 줄인다(거노가 본 상태 — UI 가 온전한 채로 축소).
        // 그 외 = 스왑체인만 줄인다(UI 가 잘림). 두 증상이 다르다는 게
        // 원인 판별의 핵심이었다.
        if std::env::var("KASATERM_FORCE_SURFACE_HALF_KIND").as_deref() == Ok("view") {
            if let Some(w) = self.window.as_ref() {
                gpu::shrink_view_for_test(w);
            }
        } else if let Some(g) = self.gpu.as_mut() {
            g.resize(size.width / 2, size.height / 2);
            eprintln!(
                "[forcehalf] 스왑체인만 {}x{} 로 축소(창은 {}x{} 그대로)",
                size.width / 2,
                size.height / 2,
                size.width,
                size.height
            );
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// 모니터 이동 재현. `KASATERM_AUTOMOVESCREEN_MS="7000,11000"` 처럼 콤마로
    /// 여러 시각을 주면 그때마다 창을 **다른 물리 모니터로** 옮긴다(핑퐁).
    /// `KASATERM_AUTOCAPTURE_MS` 를 그 사이사이에 끼워 이동 전/후 프레임을
    /// 비교하면 "큰 모니터로 옮기면 화면이 구석에 처박힌다" 를 헤드리스에서
    /// 그대로 볼 수 있다. 레이어 속성만 흉내 내는 재현은 실패했다 — AppKit 이
    /// 진짜로 backing scale 을 바꿔야 한다.
    pub(crate) fn run_pending_automovescreen(&mut self) {
        use std::sync::OnceLock;
        static DUE: OnceLock<Vec<Instant>> = OnceLock::new();
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOMOVESCREEN_MS")
                .ok()
                .map(|s| {
                    let now = Instant::now();
                    s.split(',')
                        .filter_map(|p| p.trim().parse::<u64>().ok())
                        .map(|ms| now + std::time::Duration::from_millis(ms))
                        .collect()
                })
                .unwrap_or_default()
        });
        let i = NEXT.load(std::sync::atomic::Ordering::Relaxed);
        let Some(at) = due.get(i) else { return };
        if Instant::now() < *at {
            return;
        }
        NEXT.store(i + 1, std::sync::atomic::Ordering::Relaxed);
        let Some(w) = self.window.clone() else { return };
        eprintln!("[movescreen] #{i} 이동 시작");
        gpu::log_layer_geometry(&w, &format!("이동#{i} 전"));
        self.log_window_placement(&format!("이동#{i} 전"));
        gpu::move_window_to_other_screen(&w);
        gpu::log_layer_geometry(&w, &format!("이동#{i} 직후"));
        *LAYERGEOM_DUE.lock().unwrap() = Some(Instant::now() + std::time::Duration::from_millis(1200));
    }
    /// 이동 뒤 AppKit 이 프레임/스케일을 정착시킬 시간을 준 다음 한 번 더 실측.
    /// `ScaleFactorChanged` 는 `setFrame:` 과 같은 턴에 안 올 수 있어서
    /// "직후" 값만 보면 정상으로 오판한다. (검증 전용이라 struct App 필드를
    /// 늘리지 않는다 — 병렬 작업 충돌 핫스팟이다.)
    pub(crate) fn run_pending_layergeom(&mut self) {
        let due = { *LAYERGEOM_DUE.lock().unwrap() };
        let Some(at) = due else { return };
        if Instant::now() < at {
            return;
        }
        *LAYERGEOM_DUE.lock().unwrap() = None;
        if let Some(w) = self.window.clone() {
            gpu::log_layer_geometry(&w, "정착 후");
            self.log_window_placement("정착 후");
        }
    }
    /// 저장될 창 좌표가 **복원 때 살아남는지**를 그 자리에서 판정해 찍는다.
    /// 저장은 조용히 성공하고 복원만 조용히 실패하므로, 둘을 따로 보면
    /// "위치가 왜 안 돌아오지" 를 영영 못 잡는다 — `resumed` 의 on_screen
    /// 판정을 그대로 재현해 같은 줄에 놓는 것이 요점이다.
    pub(crate) fn log_window_placement(&mut self, tag: &str) {
        let Some(w) = self.window.clone() else { return };
        let Ok(p) = w.outer_position() else { return };
        let (px, py) = (p.x as f64, p.y as f64);
        let mut restorable = false;
        for m in w.available_monitors() {
            let mp = m.position();
            let ms = m.size();
            let ok = px >= mp.x as f64
                && px < (mp.x as f64 + ms.width as f64 - 60.0)
                && py >= mp.y as f64
                && py < (mp.y as f64 + ms.height as f64 - 60.0);
            eprintln!(
                "[winpos]   모니터 @({},{}) {}x{} sf={} → {}",
                mp.x,
                mp.y,
                ms.width,
                ms.height,
                m.scale_factor(),
                if ok { "통과" } else { "탈락" }
            );
            restorable |= ok;
        }
        eprintln!(
            "[winpos] {tag}: outer=({px},{py}) inner={}x{} sf={} → 복원 {}",
            w.inner_size().width,
            w.inner_size().height,
            w.scale_factor(),
            if restorable { "됨" } else { "★버려짐★" }
        );
    }
    /// 줌 클릭 매핑 프로브. `KASATERM_AUTOZOOMPROBE_MS` 뒤에 활성 pane 을 줌하고,
    /// 작업영역 전체에 격자로 점을 찍어 `px_to_pane_cell` 이 어디로 보내는지 찍는다.
    ///
    /// 지켜야 할 불변식은 하나다 — **줌 중엔 작업영역 안 모든 점이 줌된 pane 으로
    /// 가야 한다.** 예전엔 원본 split 박스로 판정해 아래 절반이 숨은 pane 으로
    /// 샜고(거노: "최대화하고 위치 매핑이 이상해"), 화면엔 그 pane 이 안 보이니
    /// 클릭이 사라지는 것처럼 보였다. 눈으로 보는 캡처로는 절대 안 잡히는 종류라
    /// 좌표를 직접 찍는 프로브를 남긴다.
    pub(crate) fn run_pending_autozoomprobe(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOZOOMPROBE_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if Instant::now() < *due || DONE.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(target) = self.ws.lock().unwrap().active_pane.clone() else {
            eprintln!("[zoomprobe] 활성 pane 없음");
            return;
        };
        let Some(size) = self.window.as_ref().map(|w| w.inner_size()) else {
            eprintln!("[zoomprobe] 창 없음");
            return;
        };
        let scale = self.effective_scale();
        let (lw, lh) = (size.width as f32 / scale, size.height as f32 / scale);
        let probe = |app: &Self, tag: &str| {
            // 작업영역 안쪽만 — 패딩·타이틀바 밖은 애초에 pane 이 아니다.
            let x0 = app.effective_sidebar_w() + WINDOW_PADDING + 4.0;
            for i in 0..5 {
                for j in 0..5 {
                    let px = x0 + (lw - x0 - 8.0) * (i as f32 / 4.0);
                    let py = TITLE_HEIGHT + 4.0 + (lh - TITLE_HEIGHT - 40.0) * (j as f32 / 4.0);
                    let hit = app.px_to_pane_cell(px, py);
                    eprintln!(
                        "[zoomprobe] {tag} ({px:.0},{py:.0}) → {}",
                        hit.map_or("(없음)".to_string(), |(p, c, r)| format!("{p} {c},{r}"))
                    );
                }
            }
        };
        probe(self, "before");
        self.toggle_pane_zoom(&target);
        eprintln!("[zoomprobe] zoomed={target}");
        probe(self, "after");
    }
    /// Headless Info 패널 repro: `KASATERM_AUTOINFO_MS` 뒤에 우측 칼럼을 열고
    /// Info 탭으로 넘긴다. 그 탭은 클릭으로만 갈 수 있어 캡처 하네스에서 볼
    /// 방법이 없었다 — 프로세스·포트 목록은 눈으로 봐야 폭·정렬을 판단한다.
    /// AUTOSEND 로 pane 에 자식 프로세스를 물려두면 목록이 채워진 채 찍힌다.
    ///
    /// `KASATERM_AUTOINFO=hover|menu` 를 주면 탭이 열리고 1.5초 뒤(첫 수집이
    /// 끝나 행 좌표가 생긴 뒤) 첫 프로세스 행에 커서를 올리거나 우클릭 메뉴를
    /// 띄운다. 종료(×) 버튼과 메뉴는 호버·우클릭에만 나타나 정적 캡처로는
    /// 존재 자체를 확인할 수 없다.
    /// `KASATERM_AUTOTITLESYNC_MS` — claude 안에서 친 `/rename` 이 pane 탭 이름까지
    /// 닿는지, 그리고 **사람이 붙인 이름을 안 밀어내는지** 한 프레임에 잇달아 잰다.
    ///
    /// 사람 손을 못 흉내내는 건 하나뿐이다 — claude 가 transcript 에 남기는
    /// `custom-title` 레코드. 그것만 가짜로 심고 나머지는 제품 경로를 그대로 태운다.
    /// 실제 claude 를 띄워 `/rename` 을 치는 검증은 대화 하나를 실제로 소모하고,
    /// 정작 갈리는 자리(사람 개명과 겹칠 때)는 손으로 재현하기가 더 어렵다.
    ///
    /// 네 걸음이 규칙 전부를 덮는다: 붙는가 · 갱신되는가 · 사람 것을 안 덮는가 ·
    /// 그 뒤로도 안 덮는가.
    pub(crate) fn run_pending_autotitlesync(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOTITLESYNC_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let Some(pid) = self.ws.lock().unwrap().active_pane.clone() else {
            eprintln!("[autotitlesync] 미측정 — 활성 pane 이 없다");
            return;
        };
        for (step, in_transcript, by_hand) in [
            ("① claude 가 개명", "지어준이름", None),
            ("② claude 가 또 개명", "두번째이름", None),
            ("③ 사람이 개명", "두번째이름", Some("사람이붙인이름")),
            ("④ 그 뒤 claude 가 또", "네번째이름", None),
        ] {
            if let Some(h) = by_hand {
                // `surface.rename` 이 닿는 자리와 같은 두 칸 — 그 경로를 통째로
                // 부르지 않는 건 소켓 왕복이 이 하네스의 관심사가 아니어서다.
                let mut ws = self.ws.lock().unwrap();
                if let Some(p) = ws.panes.get_mut(&pid) {
                    p.title = Some(h.to_string());
                    p.title_pinned = true;
                }
            }
            if let Err(e) = self.titlesync_seed(&pid, in_transcript) {
                eprintln!("[autotitlesync] 미측정 — 가짜 transcript 를 못 썼다: {e}");
                return;
            }
            // 동기화는 mtime 이 그대로면 파일을 안 읽는다. 네 걸음을 같은 밀리초에
            // 몰면 ②부터가 「안 읽음」이 되는데, 결과만 보면 「물러섰다」와 구별이
            // 안 된다 — 그러면 ③이 무엇을 증명했는지 말할 수 없다.
            std::thread::sleep(std::time::Duration::from_millis(20));
            self.sync_session_titles_now();
            let ws = self.ws.lock().unwrap();
            let (title, pinned) = ws
                .panes
                .get(&pid)
                .map(|p| (p.title.clone().unwrap_or_default(), p.title_pinned))
                .unwrap_or_default();
            eprintln!(
                "[autotitlesync] {step}  전사본={in_transcript:?} → 제목={title:?} 핀={pinned}"
            );
        }
        // ⑤ 사람 흔적을 걷으면 다시 claude 쪽을 따라간다. 이 걸음을 마지막에 두는
        // 이유는 캡처 때문이다 — `KASATERM_AUTOCAPTURE_MS` 를 뒤에 붙이면 그 이름이
        // **타이틀바에 실제로 그려진 화면**이 찍힌다. 위 네 줄은 값이 맞는지만
        // 말하고, 그 값이 사람 눈에 닿는지는 말해 주지 않는다.
        {
            let mut ws = self.ws.lock().unwrap();
            if let Some(p) = ws.panes.get_mut(&pid) {
                p.title = None;
                p.title_pinned = false;
            }
        }
        if self.titlesync_seed(&pid, "다섯번째이름").is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            self.sync_session_titles_now();
            let ws = self.ws.lock().unwrap();
            let title = ws
                .panes
                .get(&pid)
                .and_then(|p| p.title.clone())
                .unwrap_or_default();
            eprintln!(
                "[autotitlesync] ⑤ 사람 이름을 걷고  전사본=\"다섯번째이름\" → 제목={title:?}"
            );
        }
    }

    /// `autotitlesync` 가 쓸 가짜 transcript — claude `/rename` 이 남기는 것과 같은
    /// 모양이다. `nameSource` 가 없는 것까지 같다(그 표식은 우리 CLI 개명만 남긴다).
    fn titlesync_seed(&mut self, pid: &str, name: &str) -> std::io::Result<()> {
        let cwd = std::path::PathBuf::from(TITLESYNC_CWD);
        std::fs::create_dir_all(&cwd)?;
        let jsonl = crate::socket::project_jsonl(&cwd, TITLESYNC_SID)
            .ok_or_else(|| std::io::Error::other("project_jsonl 이 경로를 못 만든다"))?;
        if let Some(dir) = jsonl.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = format!(
            "{{\"type\":\"custom-title\",\"customTitle\":\"{name}\",\
             \"sessionId\":\"{TITLESYNC_SID}\"}}\n"
        );
        std::fs::write(&jsonl, body)?;
        self.pane_claude_sid.insert(pid.to_string(), TITLESYNC_SID.to_string());
        Ok(())
    }

    /// `KASATERM_AUTOULTRASCAN_MS` — `/effort` 로 켜고 **프롬프트 없이** 끄는 구간에서
    /// ultracode 가 살아남는지 잰다.
    ///
    /// 훅(`collab-hooks/ultracode-mark.py`)은 UserPromptSubmit 이라 프롬프트가 있어야
    /// 돈다. 켜자마자 앱을 끄면 표식이 한 번도 안 써지고, 그러면 저장이 xhigh 로
    /// 굳어 다음 실행이 ultracode 를 잃는다 — 거노가 두 번 물린 자리다. 앱이
    /// transcript 를 직접 훑는 경로가 그 구간을 메우는지 본다.
    ///
    /// 판정은 **둘 다** 찍는다. 글로우(`pane_ultracode`)만 보면 화면은 맞는데 저장은
    /// 틀린 상태를 놓치고, 실제로 이 버그가 그 모양이었다.
    pub(crate) fn run_pending_autoultrascan(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOULTRASCAN_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let Some(pid) = self.ws.lock().unwrap().active_pane.clone() else {
            eprintln!("[autoultrascan] 미측정 — 활성 pane 이 없다");
            return;
        };
        let Some(path) = Self::ultrascan_path() else {
            eprintln!("[autoultrascan] 미측정 — 가짜 transcript 경로를 못 만든다");
            return;
        };
        // 출발점은 「지난 실행이 ultracode 를 켠 채로 끝난 세션」이다. 이 마커는 우리가
        // 이 세션을 물기 **전** 것이라 유령이고, ①이 그걸 안 믿는지를 본다 —
        // `--resume` 이 같은 jsonl 에 이어 쓰므로 이 구별이 없으면 남의 상태를
        // 물려받는다.
        if std::fs::write(&path, format!("{}\n", Self::ultrascan_cmd("ultracode"))).is_err() {
            eprintln!("[autoultrascan] 미측정 — 가짜 transcript 를 못 썼다");
            return;
        }
        self.pane_claude_sid.insert(pid.clone(), ULTRASCAN_SID.to_string());
        for (step, add) in [
            ("① 켠 채 끝난 세션을 물려받음", None),
            ("② /effort ultracode", Some(Self::ultrascan_cmd("ultracode"))),
            ("③ /effort xhigh", Some(Self::ultrascan_cmd("xhigh"))),
            ("④ ultra_effort_enter", Some(ULTRASCAN_ENTER.to_string())),
        ] {
            if let Some(line) = add {
                if Self::ultrascan_append(&path, &line).is_err() {
                    eprintln!("[autoultrascan] {step} 미측정 — append 실패");
                    return;
                }
            }
            // mtime 이 같으면 꼬리를 다시 안 읽는다 — `autotitlesync` 와 같은 이유로
            // 걸음 사이를 벌린다.
            std::thread::sleep(std::time::Duration::from_millis(20));
            self.sync_session_titles_now();
            self.refresh_pane_ultracode();
            let glow = self.pane_ultracode.contains(&pid);
            let saved = self
                .agent_cfg_snapshot()
                .get(&pid)
                .map(|c| c.1.clone())
                .unwrap_or_default();
            eprintln!("[autoultrascan] {step} → 글로우={glow} 저장effort={saved:?}");
        }
        // ⑤ 복원 기준선. `--effort ultracode` 로 되살린 pane 은 transcript 에 흔적이
        // 없어 스캔도 훅도 볼 것이 없다 — 앱이 자기가 그렇게 띄웠다는 표시만이
        // 근거다. 여기서는 그 표시를 제품과 **같은 문**(`mark_restored_ultracode`)으로
        // 세우고, 마커가 하나도 없는 파일에서도 저장까지 가는지 본다.
        //
        // ⚠️ 「복원이 그 표시를 세운다」까지는 여기서 못 잰다. 복원 리그를 세우려면
        // 저장본에 was_agent 를 넣어야 하고, 그러면 900ms 뒤 진짜 `claude` 가 뜬다.
        if Self::ultrascan_append(&path, "{}").is_ok() {
            self.pane_claude_sid.remove(&pid);
            self.mark_restored_ultracode(&pid);
            std::thread::sleep(std::time::Duration::from_millis(20));
            self.sync_session_titles_now();
            self.refresh_pane_ultracode();
            let glow = self.pane_ultracode.contains(&pid);
            let saved = self
                .agent_cfg_snapshot()
                .get(&pid)
                .map(|c| c.1.clone())
                .unwrap_or_default();
            // sid 를 일부러 뗐다 — 복원 직후엔 bind-transcript 훅이 아직 안 와서
            // `pane_claude_sid` 가 비어 있다. 그 상태에서도 잡히는지가 이 걸음의
            // 요점이고, 처음 짤 때 실제로 여기서 빠뜨렸다.
            eprintln!("[autoultrascan] ⑤ 복원 기준선(sid 없음) → 글로우={glow} 저장effort={saved:?}");
        }
    }

    /// `autoultrascan` 이 쓸 가짜 transcript 경로.
    fn ultrascan_path() -> Option<std::path::PathBuf> {
        let cwd = std::path::PathBuf::from(ULTRASCAN_CWD);
        std::fs::create_dir_all(&cwd).ok()?;
        let p = crate::socket::project_jsonl(&cwd, ULTRASCAN_SID)?;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).ok()?;
        }
        Some(p)
    }

    /// `/effort <level>` 이 transcript 에 남기는 줄 — 실제 레코드에서 뜬 모양이다.
    fn ultrascan_cmd(level: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":"<local-command-stdout>Set effort level to {level}"}}}}"#
        )
    }

    fn ultrascan_append(path: &std::path::Path, line: &str) -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(f, "{line}")
    }

    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autoinfo(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static OPENED: AtomicBool = AtomicBool::new(false);
        static ACTED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOINFO_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if Instant::now() < *due {
            return;
        }
        if !OPENED.swap(true, Ordering::Relaxed) {
            if !self.git.col_visible {
                self.toggle_git_col();
            }
            self.info.tab = crate::state::SideTab::Info;
            self.info.scroll = 0.0;
            eprintln!("[autoinfo] Info 탭 열림 (col_visible={})", self.git.col_visible);
            return;
        }
        let act = match std::env::var("KASATERM_AUTOINFO").ok() {
            Some(v)
                if v == "hover"
                    || v == "menu"
                    || v == "panemenu"
                    || v.starts_with("panechars") =>
            {
                v
            }
            _ => return,
        };
        if ACTED.load(Ordering::Relaxed)
            || Instant::now() < *due + std::time::Duration::from_millis(1500)
        {
            return;
        }
        ACTED.store(true, Ordering::Relaxed);
        // 학생 줄 우클릭 메뉴 — 프로세스 행과 다른 목록(`group_rects`)을 쓰고,
        // 캐릭터 목록은 **한 번 더 눌러야** 나오므로 정적 캡처로는 존재 자체를
        // 확인할 수 없다. `panemenu` 는 테마 목록까지, `panechars` 는 첫 테마를
        // 골라 캐릭터 목록까지 편 상태로 세운다.
        if act == "panemenu" || act.starts_with("panechars") {
            let Some((pane, r)) = self
                .info
                .group_rects
                .iter()
                .find(|(k, _)| k.starts_with('%'))
                .cloned()
            else {
                eprintln!("[autoinfo] 학생 줄 없음 — group_rects 에 pane 머리가 안 잡혔다");
                return;
            };
            let (cx, cy) = (r.0 + r.2 * 0.5, r.1 + r.3 * 0.5);
            // `panechars:<테마 id>` 로 볼 단을 고른다. 기본(첫 테마)만으로는 **번들
            // 로스터를 못 연다** — `list_themes` 에 번들이 없어서다. 그런데 켠 학생
            // 대부분이 거기 있으니, 거르기를 시험할 자리가 바로 그 단이다.
            let opened = if let Some(id) = act.strip_prefix("panechars:") {
                Some(id.to_string())
            } else if act == "panechars" {
                let first = kasa_mcp::character::list_themes().into_iter().next();
                if first.is_none() {
                    eprintln!("[autoinfo] 테마가 하나도 없다 — 캐릭터 목록을 못 편다");
                }
                first.map(|(id, _)| id)
            } else {
                None
            };
            self.cursor_px = (cx, cy);
            self.info.pane_menu = Some((cx, cy, pane.clone(), opened.clone()));
            eprintln!(
                "[autoinfo] act={act} pane={pane} at ({cx:.0},{cy:.0}) opened={opened:?}"
            );
            return;
        }
        let Some((pid, r)) = self.info.proc_rects.first().copied() else {
            eprintln!("[autoinfo] 프로세스 행 없음 — 좌표가 아직 안 생겼다");
            return;
        };
        let (cx, cy) = (r.0 + r.2 * 0.5, r.1 + r.3 * 0.5);
        if act == "menu" {
            self.info.ctx_menu = Some((cx, cy, pid));
        }
        // hover 든 menu 든 커서는 행 위에 둔다 — menu 도 그 행이 하이라이트된
        // 상태로 찍혀야 어느 프로세스를 겨눈 메뉴인지 보인다.
        self.cursor_px = (cx, cy);
        eprintln!("[autoinfo] act={act} pid={pid} at ({cx:.0},{cy:.0})");
    }
    /// 스크롤을 **경계에 걸친 자리**에 세워 두는 구멍. 잘라내기(시저) 작업은
    /// "위로 반쯤 나간 행이 헤더를 덮나"를 봐야 하는데, 그 상태는 목록을 조금
    /// 밀어야만 나온다 — 스크롤이 0 이면 첫 행이 마침 경계에 딱 붙어 있어 클리핑이
    /// 있으나 없으나 그림이 같고, 그래서 **없는 클리핑이 통과해 버린다**.
    ///
    /// - `KASATERM_AUTOCOLSCROLL="<탭>:<px>"` — 우측 칼럼을 열고 그 탭으로 바꾼 뒤
    ///   본문을 `px` 만큼 민다. 탭은 `sessions` | `mcp` | `info` | `git`.
    /// - `KASATERM_AUTOFTSCROLL="<px>"` — 파일트리를 켜고 그만큼 민다.
    /// - `KASATERM_AUTOGITSCROLL="<px>"` — `AUTOCOLSCROLL="git:<px>"` 의 준말.
    /// - `_MS` 로 시각을 옮긴다(기본 5000). 캡처(`AUTOCAPTURE_MS`)보다 앞에 둘 것.
    ///
    /// 스크롤 값은 그리기 쪽 `clamp` 가 다시 잡는다 — 목록이 짧아 밀 데가 없으면
    /// 0 으로 돌아오고, 로그에 요청값과 실제값이 함께 찍혀 "하네스는 돌았는데
    /// 화면이 안 움직인 것"과 "하네스가 안 돈 것"이 구분된다.
    ///
    /// **두 단계로 나뉜다.** 먼저 칼럼을 열어 탭만 세우고, 1.5초 뒤에 스크롤을
    /// 넣는다. 세션·MCP·Info 목록은 전부 워커 스레드가 채우므로(디스크 stat·`ps`),
    /// 탭을 연 프레임에는 아직 0행이다 — 그 자리에서 밀면 clamp 가 통째로 먹고
    /// **스크롤이 0 인 화면을 "밀어 봤다"고 찍게 된다**.
    /// `AUTOCOLSCROLL`(또는 `AUTOGITSCROLL` 준말)을 `(탭, 픽셀)` 로. 두 단계가
    /// 같은 해석을 쓰도록 여기 한 곳에 둔다 — 열 때와 밀 때가 다른 탭을 고르면
    /// 「열긴 열었는데 안 밀린다」로 끝난다.
    fn autocolscroll_spec() -> Option<(crate::state::SideTab, f32)> {
        use crate::state::SideTab;
        let spec = std::env::var("KASATERM_AUTOCOLSCROLL").ok().or_else(|| {
            std::env::var("KASATERM_AUTOGITSCROLL").ok().map(|px| format!("git:{px}"))
        })?;
        let Some((tab, px)) = spec.split_once(':') else {
            eprintln!("[autocolscroll] 형식은 \"<탭>:<px>\" 다 — 받은 것: {spec:?}");
            return None;
        };
        let px = match px.trim().parse::<f32>() {
            Ok(v) => v,
            Err(_) => {
                eprintln!("[autocolscroll] 픽셀이 숫자가 아니다: {px:?}");
                return None;
            }
        };
        let tab = match tab.trim() {
            "sessions" => SideTab::Sessions,
            "mcp" => SideTab::Mcp,
            "info" => SideTab::Info,
            "git" => SideTab::Git,
            "machines" => SideTab::Machines,
            other => {
                eprintln!("[autocolscroll] 모르는 탭 {other:?} — sessions|mcp|info|git|machines");
                return None;
            }
        };
        Some((tab, px))
    }

    pub(crate) fn run_pending_autocolscroll(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static OPENED: AtomicBool = AtomicBool::new(false);
        static DIFFED: AtomicBool = AtomicBool::new(false);
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let any = std::env::var_os("KASATERM_AUTOCOLSCROLL").is_some()
                || std::env::var_os("KASATERM_AUTOFTSCROLL").is_some()
                || std::env::var_os("KASATERM_AUTOGITSCROLL").is_some();
            any.then(|| {
                let ms: u64 = std::env::var("KASATERM_AUTOCOLSCROLL_MS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(5000);
                Instant::now() + std::time::Duration::from_millis(ms)
            })
        });
        let Some(due) = due else { return };
        if Instant::now() < *due {
            return;
        }
        if !OPENED.swap(true, Ordering::Relaxed) {
            if let Some(tab) = Self::autocolscroll_spec().map(|(t, _)| t) {
                if !self.git.col_visible {
                    self.toggle_git_col();
                }
                self.info.tab = tab;
                self.chrome_dirty = true;
                eprintln!("[autocolscroll] 칼럼 열림 — 목록이 차기를 1.5초 기다린다");
            }
            return;
        }
        if Instant::now() < *due + std::time::Duration::from_millis(1500) {
            return;
        }
        // `KASATERM_AUTOCOLDIFF=<개수>` — 변경 목록에서 그만큼의 파일 diff 를 펼친다.
        // git 칼럼에서 「끝까지 스크롤이 닿나」를 물으려면 목록이 화면보다 길어야 하는데,
        // 이 레포의 변경 파일 몇 개로는 절대 안 넘친다(11행 = 263px vs 화면 1152px).
        // 펼친 diff 만이 그 길이를 만든다.
        //
        // ⚠️ **칼럼을 여는 단계에서는 못 한다** — git 스캔은 칼럼이 보이게 된 뒤에
        // 돌아서, 그 시점의 `col_data` 는 아직 비어 있다(실측: 「펼치기 0개」).
        // 그래서 목록이 찬 뒤인 여기서 걸고, 본문은 워커가 가져오므로 다시 1.5초
        // 기다렸다가 잰다.
        if !DIFFED.swap(true, Ordering::Relaxed) {
            if let Some(n) = std::env::var("KASATERM_AUTOCOLDIFF")
                .ok()
                .and_then(|s| s.trim().parse::<usize>().ok())
            {
                let picks: Vec<(bool, String)> = self
                    .git
                    .col_data
                    .lock()
                    .map(|g| {
                        g.staged
                            .iter()
                            .map(|(_, p)| (true, p.clone()))
                            .chain(g.unstaged.iter().map(|(_, p)| (false, p.clone())))
                            .take(n)
                            .collect()
                    })
                    .unwrap_or_default();
                eprintln!("[autocolscroll] diff 펼치기 {}개", picks.len());
                for (staged, path) in picks {
                    self.toggle_git_diff(staged, path);
                }
                return;
            }
        }
        if std::env::var_os("KASATERM_AUTOCOLDIFF").is_some()
            && Instant::now() < *due + std::time::Duration::from_millis(3000)
        {
            return;
        }
        // 로그는 한 번만, **세우는 것은 매 틱**이다. 목록을 채우는 pump 들이
        // 자기 판단으로 스크롤을 0 으로 되돌리기 때문이다 — `pump_info` 는 pane
        // 집합이 달라지면(첫 수집이 곧 그 경우다), `pump_sessions_col` 은 cwd 가
        // 달라지면 되돌린다. 한 번만 세우면 그 되돌림이 캡처 직전에 끼어들어
        // **스크롤이 0 인 화면을 「밀어 봤다」고 찍는다**(실측: Info 탭이 그랬다).
        let first = !FIRED.swap(true, Ordering::Relaxed);
        if let Ok(px) = std::env::var("KASATERM_AUTOFTSCROLL") {
            match px.trim().parse::<f32>() {
                Ok(px) => {
                    if !self.file_tree.visible {
                        self.toggle_file_tree();
                    }
                    if first {
                        self.refresh_file_tree();
                    }
                    self.file_tree.scroll = px;
                    // 그리기가 clamp 하므로 한 프레임 태운 뒤에 실제값을 읽는다.
                    self.render_frame();
                    if first {
                        eprintln!(
                            "[autocolscroll] 파일트리 요청={px} 실제={} 항목={}",
                            self.file_tree.scroll,
                            self.file_tree.nodes.len()
                        );
                    }
                }
                Err(_) if first => {
                    eprintln!("[autocolscroll] AUTOFTSCROLL 이 숫자가 아니다: {px:?}")
                }
                Err(_) => {}
            }
        }
        use crate::state::SideTab;
        let Some((tab, px)) = Self::autocolscroll_spec() else { return };
        // 탭도 매 틱 다시 세운다 — 칼럼을 여는 다른 경로가 탭을 갈아 끼울 수 있다.
        self.info.tab = tab;
        // `KASATERM_AUTOCOLWHEEL=<노치>` — 스크롤을 **손으로 세우는 대신 칼럼 위에서
        // 휠을 굴린다.** 값을 대입하는 길은 스크롤 상한 계산을 통째로 건너뛰므로,
        // 「끝까지 스크롤이 닿나」는 이 길로만 물을 수 있다. 음수면 위로 굴린다.
        //
        // 이 갈래에서는 스크롤 재설정을 **첫 틱에만** 한다 — 매 틱 다시 세우면 그
        // 대입이 휠이 밀어 둔 자리를 도로 지운다.
        let wheel = std::env::var("KASATERM_AUTOCOLWHEEL")
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok());
        if wheel.is_none() || first {
            match tab {
                SideTab::Sessions => self.sessions_col.scroll = px,
                SideTab::Mcp => self.mcp_col.scroll = px,
                SideTab::Info => self.info.scroll = px,
                SideTab::Git => self.git.col_scroll = px,
                SideTab::Persona => self.persona.bubble_scroll = px,
                SideTab::Machines => self.info.machines_col.scroll = px,
            }
        }
        self.chrome_dirty = true;
        self.render_frame();
        if !first {
            return;
        }
        if let Some(notches) = wheel {
            use winit::event::MouseScrollDelta;
            let (gx, gw) = (self.git_col_x(), self.git_col_w());
            self.cursor_px = (gx + gw * 0.5, TITLE_HEIGHT + 200.0);
            let step = if notches >= 0 { -1.0 } else { 1.0 };
            for _ in 0..notches.abs() {
                self.handle_wheel(MouseScrollDelta::LineDelta(0.0, step));
            }
            self.render_frame();
        }
        // 읽는 것은 휠까지 굴린 **뒤**다. 앞에서 읽으면 대입한 값을 그대로 되읽어
        // 「휠이 안 먹었다」와 구분이 안 된다.
        let actual = match tab {
            SideTab::Sessions => self.sessions_col.scroll,
            SideTab::Mcp => self.mcp_col.scroll,
            SideTab::Info => self.info.scroll,
            SideTab::Git => self.git.col_scroll,
            SideTab::Persona => self.persona.bubble_scroll,
            SideTab::Machines => self.info.machines_col.scroll,
        };
        // 행 수를 함께 찍는 이유: 요청과 실제가 갈렸을 때 「clamp 이 먹었다」와
        // 「목록이 아직 안 찼다」를 구분하는 유일한 단서다.
        let rows = match tab {
            SideTab::Sessions => self.sessions_col.view.len(),
            SideTab::Mcp => self.mcp_col.row_rects.len(),
            SideTab::Info => self.info.proc_rects.len(),
            SideTab::Git => self.git.col_file_rects.len(),
            SideTab::Persona => self.persona.hits.len(),
            SideTab::Machines => self.info.machines_col.btn_rects.len(),
        };
        let (vis_h, content_h) = self.git.col_list_extent;
        // 펼침·캐시를 함께 찍는다: 내용 높이가 안 자랐을 때 「diff 를 안 펼쳤다」와
        // 「펼쳤는데 캐시가 아직 안 왔다」는 화면으로 구분이 안 된다(둘 다 파일 목록만
        // 보인다).
        eprintln!(
            "[autocolscroll] 요청={px} 실제={actual} 행={rows} git기하=(보임 {vis_h:.0} / 내용 {content_h:.0} / 상한 {:.0}) 펼침={} 캐시={}",
            (content_h - vis_h).max(0.0),
            self.git.col_expanded.len(),
            self.git.col_diff_cache.len()
        );
    }
    /// `KASATERM_AUTOCOMMITSDRAG="<px>"` (+ `_MS`) — 칼럼 발치 「최근 커밋」 구역의
    /// 손잡이를 그 시각에 **실제로 잡아** 위로 `px` 만큼 끌고 놓는다(양수면 커진다).
    ///
    /// 높이를 직접 대입하지 않는 건 손잡이 자리 판정까지 지나야 하기 때문이다 —
    /// rect 가 어긋나 있으면 높이는 멀쩡히 바뀌는데 실제 클릭은 그 아래 첫 커밋 행에
    /// 먹히고, 그 어긋남은 스크린샷에 안 찍힌다.
    ///
    /// 늘어난 만큼 커밋이 더 오는지는 **이 함수가 끝난 뒤** 확인해야 한다. 요청 개수는
    /// 다음 렌더가 쓰고 목록은 폴러(1.2초)가 채우므로, 캡처는 넉넉히 뒤에 걸 것.
    pub(crate) fn run_pending_autocommitsdrag(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<(Instant, f32)>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let px: f32 = std::env::var("KASATERM_AUTOCOMMITSDRAG").ok()?.trim().parse().ok()?;
            let ms: u64 = std::env::var("KASATERM_AUTOCOMMITSDRAG_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(6000);
            Some((Instant::now() + std::time::Duration::from_millis(ms), px))
        });
        let Some((due, px)) = *due else { return };
        if Instant::now() < due || FIRED.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(gr) = self.git.col_commits_grip else {
            eprintln!("[autocommitsdrag] 손잡이 없음 — Git 탭이 아니거나 커밋 목록이 비었다");
            return;
        };
        let (x, y) = (gr.0 + gr.2 / 2.0, gr.1 + gr.3 / 2.0);
        let pressed = self.commits_grip_press(x, y);
        let moved = self.commits_grip_drag(y - px);
        let released = self.commits_grip_release();
        // 요청 개수는 아직 옛값이다(렌더가 다음 프레임에 쓴다) — 그래서 여기선 높이만
        // 믿을 값이고, 개수는 캡처한 그림으로 센다.
        eprintln!(
            "[autocommitsdrag] 손잡이=({x:.0},{y:.0}) 누름={pressed} 끌림={moved:?} 놓음={released} 높이={:?}",
            self.git.col_commits_h
        );
        self.chrome_dirty = true;
        self.render_frame();
    }
    /// `KASATERM_FORCE_HANDLE_MENU=*` → 활성 pane 의 ⋮ 메뉴를 연다. 이 env 는
    /// 생성자에서 pane id 를 그대로 받는데(main.rs), 로컬 PTY 모드의 leaf id 는
    /// 곧 셸 pid 라 실행 전에는 알 수가 없다 — 그래서 `*` 만 여기서 한 번
    /// 실제 id 로 바꿔 준다. pane 이 생긴 뒤에 도는 틱에서 호출된다.
    pub(crate) fn resolve_force_handle_menu(&mut self) {
        if self.handle_menu.as_deref() != Some("*") {
            return;
        }
        let Some(id) = self.ws.lock().unwrap().active_pane.clone() else { return };
        self.handle_menu = Some(id);
        self.chrome_dirty = true;
    }
    /// `KASATERM_AUTOMENUPICK=<idx>` — 열려 있는 ⋮ 메뉴의 idx 번째 항목을
    /// **진짜 클릭**한다(`KASATERM_FORCE_HANDLE_MENU=*` 로 연 뒤). 화면
    /// 새로고침처럼 "깨진 화면을 고치는" 동작은 고쳐지는 걸 캡처로 봐야
    /// 검증이 되는데, winit `KeyEvent` 는 외부에서 만들 수 없어 단축키로는
    /// 하네스를 못 짠다 — 마우스 경로가 유일한 자동 검증 통로다.
    pub(crate) fn run_pending_automenuclick(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        static DUE: OnceLock<Option<(Instant, usize)>> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            let idx = std::env::var("KASATERM_AUTOMENUPICK")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())?;
            let ms = std::env::var("KASATERM_AUTOMENUCLICK_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(5000);
            Some((Instant::now() + std::time::Duration::from_millis(ms), idx))
        });
        let Some((due, idx)) = *due else { return };
        if DONE.load(Ordering::Relaxed) || Instant::now() < due {
            return;
        }
        let Some(wid) = self.window.as_ref().map(|w| w.id()) else { return };
        let Some(&(act, r)) = self.handle_menu_hits.get(idx) else {
            eprintln!(
                "[automenuclick] idx{idx} 없음 (rects={})",
                self.handle_menu_hits.len()
            );
            DONE.store(true, Ordering::Relaxed);
            return;
        };
        DONE.store(true, Ordering::Relaxed);
        self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        for state in [ElementState::Pressed, ElementState::Released] {
            self.window_event(
                event_loop,
                wid,
                WindowEvent::MouseInput {
                    device_id: DeviceId::dummy(),
                    state,
                    button: MouseButton::Left,
                },
            );
        }
        eprintln!("[automenuclick] idx{idx} {act:?} 클릭 @({:.0},{:.0})", self.cursor_px.0, self.cursor_px.1);
    }
    /// `KASATERM_AUTOHDRMENU_MS` — 헤더 우클릭 → ⋮ 메뉴 경로를 통째로 검증한다.
    /// 0: 활성 pane 헤더 켜기 → 1: 헤더 띠 중앙 **진짜 우클릭**(winit MouseInput
    /// 을 window_event 로) → 메뉴가 열렸는지 → 2: 메뉴의 상단바 토글 항목 좌클릭
    /// → 헤더가 꺼졌는지. 상태를 손으로 안 세팅하고 이벤트를 흘리는 이유는
    /// automenuclick 과 같다 — handler 디스패치까지 타야 잡히는 버그가 있다.
    /// 부팅 기본인 홀 pane 에서 돌린다 — 분할 게이트가 있는 `header_at_px` 를
    /// 우클릭이 썼다면 여기서 잡힌다.
    pub(crate) fn run_pending_autohdrmenu(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::OnceLock;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static STEP: AtomicUsize = AtomicUsize::new(0);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOHDRMENU_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        let step = STEP.load(Ordering::Relaxed);
        if step > 2
            || Instant::now() < *due + std::time::Duration::from_millis(900 * step as u64)
        {
            return;
        }
        STEP.store(step + 1, Ordering::Relaxed);
        let Some(target) = self.ws.lock().unwrap().active_pane.clone() else { return };
        let Some(wid) = self.window.as_ref().map(|w| w.id()) else { return };
        match step {
            0 => {
                self.toggle_pane_header(&target);
                let on = self
                    .ws
                    .lock()
                    .unwrap()
                    .panes
                    .get(&target)
                    .is_some_and(|p| p.has_header());
                eprintln!("[autohdrmenu] 0 헤더 켬 has_header={on}");
            }
            1 => {
                let (cols, rows) = self.window_cells();
                let pad = WINDOW_PADDING + self.effective_sidebar_w();
                let Some((_, rx, ry, rw, _)) = self
                    .effective_leaf_rects(cols, rows)
                    .into_iter()
                    .find(|(i, ..)| i == &target)
                else {
                    eprintln!("[autohdrmenu] 1 대상 rect 없음");
                    return;
                };
                let x = pad + (rx as f32 + rw as f32 / 2.0) * self.cell.w;
                let y = TITLE_HEIGHT + ry as f32 * self.cell.h + PANE_HEADER_HEIGHT / 2.0;
                self.cursor_px = (x, y);
                for state in [ElementState::Pressed, ElementState::Released] {
                    self.window_event(
                        event_loop,
                        wid,
                        WindowEvent::MouseInput {
                            device_id: DeviceId::dummy(),
                            state,
                            button: MouseButton::Right,
                        },
                    );
                }
                eprintln!(
                    "[autohdrmenu] 1 우클릭@({x:.0},{y:.0}) menu={:?}",
                    self.handle_menu
                );
            }
            _ => {
                let Some(&(_, r)) = self
                    .handle_menu_hits
                    .iter()
                    .find(|(a, _)| matches!(a, ActionKind::ToggleHeader))
                else {
                    eprintln!(
                        "[autohdrmenu] 2 토글 항목 없음 (rects={})",
                        self.handle_menu_hits.len()
                    );
                    return;
                };
                self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                for state in [ElementState::Pressed, ElementState::Released] {
                    self.window_event(
                        event_loop,
                        wid,
                        WindowEvent::MouseInput {
                            device_id: DeviceId::dummy(),
                            state,
                            button: MouseButton::Left,
                        },
                    );
                }
                let on = self
                    .ws
                    .lock()
                    .unwrap()
                    .panes
                    .get(&target)
                    .is_some_and(|p| p.has_header());
                eprintln!(
                    "[autohdrmenu] 2 토글 클릭 has_header={on} menu={:?}",
                    self.handle_menu
                );
            }
        }
    }
    /// `KASATERM_AUTOPILLCLICK_MS` 뒤에 타이틀바 사용량 pill 을 **진짜로 클릭**한다.
    /// 다른 probe 처럼 상태를 손으로 세팅하지 않고 winit `MouseInput` 을 그대로
    /// `window_event` 에 흘려보내 handler 디스패치까지 태운다 — "render 는 그렸는데
    /// handler 가 안 잡는다"(⋮ 메뉴의 상단바 토글이 실제로 그랬다) 종류의 버그는
    /// 이 경로로만 잡히기 때문이다. 두 번째 클릭 좌표를 `KASATERM_AUTOPILLPICK`
    /// (드롭다운 행 인덱스)으로 주면 그 항목까지 눌러 전환 결과를 확인한다.
    /// Function-local statics — struct App 은 건드리지 않는다(병렬 작업 규칙).
    pub(crate) fn run_pending_autopillclick(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicU8, Ordering};
        use std::sync::OnceLock;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static STEP: AtomicU8 = AtomicU8::new(0);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOPILLCLICK_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        let step = STEP.load(Ordering::Relaxed);
        // 단계마다 800ms 간격 — 클릭 결과가 다음 프레임에 그려질 시간을 준다.
        if Instant::now() < *due + std::time::Duration::from_millis(800 * step as u64) {
            return;
        }
        let Some(wid) = self.window.as_ref().map(|w| w.id()) else { return };
        let click = |app: &mut Self, x: f32, y: f32| {
            app.cursor_px = (x, y);
            for state in [ElementState::Pressed, ElementState::Released] {
                app.window_event(
                    event_loop,
                    wid,
                    WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button: MouseButton::Left },
                );
            }
        };
        match step {
            0 => {
                // 손잡이가 둘이다 — Info 탭 pill(기본)과 늘 보이는 상태줄 세그먼트.
                // `KASATERM_AUTOPILLCLICK=status` 로 후자를 누른다. 두 손잡이는 창의
                // 위/아래 끝이라 메뉴가 펼쳐질 방향이 반대고, 그 방향 계산이 실제로
                // 틀렸던 자리다(상태줄에서 열면 메뉴가 창 밖으로 나갔다).
                let handle = std::env::var("KASATERM_AUTOPILLCLICK").unwrap_or_default();
                let (name, rect) = if handle == "status" {
                    ("status", self.status_account_rect)
                } else {
                    ("chip", self.account_chip_rect)
                };
                let Some(r) = rect else {
                    eprintln!("[autopillclick] {name} rect 없음 — 손잡이가 안 그려졌다");
                    STEP.store(9, Ordering::Relaxed);
                    return;
                };
                let (x, y) = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                click(self, x, y);
                // 행 y 범위를 함께 찍는다 — 메뉴가 «열렸는데 화면 밖»인 경우
                // rows 개수만 봐서는 성공과 구분이 안 된다.
                let span = match (
                    self.account_menu_hits.first(),
                    self.account_menu_hits.last(),
                ) {
                    (Some((_, a)), Some((_, b))) => format!("y={:.0}..{:.0}", a.1, b.1 + b.3),
                    _ => "y=-".to_string(),
                };
                eprintln!(
                    "[autopillclick] {name}({x:.0},{y:.0}) 클릭 → account_menu={} rows={} {span}",
                    self.account_menu,
                    self.account_menu_hits.len()
                );
                STEP.store(1, Ordering::Relaxed);
            }
            n => {
                // 픽은 쉼표로 여러 개를 준다 — Orca 구조는 「제공자 행 → 서브메뉴 행」
                // 2단이라 한 번의 클릭으로는 전환까지 못 간다.
                let picks = std::env::var("KASATERM_AUTOPILLPICK").unwrap_or_default();
                let picks: Vec<usize> =
                    picks.split(',').filter_map(|s| s.trim().parse::<usize>().ok()).collect();
                match picks.get(n as usize - 1) {
                    Some(&i) => {
                        match self.account_menu_hits.get(i).map(|(_, r)| *r) {
                            Some(r) => {
                                click(self, r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                                eprintln!(
                                    "[autopillclick] row{i} 클릭 → claude='{}' codex='{}' menu={} rows={}",
                                    self.set_claude_account,
                                    self.set_codex_account,
                                    self.account_menu,
                                    self.account_menu_hits.len()
                                );
                            }
                            None => eprintln!("[autopillclick] row{i} 없음"),
                        }
                        STEP.store(n + 1, Ordering::Relaxed);
                    }
                    None => STEP.store(9, Ordering::Relaxed),
                }
            }
        }
    }
    /// Headless inline-settings repro: open settings after
    /// `KASATERM_AUTOSETTINGS_MS`, on the category named in `KASATERM_AUTOSETTINGS`
    /// ("appearance" / "shell" / "claude" / "students" / "feedback", default General), then arm
    /// the requested delay and apply an optional settings action.
    pub(crate) fn run_pending_autosettings(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOSETTINGS_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let cat_env = std::env::var("KASATERM_AUTOSETTINGS").unwrap_or_default();
        let cat = match cat_env.as_str() {
            "appearance" => SettingsCat::Appearance,
            "shell" => SettingsCat::Shell,
            "claude" => SettingsCat::Claude,
            "theme" => SettingsCat::Theme,
            // 옛 이름 그대로지만 이제 진짜 자기 칸이 있다(2026-08-26 분리) —
            // 이 env 로 「캐릭터 목록」을 보던 밖의 스크립트는 그대로 맞는 화면에 온다.
            "students" => SettingsCat::Students,
            "feedback" => SettingsCat::Feedback,
            _ => SettingsCat::General,
        };
        // 딥링크 검증: KASATERM_AUTOSETTINGS_STUDENT 로 특정 학생 선택 상태(=프사
        // 클릭 결과)를 헤드리스로 재현 — persona 편집기가 뜬 화면을 캡처한다.
        let student = std::env::var("KASATERM_AUTOSETTINGS_STUDENT")
            .ok()
            .filter(|s| !s.is_empty());
        eprintln!("[autosettings] open settings window cat={cat_env} student={student:?}");
        self.open_settings_window(event_loop, Some(cat), student);
        // 클릭이 유일한 진입점인 동작을 헤드리스로 실행한다. 테마 복제는 누르기
        // 전엔 아무 흔적도 안 남겨서, 이 손잡이가 없으면 「눌러 봤다」를 사람 손으로만
        // 확인할 수 있다. UI 래퍼(토스트·폴더 열기)는 건너뛰고 파일을 쓰는 부분만
        // 부른다 — 검증 때마다 Finder 창이 튀어나오면 그게 더 방해다.
        match std::env::var("KASATERM_AUTOSETTINGS_ACTION").unwrap_or_default().as_str() {
            "" => {}
            // 파일 쓰기만 부르지 않고 UI 액션을 그대로 태운다 — 복제는 만든 뒤
            // 이름 칸에 포커스를 옮기는 것까지가 한 동작이라, `create_theme` 만
            // 부르면 정작 사람이 겪는 절반을 안 지나간다.
            // 원본 뷰는 전환을 눌러야 열린다. 상태만 켜지 않고 액션을 그대로
            // 태우는 이유는 버퍼 채우기(reload_student_raw)가 그 액션 안에 있어서다
            // — 플래그만 세우면 빈 편집기를 찍고는 "잘 뜬다"고 읽게 된다.
            "student-raw" => {
                eprintln!("[autosettings] 원본 뷰 열기");
                self.settings_apply(SettingsAction::ToggleStudentRaw(true));
            }
            "student-raw-yaml" => {
                eprintln!("[autosettings] 원본 뷰 열기(YAML)");
                self.settings_apply(SettingsAction::ToggleStudentRaw(true));
                self.settings_apply(SettingsAction::StudentRawFormat(true));
            }
            "export-theme" => {
                eprintln!("[autosettings] 새 테마 만들기");
                self.settings_apply(SettingsAction::ExportTheme);
            }
            // `rename-theme:<id>=<새 이름>` — 이름 칸을 포커스하고 버퍼를 채운 뒤
            // 커밋까지. 키 이벤트 경로가 헤드리스엔 없어 버퍼를 직접 심는다.
            a if a.starts_with("rename-theme:") => {
                let rest = a.trim_start_matches("rename-theme:");
                let (id, label) = rest.split_once('=').unwrap_or((rest, ""));
                eprintln!("[autosettings] 테마 이름 '{id}' → '{label}'");
                self.settings_apply(SettingsAction::FocusThemeLabel(id.to_string()));
                if let Some((_, buf)) = self.theme_label_edit.as_mut() {
                    *buf = label.to_string();
                }
                self.flush_theme_label();
            }
            a if a.starts_with("delete-theme:") => {
                let id = a.trim_start_matches("delete-theme:").to_string();
                eprintln!("[autosettings] 테마 치우기 '{id}'");
                self.settings_apply(SettingsAction::DeleteTheme(id));
            }
            // `rename-student:<새 이름>` — 상세 화면에 열린 캐릭터의 이름을 바꾼다.
            // 이름은 로스터의 **키**라 잘못 쓰면 그 캐릭터가 통째로 사라지는데,
            // 그걸 막는 방어(빈 이름·중복)가 실제로 먹는지는 이 손잡이로만 잰다.
            a if a.starts_with("rename-student:") => {
                let label = a.trim_start_matches("rename-student:").to_string();
                eprintln!("[autosettings] 캐릭터 이름 → '{label}'");
                self.settings_apply(SettingsAction::FocusStudentName);
                self.students_name = label;
                self.flush_student_name();
                eprintln!("[autosettings] 저장된 이름 = {:?}", self.students_selected);
            }
            "close-student" => {
                eprintln!("[autosettings] 캐릭터 목록으로");
                self.settings_apply(SettingsAction::CloseStudent);
            }
            // `select-theme:<id>` — 빈 id 는 번들로 되돌린다. 전환은 캐시 셋을 함께
            // 비워야 화면이 한 테마로 보이는데, 그게 실제로 먹었는지는 전환 **뒤**
            // 그린 화면으로만 확인된다(캡처가 이 다음에 걸린다).
            a if a.starts_with("select-theme:") => {
                let id = a.trim_start_matches("select-theme:").to_string();
                eprintln!("[autosettings] 테마 전환 → '{id}'");
                self.settings_apply(SettingsAction::SelectTheme(id));
            }
            // 팔레트 편집 딥링크 — 복제 입구를 누른 뒤의 편집 그리드를 캡처한다.
            // 이 화면은 custom 테마일 때만 열려서, 이 손잡이 없이는 캡처 한 장에
            // 클릭이 한 번 끼어야 한다.
            "start-custom-theme" => {
                eprintln!("[autosettings] 커스텀 팔레트 시작");
                self.settings_apply(SettingsAction::StartCustomTheme);
            }
            // `focus-palette:<i>` — 팔레트 칸 i 의 hex 필드가 포커스된 상태.
            a if a.starts_with("focus-palette:") => {
                if let Ok(i) = a.trim_start_matches("focus-palette:").parse::<usize>() {
                    eprintln!("[autosettings] 팔레트 칸 {i} 포커스");
                    self.settings_apply(SettingsAction::StartCustomTheme);
                    self.settings_apply(SettingsAction::FocusPaletteHex(i));
                }
            }
            // `picker-probe:<i>` — 클릭 없이 픽 로직만 검증: Hue 1/3(=120°),
            // SV (0.75, 위에서 1/4) 를 찍고 결과 hex 를 로그로 남긴다.
            // 기대값 #30bf30 — hsv(120, .75, .75). 캡처는 렌더를, 이건 수학을 본다.
            a if a.starts_with("picker-probe:") => {
                if let Ok(i) = a.trim_start_matches("picker-probe:").parse::<usize>() {
                    self.settings_apply(SettingsAction::StartCustomTheme);
                    self.settings_apply(SettingsAction::FocusPaletteHex(i));
                    let r = (0.0, 0.0, 300.0, 300.0);
                    self.picker_pick(&SettingsAction::PickerHue, r, (100.0, 8.0));
                    self.picker_pick(&SettingsAction::PickerSV, r, (225.0, 75.0));
                    eprintln!(
                        "[picker-probe] 칸 {i} hsv={:?} hex={}",
                        self.set_picker_hsv, self.set_palette_edit
                    );
                }
            }
            other => eprintln!("[autosettings] 모르는 액션 '{other}'"),
        }
        // 피드백 본문은 키 이벤트로만 채워지는데 헤드리스엔 그 경로가 없다 —
        // 버퍼를 직접 심어 wrap·캐럿·활성 버튼을 캡처로 본다.
        // KASATERM_AUTOFEEDBACK_SAVE=1 이면 저장까지 눌러, 캡처엔 비워진 폼과
        // 토스트가 남는다(파일이 실제로 떨어졌는지는 폴더로 확인).
        // 한글 조합 검증: KASATERM_AUTOSETTINGS_TYPE 의 자모를 계정 이름 필드에
        // 한 글자씩 먹여, 조합기가 완성 음절을 만드는지 낱자로 흘리는지 찍는다.
        // 실제 IME 없이 재현할 수 있는 건 macOS 가 OS IME 를 끄고 자모를 그대로
        // 받기 때문 — 그 경로가 곧 거노가 치는 경로다.
        // 배율/폰트를 흐트러뜨린 뒤 "1:1 로 되돌리기"가 둘 다 되돌리는지. 되돌린
        // 값이 맞아도 격자를 다시 안 재면 화면만 옛 크기로 남으므로 cells 도 찍는다.
        if std::env::var("KASATERM_AUTOSETTINGS_RESET").is_ok() {
            self.change_ui_zoom(0.3);
            self.font_size = 22.0;
            self.apply_effective_scale();
            eprintln!(
                "[autoreset] 흐트러뜨림: zoom={:.2} font={} cells={:?}",
                self.ui_zoom,
                self.font_size,
                self.window_cells()
            );
            self.settings_apply(crate::SettingsAction::ResetScale);
            eprintln!(
                "[autoreset] 되돌린 뒤: zoom={:.2} font={} cells={:?}",
                self.ui_zoom,
                self.font_size,
                self.window_cells()
            );
        }
        if let Ok(t) = std::env::var("KASATERM_AUTOFEEDBACK_TEXT") {
            self.feedback_caret = t.chars().count();
            self.feedback_body = t;
            self.settings_input = Some(SettingsInput::FeedbackBody);
            if std::env::var("KASATERM_AUTOFEEDBACK_SAVE").is_ok_and(|v| v == "1") {
                self.save_feedback();
            }
        }
    }
    /// Headless raw-editor selection seed: KASATERM_TEST_MD_SELECT="al,ac,cl,cc"
    /// plants a selection (anchor line/col → cursor line/col) on the active
    /// raw editor after KASATERM_TEST_MD_SELECT_MS (default 6000 — pair with
    /// KASATERM_AUTOOPEN so the editor exists first). Mouse drags aren't
    /// injectable headlessly; this lets a capture prove the selection band.
    /// Function-local statics — no App field (parallel-work rule).
    pub(crate) fn run_pending_automdselect(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_TEST_MD_SELECT").ok().map(|_| {
                let ms: u64 = std::env::var("KASATERM_TEST_MD_SELECT_MS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(6000);
                Instant::now() + std::time::Duration::from_millis(ms)
            })
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let spec = std::env::var("KASATERM_TEST_MD_SELECT").unwrap_or_default();
        let nums: Vec<usize> = spec.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        let [al, ac, cl, cc] = nums[..] else {
            eprintln!("[automdselect] expected al,ac,cl,cc — got {spec:?}");
            return;
        };
        {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.active_mut() else { return };
            pane.dirty = true;
            if let Some(m) = pane.markdown_mut() {
                m.sel_anchor = Some((al, ac));
                m.cur_line = cl;
                m.cur_col = cc;
            }
        }
        self.md_ensure_caret_visible();
        eprintln!("[automdselect] anchor=({al},{ac}) cursor=({cl},{cc})");
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Headless 편집기 스크립트: `KASATERM_TEST_MD_SCRIPT` 에 `|` 로 이은 단계를
    /// `KASATERM_TEST_MD_STEP_MS`(기본 700) 간격으로 하나씩 실행한다. 첫 단계는
    /// `KASATERM_TEST_MD_SCRIPT_MS`(기본 5000, `KASATERM_AUTOOPEN` 이 편집기를
    /// 띄운 뒤여야 한다) 에 시작.
    ///
    /// 단계: `scroll:<px>` 절대 스크롤 · `mode:raw|render` 토글 · `cap:<경로>` 캡처.
    /// 키 입력이 아니라 상태를 직접 건드리는 이유는 winit `KeyEvent` 가 밖에서
    /// 만들 수 없어서다(비공개 필드) — 키 경로 자체는 유닛 테스트가 맡는다.
    /// autosettings 처럼 함수-로컬 static 이라 `struct App` 은 안 건드린다.
    pub(crate) fn run_pending_automdscript(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::OnceLock;
        static PLAN: OnceLock<Option<(Instant, u64, Vec<String>)>> = OnceLock::new();
        static DONE: AtomicUsize = AtomicUsize::new(0);
        // 이 함수는 about_to_wait 에서만 도는데, 앱은 할 일이 없으면 `Wait` 로
        // 완전히 잠들어 about_to_wait 자체가 안 돈다. 그러면 다음 단계 시각이
        // 와도 아무도 깨우지 않아 스크립트가 중간에 멎는다. 남은 단계가 있는
        // 동안은 펌프를 켠다(`MDSCRIPT_LEFT`).
        //
        // 그리고 **밀린 단계는 한 번에 다 소화한다.** 한 패스에 한 단계만 처리하면
        // 스크립트가 사실상 "프레임 수"로 페이싱된다 — 디버그 빌드의 raw 편집기는
        // 한 프레임이 swash 글리프 힌팅에 수 초를 쓰므로(샘플러로 확인), 같은
        // 스크립트가 판마다 3~7단계에서 제멋대로 끊겼다. 코드 문제로 보였지만
        // 실은 하네스가 프레임을 못 따라간 것이다.
        let plan = PLAN.get_or_init(|| {
            let spec = std::env::var("KASATERM_TEST_MD_SCRIPT").ok()?;
            let start: u64 = std::env::var("KASATERM_TEST_MD_SCRIPT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5000);
            let step: u64 = std::env::var("KASATERM_TEST_MD_STEP_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(700);
            let steps: Vec<String> = spec.split('|').map(|s| s.trim().to_string()).collect();
            Some((
                Instant::now() + std::time::Duration::from_millis(start),
                step,
                steps,
            ))
        });
        let Some((start, step_ms, steps)) = plan else { return };
        loop {
            let n = DONE.load(Ordering::Relaxed);
            MDSCRIPT_LEFT.store(n < steps.len(), Ordering::Relaxed);
            if n >= steps.len() {
                return;
            }
            let due = *start + std::time::Duration::from_millis(*step_ms * n as u64);
            if Instant::now() < due {
                return;
            }
            DONE.store(n + 1, Ordering::Relaxed);
            // 마지막 단계가 캡처면 거기서 멈춘다 — 캡처는 *다음* 프레임에
            // 찍히므로, 뒤에 밀린 단계를 이어서 돌리면 찍히기도 전에 화면이
            // 바뀐다.
            let stop_after = steps[n].starts_with("cap:");
            self.run_one_mdstep(&steps[n].clone(), event_loop);
            if stop_after {
                return;
            }
        }
    }

    fn run_one_mdstep(&mut self, step: &str, event_loop: &ActiveEventLoop) {
        let step = step.to_string();
        // pane 이 필요 없는 단계를 먼저 — 닫기 확인을 해소한 뒤엔 마크다운 pane
        // 자체가 사라지는데, 정작 그 "닫힌 뒤 화면"이 캡처하고 싶은 것이다.
        match step.split_once(':') {
            Some(("cap", p)) => {
                // `pending_capture` 큐를 거치지 않고 바로 무장한다 — 그 큐의 드레인은
                // 이 함수보다 **앞에서** 돌아, 큐에 넣으면 빨라야 다음 패스에나
                // 집히고 그 사이에 다음 단계가 끼어들면 바뀐 화면이 찍힌다
                // (실제로 raw 캡처가 render 로 되돌린 뒤 화면을 담았다).
                if let Some(g) = self.gpu.as_mut() {
                    g.capture_next = Some(p.to_string());
                }
                eprintln!("[mdscript] cap → {p}");
                self.wake_after_mdstep();
                return;
            }
            // 활성 탭 바꾸기 — `tab:<idx>`. `KASATERM_AUTOOPEN` 은 에이전트 경로
            // (`as_tab`)라 탭을 **앞으로 끌어내지 않는다**(사람이 파일트리에서
            // 누른 것만 그렇게 한다). 그래서 이 단계가 없으면 뒤에 오는 편집기
            // 단계들이 전부 「마크다운 pane 없음」으로 조용히 빠진다.
            Some(("tab", v)) => {
                let idx: usize = v.trim().parse().unwrap_or(0);
                let picked = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
                if let (Some(id), Ok(mut ws)) = (picked, self.ws.lock()) {
                    if let Some(p) = ws.panes.get_mut(&id) {
                        p.active_tab = idx.min(p.tabs.len().saturating_sub(1));
                        p.dirty = true;
                        eprintln!("[mdscript] tab={} of {}", p.active_tab, p.tabs.len());
                    }
                }
                self.chrome_dirty = true;
                self.wake_after_mdstep();
                return;
            }
            // 모달 버튼 누르기 — 저장/저장 안 함/취소.
            Some(("pick", v)) => {
                let btn = match v {
                    "save" => ConfirmBtn::Save,
                    "cancel" => ConfirmBtn::Cancel,
                    _ => ConfirmBtn::Close,
                };
                self.confirm_dialog_pick(btn, event_loop);
                eprintln!("[mdscript] pick={v} modal_left={}", self.confirm_close.is_some());
                self.wake_after_mdstep();
                return;
            }
            _ => {}
        }
        // 활성 pane 이 아니라 **마크다운 pane** 을 찾는다. 옆 셸이 먼저 죽으면
        // 포커스가 그쪽으로 넘어가고, 그러면 단계들이 아무 말 없이 반환돼
        // 스크립트가 중간에 멈춘 것처럼 보였다(실제로 4단계에서 끊겼다).
        let Some(id) = self.ws.lock().ok().and_then(|w| {
            let act = w.active_pane.clone();
            let is_md = |i: &String| w.panes.get(i).is_some_and(|p| p.markdown().is_some());
            act.filter(&is_md)
                .or_else(|| w.panes.keys().find(|i| is_md(i)).cloned())
        }) else {
            eprintln!("[mdscript] {step}: 마크다운 pane 없음");
            return;
        };
        match step.split_once(':') {
            Some(("scroll", v)) => {
                let px: f32 = v.parse().unwrap_or(0.0);
                if let Ok(mut ws) = self.ws.lock() {
                    if let Some(pane) = ws.panes.get_mut(&id) {
                        pane.dirty = true;
                        if let Some(m) = pane.markdown_mut() {
                            m.scroll = px;
                        }
                    }
                }
                eprintln!("[mdscript] scroll={px}");
            }
            // 렌더 뷰 선택을 문서 좌표로 직접 세운다 — `sel:<ax>,<ay>,<bx>,<by>`.
            // 마우스 드래그는 밖에서 만들 수 없어(winit) 상태를 세워 띠 렌더와
            // 복사 추출만 확인한다. `selcopy` 는 그 결과를 로그로 찍는다(클립보드는
            // 건드리지 않는다 — 검증이 사용자 클립보드를 덮으면 안 된다).
            Some(("sel", v)) => {
                let n: Vec<f32> =
                    v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                if n.len() != 4 {
                    eprintln!("[mdscript] sel: 좌표 4개 필요(ax,ay,bx,by)");
                    return;
                }
                self.md_render_sel = Some(crate::MdRenderSel {
                    pane: id.clone(),
                    anchor: (n[0], n[1]),
                    end: (n[2], n[3]),
                    dragging: false,
                });
                if let Ok(mut ws) = self.ws.lock() {
                    if let Some(pane) = ws.panes.get_mut(&id) {
                        pane.dirty = true;
                    }
                }
                eprintln!("[mdscript] sel=({},{})-({},{})", n[0], n[1], n[2], n[3]);
            }
            Some(("mode", v)) => {
                let want_raw = v == "raw";
                self.set_md_mode(&id, want_raw);
                let at = self.md_anchor_line(&id);
                eprintln!("[mdscript] mode={v} anchor_line={at:?}");
            }
            // 거터 표시가 실제로 섰는지를 **개수로** 찍는다. 스샷만 보면 「바가
            // 안 보인다」와 「diff 가 비었다」가 같아 보인다 — 원인이 렌더인지
            // 계산인지 가르려면 이 줄이 있어야 한다.
            _ if step == "diff" => {
                // 표시는 틱에서 뜬다. 스크립트가 편집 직후에 물으면 아직 없을 수
                // 있어, 여기서 한 번 밀어 준 뒤 읽는다.
                self.diff_refresh();
                let st = self.ws.lock().ok().and_then(|w| {
                    w.panes.get(&id).and_then(|p| p.markdown()).map(|m| {
                        let head = match &m.diff_head {
                            None => "미시도".to_string(),
                            Some(crate::gitdiff::HeadText::Absent) => "HEAD없음".to_string(),
                            Some(crate::gitdiff::HeadText::Lines(l)) => format!("{}줄", l.len()),
                        };
                        let (marks, dels, hunks) = m.diff.as_ref().map_or((0, 0, 0), |d| {
                            (
                                d.marks.iter().filter(|x| x.is_some()).count(),
                                d.dels.len(),
                                d.hunks.len(),
                            )
                        });
                        (head, marks, dels, hunks, m.diff_peek)
                    })
                });
                // 본문 박스도 함께 — `click:` 좌표가 본문 기준 상대라, [되돌리기]
                // 처럼 오른쪽 끝에 붙는 것을 치려면 폭을 알아야 한다.
                let box_ = self.md_body_rects.get(&id).copied();
                // 행 높이·화면 행 수도 — `click:<dx>,<dy>` 의 dy 는 이 둘 없이는
                // 세울 수 없다(랩이 켜지면 화면 행이 버퍼 줄보다 많아진다).
                let geom = self.gpu.as_mut().map(|g| g.raw_editor_metrics());
                eprintln!(
                    "[mdscript] diff head/마커/삭제/헝크/펼침={st:?} 본문={box_:?} (pad,lh)={geom:?}"
                );
            }
            // Raw 편집기 클릭 → 캐럿. 좌표는 **본문 박스 기준 상대 px**
            // (`click:<dx>,<dy>`) 이라 창 크기가 달라도 같은 자리를 가리킨다.
            // 마우스 이벤트를 밖에서 만들 수 없어 히트테스트 진입점을 직접 부른다.
            Some(("click", v)) => {
                let (dx, dy) = v.split_once(',').unwrap_or((v, "0"));
                let (dx, dy): (f32, f32) =
                    (dx.trim().parse().unwrap_or(0.0), dy.trim().parse().unwrap_or(0.0));
                let Some(&(bx, by, _, _)) = self.md_body_rects.get(&id) else {
                    eprintln!("[mdscript] click: 본문 박스 없음(raw 모드인지 확인)");
                    return;
                };
                // 실물 press 경로와 같은 함수를 쓴다 — 캐럿·드래그 앵커·연타
                // 선택의 순서가 계약이라, 여기서 md_click_caret 만 부르면
                // 더블클릭이 재현되지 않아 검증이 실물과 어긋난다. 같은
                // 좌표로 짧은 간격(`_STEP_MS` 450 이하)에 두 번 주면 단어
                // 선택, 세 번이면 줄 선택이 걸린다.
                // 실물 press 와 같은 순서: 변경 바 → 접기 삼각형 → 캐럿. 하나라도
                // 빼면 하네스가 다른 클릭을 캐럿 클릭으로 재현해 검증이 거짓말을 한다.
                if self.md_diff_click(&id, bx + dx, by + dy) {
                    let st = self.ws.lock().ok().and_then(|w| {
                        w.panes.get(&id).and_then(|p| p.markdown()).map(|m| {
                            (m.diff_peek, m.edit_lines.len(), m.cur_line)
                        })
                    });
                    eprintln!("[mdscript] diff click=({dx},{dy}) peek/줄수/캐럿={st:?}");
                    self.wake_after_mdstep();
                    return;
                }
                if self.md_fold_click(&id, bx + dx, by + dy) {
                    let f = self.ws.lock().ok().and_then(|w| {
                        w.panes.get(&id).and_then(|p| p.markdown()).map(|m| m.folds.clone())
                    });
                    eprintln!("[mdscript] fold click=({dx},{dy}) folds={f:?}");
                    self.wake_after_mdstep();
                    return;
                }
                let clicks = self.md_press_caret(&id, bx + dx, by + dy);
                let at = self.ws.lock().ok().and_then(|w| {
                    w.panes
                        .get(&id)
                        .and_then(|p| p.markdown())
                        .map(|m| (m.cur_line, m.cur_col, m.sel_anchor, m.selected_text()))
                });
                eprintln!("[mdscript] click=({dx},{dy}) clicks={clicks} caret={at:?}");
            }
            // 누른 채로 끌고 간 자리 — `drag:<dx>,<dy>`(앞에 `click:` 이 앵커를
            // 세워 둔 뒤에 쓴다). CursorMoved 의 드래그 갈래가 부르는 것과 같은
            // 함수라, 접힌 줄을 가로지르는 선택 밴드도 실물과 같은 경로로 선다.
            Some(("drag", v)) => {
                let (dx, dy) = v.split_once(',').unwrap_or((v, "0"));
                let (dx, dy): (f32, f32) =
                    (dx.trim().parse().unwrap_or(0.0), dy.trim().parse().unwrap_or(0.0));
                let Some(&(bx, by, _, _)) = self.md_body_rects.get(&id) else {
                    eprintln!("[mdscript] drag: 본문 박스 없음(raw 모드인지 확인)");
                    return;
                };
                self.md_click_caret(&id, bx + dx, by + dy);
                let at = self.ws.lock().ok().and_then(|w| {
                    w.panes
                        .get(&id)
                        .and_then(|p| p.markdown())
                        .map(|m| (m.cur_line, m.cur_col, m.sel_anchor))
                });
                eprintln!("[mdscript] drag=({dx},{dy}) caret={at:?}");
            }
            // Cmd+D — `occ:`. 첫 번은 캐럿 낱말, 그 뒤로는 다음 출현에 커서 추가.
            // Cmd+Opt+↑↓ 는 `vcaret:up|down`. 실물 키가 부르는 것과 같은 함수다.
            Some(("occ", _)) | Some(("vcaret", _)) => {
                let down = step.split_once(':').map(|(_, v)| v) != Some("up");
                let occ = step.starts_with("occ");
                let got = {
                    let mut ws = self.ws.lock().unwrap();
                    ws.panes.get_mut(&id).and_then(|p| {
                        p.dirty = true;
                        p.markdown_mut()
                    })
                    .map(|m| {
                        let ok =
                            if occ { m.select_next_occurrence() } else { m.add_caret_vert(down) };
                        (ok, m.carets())
                    })
                };
                eprintln!("[mdscript] {step} → {got:?}");
            }
            // 편집기에 글자를 넣어 본다 — `type:<문자열>`. 키 이벤트를 밖에서
            // 만들 수 없어(winit `KeyEvent`) 삽입 진입점을 직접 부른다.
            // 실타이핑의 비용 구조를 재려면 **한 단계에 한 글자**로 써야 한다
            // (`type:a|type:b|…`): 한 단계에 여러 글자를 넣으면 그 사이에
            // 프레임이 안 그려져 버퍼 재파싱이 한 번으로 접혀 버린다.
            Some(("type", v)) => {
                self.md_insert_into(&id, v);
                let at = self.ws.lock().ok().and_then(|w| {
                    w.panes
                        .get(&id)
                        .and_then(|p| p.markdown())
                        .map(|m| (m.cur_line, m.cur_col))
                });
                eprintln!("[mdscript] type={v:?} caret={at:?}");
            }
            // 자동완성 팝업을 캐럿 앞 낱말로 열어 본다 — `complete:`. 실물 키가
            // 부르는 것과 **같은 함수**를 직접 부른다(winit `KeyEvent` 를 밖에서
            // 못 만들어 타이핑으로는 팝업까지 갈 수 없다).
            Some(("complete", _)) => {
                let got = {
                    let mut ws = self.ws.lock().unwrap();
                    ws.panes.get_mut(&id).and_then(|p| {
                        p.dirty = true;
                        p.markdown_mut()
                    })
                    .map(|m| {
                        m.complete_refresh();
                        m.complete.as_ref().map(|c| (c.items.clone(), c.sel, c.from_col))
                    })
                };
                eprintln!("[mdscript] complete={got:?}");
                // 실물 키 경로가 하는 그대로 서버에도 물어 둔다. 응답은 틱에서
                // 받으므로 뒤에 `citems:` 단계를 두고 갈아끼워진 후보를 본다.
                self.lsp_complete_request(&id);
            }
            // 그 자리에 마우스를 멈춘 것으로 친다 — `hover:<dx>,<dy>`(본문 박스
            // 기준). 실제 커서를 밖에서 못 움직여서 상태를 직접 세우고, 멈춘
            // 시각을 과거로 둬 대기 시간을 건너뛴다. 답은 틱이 받으므로 뒤에
            // `tip:` 단계를 둔다.
            Some(("hover", v)) => {
                let (dx, dy) = v.split_once(',').unwrap_or((v, "0"));
                let (dx, dy): (f32, f32) =
                    (dx.trim().parse().unwrap_or(0.0), dy.trim().parse().unwrap_or(0.0));
                let Some(&(bx, by, _, _)) = self.md_body_rects.get(&id) else {
                    eprintln!("[mdscript] hover: 본문 박스 없음(raw 모드인지 확인)");
                    return;
                };
                let past = std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(1))
                    .unwrap_or_else(std::time::Instant::now);
                self.hover = Some(crate::HoverState {
                    at: (bx + dx, by + dy),
                    since: past,
                    req: None,
                    text: None,
                });
                self.lsp_hover_tick();
                eprintln!("[mdscript] hover=({dx},{dy}) 요청={}", self.hover.is_some());
            }
            // 지금 떠 있는 툴팁 글 — `tip:`.
            Some(("tip", _)) => {
                let t = self.hover.as_ref().and_then(|h| h.text.clone());
                eprintln!("[mdscript] tip={t:?}");
            }
            // 줄 접기(word wrap) 토글 — `wrap:`. Alt+Z 가 부르는 것과 같은
            // 상태를 직접 세운다(수정키 조합을 밖에서 만들 수 없다).
            Some(("wrap", _)) => {
                let on = {
                    let mut ws = self.ws.lock().unwrap();
                    ws.panes.get_mut(&id).and_then(|p| {
                        p.dirty = true;
                        p.markdown_mut()
                    })
                    .map(|m| {
                        m.wrap = !m.wrap;
                        if m.wrap {
                            m.h_scroll = 0.0;
                        }
                        m.wrap
                    })
                };
                eprintln!("[mdscript] wrap={on:?}");
            }
            // 정의로 뛴다 — `goto:`. Cmd+클릭이 부르는 것과 같은 함수를 직접
            // 부른다(수정키 상태를 밖에서 만들 수 없다). 응답은 틱이 받으므로
            // 뒤에 `caret:` 단계를 두고 옮겨진 자리를 본다.
            Some(("goto", _)) => {
                self.lsp_goto_request(&id);
                eprintln!("[mdscript] goto 요청");
            }
            // 지금 캐럿이 어느 파일 몇 줄인지 — `caret:`.
            Some(("caret", _)) => {
                let got = {
                    let ws = self.ws.lock().unwrap();
                    ws.active_pane
                        .as_ref()
                        .and_then(|a| ws.panes.get(a))
                        .and_then(|p| p.markdown())
                        .map(|m| {
                            (
                                m.doc.path.rsplit('/').next().unwrap_or("").to_string(),
                                m.cur_line,
                                m.cur_col,
                            )
                        })
                };
                eprintln!("[mdscript] caret={got:?}");
            }
            // 지금 팝업에 들어 있는 후보만 찍는다 — `citems:`. `complete:` 를 다시
            // 부르면 버퍼 낱말로 덮어써서 서버 답이 왔는지 알 수 없다.
            Some(("citems", _)) => {
                let got = {
                    let ws = self.ws.lock().unwrap();
                    ws.panes
                        .get(&id)
                        .and_then(|p| p.markdown())
                        .and_then(|m| m.complete.as_ref())
                        .map(|c| (c.items.clone(), c.sel, c.lsp_req))
                };
                eprintln!("[mdscript] citems={got:?}");
            }
            // LSP 진단 확인 — `diags:`. rust-analyzer 의 첫 인덱싱은 수 초~수십 초라
            // 이 단계 앞에 넉넉한 `_STEP_MS` 를 두거나 여러 번 찍어야 한다.
            Some(("diags", _)) => {
                let path = {
                    let ws = self.ws.lock().unwrap();
                    ws.panes.get(&id).and_then(|p| p.markdown()).map(|m| m.doc.path.clone())
                };
                match path {
                    Some(p) => {
                        let ds = self.lsp_diags(&p);
                        eprintln!(
                            "[mdscript] diags n={} {:?}",
                            ds.len(),
                            ds.iter()
                                .take(4)
                                .map(|d| (d.line, d.col, d.severity, d.message.clone()))
                                .collect::<Vec<_>>()
                        );
                    }
                    None => eprintln!("[mdscript] diags: 편집기 pane 이 아님"),
                }
            }
            // 선택 텍스트 확인. 클립보드 대신 로그로 찍는다(위 `sel:` 주석 참고).
            Some(("selcopy", _)) => {
                match self.md_render_selection_text() {
                    Some(t) => eprintln!("[mdscript] selcopy={t:?}"),
                    None => eprintln!("[mdscript] selcopy=<없음>"),
                }
            }
            // `[[이름]]` 링크를 눌러 본다 — `wiki:<이름>`. 클릭 좌표 대신 목적지를
            // 직접 넘긴다: 링크 글자의 화면 위치는 창 폭과 스크롤에 따라 움직여
            // 좌표로 짚으면 검증이 창 크기에 묶인다. 확인하려는 건 히트테스트가
            // 아니라 **어느 파일이 열리는가** 다(볼트가 주제 폴더로 갈라져 있다).
            Some(("wiki", v)) => {
                self.open_md_dest(&format!("wiki:{v}"));
                let opened = self.ws.lock().ok().map(|w| {
                    w.panes
                        .values()
                        .filter_map(|p| p.markdown().map(|m| m.doc.path.clone()))
                        .collect::<Vec<_>>()
                });
                eprintln!("[mdscript] wiki={v} 열린문서={opened:?}");
            }
            // 이 마크다운 탭을 닫아 본다 — 저장 안 한 편집분이 있으면 확인
            // 모달이 떠야 하고, 그 화면이 이 단계의 관찰 대상이다.
            Some(("close", _)) => {
                let tab = self
                    .ws
                    .lock()
                    .ok()
                    .and_then(|w| w.panes.get(&id).map(|p| p.active_tab))
                    .unwrap_or(0);
                self.confirm_or_close_tab(&id, tab);
                let why = self.confirm_close.as_ref().map(|c| match &c.why {
                    CloseWhy::Busy(p) => format!("busy:{p}"),
                    CloseWhy::Dirty(d) => format!(
                        "dirty:{}",
                        d.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(",")
                    ),
                    CloseWhy::LastPane => "lastpane".to_string(),
                });
                eprintln!("[mdscript] close why={why:?}");
            }
            // 편집 키는 winit `KeyEvent` 를 밖에서 만들 수 없어(비공개 필드)
            // 순수 메서드를 직접 부른다. 키→메서드 배선은 유닛 테스트가 아니라
            // 코드 경로로 확인하고, 여기선 **결과가 화면에 어떻게 그려지는지**만
            // 본다 — 들여쓴 항목이 실제로 한 단 들어가 보이는지 같은 것.
            // 한글 조합. `md_editor_input` 이 자모에 대해 하는 일과 **같은
            // 코드**(소유권 주장 → `md_feed_jamo`)를 탄다 — winit KeyEvent 를
            // 못 만들어 조합 경로만 검증 사각지대였던 걸 여기서 메운다.
            Some(("jamo", v)) => {
                for c in v.chars() {
                    self.ime_retarget(crate::ImeFocus::Editor(id.clone()));
                    let took = self.md_feed_jamo(c);
                    eprintln!(
                        "[mdscript] jamo {c} took={took} preedit={:?} focus={:?}",
                        self.preedit, self.ime_focus
                    );
                }
                self.md_ensure_caret_visible();
            }
            Some(("edit", v)) => {
                {
                    let Ok(mut ws) = self.ws.lock() else { return };
                    let Some(pane) = ws.panes.get_mut(&id) else { return };
                    pane.dirty = true;
                    let Some(m) = pane.markdown_mut() else { return };
                    match v {
                        "tab" => m.indent(false),
                        "untab" => m.indent(true),
                        "enter" => m.newline(),
                        // 되돌리기가 **한 번의 undo 로 통째로** 취소되는지 보려면
                        // 여기 있어야 한다 — 헝크 되돌리기는 여러 줄을 한꺼번에
                        // 갈아끼우므로, 스냅샷을 한 번만 쌓았는지가 계약이다.
                        "undo" => {
                            m.do_undo();
                        }
                        "redo" => {
                            m.do_redo();
                        }
                        "find" => m.find_open(false),
                        "replace" => m.find_open(true),
                        "next" => m.find_step(false),
                        "prev" => m.find_step(true),
                        // `at <line>,<col>` 은 캐럿 이동, 나머지는 그대로 타이핑.
                        _ => match v.strip_prefix("at ") {
                            Some(pos) => {
                                let (l, c) = pos.split_once(',').unwrap_or((pos, "0"));
                                m.cur_line = l.trim().parse().unwrap_or(0);
                                m.cur_col = c.trim().parse().unwrap_or(0);
                            }
                            // 찾기 바가 열려 있으면 타이핑은 검색어로 — 실제
                            // 키 경로(md_editor_insert)와 같은 갈림길이다.
                            None if m.find.is_some() => m.find_type(v),
                            None => m.insert_at_caret(v),
                        },
                    }
                    eprintln!("[mdscript] edit={v} caret=({},{})", m.cur_line, m.cur_col);
                }
                self.md_ensure_caret_visible();
            }
            _ => eprintln!("[mdscript] 모르는 단계: {step:?}"),
        }
        self.wake_after_mdstep();
    }

    fn wake_after_mdstep(&mut self) {
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Headless **탭 전이** 검증: `KASATERM_AUTOTABCYCLE_MS` 뒤에 탭을 소환→전환→
    /// 꺼내기→합치기→닫기 순으로 밟으며, 단계마다 미니맵과 인포가 **실제로 읽는
    /// 재료**를 갈라 찍는다.
    ///
    /// 이 계열 버그가 나는 자리는 정해져 있다 — pane 식별자가 둘이라서다. **BSP
    /// leaf**(`ws.panes`·`pane_activity` 의 키)와 **PTY id**(`self.pty`·
    /// `pane_claude_seen` 의 키)는 탭이 없을 때만 같고, 탭이 생기는 순간 갈렸다가
    /// 탭을 꺼내면 다시 붙는다. 정지 화면은 어느 쪽으로 물어도 맞아 보이므로,
    /// 갈렸다 붙는 **전이**를 밟지 않으면 어긋남이 드러나지 않는다.
    ///
    /// 실제 claude 는 띄우지 않는다 — 그건 `autostudent` 담당이고, rustc 빌드와
    /// 프로세스 표 캐시로 단계마다 수 초가 든다. 여기서 보는 것은 **키 정합성**이라
    /// 얼굴 자격(`pane_claude_seen`)을 실제 경로와 같은 키(PTY id)로 심어 두면 충분하다.
    ///
    /// `KASATERM_AUTOTABCYCLE_CAP` 에 접두를 주면 단계마다 `<접두>-N.png` 를 찍는다.
    pub(crate) fn run_pending_autotabcycle(&mut self) {
        use std::sync::atomic::{AtomicU8, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<(Instant, String)>> = OnceLock::new();
        static STEP: AtomicU8 = AtomicU8::new(0);
        static HOST: OnceLock<String> = OnceLock::new();
        static NEIGHBOR: OnceLock<String> = OnceLock::new();
        static TORN: OnceLock<String> = OnceLock::new();
        let due = DUE.get_or_init(|| {
            let ms = std::env::var("KASATERM_AUTOTABCYCLE_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())?;
            let who = std::env::var("KASATERM_AUTOTABCYCLE_STUDENT")
                .unwrap_or_else(|_| "미도리".to_string());
            Some((Instant::now() + std::time::Duration::from_millis(ms), who))
        });
        let Some((due, who)) = due else { return };
        let who = who.clone();
        let step = STEP.load(Ordering::Relaxed);
        if step > 6 {
            return;
        }
        // 단계마다 2500ms. 가짜 claude 가 rustc 로 구워지고 프로세스 표 캐시(300ms)와
        // pty 의 proc 캐시(500ms)가 한 바퀴 돌아야 `runs_claude` 가 참이 되며, 인포는
        // 워커가 만든 스냅샷을 그리므로 그쪽도 한 번은 돌아야 한다 — 좁히면 「아직 안
        // 들어온 것」을 「빠진 것」으로 오독한다(700ms 로 두었다가 인포가 pane 을 하나로
        // 세는 화면을 찍었다).
        if Instant::now() < *due + std::time::Duration::from_millis(2500 * step as u64) {
            return;
        }
        STEP.store(step + 1, Ordering::Relaxed);
        let leaves = |app: &Self| -> Vec<String> {
            app.pty_layout
                .as_ref()
                .map(|t| t.leaves().iter().map(|s| s.to_string()).collect())
                .unwrap_or_default()
        };
        match step {
            // 1) claude pane 하나 + 이웃 하나. 이웃이 있어야 꺼내기가 갈 곳이 생긴다.
            0 => {
                // 볼 화면을 먼저 연다 — 배치도는 **펼친 방 카드** 안에만 그려지고
                // (`sidebar_row_rects` 가 거기서 실린다) 인포 트리는 우측 칼럼 Info
                // 탭에만 있다. 안 열면 단계별 캡처가 빈 터미널만 찍는다.
                if !self.sidebar_visible {
                    self.toggle_sidebar();
                }
                if !self.expanded_windows.contains(&self.active_window) {
                    self.toggle_window_expand(self.active_window);
                }
                if !self.git.col_visible {
                    self.toggle_git_col();
                }
                self.info.tab = crate::state::SideTab::Info;
                // 펼침은 애니메이션이라 첫 프레임엔 카드가 납작해 칸 rect 가 하나도
                // 안 실린다 — 다 펴질 때까지 프레임을 돌린다(autostash 와 같은 이유).
                for _ in 0..40 {
                    self.render_frame();
                    if !self.sidebar_row_rects.is_empty() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                if leaves(self).len() < 2 {
                    let _ = self.split_active_pane(kasa_pty::SplitDir::Horizontal);
                }
                let ls = leaves(self);
                if ls.len() < 2 {
                    eprintln!("[tabcycle] FAIL — pane 둘을 못 만들었다: {ls:?}");
                    STEP.store(7, Ordering::Relaxed);
                    return;
                }
                let host = HOST.get_or_init(|| ls[0].clone()).clone();
                NEIGHBOR.get_or_init(|| ls[1].clone());
                self.ws.lock().unwrap().pane_character.insert(host.clone(), who);
                // 얼굴·테두리는 `pane_accent` 가 **실제로 도는 에이전트**를 요구한다
                // (`active_agent()`). 자격 집합만 손으로 심으면 키 정합은 봐도 화면은
                // 못 보므로, autostudent 와 같은 가짜 claude 를 실제로 띄운다.
                if let Some(pty) = self.pty.get(&host) {
                    let _ = pty.send_bytes(FAKE_CLAUDE_SCRIPT.as_bytes());
                }
                self.pane_claude_seen.insert(host);
            }
            // 2) 같은 pane 에 탭으로 학생을 하나 더. 여기서 leaf 와 PTY id 가 갈린다.
            1 => {
                let Some(host) = HOST.get().cloned() else { return };
                match self.spawn_new_tab(&host, true) {
                    Ok(pid) => {
                        // 자격은 실제 경로와 **같은 키**로 심는다 — `note_claude_panes`
                        // 는 `self.pty` 를 훑으므로 PTY id 로 들어간다. leaf 로 심으면
                        // 이 하네스가 검증하려는 어긋남 자체가 사라진다.
                        self.ws.lock().unwrap().pane_character.insert(pid.clone(), who);
                        if let Some(pty) = self.pty.get(&pid) {
                            // 이 탭에만 OSC 제목을 단다 — 앞에 별표(U+2733)를 붙여
                            // **진짜 claude 가 보내는 꼴**(`✳ 작업명`)을 만든다. 가짜
                            // claude 는 제목을 안 달아서, 이걸 안 심으면 탭 라벨의
                            // 접두 벗기기 경로가 통째로 안 돌고도 통과해 버린다.
                            // 덤으로 형제 탭과 이름이 갈려 「둘 다 미도리」가 아니게 된다.
                            // ⚠️두 번에 나눠 보내면 안 된다 — 셸이 첫 줄을 아직
                            // exec 하기 전이라 둘째가 **그 명령줄 안으로 빨려 들어가고**,
                            // 화면엔 printf 만 에코된 채 아무것도 안 돈다(실측).
                            let _ = pty.send_bytes(
                                format!(
                                    "printf '\\033]0;\\342\\234\\263 탭작업\\007'; {FAKE_CLAUDE_SCRIPT}"
                                )
                                .as_bytes(),
                            );
                        }
                        self.pane_claude_seen.insert(pid);
                    }
                    Err(e) => eprintln!("[tabcycle] FAIL — 탭 소환 실패: {e}"),
                }
            }
            // 3) 앞 탭으로 전환 — 활성 표시가 양쪽 화면에서 따라오는가.
            2 => {
                let Some(host) = HOST.get().cloned() else { return };
                if let Some(p) = self.ws.lock().unwrap().panes.get_mut(&host) {
                    p.active_tab = 0;
                    p.dirty = true;
                }
            }
            // 4) 꺼내기 — 탭을 이웃 pane 의 **가장자리**에 떨구면 split 되어 독립
            //    pane 이 된다(`drop_tab_into_body`). 중앙에 놓는 합치기와 짝인 경로다.
            3 => {
                let (Some(host), Some(nb)) = (HOST.get().cloned(), NEIGHBOR.get().cloned())
                else {
                    return;
                };
                let tabs = self
                    .ws
                    .lock()
                    .unwrap()
                    .panes
                    .get(&host)
                    .map(|p| p.tabs.len())
                    .unwrap_or(0);
                if tabs < 2 {
                    eprintln!("[tabcycle] FAIL — 꺼낼 탭이 없다(tabs={tabs})");
                    STEP.store(7, Ordering::Relaxed);
                    return;
                }
                let td = TabDrag {
                    pane: host,
                    from: 1,
                    start: (0.0, 0.0),
                    active: true,
                    target: 0,
                    drop_pane: nb.clone(),
                };
                self.drop_tab_into_body(&td, &nb, DropZone::Right);
                // 꺼낸 pane 이 곧 새 활성 pane 이다(`drop_tab_into_body` 마지막 줄).
                if let Some(new) = self.ws.lock().unwrap().active_pane.clone() {
                    TORN.get_or_init(|| new);
                }
            }
            // 5) 다시 탭으로 합치기 — 2번 상태로 정확히 돌아오는가.
            4 => {
                let (Some(host), Some(torn)) = (HOST.get().cloned(), TORN.get().cloned())
                else {
                    return;
                };
                if !self.merge_pane_into_tabs(&torn, &host) {
                    eprintln!("[tabcycle] FAIL — 합치기 거부됨({torn}→{host})");
                }
            }
            // 6) 탭 닫기 — 덱·트리가 사라지고 원래 모습으로 돌아오는가.
            5 => {
                let Some(host) = HOST.get().cloned() else { return };
                self.close_tab(&host, 1);
            }
            // 7) claude 가 내려가 셸만 남은 pane — 얼굴·이름이 빠지는가(5c761f6 회귀).
            _ => {
                let Some(host) = HOST.get().cloned() else { return };
                self.pane_claude_seen.remove(&host);
                self.ws.lock().unwrap().pane_character.remove(&host);
            }
        }
        // 판정은 **그린 뒤**에 한다. 배치도 칸(`sidebar_row_rects`)은 렌더가 채우는
        // 값이라, 액션 직후에 읽으면 한 단계 전 화면을 현재로 착각한다 — 실측에서
        // 꺼내기·합치기 두 칸이 정확히 한 단계씩 밀려 「칸이 안 따라온다」로 보였다.
        self.chrome_dirty = true;
        let cap = std::env::var("KASATERM_AUTOTABCYCLE_CAP").ok();
        if let (Some(pre), Some(g)) = (cap.as_ref(), self.gpu.as_mut()) {
            g.capture_next = Some(format!("{pre}-{}.png", step + 1));
        }
        // 인포는 1.5초 게이트를 지나 **별도 스레드**가 만든 스냅샷을 그린다. 액션 직후에
        // 판정하면 한 단계 전 화면을 현재로 착각한다 — 실측에서 탭을 도로 합친 뒤에도
        // 화면은 `pane 3` 에 탭이 형제로 선 그림이었고, 즉시값(`info_targets`)만 보던
        // 판정은 그걸 통과시켰다. 게이트를 풀어 워커를 띄우고, 끝나면 한 번 더 불러
        // 완성된 스냅샷을 `view` 로 당긴다.
        self.info.last_refresh = None;
        self.pump_info();
        for _ in 0..100 {
            if !self.info.busy.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        self.pump_info();
        self.render_frame();
        const LABELS: [&str; 7] = [
            "1-claude pane",
            "2-탭소환",
            "3-탭전환",
            "4-꺼내기",
            "5-합치기",
            "6-탭닫기",
            "7-셸만",
        ];
        self.dump_tabcycle(LABELS[step as usize]);
    }
    /// `autotabcycle` 의 한 줄 판정. 미니맵과 인포는 **출처가 다르다** — 미니맵은
    /// `ws.panes` 의 탭 배열을 세고, 인포는 `self.pty` 를 훑어 `outer` 로 되짚는다.
    /// 그래서 합쳐 찍으면 어긋남이 그대로 묻히고, 갈라 찍어야 어느 쪽이 틀렸는지가
    /// 보인다. `고아탭` = 인포에서 host 아래로 접히지 못하고 최상위에 남은 탭(=한
    /// pane 이 여럿으로 보이던 3b294d6 의 증상).
    fn dump_tabcycle(&self, step: &str) {
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|t| t.leaves().iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        let mut out = String::new();
        for leaf in &leaves {
            let (tabs, tab_pid, ch) = {
                let ws = self.ws.lock().unwrap();
                let tabs = ws.panes.get(leaf).map(|p| p.tabs.len()).unwrap_or(0);
                let tab_pid = ws.active_tab_pid(leaf);
                let ch = self.display_pane_char(&ws, leaf).unwrap_or_default();
                (tabs, tab_pid, ch)
            };
            // 인포는 **화면이 그리는 것**(`info.view`)으로 센다. 즉시값(`info_targets`)을
            // 보면 「코드상 맞는데 화면은 틀린」 것을 통과시킨다. 탭이 하나인 pane 은
            // 접지 않아 `tabs` 가 비고 그룹 자신이 한 줄이다(`fold_tabs` 계약).
            let info_rows = self
                .info
                .view
                .panes
                .iter()
                .find(|g| g.pane == *leaf)
                .map(|g| g.tabs.len().max(1))
                .unwrap_or(0);
            let ready = self.pane_claude_ready(leaf);
            let status = self
                .pane_activity
                .get(leaf)
                .map(|a| a.status.clone())
                .unwrap_or_else(|| "-".to_string());
            out.push_str(&format!(
                " {leaf}[탭{tabs} 인포{info_rows} 얼굴{} 이름{ch:?} {status} tabpid={tab_pid}]",
                if ready { "O" } else { "X" }
            ));
        }
        // 고아탭 = 화면에 **최상위 그룹으로 섰는데 배치 트리엔 없는** pane. 탭이
        // host 아래로 접히지 못하면 정확히 이 모양이 된다(pane 하나가 여럿으로 보이던
        // 3b294d6 의 증상).
        let stray: Vec<&str> = self
            .info
            .view
            .panes
            .iter()
            .filter(|g| !leaves.contains(&g.pane))
            .map(|g| g.pane.as_str())
            .collect();
        eprintln!(
            "[tabcycle/{step}]{out} 고아탭={stray:?} 배치도칸={}",
            self.sidebar_row_rects.len()
        );
    }
    /// Headless "+" 셸 피커 repro: `KASATERM_AUTOSHELLMENU_MS` 후 피커 팝업을 연다 —
    /// 항목(기본 셸·Claude 학생 등)을 클릭 없이 캡처. autosettings 처럼 함수-로컬
    /// static(병렬 작업 규칙: struct App 무접촉).
    pub(crate) fn run_pending_autoshellmenu(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOSHELLMENU_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        eprintln!("[autoshellmenu] open shell picker");
        self.shell_menu_open = true;
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Headless 파일트리 우클릭 메뉴 repro: `KASATERM_TEST_FTMENU_MS` 후 트리
    /// 첫 파일을 선택하고 컨텍스트 메뉴를 연다. 우클릭은 마우스 이벤트라 헤드리스
    /// 주입이 안 되는데, "…에서 열기" 항목은 기기에 설치된 앱 수만큼 늘어나므로
    /// 눈으로 한 번은 확인해야 한다. autoshellmenu 처럼 함수-로컬 static.
    pub(crate) fn run_pending_autoftmenu(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_TEST_FTMENU_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        // 트리가 아직 안 채워졌으면 다음 프레임에 다시 본다 — 파일트리는 워커가
        // 비동기로 채우므로 고정 지연만으로는 빈 트리에 메뉴를 띄울 수 있다.
        let Some(first) = self.file_tree.nodes.first().map(|n| n.path.clone()) else {
            return;
        };
        FIRED.store(true, Ordering::Relaxed);
        eprintln!("[autoftmenu] context menu on {}", first.display());
        self.file_tree.selected = Some(first);
        self.file_tree.ctx_menu = Some((260.0, 200.0));
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Headless file-open repro: schedule `open_file_split` on the path in
    /// `KASATERM_AUTOOPEN` after `KASATERM_AUTOOPEN_MS` (default 4000ms), so a
    /// background run can prove the preview pane + file-tree highlight without
    /// a real double-click (mouse events aren't injectable headlessly).
    pub(crate) fn arm_autoopen(&mut self) {
        let Ok(p) = std::env::var("KASATERM_AUTOOPEN") else { return };
        let ms: u64 = std::env::var("KASATERM_AUTOOPEN_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4000);
        self.autoopen_path = Some(std::path::PathBuf::from(p));
        self.autoopen_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    pub(crate) fn run_pending_autoopen(&mut self) {
        let Some(due) = self.autoopen_at else { return };
        if Instant::now() < due {
            return;
        }
        self.autoopen_at = None;
        if let Some(p) = self.autoopen_path.take() {
            eprintln!("[autoopen] open_file {}", p.display());
            // 사람 경로(`open_file_split`)가 아니라 미리보기 경로로 연다: "파일 열기"
            // 설정이 App/Terminal 이면 사람 경로는 파일을 외부 앱으로 넘겨버려,
            // 내장 뷰를 증명하려는 이 하네스가 아무것도 열지 못한다.
            self.open_file(p, None, true);
        }
    }
    /// `KASATERM_PANELABEL_DEBUG=1` — 사이드바 pane 줄에 **실제로 적히는 이름**을
    /// 바뀔 때마다 한 줄씩 찍는다. 사이드바를 열고 방을 펼치고 목록 모드로 바꾸는
    /// 세 단계를 거치지 않고도 이름 규칙을 물을 수 있다.
    ///
    /// 세 소스를 같이 찍는 게 요점이다 — 붙인 이름(GUI 사본)과 OSC(PTY 사본)는
    /// **저장소가 달라서**, 하나만 보면 「이름을 붙였는데 왜 안 바뀌지」의 원인이
    /// 어느 쪽인지 못 가른다. 표시값이 그 둘 중 어느 것을 골랐는지가 그대로 보인다.
    ///
    /// 바뀔 때만 찍는다 — 매 프레임 찍으면 초당 수십 줄이라 로그에서 변화를 못 찾는다.
    pub(crate) fn probe_pane_labels(&mut self) {
        use std::sync::OnceLock;
        static ON: OnceLock<bool> = OnceLock::new();
        if !*ON.get_or_init(|| std::env::var_os("KASATERM_PANELABEL_DEBUG").is_some()) {
            return;
        }
        let ids: Vec<String> = {
            let ws = self.ws.lock().unwrap();
            let mut v: Vec<String> = ws.panes.keys().cloned().collect();
            v.sort();
            v
        };
        for id in ids {
            let shown = self.pane_row_label(&id);
            let osc = self.pty.get(&id).and_then(|p| p.osc_title()).unwrap_or_default();
            let (pinned, pin) = {
                let ws = self.ws.lock().unwrap();
                match ws.panes.get(&id) {
                    Some(p) => (p.title.clone().unwrap_or_default(), p.title_pinned),
                    None => (String::new(), false),
                }
            };
            let line = format!("붙인이름={pinned:?} 핀={pin} osc={osc:?} → 표시={shown:?}");
            // 상태는 모듈 static 이다 — `struct App` 은 병렬 작업 충돌 핫스팟이라
            // 하네스가 거기 필드를 늘리면 남의 작업과 매번 부딪힌다(CLAUDE.md).
            static SEEN: OnceLock<
                std::sync::Mutex<std::collections::HashMap<String, String>>,
            > = OnceLock::new();
            let mut seen = SEEN.get_or_init(Default::default).lock().unwrap();
            if seen.get(&id) == Some(&line) {
                continue;
            }
            eprintln!("[panelabel] {id} {line}");
            seen.insert(id, line);
        }
    }
    /// Headless verification helper. Reads `KASATERM_AUTOSPLIT` ("h" / "v"
    /// / "hv" / "vh" ...) and fires the matching splits from
    /// `about_to_wait` after `KASATERM_AUTOSPLIT_MS` (default 2500ms),
    /// so a background `cargo run` can prove multi-pane rendering
    /// without a human pressing Cmd+D.
    pub(crate) fn run_pending_autosplits(&mut self) {
        if self.autosplit_plan.is_empty() {
            return;
        }
        let now = Instant::now();
        let due = match self.autosplit_at {
            Some(t) => t,
            None => return,
        };
        if now < due {
            return;
        }
        let dir = self.autosplit_plan.remove(0);
        if let Err(e) = self.split_active_pane(dir) {
            eprintln!("[autosplit] split failed: {e}");
        }
        // Chain the next split 500ms later so the renderer has time to
        // settle and a screenshot can capture intermediate states.
        self.autosplit_at = if self.autosplit_plan.is_empty() {
            None
        } else {
            Some(now + std::time::Duration::from_millis(500))
        };
    }
    /// Headless repro for the window sidebar: spawn KASATERM_AUTOWINDOWS extra
    /// windows, one every 600ms, so a screenshot can capture the multi-tab
    /// sidebar without a human pressing Cmd+T.
    pub(crate) fn run_pending_autowindows(&mut self) {
        if self.autowindow_left == 0 {
            return;
        }
        let now = Instant::now();
        let Some(due) = self.autowindow_at else { return };
        if now < due {
            return;
        }
        self.new_window();
        self.autowindow_left -= 1;
        self.autowindow_at = if self.autowindow_left == 0 {
            None
        } else {
            Some(now + std::time::Duration::from_millis(600))
        };
    }
    pub(crate) fn arm_autowindows(&mut self) {
        let Ok(n_str) = std::env::var("KASATERM_AUTOWINDOWS") else { return };
        let Ok(n) = n_str.parse::<usize>() else { return };
        if n == 0 {
            return;
        }
        let ms: u64 = std::env::var("KASATERM_AUTOWINDOWS_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2500);
        eprintln!("[autowindow] armed: {n} window(s) in {ms}ms");
        self.autowindow_left = n;
        self.autowindow_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    pub(crate) fn run_pending_autotoggle(&mut self) {
        let Some(due) = self.autotoggle_sidebar_at else { return };
        if Instant::now() < due {
            return;
        }
        self.toggle_sidebar();
        eprintln!(
            "[autotoggle] flipped → visible={} remaining={}",
            self.sidebar_visible, self.autotoggle_left
        );
        if self.autotoggle_left > 0 {
            self.autotoggle_left -= 1;
            self.autotoggle_sidebar_at =
                Some(Instant::now() + std::time::Duration::from_millis(1500));
        } else {
            self.autotoggle_sidebar_at = None;
        }
    }
    /// 사이드바 방 펼치기 헤드리스 재현 — `KASATERM_AUTOEXPAND` 에 방 인덱스를
    /// 콤마로(`0,2`). 펼침은 클릭 손잡이가 유일한 입구라, 상태를 직접 세워야
    /// 목록의 배치·잘림·넘침을 캡처로 볼 수 있다. 방이 아직 없어도 인덱스만
    /// 담아 두면 나중에 생기는 방에 그대로 적용된다.
    /// `KASATERM_AUTOALERT="0,2"` — 그 방들에 "못 본 알림"을 세운다.
    ///
    /// 알림·대기 표시는 밖에서 일이 일어나야(claude 가 끝나거나 물어봐야) 켜지는데,
    /// 헤드리스에는 그 일이 없다. 상태만 세워 두면 캡처가 곧 그 표시의 스크린샷이
    /// 된다 — 색·자리·속도가 정말 갈리는지는 눈으로만 확인된다.
    pub(crate) fn arm_autoalert(&mut self) {
        let Ok(v) = std::env::var("KASATERM_AUTOALERT") else { return };
        for i in v.split(',').filter_map(|s| s.trim().parse::<usize>().ok()) {
            self.window_alert.insert(i);
        }
        eprintln!("[autoalert] {:?}", self.window_alert);
    }
    /// `KASATERM_AUTOWAIT="%2"` — 그 pane 을 "손을 기다리는 중"으로 세운다.
    ///
    /// 한 번 세우고 끝낼 수 없다. `refresh_pane_activity` 가 틱마다 transcript 를
    /// 다시 읽어 `pane_activity` 를 통째로 덮어쓰므로, 캡처가 뜰 즈음엔 세워 둔
    /// 상태가 이미 지워져 있다(실측: 띠가 한 장도 안 나왔다). 그래서 매 틱 덮는다.
    pub(crate) fn apply_autowait(&mut self) {
        use std::sync::OnceLock;
        static IDS: OnceLock<Vec<String>> = OnceLock::new();
        let ids = IDS.get_or_init(|| {
            std::env::var("KASATERM_AUTOWAIT")
                .map(|v| {
                    v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                })
                .unwrap_or_default()
        });
        for spec in ids {
            // `%2:blocked` 처럼 상태를 붙일 수 있고, `*` 는 활성 pane 이다.
            //
            // 어휘가 둘인데(`waiting`/`blocked`) **프로덕션에서 실제로 들어오는 건
            // `blocked` 뿐**이라(화면 감지 경로가 그것만 쓴다), waiting 만 심을 수
            // 있으면 정작 실제 경로를 한 번도 못 본다 — 표시 여섯 자리가 waiting 만
            // 보다가 승인 대기가 통째로 안 그려진 걸 오래 몰랐던 이유다(2026-08-11).
            let (id, st) = spec.split_once(':').unwrap_or((spec.as_str(), "waiting"));
            let target = if id == "*" {
                self.ws.lock().unwrap().active_pane.clone()
            } else {
                Some(id.to_string())
            };
            let Some(target) = target else { continue };
            self.pane_activity.entry(target).or_default().status = st.into();
        }
    }
    /// Headless 학생 오버레이 repro: `KASATERM_AUTOSTUDENT_MS`.
    ///
    /// 학생 표시(statusline 프사·standing·배너 도트)는 밖에서 claude 가 실제로 돌고
    /// statusline.py 가 자리표시자를 내보내야 켜지는데, 헤드리스엔 그 일이 없다.
    /// 그래서 지금까지 **이 층을 아예 재현할 수 없었다**.
    ///
    /// 게이트가 셋인데, 셋 다 **우회 코드 없이** 진짜로 만족시킨다:
    /// ① `runs_claude` — 셸의 직속 자식 이름이 claude 여야 한다. env 우회를 render 에
    ///    심는 대신 **`claude` 라는 이름의 실제 바이너리를 rustc 로 즉석에서 굽는다**
    ///    (300초 자는 3줄짜리, 0.1초면 빌드된다). 판정 코드가 손대지 않은 채로 참이
    ///    되므로 게이트 자체도 같이 검증된다.
    ///
    ///    막다른 길 셋을 먼저 밟았다(다시 밟지 말라고 적어 둔다): macOS 의 `ps -o comm`
    ///    은 **실제로 실행된 바이너리의 경로**를 준다. 그래서 `/bin/sh` 복사본은
    ///    SIP/AMFI 가 SIGKILL(exit 137) 하고, 심링크는 `/bin/zsh` 로 풀려 보이고,
    ///    셰방 스크립트는 인터프리터 이름으로 뜬다. 이름이 claude 인 **파일을 진짜로
    ///    실행**하는 것 말고는 방법이 없다.
    /// ② `display_pane_char` — `ws.pane_character` 에 실재하는 학생명을 배정한다
    ///    (셸 pane 은 `is_claude_agents()` 가 false 라 이 폴백이 정본이 된다).
    /// ③ 셀에 U+FFFC — 그 가짜 claude 가 statusline 을 흉내 낸 줄을 **PTY 로 실제
    ///    출력**한다. 셀 그리드에 손으로 써넣지 않는 이유: 다음 pump 가 덮어쓴다.
    ///
    /// ④ standing(입력박스 위 전신) — 위 셋이 다 열려도 **앵커**가 따로 걸린다.
    ///    그래서 가짜 claude 가 입력박스를 테두리 두 줄까지 온전히 찍고,
    ///    `autostudent_assert_standing` 이 전신이 정말 그려졌는지 따로 판정한다.
    pub(crate) fn run_pending_autostudent(&mut self, _event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicU8, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<(Instant, String)>> = OnceLock::new();
        static STEP: AtomicU8 = AtomicU8::new(0);
        let due = DUE.get_or_init(|| {
            let ms = std::env::var("KASATERM_AUTOSTUDENT_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())?;
            let who = std::env::var("KASATERM_AUTOSTUDENT")
                .unwrap_or_else(|_| "미도리".to_string());
            Some((Instant::now() + std::time::Duration::from_millis(ms), who))
        });
        let Some((due, who)) = due else { return };
        let step = STEP.load(Ordering::Relaxed);
        // 2초씩 두는 이유: rustc 빌드 + 프로세스 표 캐시(300ms)와 pty 의 proc 캐시
        // (500ms)가 겹쳐, 바로 물으면 아직 셸 이름이 돌아온다.
        const AT_MS: [u64; 2] = [0, 2000];
        let Some(&off) = AT_MS.get(step as usize) else {
            return;
        };
        if Instant::now() < *due + std::time::Duration::from_millis(off) {
            return;
        }
        // 학생을 심은 pane 을 **기억**한다. 3단계에서 방을 새로 열면 활성 pane 이
        // 그쪽으로 옮겨가므로, 그때 active_pane 을 다시 읽으면 엉뚱한 pane 을
        // 학생으로 알고 판정한다(실측으로 한 번 속았다).
        static PLANTED: OnceLock<String> = OnceLock::new();
        let pid = match PLANTED.get() {
            Some(p) => p.clone(),
            None => {
                let Some(p) = self.ws.lock().unwrap().active_pane.clone() else { return };
                PLANTED.get_or_init(|| p).clone()
            }
        };
        if step == 0 {
            STEP.store(1, Ordering::Relaxed);
            if crate::theme::character_slug(who).is_none() {
                eprintln!("[autostudent] FAIL — 없는 학생명 {who:?}");
                STEP.store(2, Ordering::Relaxed);
                return;
            }
            if let Ok(mut ws) = self.ws.lock() {
                ws.pane_character.insert(pid.clone(), who.clone());
            }
            self.pane_claude_seen.insert(pid.clone());
            // 순서가 중요하다: **먼저 찍고 그 다음에** 가짜 claude 를 띄운다. 그래야
            // 화면 바닥 두 줄이 rule + statusline 인 채로 남는다(셸은 foreground job
            // 이 끝날 때까지 프롬프트를 안 찍는다). exec 은 쓰지 않는다 — 셸을 갈아
            // 치우면 그게 pane 의 셸이 돼 버려 직속 자식이 사라진다.
            // U+FFFC 는 8진 이스케이프(EF BF BC)로 — 셸마다 \u 지원이 갈린다.
            //
            // 찍는 것은 claude 입력박스 **한 벌 전체**다. 오래도록 아래 테두리
            // 한 줄만 찍었는데, 그러면 `find_standing_anchor` 의 위쪽 스캔이 걸릴
            // rule 이 없어 앵커가 늘 None 이고 standing 이 통째로 안 그려진다 —
            // 그 위 행은 셸이 되울린 명령줄이라 label>24 로 즉시 탈락한다. 프사만
            // 뜨고 전신이 안 서는 층을 이 하네스가 못 태우고 있었다.
            //
            //   (빈 행)                  ← 앵커. 비어 있어야 한다(내용이 0열부터
            //                              시작하면 left_c 가 음수가 되어 None)
            //   ────…──── 대시보드 ──    ← 윗 테두리. 텍스트 섬을 일부러 넣어
            //                              max_label 24 분기까지 태운다(세션명이
            //                              박히면 standing 이 사라졌던 거노 실사고).
            //                              **라벨은 오른쪽 끝**에 둔다 — 실제 claude 가
            //                              그 모양이고, 왼쪽 대시 run 이 짧으면 좌측
            //                              제목 인레이가 폭 부족으로 포기한다(4칸으로
            //                              뒀다가 `autoboxlabel` 이 좌측을 못 봤다)
            //   ❯                        ← 입력 영역
            //   ──────────────           ← 아래 테두리(순수 '─')
            //   FFFC×4 ctx 42%           ← statusline = face_row
            let script = crate::testkit::FAKE_CLAUDE_SCRIPT;
            if let Some(pty) = self.pty.get(&pid) {
                let _ = pty.send_bytes(script.as_bytes());
            }
            eprintln!("[autostudent] pane={pid} 학생={who} — 가짜 claude 띄우는 중");
            return;
        }
        STEP.store(2, Ordering::Relaxed);
        // 게이트가 실제로 열렸는지 셋 다 따로 찍는다 — 하나만 닫혀도 화면엔
        // 똑같이 "아무것도 안 뜸"이라, 뭉뚱그리면 어디가 막혔는지 못 짚는다.
        let proc = self
            .pty
            .get(&pid)
            .and_then(|p| p.active_process_name())
            .unwrap_or_default();
        let g1 = proc.contains("claude");
        let g2 = {
            let ws = self.ws.lock().unwrap();
            self.display_pane_char(&ws, &pid)
                .and_then(|n| crate::theme::character_slug(&n).map(|s| s.to_string()))
        };
        let g3 = self
            .ws
            .lock()
            .unwrap()
            .panes
            .get(&pid)
            .and_then(|p| p.term())
            .map(|t| t.cells.iter().any(|row| row.iter().any(|c| c.ch == '\u{fffc}')))
            .unwrap_or(false);
        eprintln!(
            "[autostudent] ①runs_claude={g1}(proc={proc:?}) ②학생slug={g2:?} ③U+FFFC={g3}"
        );
        eprintln!(
            "[autostudent] {}",
            if g1 && g2.is_some() && g3 { "PASS — 세 게이트 다 열림" } else { "FAIL" }
        );
        // 게이트가 열린 **바로 그 프레임**을 찍는다. `pending_capture` 큐에 넣으면
        // 그 뒤로 아무도 프레임을 안 내보내(학생 오버레이엔 애니 펌프가 없다) 자동
        // 캡처가 영영 발화하지 않는다 — 실측으로 두 번 놓쳤다. mdscript 의 `cap:` 과
        // 같은 방식으로 gpu 에 직접 무장하고 그 자리에서 한 장 그린다.
        self.chrome_dirty = true;
        if let Ok(path) = std::env::var("KASATERM_AUTOSTUDENT_CAP") {
            if let Some(g) = self.gpu.as_mut() {
                g.capture_next = Some(path);
            }
        }
        self.render_frame();
        self.autostudent_assert_standing(&pid);
    }
    /// 입력박스 보더의 좌=대화요약 / 우=pane 이름(`/rename`)이 **각자 제자리에**
    /// 그려졌는지: `KASATERM_AUTOBOXLABEL_MS`.
    ///
    /// `KASATERM_AUTOSTUDENT_MS` 와 함께 켜라 — 입력박스를 찍는 건 그쪽 가짜 claude 다.
    /// `KASATERM_TEXT_LOG` 도 필요하다(신고 통이 그 env 로 열린다).
    ///
    /// 이름의 정본은 transcript jsonl 이라 헤드리스엔 없다. 그래서 **가짜 jsonl 을
    /// 심는다** — 진짜 claude 를 띄우는 대신, 판정 대상(`pane_rename_label` →
    /// `session_rename_for`)이 손대지 않은 채로 참이 되게. 프로젝트 디렉터리는
    /// `/tmp/...` 로 슬러그가 나게 골라 거노 실제 프로젝트 폴더를 안 건드린다.
    ///
    /// 판정 셋: ①좌측 요약 ②우측 이름 ③**겹치지 않았나**. ③이 없으면 좌우가 한
    /// 낱말로 붙어 읽히는 실제 버그를 통과시킨다 — 신고는 "썼다"만 말하기 때문이다.
    /// 음성 대조군으로 심지 않은 문자열을 하나 물어 판정이 항상-PASS 가 아님을 보인다.
    pub(crate) fn run_pending_autoboxlabel(&mut self) {
        use std::sync::atomic::{AtomicU8, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static STEP: AtomicU8 = AtomicU8::new(0);
        let due = DUE.get_or_init(|| {
            let ms = std::env::var("KASATERM_AUTOBOXLABEL_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())?;
            Some(Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        // 두 단계인 이유: 좌측 제목은 타이프라이터라 **시간이 지나야** 다 드러난다
        // (`title_typewriter_frame` 이 elapsed/글자당ms). 심은 직후 한 프레임만 그리고
        // 판정하면 좌측은 커서 '▍' 하나뿐이라 "안 그렸다"로 읽힌다 — 실측으로 한 번
        // 그렇게 FAIL 을 냈다. 1단계에서 심고, 2단계에서 다 드러난 뒤 잰다.
        let step = STEP.load(Ordering::Relaxed);
        const AT_MS: [u64; 2] = [0, 1200];
        let Some(&off) = AT_MS.get(step as usize) else { return };
        if Instant::now() < *due + std::time::Duration::from_millis(off) {
            return;
        }
        STEP.store(step + 1, Ordering::Relaxed);
        let Some(pid) = self.ws.lock().unwrap().active_pane.clone() else { return };
        if step == 1 {
            // **판정한 그 프레임**을 찍는다(`_CAP`). 별도 시각에 찍으면 타이프라이터
            // 때문에 픽셀과 판정이 어긋나 "숫자는 PASS 인데 화면엔 없다"가 된다 —
            // 실측으로 한 번 그렇게 봤다.
            // cwd 를 **다시** 심는다. `SocketViewCwd`(handler)가 살아 있어 1단계에서
            // 심어 둔 `pane_view_cwd` 를 실제 cwd 로 덮는다 — 그러면 인레이가 없는
            // jsonl 을 가리켜 좌·우가 한꺼번에 사라진다. 판정 프레임 직전이 유일하게
            // 안전한 시점이다.
            self.boxlabel_seed(&pid);
            // 신고 통을 **비우고** 한 프레임 그린다. 안 비우면 `drew_text` 가
            // "지금까지 한 번이라도"에 답해 **꺼진 기능도 통과한다** — 실제로 그랬다
            // (인레이가 초반엔 그리다 cwd 가 덮여 꺼졌는데 옛 신고가 PASS 를 냈다).
            // 캡처도 같이 이 프레임에 걸어 픽셀과 판정을 맞물린다.
            self.chrome_dirty = true;
            if let Some(g) = self.gpu.as_mut() {
                crate::gpu::clear_text_logs(g);
                if let Ok(path) = std::env::var("KASATERM_AUTOBOXLABEL_CAP") {
                    g.capture_next = Some(path);
                }
            }
            self.render_frame();
            self.autoboxlabel_judge();
            return;
        }

        const SUMMARY: &str = "테스트요약";
        const RENAME: &str = "지어준이름";
        let cwd = std::path::PathBuf::from(BOXLABEL_CWD);
        let sid = BOXLABEL_SID;
        let _ = std::fs::create_dir_all(&cwd);
        let Some(jsonl) = crate::socket::project_jsonl(&cwd, sid) else {
            eprintln!("[autoboxlabel] FAIL — project_jsonl 이 경로를 못 만든다");
            return;
        };
        if let Some(dir) = jsonl.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // 좌측 요약은 **`type:"ai-title"`** 레코드에서 온다 — `"summary"` 로 적었다가
        // 좌측이 안 뜨는 FAIL 을 한 번 봤다(파서가 타입으로 갈라 받는다).
        // 우측 이름은 `custom-title`.
        let body = format!(
            "{{\"type\":\"ai-title\",\"aiTitle\":\"{SUMMARY}\"}}\n\
             {{\"type\":\"custom-title\",\"customTitle\":\"{RENAME}\",\
             \"sessionId\":\"{sid}\",\"nameSource\":\"user\"}}\n"
        );
        if let Err(e) = std::fs::write(&jsonl, body) {
            eprintln!("[autoboxlabel] FAIL — 가짜 jsonl 을 못 썼다: {e}");
            return;
        }
        self.boxlabel_seed(&pid);
        // 헤더 띠는 **일부러 켜지 않는다.** 켜면 `{캐릭터} %N` 이 띠에서도 그려져
        // 판정이 통과하는데, 거노 화면의 학생 pane 은 대부분 단일 탭이라 띠가 없다
        // (`has_header()` = 탭>1 ‖ 이미지 ‖ md ‖ ⋮강제; 학생 띠는 거노가 폐기,
        // main.rs:2146). 그러면 "하네스는 보는데 화면엔 없다"가 된다 — 오늘 그 모양에
        // 두 번 물렸다. 띠를 끈 채로 통과하면 그건 **타이틀바**가 실었다는 뜻이고,
        // 그게 거노가 실제로 보는 자리다.
        // 여기서 한 번 그려 타이프라이터 시계를 출발시킨다. 판정은 2단계.
        self.chrome_dirty = true;
        self.render_frame();
        eprintln!("[autoboxlabel] pane={pid} 가짜 jsonl 심었다 — 타이핑 기다리는 중");
    }

    /// `[Image #N]` 썸네일 툴팁 하네스 — `KASATERM_AUTOIMGTIP_MS`.
    ///
    /// 붙여넣기를 자동화할 수 없어서(클립보드 → claude 입력창) 반대편에서 접근한다:
    /// 그림 한 장이 든 가짜 transcript 를 심고, pane 화면에 그 참조 글자를 찍고,
    /// 커서를 그 글자 위에 놓는다. 실제 경로(그리드 판독 → transcript 조회 →
    /// base64 디코드 → 텍스처 업로드)가 그대로 돈다.
    ///
    /// 세 단계인 이유: 셸이 글자를 뱉는 데 한 틱, 커서를 놓고 나서 툴팁이 뜨기까지
    /// `IMAGE_TIP_DELAY` 가 또 필요하다. 한 번에 하면 「아직 안 뜰 시각」을 찍고
    /// 「안 그린다」로 읽는다.
    pub(crate) fn run_pending_autoimgtip(&mut self) {
        use std::sync::atomic::{AtomicU8, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static STEP: AtomicU8 = AtomicU8::new(0);
        let due = DUE.get_or_init(|| {
            let ms = std::env::var("KASATERM_AUTOIMGTIP_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())?;
            Some(Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        let step = STEP.load(Ordering::Relaxed);
        const AT_MS: [u64; 4] = [0, 900, 1800, 2400];
        let Some(&off) = AT_MS.get(step as usize) else { return };
        if Instant::now() < *due + std::time::Duration::from_millis(off) {
            return;
        }
        STEP.store(step + 1, Ordering::Relaxed);
        let Some(pid) = self.ws.lock().unwrap().active_pane.clone() else {
            eprintln!("[autoimgtip] FAIL — 활성 pane 이 없다");
            return;
        };
        match step {
            0 => {
                if let Err(e) = self.imgtip_seed(&pid) {
                    eprintln!("[autoimgtip] FAIL — 가짜 transcript 를 못 심었다: {e}");
                    return;
                }
                // 프롬프트가 함께 남으면 참조가 몇 행에 앉을지 모르는데, 어차피
                // 그리드에서 찾아 쓸 것이라 상관없다.
                if let Some(pty) = self.pty.get(&pid) {
                    let _ = pty.send_bytes(format!("printf '[Image #{IMGTIP_N}]\\n'\n").as_bytes());
                }
                eprintln!("[autoimgtip] pane={pid} 참조를 찍고 셸 출력을 기다린다");
            }
            1 => {
                // 참조가 실제로 앉은 셀을 그리드에서 읽는다 — 좌표를 손으로
                // 계산하면 그 계산이 검사 대상과 같은 코드가 된다.
                let found = {
                    let ws = self.ws.lock().unwrap();
                    ws.panes.get(&pid).and_then(|p| p.term()).and_then(|t| {
                        let rows: Vec<Vec<crate::GridCell>> = t.cells.clone();
                        crate::render::find_image_refs(&rows).into_iter().next()
                    })
                };
                let Some(r) = found else {
                    eprintln!("[autoimgtip] FAIL — 화면에 [Image #{IMGTIP_N}] 이 안 보인다");
                    return;
                };
                let mid = ((r.col0 + r.col1) / 2) as u16;
                let Some(px) = self.hover_cell_px(&pid, mid, r.row as u16) else {
                    eprintln!("[autoimgtip] FAIL — 셀 ({},{}) 로 커서를 못 옮겼다", r.row, mid);
                    return;
                };
                // `autohover` 를 세워 두면 실제 마우스 이벤트가 이 자리를 안 덮는다.
                self.autohover = Some(px);
                self.cursor_px = px;
                self.chrome_dirty = true;
                self.render_frame();
                eprintln!(
                    "[autoimgtip] 참조 행={} 칸={}~{} → 커서 ({:.1},{:.1})",
                    r.row, r.col0, r.col1, px.0, px.1
                );
            }
            2 => {
                self.chrome_dirty = true;
                if let Some(g) = self.gpu.as_mut() {
                    if let Ok(path) = std::env::var("KASATERM_AUTOIMGTIP_CAP") {
                        g.capture_next = Some(path);
                    }
                }
                self.render_frame();
                let drew = self
                    .gpu
                    .as_ref()
                    .is_some_and(|g| g.drawn_image_keys().any(|k| k.starts_with("imgtip:")));
                let thumb = self
                    .image_tip
                    .as_ref()
                    .and_then(|t| t.thumb.as_ref().map(|(_, w, h)| (*w, *h)));
                eprintln!("[autoimgtip] {} — 썸네일={thumb:?} 그림그림={drew}",
                    if drew { "PASS" } else { "FAIL" });
            }
            // 커서를 치우면 접힌다. 그림이 뜬 것만 재고 여기서 멈추면 "한 번 뜨면
            // 안 사라지는" 툴팁을 통과시킨다.
            _ => {
                let away = (4.0, 4.0);
                self.autohover = Some(away);
                self.cursor_px = away;
                self.chrome_dirty = true;
                self.render_frame();
                let drew = self
                    .gpu
                    .as_ref()
                    .is_none_or(|g| g.drawn_image_keys().any(|k| k.starts_with("imgtip:")));
                eprintln!(
                    "[autoimgtip] 접힘 {} — 남은그림={drew} 상태={}",
                    if !drew { "PASS" } else { "FAIL" },
                    if self.image_tip.is_none() { "비었다" } else { "남았다" }
                );
            }
        }
    }

    /// 목표 셀의 화면 좌표(logical px). 원점 계산을 복제하지 않고 렌더·마우스와
    /// **같은 함수**(`px_to_pane_cell`)로 되물어 가며 맞춘다 — 복제한 좌표는 틀려도
    /// 하네스만 통과시키고 화면은 안 맞는다.
    fn hover_cell_px(&mut self, pid: &str, col: u16, row: u16) -> Option<(f32, f32)> {
        let fs = self.pane_font_scales.get(pid).copied().unwrap_or(1.0).max(0.1);
        let (cw, ch) = (self.cell.w * fs, self.cell.h * fs);
        let (w, h) = self.window.as_ref().map(|w| {
            let s = self.effective_scale();
            let sz = w.inner_size();
            (sz.width as f32 / s, sz.height as f32 / s)
        })?;
        let mut p = (w * 0.5, h * 0.5);
        for _ in 0..5 {
            let (id, c, r) = self.px_to_pane_cell(p.0, p.1)?;
            if id != pid {
                return None;
            }
            if c == col && r == row {
                return Some(p);
            }
            p = (
                p.0 + (col as f32 - c as f32) * cw,
                p.1 + (row as f32 - r as f32) * ch,
            );
        }
        None
    }

    /// `autoimgtip` 이 쓸 가짜 transcript — 그림 한 장이 `[Image #7]` 로 붙은 프롬프트.
    fn imgtip_seed(&mut self, pid: &str) -> std::io::Result<()> {
        use base64::Engine as _;
        let cwd = std::path::PathBuf::from(IMGTIP_CWD);
        std::fs::create_dir_all(&cwd)?;
        let jsonl = crate::socket::project_jsonl(&cwd, IMGTIP_SID).ok_or_else(|| {
            std::io::Error::other("project_jsonl 이 경로를 못 만든다")
        })?;
        if let Some(dir) = jsonl.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 눈으로 바로 갈리는 그림 — 주황 바탕에 흰 대각선. 세로가 짧아서 액자
        // 비율이 원본을 따라가는지도 캡처에서 함께 보인다.
        let img = image::RgbaImage::from_fn(240, 150, |x, y| {
            let on_diag = (x as i32 * 150 / 240 - y as i32).abs() < 6;
            if on_diag {
                image::Rgba([255, 255, 255, 255])
            } else {
                image::Rgba([235, 120, 40, 255])
            }
        });
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(std::io::Error::other)?;
        let body = format!(
            "{{\"type\":\"user\",\"imagePasteIds\":[{IMGTIP_N}],\"message\":{{\"role\":\"user\",\
             \"content\":[{{\"type\":\"text\",\"text\":\"[Image #{IMGTIP_N}] 이거 뭐야\"}},\
             {{\"type\":\"image\",\"source\":{{\"type\":\"base64\",\"media_type\":\"image/png\",\
             \"data\":\"{}\"}}}}]}}}}\n",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        std::fs::write(&jsonl, body)?;
        self.pane_claude_sid.insert(pid.to_string(), IMGTIP_SID.to_string());
        Ok(())
    }

    /// `autoboxlabel` 이 pane 을 가짜 transcript 로 가리키게 한다.
    ///
    /// **둘 다** 심어야 한다 — 인레이는 `pane_view_cwd` 를 먼저 보고 없을 때만
    /// `pane_cwd_cache` 로 간다(render.rs:1298~). 캐시에만 심으면 `SocketViewCwd` 가
    /// 실제 cwd(`~/Desktop`)를 채우는 순간 그쪽이 이겨 `project_jsonl` 이 없는 파일을
    /// 가리키고 좌·우가 **한꺼번에** 사라진다(둘이 같은 jsonl 을 쓴다).
    ///
    /// 그리고 그 소켓 갱신이 계속 도니 **판정 프레임 직전에 다시** 불러야 한다.
    fn boxlabel_seed(&mut self, pid: &str) {
        let cwd = std::path::PathBuf::from(BOXLABEL_CWD);
        self.pane_view_cwd.insert(pid.to_string(), cwd.clone());
        self.pane_cwd_cache.insert(pid.to_string(), cwd);
        self.pane_claude_sid.insert(pid.to_string(), BOXLABEL_SID.to_string());
    }

    /// `autoboxlabel` 2단계 판정. 1단계와 나눈 이유는 그쪽 주석에.
    fn autoboxlabel_judge(&mut self) {
        const SUMMARY: &str = "테스트요약";
        const RENAME: &str = "지어준이름";
        const NEVER: &str = "심지않은문자열";
        let Some(g) = self.gpu.as_ref() else {
            eprintln!("[autoboxlabel] 미측정 — gpu 렌더러가 아니다");
            return;
        };
        let (left, right, never) =
            (g.drew_text(SUMMARY), g.drew_text(RENAME), g.drew_text(NEVER));
        // 정체 표시(`{캐릭터} %N`) — 보더 우측을 `/rename` 자리로 비웠으니 "이 pane 이
        // 누구인가"는 **타이틀바**가 든다(거노 2026-08-05). 하네스가 헤더 띠를 안 켜니
        // (그쪽 주석 참고) 이 판정이 통과하면 타이틀바가 실었다는 뜻이다 — 거노가
        // 단일 탭에서 실제로 보는 자리. 인레이와 달리 크롬 텍스트 draw 라 `text_log` 가
        // 직접 잡는다.
        let who = {
            let ws = self.ws.lock().unwrap();
            ws.active_pane
                .clone()
                .and_then(|p| self.display_pane_char(&ws, &p).map(|n| (n, p)))
        };
        if let Some((name, pid)) = who.as_ref() {
            let want = format!("{name} {pid}");
            eprintln!(
                "[autoboxlabel] 정체 표시 {want:?} → {}",
                match g.drew_text(&want) {
                    Some(true) => "그림 PASS",
                    Some(false) => "안 그림 FAIL — 타이틀바에 pane 아이디가 안 붙었다",
                    None => "미측정",
                }
            );
        }
        if left.is_none() {
            eprintln!("[autoboxlabel] 미측정 — KASATERM_TEXT_LOG 를 켜라");
            return;
        }
        eprintln!(
            "[autoboxlabel] 좌측 요약 {} / 우측 이름 {} / 대조군 {}",
            if left == Some(true) { "그림" } else { "안 그림" },
            if right == Some(true) { "그림" } else { "안 그림" },
            if never == Some(true) { "그림(오염!)" } else { "안 그림" },
        );
        let span = g.staged_span();
        match (left, right, never) {
            (_, _, Some(true)) => eprintln!(
                "[autoboxlabel] FAIL — 심지도 않은 문자열이 그려졌다고 나온다(신고 통이 오염됐다)"
            ),
            (Some(true), Some(true), _) => match span {
                Some((l, c0, _)) if l >= 0 && (c0 as i64) <= l => eprintln!(
                    "[autoboxlabel] FAIL — 좌측이 {l} 열까지 쓰는데 우측이 {c0} 열에서 시작한다(겹침)"
                ),
                Some((l, c0, c1)) => eprintln!(
                    "[autoboxlabel] PASS — 좌 …{l} / 우 {c0}-{c1}, 겹치지 않는다"
                ),
                None => eprintln!("[autoboxlabel] FAIL — 자리 신고가 없다(우측 인레이가 안 불렸다)"),
            },
            (Some(true), _, _) => eprintln!(
                "[autoboxlabel] FAIL — 좌측만 그렸다. 우측은 `session_rename_for` 가 \
                 custom-title 을 못 찾거나 폭이 모자라 포기한 것이다"
            ),
            (_, Some(true), _) => eprintln!("[autoboxlabel] FAIL — 우측만 그렸다(좌측 요약이 죽었다)"),
            _ => eprintln!(
                "[autoboxlabel] FAIL — 둘 다 안 그렸다. 입력박스를 못 찾은 쪽이 크다 \
                 — AUTOSTUDENT 로 가짜 박스를 먼저 찍고 이 하네스를 그 뒤에 둬라"
            ),
        }
    }

    /// 프사와 별개로 **전신이 입력박스 위에 섰는지**.
    ///
    /// 프사(`:profile`)는 statusline 한 줄만 있으면 뜨는데 standing 은 앵커가 더
    /// 걸린다(테두리 두 줄 + 앵커 행이 비어 있어야 함). 그래서 "프사는 뜨는데
    /// 전신만 안 선다"가 실제로 나오고, 프사만 세던 판정은 그걸 통과시켰다.
    ///
    /// `find_standing_anchor` 를 다시 부르지 않는다 — 그러면 검사 대상과 같은
    /// 코드를 믿는 셈이고, 자리는 멀쩡한데 부르는 쪽이 없던 #48 을 또 놓친다.
    /// 한 프레임 그린 뒤 GPU 에 올라간 키와 사각형만 본다. 위치까지 재는 이유:
    /// 키 존재만 보면 전신이 엉뚱한 자리(프사 아래·화면 밖)에 그려져도 PASS 다.
    /// 발은 statusline 프사보다 **위**여야 한다.
    fn autostudent_assert_standing(&mut self, pid: &str) {
        let slug = {
            let ws = self.ws.lock().unwrap();
            match self
                .display_pane_char(&ws, pid)
                .and_then(|n| crate::theme::character_slug(&n))
            {
                Some(s) => s,
                None => return,
            }
        };
        // 그리드 진단을 먼저 찍는다 — 앵커가 안 잡혔을 때 거노 실화면의
        // `KASATERM_STUDENT_DEBUG` 출력과 **같은 단위**로 견줄 수 있어야 한다.
        // 여기 숫자와 실화면 숫자가 다른 지점이 곧 원인이다.
        if let Some(rows) = self
            .ws
            .lock()
            .unwrap()
            .panes
            .get(pid)
            .and_then(|p| p.term())
            .map(|t| t.cells.clone())
        {
            if let Some(sr) = rows
                .iter()
                .rposition(|row| row.iter().any(|c| c.ch == '\u{fffc}'))
            {
                for back in 1..=4usize {
                    let Some(r) = sr.checked_sub(back) else { break };
                    let (mut dash, mut label, mut cw) = (0usize, 0usize, 0usize);
                    for (i, c) in rows[r].iter().enumerate() {
                        match c.ch {
                            '─' => {
                                dash += 1;
                                cw = i + 1;
                            }
                            ' ' | '\0' => {}
                            _ => {
                                label += 1;
                                cw = i + 1;
                            }
                        }
                    }
                    eprintln!(
                        "[autostudent]   rows[{r}] (face_row-{back}) dash={dash} label={label} 내용폭={cw} → rule={}",
                        dash >= 8 && dash > cw / 2
                    );
                }
            }
        }
        let Some(g) = self.gpu.as_ref() else {
            eprintln!("[autostudent] standing 미측정 — gpu 렌더러가 아니다");
            return;
        };
        let pfx = format!("student:{slug}:");
        let keys: Vec<String> =
            g.drawn_image_keys().filter(|k| k.starts_with(&pfx)).map(str::to_string).collect();
        // `:profile` 은 이제 statusline 에 안 그린다(2026-08-11 프사 제거) — 옛
        // 판정은 그 rect 를 세로 기준으로 삼았고, 없으면 「보류」로 조용히 빠져나가
        // 검증이 통째로 무의미해졌다. 기준을 **화면 하단**으로 바꾼다: 전신은
        // 입력박스 위에 서므로 발이 마지막 두 행보다 위여야 한다.
        let stand: Vec<(f32, f32, f32, f32)> =
            keys.iter().flat_map(|k| g.drawn_image_rects(k)).collect();
        let feet = stand.iter().map(|r: &(f32, f32, f32, f32)| r.1 + r.3).fold(f32::MIN, f32::max);
        let floor = g.surface_size().1 as f32 / g.scale() - 2.0 * g.cell_h;
        eprintln!("[autostudent] 전신 {}개 {keys:?}", stand.len());
        if stand.is_empty() {
            eprintln!(
                "[autostudent] standing FAIL — 앵커가 안 잡혔다. 위 rule 표를 보라: \
                 face_row-1 이 rule(label 0)이어야 아래 테두리로 인정되고, 그 위 \
                 16행 안에 rule 이 하나 더(label≤24) 있어야 윗 테두리다. 앵커 행\
                 (윗 테두리 바로 위)에 0열부터 시작하는 내용이 있으면 left_c 가 \
                 음수가 되어 역시 None 이다."
            );
            return;
        }
        eprintln!(
            "[autostudent] 전신 발={feet:.0} 하한={floor:.0} → {}",
            if feet <= floor + 1.0 {
                "PASS — 입력박스 위에 섰다"
            } else {
                "FAIL — 그려졌지만 statusline 아래로 내려갔다(앵커 행 계산이 틀렸다)"
            }
        );
    }
    pub(crate) fn run_pending_autoforeignsplit(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOFOREIGNSPLIT_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        // 활성이 **아닌** window 의 leaf 하나를 고른다 — 활성 window 를 쪼개는 건
        // 옛 코드로도 통과해 회귀를 못 잡는다.
        let Some((win, target)) = (0..self.windows.len())
            .filter(|w| *w != self.active_window)
            .find_map(|w| self.window_leaves(w).into_iter().next().map(|l| (w, l)))
        else {
            eprintln!(
                "[autoforeignsplit] FAIL — 비활성 window 가 없다(window {}개)",
                self.windows.len()
            );
            return;
        };
        let before = self.window_leaves(win).len();
        let prev = self.ws.lock().unwrap().active_pane.clone();
        self.ws.lock().unwrap().active_pane = Some(target.clone());
        let outcome = self.split_pane_auto(None);
        // 소켓 split 은 기본 no-focus 라 부른 뒤 되돌린다(handler.rs SocketSplit).
        if let Some(prev) = prev {
            self.ws.lock().unwrap().active_pane = Some(prev);
        }
        let after = self.window_leaves(win).len();
        match outcome {
            Ok(new_id) => eprintln!(
                "[autoforeignsplit] {}: window {win}(활성={}) {target} → {new_id}, leaf {before}→{after}",
                if after == before + 1 { "PASS" } else { "FAIL(트리에 안 꽂힘)" },
                win == self.active_window
            ),
            Err(e) => eprintln!("[autoforeignsplit] FAIL — split 거부: {e:#}"),
        }
    }
    /// `KASATERM_AUTOUNREAD="%2"` — 그 pane 을 "끝났는데 아직 안 본" 상태로 세운다.
    /// 방 단위인 `KASATERM_AUTOALERT` 의 pane 판 — 완료 숨쉬기가 방 전체가 아니라
    /// **그 세션 줄** 에만 걸리는지 보려면 둘을 따로 세울 수 있어야 한다.
    ///
    /// autowait 과 같은 이유로 매 틱 다시 넣는다: `sync_dock_badge` 가 활성 pane 을
    /// 지우고 지나가므로 한 번 세워 두면 캡처 전에 사라질 수 있다.
    pub(crate) fn apply_autounread(&mut self) {
        use std::sync::OnceLock;
        static IDS: OnceLock<Vec<String>> = OnceLock::new();
        let ids = IDS.get_or_init(|| {
            std::env::var("KASATERM_AUTOUNREAD")
                .map(|v| {
                    v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                })
                .unwrap_or_default()
        });
        for id in ids {
            self.unread_panes.insert(id.clone());
        }
    }
    pub(crate) fn arm_autoexpand(&mut self) {
        let Ok(v) = std::env::var("KASATERM_AUTOEXPAND") else { return };
        for i in v.split(',').filter_map(|s| s.trim().parse::<usize>().ok()) {
            self.expanded_windows.insert(i);
        }
        eprintln!("[autoexpand] {:?}", self.expanded_windows);
    }
    pub(crate) fn arm_autotoggle(&mut self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOTOGGLE_SIDEBAR_MS") else { return };
        let Ok(ms) = ms_str.parse::<u64>() else { return };
        self.autotoggle_left = std::env::var("KASATERM_AUTOTOGGLE_SIDEBAR_N")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        eprintln!("[autotoggle] sidebar flip in {ms}ms (repeat={})", self.autotoggle_left);
        self.autotoggle_sidebar_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    /// Headless arona-panel verification: open the arona window after
    /// `KASATERM_AUTOARONA_MS` (아로나 게이트 + webview load 포함 전체 경로).
    pub(crate) fn arm_autoarona(&mut self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOARONA_MS") else { return };
        let Ok(ms) = ms_str.parse::<u64>() else { return };
        eprintln!("[autoarona] toggle in {ms}ms");
        self.autoarona_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    /// Headless native board-room verification. Unlike AUTOARONA this never
    /// creates a webview; it switches to the PTY-less WGPU room.
    pub(crate) fn arm_autoboard(&mut self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOBOARD_MS") else { return };
        let Ok(ms) = ms_str.parse::<u64>() else { return };
        self.autoboard_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }

    pub(crate) fn run_pending_autoboard(&mut self) {
        let Some(due) = self.autoboard_at else { return };
        if Instant::now() < due {
            return;
        }
        self.autoboard_at = None;
        self.toggle_board_room();
        eprintln!("[autoboard] toggled → open={}", self.board_room_active());
    }
    pub(crate) fn run_pending_autoarona(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
    ) {
        let Some(due) = self.autoarona_at else { return };
        if Instant::now() < due {
            return;
        }
        self.autoarona_at = None;
        self.toggle_arona_panel(event_loop);
        eprintln!(
            "[autoarona] toggled → open={}",
            self.inline_web
                .as_ref()
                .is_some_and(|h| h.kind == crate::InlineWebKind::Arona)
        );
    }
    pub(crate) fn arm_autosplit(&mut self) {
        let Ok(plan) = std::env::var("KASATERM_AUTOSPLIT") else { return; };
        let ms: u64 = std::env::var("KASATERM_AUTOSPLIT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2500);
        let dirs: Vec<kasa_pty::SplitDir> = plan
            .chars()
            .filter_map(|c| match c {
                'h' | 'H' => Some(kasa_pty::SplitDir::Horizontal),
                'v' | 'V' => Some(kasa_pty::SplitDir::Vertical),
                _ => None,
            })
            .collect();
        if dirs.is_empty() {
            return;
        }
        eprintln!("[autosplit] armed: {plan:?} in {ms}ms");
        self.autosplit_plan = dirs;
        self.autosplit_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    /// Headless cross-pane tab-merge simulation. Reads
    /// KASATERM_AUTODRAG="src:from:dst" (e.g. "%2:0:%0") and fires
    /// `simulate_tab_merge` after KASATERM_AUTODRAG_MS (default 5500).
    pub(crate) fn arm_autodrag(&mut self) {
        let Ok(env) = std::env::var("KASATERM_AUTODRAG") else { return };
        let parts: Vec<&str> = env.split(':').collect();
        if parts.len() < 3 {
            eprintln!("[autodrag] expected src:from:dst, got {env:?}");
            return;
        }
        let from: usize = parts[1].parse().unwrap_or(0);
        let ms: u64 = std::env::var("KASATERM_AUTODRAG_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5500);
        self.autodrag_plan = Some((parts[0].to_string(), from, parts[2].to_string()));
        self.autodrag_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
        eprintln!("[autodrag] armed: src={} from={} dst={} fire_in={ms}ms",
            parts[0], from, parts[2]);
    }
    pub(crate) fn run_pending_autodrag(&mut self) {
        let Some(t) = self.autodrag_at else { return };
        if Instant::now() < t { return; }
        self.autodrag_at = None;
        let Some((src, from, dst)) = self.autodrag_plan.take() else { return };
        self.simulate_tab_merge(&src, from, &dst);
    }
    /// Headless cross-window pane move. KASATERM_AUTOPANEMOVE=<dst window idx>
    /// relocates the active window's first leaf beside that window's first leaf
    /// via `move_pane`, exercising the sidebar-chip drop path without a drag.
    pub(crate) fn arm_autopanemove(&mut self) {
        let Ok(env) = std::env::var("KASATERM_AUTOPANEMOVE") else { return };
        let Ok(dst) = env.parse::<usize>() else {
            eprintln!("[autopanemove] expected a window index, got {env:?}");
            return;
        };
        let ms: u64 = std::env::var("KASATERM_AUTOPANEMOVE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5500);
        self.autopanemove_dst = Some(dst);
        self.autopanemove_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
        eprintln!("[autopanemove] armed: dst_window={dst} fire_in={ms}ms");
    }
    pub(crate) fn run_pending_autopanemove(&mut self) {
        let Some(t) = self.autopanemove_at else { return };
        if Instant::now() < t { return; }
        self.autopanemove_at = None;
        let Some(dst_win) = self.autopanemove_dst.take() else { return };
        let moving = self
            .pty_layout
            .as_ref()
            .and_then(|l| l.leaves().first().map(|s| s.to_string()));
        let target = self
            .windows
            .get(dst_win)
            .and_then(|w| w.as_ref())
            .and_then(|l| l.leaves().first().map(|s| s.to_string()));
        match (moving, target) {
            (Some(m), Some(tg)) => {
                eprintln!("[autopanemove] move {m} → window {dst_win} (target {tg})");
                self.move_pane(&m, &tg, DropZone::Right);
            }
            (m, tg) => eprintln!("[autopanemove] skipped: moving={m:?} target={tg:?}"),
        }
    }
    /// Headless drag-preview repro. KASATERM_FORCE_DRAG="%N" (or empty = first
    /// leaf) parks that leaf in an active header_drag with the cursor in a
    /// sibling pane's lower half (Down zone), then stops — so a capture shows
    /// the floating ghost + vacated-slot scrim mid-drag.
    pub(crate) fn arm_force_drag(&mut self) {
        let Ok(env) = std::env::var("KASATERM_FORCE_DRAG") else { return };
        let ms: u64 = std::env::var("KASATERM_FORCE_DRAG_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4000);
        self.force_drag_leaf = Some(env);
        self.force_drag_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
        eprintln!("[force_drag] armed in {ms}ms");
    }
    /// 헤드리스 pane 병합 검증. `KASATERM_AUTOPANEMERGE="%N"`(빈값=첫 leaf) 이면
    /// 그 leaf 를 header_drag 로 집어 **형제의 본문 중앙**에 커서를 두고 라이브
    /// 프리뷰를 적용한 뒤, 릴리즈 핸들러와 똑같은 경로(`take_center_drop`)로 놓는다.
    /// 예약 상태를 `struct App` 필드가 아니라 모듈 static 에 두는 이유는 검증
    /// 전용 스캐폴딩이 병렬 작업의 충돌 핫스팟(App 필드 정의)을 늘리지 않게 하려는
    /// 것이다.
    pub(crate) fn arm_auto_pane_merge(&mut self) {
        let Ok(env) = std::env::var("KASATERM_AUTOPANEMERGE") else { return };
        let ms: u64 = std::env::var("KASATERM_AUTOPANEMERGE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4000);
        *auto_merge_slot().lock().unwrap() =
            Some((Instant::now() + std::time::Duration::from_millis(ms), env));
        eprintln!("[panemerge] armed in {ms}ms");
    }
    pub(crate) fn run_pending_auto_pane_merge(&mut self) {
        let due = {
            let mut slot = auto_merge_slot().lock().unwrap();
            match slot.as_ref() {
                Some((t, _)) if Instant::now() >= *t => slot.take().map(|(_, w)| w),
                _ => None,
            }
        };
        let Some(want) = due else { return };
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|l| l.leaves().into_iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        if leaves.len() < 2 {
            eprintln!("[panemerge] need 2+ panes, have {}", leaves.len());
            return;
        }
        let pane = if leaves.iter().any(|s| *s == want) { want } else { leaves[0].clone() };
        // carried pane 을 빼면 형제가 창을 통째로 채운다 — 그 중앙이 곧 Center 존.
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        let win_w = cols as f32 * self.cell.w;
        let win_h = rows as f32 * self.cell.h;
        self.cursor_px = (pad + win_w / 2.0, TITLE_HEIGHT + win_h * 0.5);
        self.header_drag =
            Some(HeaderDrag { pane: pane.clone(), start: (0.0, 0.0), active: true, from_handle: false });
        self.update_live_drag();
        let hit = self.live_drag_hit(&pane);
        eprintln!("[panemerge] src={pane} cursor=({:.0},{:.0}) hit={hit:?} preview_leaves={:?}",
            self.cursor_px.0, self.cursor_px.1,
            self.pty_layout.as_ref().map(|l| l.leaves().len()));
        let dst = hit.as_ref().map(|(t, _)| t.clone());
        let before = dst.as_ref().and_then(|d| {
            self.ws.lock().ok().and_then(|w| w.panes.get(d).map(|p| p.tabs.len()))
        });
        let merged = self.take_center_drop(&pane);
        self.header_drag = None;
        let after = dst.as_ref().and_then(|d| {
            self.ws.lock().ok().and_then(|w| w.panes.get(d).map(|p| p.tabs.len()))
        });
        let src_gone = self
            .pty_layout
            .as_ref()
            .map(|l| !l.leaves().iter().any(|s| *s == pane))
            .unwrap_or(true);
        let routed = dst
            .as_ref()
            .map(|d| {
                self.ws
                    .lock()
                    .ok()
                    .map(|w| w.pid_to_pane.values().filter(|v| *v == d).count())
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        eprintln!(
            "[panemerge] merged={merged} dst={dst:?} tabs={before:?}→{after:?} src_gone={src_gone} pids_routed_to_dst={routed} leaves={:?}",
            self.pty_layout.as_ref().map(|l| l.leaves().len())
        );
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    pub(crate) fn run_pending_force_drag(&mut self) {
        let Some(t) = self.force_drag_at else { return };
        if Instant::now() < t { return; }
        self.force_drag_at = None;
        let Some(want) = self.force_drag_leaf.take() else { return };
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|l| l.leaves().into_iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        if leaves.len() < 2 {
            eprintln!("[force_drag] need 2+ panes, have {}", leaves.len());
            return;
        }
        let pane = if leaves.iter().any(|s| *s == want) { want } else { leaves[0].clone() };
        // carried pane 을 제거하면 형제가 창 전체를 채운다 — 라이브 hit-test 는 그
        // base 기준이므로 커서를 *창 전체*의 가로 중앙·하단(80%)에 둬야 Down 쐐기에
        // 확실히 떨어진다(거노가 말한 1→2 밑). 형제의 옛 rect 기준으로 두면 정규화
        // 좌표상 대각선 경계라 Right 로 새기도 했다.
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        let win_w = cols as f32 * self.cell.w;
        let win_h = rows as f32 * self.cell.h;
        // `KASATERM_FORCE_DRAG_AT=center` 면 중앙(병합 프리뷰) 자리에 park —
        // 소스가 그리드에서 빠지고 타깃에 "안에 넣기" 박스가 뜬 순간을 캡처한다.
        let fy = match std::env::var("KASATERM_FORCE_DRAG_AT").as_deref() {
            Ok("center") => 0.5,
            _ => 0.8,
        };
        self.cursor_px = (pad + win_w / 2.0, TITLE_HEIGHT + win_h * fy);
        self.header_drag = Some(HeaderDrag { pane, start: (0.0, 0.0), active: true, from_handle: false });
        // 라이브 이동을 실제로 적용 — 실드래그의 mouse-move 가 하는 일을 흉내.
        self.update_live_drag();
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        eprintln!("[force_drag] parked drag; cursor=({:.0},{:.0})", self.cursor_px.0, self.cursor_px.1);
    }
    /// Pane header centre in logical px, mirroring `drop_target_at`'s box
    /// expansion. Used by `simulate_tab_merge` to land the synthetic
    /// cursor exactly where a user would aim "drop on header band".
    pub(crate) fn pane_header_center(&self, id: &str) -> Option<(f32, f32)> {
        let tree = self.pty_layout.as_ref()?;
        let leaves = tree.leaves().len();
        let (cols, rows) = self.window_cells();
        let rects = tree.leaf_rects(cols, rows);
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        let header_band = if leaves > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
        let (_, cx, cy, cw, _) = rects.into_iter().find(|(i, ..)| i == id)?;
        let bx = pad + cx as f32 * self.cell.w;
        let by = TITLE_HEIGHT + cy as f32 * self.cell.h;
        let bw = cw as f32 * self.cell.w;
        Some((bx + bw / 2.0, by - header_band / 2.0))
    }
    /// Simulate dragging `src.tabs[from]` onto `dst`'s header. Mirrors the
    /// release-handler's cross_pane merge branch so we can verify the
    /// path without a real mouse. Logs to stderr.
    pub(crate) fn simulate_tab_merge(&mut self, src: &str, from: usize, dst: &str) {
        let Some((mx, my)) = self.pane_header_center(dst) else {
            eprintln!("[autodrag] no rect for dst={dst}");
            return;
        };
        eprintln!("[autodrag] simulate src={src} from={from} dst={dst} mouse=({mx:.0},{my:.0})");
        let mut moved_pid: Option<String> = None;
        let mut moved: Option<PaneTab> = None;
        let mut src_empty = false;
        {
            let mut ws = self.ws.lock().unwrap();
            if let Some(s) = ws.panes.get_mut(src) {
                if from < s.tabs.len() {
                    let tab = s.tabs.remove(from);
                    moved_pid = tab.pid.clone();
                    moved = Some(tab);
                    if s.active_tab >= s.tabs.len() && !s.tabs.is_empty() {
                        s.active_tab = s.tabs.len() - 1;
                    }
                    src_empty = s.tabs.is_empty();
                    s.dirty = true;
                }
            }
            if let (Some(tab), Some(pid)) = (moved.take(), moved_pid.clone()) {
                ws.pid_to_pane.insert(pid, dst.to_string());
                if let Some(d) = ws.panes.get_mut(dst) {
                    let to = d.tabs.len();
                    d.tabs.insert(to, tab);
                    d.active_tab = to;
                    d.dirty = true;
                }
            }
            if src_empty {
                ws.panes.remove(src);
            }
        }
        if src_empty {
            self.collapse_layout_only(src);
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        let dst_tabs = self.ws.lock().unwrap()
            .panes.get(dst).map(|p| p.tabs.len()).unwrap_or(0);
        eprintln!("[autodrag] done; src_empty={src_empty} dst_tabs={dst_tabs}");
    }
    /// Headless repro for the in-pane tab header: queue N dummy tabs on the
    /// active pane KASATERM_AUTOTABS_MS (default 3200, after autosplit) later.
    /// [검증용] 같은 좌표를 연달아 눌러 탭을 N개 닫는다
    /// (`KASATERM_AUTOCLOSEBURST_MS`, 개수는 `KASATERM_AUTOCLOSEBURST`, 기본 3).
    ///
    /// 크롬식 「닫는 동안 자리 고정」은 **화면으로 안 보인다** — 자리가 어긋나도
    /// 그림은 멀쩡하고, 어긋났다는 건 다음 클릭이 빗나가야 비로소 드러난다. 그래서
    /// 좌표를 한 번 잡고 그 자리만 반복해 눌러, 매번 다른 탭이 실제로 닫히는지 센다.
    /// 실제 클릭과 **같은 판정**(`pane_tab_close_click`)을 지난다.
    pub(crate) fn run_pending_autocloseburst(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOCLOSEBURST_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let n: usize = std::env::var("KASATERM_AUTOCLOSEBURST")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);
        let tabs_of = |app: &App| -> usize {
            let ws = app.ws.lock().unwrap();
            ws.active_pane.as_ref().and_then(|p| ws.panes.get(p)).map_or(0, |p| p.tabs.len())
        };
        // 제목 길이를 일부러 제각각으로 만든다. 이 띠는 알약 폭이 라벨 실측이라,
        // 라벨이 다 비슷하면 **얼리지 않아도** 다음 × 가 얼추 같은 자리에 온다 —
        // 그 상태로 재면 통과해도 아무것도 증명하지 못한다.
        {
            let ws_pane = self.ws.lock().unwrap().active_pane.clone();
            if let Some(outer) = ws_pane {
                let mut ws = self.ws.lock().unwrap();
                if let Some(pane) = ws.panes.get_mut(&outer) {
                    for (i, t) in pane.tabs.iter_mut().enumerate() {
                        let n = 2 + (i * 7) % 20;
                        t.title = Some("가".repeat(n.max(2)));
                        t.title_pinned = true;
                    }
                    pane.dirty = true;
                }
            }
        }
        // 좌표를 **한 번만** 잡는다. 매번 다시 찾으면 자리가 어긋나도 따라가 버려서,
        // 정작 재려던 것이 사라진다.
        self.render_frame();
        let Some(r) = self.pane_tab_close_rects.first().map(|(_, _, r)| *r) else {
            eprintln!("[closeburst] × 자리를 못 찾았다 — 탭이 하나뿐인가");
            return;
        };
        let spot = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        self.cursor_px = spot;
        eprintln!("[closeburst] 자리=({:.1},{:.1}) 탭={}", spot.0, spot.1, tabs_of(self));
        let mut hits = 0;
        for k in 0..n {
            let before = tabs_of(self);
            // 커서는 그대로 둔다 — 손을 안 뗀 상태를 재현해야 동결이 산다.
            self.cursor_px = spot;
            let hit = self.pane_tab_close_click(spot.0, spot.1);
            // 대조군(`KASATERM_AUTOCLOSEBURST_NOFREEZE=1`): 클릭 직후 녹여 동결이
            // **없던 시절**의 동작을 재현한다. 이게 없으면 "4/4 통과"가 동결 덕인지
            // 그저 라벨이 비슷해서인지 못 가른다 — 대조 없는 통과는 증명이 아니다.
            if std::env::var_os("KASATERM_AUTOCLOSEBURST_NOFREEZE").is_some() {
                self.close_freeze.thaw();
            }
            let after = tabs_of(self);
            if hit && after < before {
                hits += 1;
            }
            // 동결이 살아 있는지, 그리고 그 자리에 지금 어떤 × 가 있는지 함께 찍는다.
            // 「맞혔다」만으로는 동결 덕인지 우연인지 못 가른다.
            let frozen = self.close_freeze.tab_slots.is_some();
            let x_here = self
                .pane_tab_close_rects
                .iter()
                .find(|(_, _, r)| {
                    spot.0 >= r.0 && spot.0 <= r.0 + r.2 && spot.1 >= r.1 && spot.1 <= r.1 + r.3
                })
                .map(|(_, i, _)| *i);
            eprintln!(
                "[closeburst] {}번째: 맞힘={hit} 탭 {before}→{after} 얼림={frozen} 그자리의탭={x_here:?}",
                k + 1
            );
            self.render_frame();
        }
        eprintln!("[closeburst] 같은 자리로 {hits}/{n} 개 닫음 (동결 성공 = n 과 같아야)");
    }

    pub(crate) fn arm_autotabs(&mut self) {
        let Ok(n_str) = std::env::var("KASATERM_AUTOTABS") else { return };
        let Ok(n) = n_str.parse::<usize>() else { return };
        if n == 0 {
            return;
        }
        let ms: u64 = std::env::var("KASATERM_AUTOTABS_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3200);
        eprintln!("[autotabs] armed: {n} tab(s) in {ms}ms");
        self.autotabs_n = n;
        self.autotabs_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }
    pub(crate) fn run_pending_autotabs(&mut self) {
        if self.autotabs_n == 0 {
            return;
        }
        let Some(due) = self.autotabs_at else { return };
        if Instant::now() < due {
            return;
        }
        let n = self.autotabs_n;
        // Spawn N real PTY-backed tabs so the headless verify cycle exercises
        // the stage-3 path (each tab has its own shell behind it). Falls back
        // to dummy label-only tabs if the spawn fails (e.g. tmux mode).
        let active = self.ws.lock().unwrap().active_pane.clone();
        if let Some(outer) = active {
            for i in 1..=n {
                if self.spawn_new_tab(&outer, true).is_err() {
                    if let Some(pane) = self.ws.lock().unwrap().panes.get_mut(&outer) {
                        let mut t = PaneTab::default();
                        t.title = Some(format!("탭 {}", i + 1));
                        pane.tabs.push(t);
                        pane.dirty = true;
                    }
                }
            }
            if let Some(pane) = self.ws.lock().unwrap().panes.get_mut(&outer) {
                pane.active_tab = 0;
                pane.dirty = true;
            }
        }
        eprintln!("[autotabs] added {n} tab(s) to active pane");
        self.autotabs_n = 0;
        self.autotabs_at = None;
        self.chrome_dirty = true;
    }

    /// [임시·검증용] Headless 스크롤 주입: `KASATERM_AUTOWHEEL_MS` 후 active pane
    /// 본문 중앙에 커서를 놓고 휠을 `KASATERM_AUTOWHEEL`(기본 10) 번 보낸다. 음수면
    /// 아래로 굴린다. `KASATERM_AUTOWHEEL_PX=<px>` 를 주면 노치(LineDelta) 대신 그
    /// 픽셀만큼의 트랙패드 델타로 보낸다 — 문서 뷰의 픽셀 스크롤 경로는 노치로는
    /// 밟히지 않아, 이것 없이는 헤드리스로 확인할 방법이 없다.
    /// mouse-tracking TUI(claude)면 SGR 로 그 pane 에 전달돼 실제 스크롤 경로를 밟아
    /// sticky prompt 를 재현한다 — sticky pill 감지/표시를 헤드리스로 확인하려는 용도.
    pub(crate) fn run_pending_autowheel(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static FIRED: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOWHEEL_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if FIRED.load(Ordering::Relaxed) || Instant::now() < *due {
            return;
        }
        FIRED.store(true, Ordering::Relaxed);
        let n: i32 = std::env::var("KASATERM_AUTOWHEEL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        let px: Option<f32> = std::env::var("KASATERM_AUTOWHEEL_PX")
            .ok()
            .and_then(|s| s.parse().ok());
        // `KASATERM_AUTOWHEEL_AT=sidebar` — 방 목록 띠 위에서 굴린다. 기본 자리
        // (pane 본문 중앙)로는 사이드바 스크롤 경로가 아예 안 밟혀서, 「목록 끝에
        // 닿을 수 없다」류 버그를 헤드리스로 잡을 방법이 없었다.
        let at_sidebar =
            std::env::var("KASATERM_AUTOWHEEL_AT").is_ok_and(|v| v.trim() == "sidebar");
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        self.cursor_px = if at_sidebar {
            (self.tab_strip_w() / 2.0, TITLE_HEIGHT + 80.0)
        } else {
            (
                pad + cols as f32 * self.cell.w / 2.0,
                TITLE_HEIGHT + rows as f32 * self.cell.h / 2.0,
            )
        };
        let dir = if n < 0 { -1.0 } else { 1.0 };
        eprintln!(
            "[autowheel] {} ticks {} px_mode={px:?} cursor=({:.0},{:.0})",
            n.abs(),
            if n < 0 { "down" } else { "up" },
            self.cursor_px.0,
            self.cursor_px.1
        );
        let before = self.autowheel_md_scroll();
        for _ in 0..n.abs() {
            let delta = match px {
                Some(v) => winit::event::MouseScrollDelta::PixelDelta(
                    winit::dpi::PhysicalPosition::new(0.0, (v * dir) as f64),
                ),
                None => winit::event::MouseScrollDelta::LineDelta(0.0, dir),
            };
            self.handle_wheel(delta);
        }
        eprintln!(
            "[autowheel] md scroll {before:?} -> {:?}",
            self.autowheel_md_scroll()
        );
        if at_sidebar {
            // 도달 가능성은 그림이 아니라 숫자로만 갈린다 — 스크롤이 잠겨 있어도
            // 화면은 멀쩡해 보인다(그려진 데까지는 정상이라).
            let win_h = self
                .window
                .as_ref()
                .map(|w| w.inner_size().height as f32 / self.effective_scale())
                .unwrap_or(800.0);
            let (tabs, ..) = self.sidebar_layout(win_h);
            let n_rooms = self.windows.len();
            let last = tabs.last().map(|(i, _)| *i);
            eprintln!(
                "[autowheel] sidebar rooms={n_rooms} scroll={:.0} max_scroll={:.0} shown={:?}..{:?}                  last_reached={}",
                self.sidebar_scroll_px,
                self.sidebar_max_scroll(win_h),
                tabs.first().map(|(i, _)| *i),
                last,
                last.is_some_and(|i| i + 1 == n_rooms)
            );
        }
    }

    /// active pane 이 문서 뷰면 그 스크롤 오프셋(logical px). autowheel 로그가
    /// "몇 픽셀 움직였나" 를 찍어야 셀 단위로 튀는 회귀를 눈이 아니라 숫자로 잡는다.
    fn autowheel_md_scroll(&self) -> Option<f32> {
        let ws = self.ws.lock().ok()?;
        let id = ws.active_pane.as_ref()?;
        ws.panes.get(id)?.markdown().map(|m| m.scroll)
    }
}

impl App {
    /// Info 패널 그룹 머리 **더블클릭 → 그 학생으로 포커스** 헤드리스 검증.
    /// `KASATERM_AUTOINFODBL_MS` 뒤에, 지금 활성이 아닌 첫 pane 그룹을 두 번
    /// 눌러 `active_pane` 이 실제로 옮겨갔는지 로그로 남긴다. 사람 손 없이
    /// 확인할 수 있는 유일한 경로다 — 포커스는 그려지는 값이 아니라 상태다.
    pub(crate) fn run_pending_autoinfodbl(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        let due = DUE.get_or_init(|| {
            std::env::var("KASATERM_AUTOINFODBL_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|ms| Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        if Instant::now() < *due || DONE.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(wid) = self.window.as_ref().map(|w| w.id()) else { return };
        let before = self.ws.lock().ok().and_then(|w| w.active_pane.clone()).unwrap_or_default();
        // `KASATERM_AUTOINFODBL=win` 이면 방 머리를, 아니면 지금 활성이 아닌
        // pane 머리를 겨눈다 — 두 경로가 서로 다른 동작(방 전환 / pane 포커스)이라
        // 따로 재야 한다.
        let want_win = std::env::var("KASATERM_AUTOINFODBL").is_ok_and(|v| v == "win");
        let target = self
            .info
            .group_rects
            .iter()
            .find(|(k, _)| {
                if want_win {
                    k.strip_prefix("win:").is_some_and(|n| n != self.active_window.to_string())
                } else {
                    !k.starts_with("win:") && *k != before
                }
            })
            .map(|(k, r)| (k.clone(), *r));
        let Some((key, r)) = target else {
            eprintln!("[autoinfodbl] 비활성 pane 그룹이 없다(그룹 {})", self.info.group_rects.len());
            return;
        };
        let (x, y) = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        for _ in 0..2 {
            self.cursor_px = (x, y);
            for state in [ElementState::Pressed, ElementState::Released] {
                self.window_event(
                    event_loop,
                    wid,
                    WindowEvent::MouseInput {
                        device_id: DeviceId::dummy(),
                        state,
                        button: MouseButton::Left,
                    },
                );
            }
        }
        let after = self.ws.lock().ok().and_then(|w| w.active_pane.clone()).unwrap_or_default();
        eprintln!(
            "[autoinfodbl] {key} 더블클릭 → active_pane {before} → {after} (win={}) 접힘={}",
            self.active_window,
            if key.starts_with("win:") {
                self.info.group_collapsed.contains(&key)
            } else {
                !self.info.pane_expanded.contains(&key)
            }
        );
    }
}

impl App {
    /// `KASATERM_AUTOPORTPOP_MS` — 포트 팝오버를 눈으로 확인하는 하네스.
    ///
    /// 격리 리그에는 dev 서버도 학생 pane 도 없어 실제 listen 포트가 잡히지
    /// 않는다. 빈 목록만 찍으면 정작 봐야 할 것(레포 묶음 머리 · 세 갈래 점 색 ·
    /// 호버 시 ×)이 하나도 안 나오므로, 스냅샷에 가짜 행을 심고 연다. 심는 값은
    /// 세 상태를 하나씩 덮는다 — 살아 있는 pane 의 것 / 재부모화된 것 / 주인이
    /// 사라진 것.
    pub(crate) fn run_pending_autoportpop(&mut self) {
        use std::sync::atomic::{AtomicU8, Ordering};
        use std::sync::OnceLock;
        static DUE: OnceLock<Option<Instant>> = OnceLock::new();
        static STEP: AtomicU8 = AtomicU8::new(0);
        let due = DUE.get_or_init(|| {
            let ms = std::env::var("KASATERM_AUTOPORTPOP_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())?;
            Some(Instant::now() + std::time::Duration::from_millis(ms))
        });
        let Some(due) = due else { return };
        let step = STEP.load(Ordering::Relaxed);
        const AT_MS: [u64; 3] = [0, 700, 1400];
        let Some(&off) = AT_MS.get(step as usize) else { return };
        if Instant::now() < *due + std::time::Duration::from_millis(off) {
            return;
        }
        STEP.store(step + 1, Ordering::Relaxed);
        // 무엇을 펼칠지. 팝오버가 여럿이라 하네스를 하나 더 만드는 대신 값으로
        // 가른다 — 캡처는 실행당 한 장이므로 어차피 두 번 돌려야 한다.
        let want = std::env::var("KASATERM_AUTOPOPOVER").unwrap_or_default();
        let tunnel = want.starts_with("tunnel");
        // 계정 게이지·드롭다운은 실제 한도 응답이 있어야 그려진다 — 격리 리그에는
        // 없으므로 값을 심는다. 5시간이 낮고 주간이 높은 조합인 것은 그게 정확히
        // 2026-08-05 사고의 형태이고, 「둘 다 나란히」가 그걸 막는지 보는 것이
        // 이 검증의 목적이라서다.
        // 「전환 직후」와 「이름이 겹치는 두 슬롯」을 함께 세운다. 둘 다 실계정이
        // 있어야 재현되는 상태라 격리 리그에서는 심을 수밖에 없다.
        //
        // 겹침은 **라벨로** 만든다 — 라벨이 있으면 `account_display` 가 그걸 그대로
        // 쓰므로 `claude auth status` 를 부를 필요가 없다(리그에는 로그인이 없다).
        // 전환 중은 배지의 `account_dir` 을 활성 슬롯과 다르게 두면 된다.
        // 하단바에 **나머지 계정까지** 세우는 줄(2026-08-27). 슬롯 넷을 심어
        // 네 상태를 한 줄에 모은다 — 활성(게이지 유지) · 여유 · 위험(90%↑) ·
        // 못 읽은 슬롯(`—`). 마지막이 특히 중요하다: 빈칸이나 0% 로 그리면
        // 「여유 있음」으로 읽혀 옮길지 말지를 정확히 반대로 만든다.
        if step == 0 && want.starts_with("statusbar-accounts") {
            self.set_statusbar_all_accounts = !want.ends_with("off");
            self.set_claude_accounts = vec![
                crate::socket::ClaudeAccount {
                    id: "acct-5".to_string(),
                    label: "개인사이오닉".to_string(),
                },
                crate::socket::ClaudeAccount {
                    id: "acct-4".to_string(),
                    label: "사이오닉팀".to_string(),
                },
                crate::socket::ClaudeAccount {
                    id: "acct-1".to_string(),
                    label: "지메일".to_string(),
                },
                crate::socket::ClaudeAccount {
                    id: "acct-3".to_string(),
                    label: "네이버".to_string(),
                },
            ];
            self.set_claude_account = "acct-1".to_string();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let mk = |dir: &str, w: Vec<(&str, f32)>, stale: bool| crate::UsageBadge {
                pct: w.iter().map(|(_, p)| *p).fold(0.0, f32::max),
                label: "7d".to_string(),
                stale,
                account_dir: dir.to_string(),
                resets_at: Some(now + 3 * 3600 + 54 * 60),
                windows: w.into_iter().map(|(l, p)| (l.to_string(), p)).collect(),
            };
            // 활성 슬롯의 키는 **작업대 경로**다 — 상태줄이 「전환 중」을 가르는
            // 근거가 그 경로라, 금고 경로로 심으면 게이지가 영영 `…` 로 남는다.
            let active_dir = crate::claude_auth::runtime_dir_for("acct-1", "acct-1")
                .map_or(String::new(), |p| p.to_string_lossy().into_owned());
            let active = mk(&active_dir, vec![("5h", 12.0), ("7d", 47.0)], false);
            if let Ok(mut g) = self.claude_usage.lock() {
                *g = Some(active.clone());
            }
            if let Ok(mut g) = self.claude_usage_all.lock() {
                g.insert(active_dir, active);
                if let Some(d) = crate::socket::claude_account_dir("acct-5") {
                    g.insert(
                        d.to_string_lossy().into_owned(),
                        mk("", vec![("5h", 8.0), ("7d", 8.0)], false),
                    );
                }
                if let Some(d) = crate::socket::claude_account_dir("acct-4") {
                    g.insert(
                        d.to_string_lossy().into_owned(),
                        mk("", vec![("5h", 91.0), ("7d", 91.0)], true),
                    );
                }
                // acct-3(네이버)은 일부러 안 넣는다 — 토큰이 만료돼 못 읽은 슬롯.
            }
            self.chrome_dirty = true;
            return;
        }
        // 펼친 목록이 실시간으로 갱신되는지 — 화면이 아니라 **신호**를 잰다.
        // 폴러는 `usage_menu_open` 원자값 하나로 박자를 바꾸므로, 목록을 열었을 때
        // 그게 서는지와 poke 가 나가는지가 곧 그 기능이다.
        if step == 1 && want.starts_with("statusbar-accounts") {
            match self.status_account_rect {
                Some(r) => {
                    self.account_menu = true;
                    self.account_menu_anchor = Some(r);
                    self.account_menu_provider = Some(crate::AccountProvider::Claude);
                    self.chrome_dirty = true;
                    eprintln!("[autoportpop] 계정 목록 폈다 anchor={r:?}");
                }
                None => eprintln!("[autoportpop] FAIL — 계정 칩이 아직 안 그려졌다"),
            }
            return;
        }
        if step == 2 && want.starts_with("statusbar-accounts") {
            use std::sync::atomic::Ordering;
            // 렌더가 한 번 돈 뒤라야 값이 서 있다 — 여닫는 손잡이가 아니라 그리는
            // 자리에서 맞추기 때문이다.
            let open = crate::handler::usage_menu_open().load(Ordering::Relaxed);
            // poke 는 폴러가 집어 가면 사라지는 값이라, 남아 있든 이미 걷혔든
            // 「열림이 섰다」가 확인되면 그 자리에서 나갔다는 뜻이다.
            eprintln!("[autoportpop] usage_menu_open={open}");
            return;
        }
        if want.starts_with("statusbar-accounts") {
            return;
        }
        if step == 0 && want == "switching" {
            self.set_claude_accounts = vec![
                crate::socket::ClaudeAccount {
                    id: "acct-2".to_string(),
                    label: "goenho0613@naver.com".to_string(),
                },
                crate::socket::ClaudeAccount {
                    id: "acct-3".to_string(),
                    label: "goenho0613@gmail.com".to_string(),
                },
            ];
            self.set_claude_account = "acct-3".to_string();
            if let Ok(mut g) = self.claude_usage.lock() {
                *g = Some(crate::UsageBadge {
                    pct: 95.0,
                    label: "7d".to_string(),
                    stale: false,
                    // 떠나온 슬롯의 값 — 활성(acct-3)과 다르니 「읽는 중」이 되어야 한다.
                    account_dir: "/tmp/kasaterm-rig-acct-2".to_string(),
                    resets_at: None,
                    windows: vec![("5h".to_string(), 12.0), ("7d".to_string(), 95.0)],
                });
            }
            self.chrome_dirty = true;
            return;
        }
        if want == "switching" {
            return;
        }
        // 계정 목록(서브메뉴). **고르기 전에** 각 슬롯의 5h·7일이 다 보이는지 보는
        // 것이 목적이라, 슬롯마다 다른 값을 심고 하나는 값 자체를 비워 둔다
        // (「한도 모름」 자리 표시가 나와야 한다 — 빈칸은 「여유 있음」으로 읽힌다).
        // `accounts-compact` 는 「간단히」 밀도 — 행이 한 줄로 접히는 쪽을 본다.
        if step == 0 && want.starts_with("accounts") {
            self.set_usage_compact = want.ends_with("compact");
            self.set_claude_accounts = vec![
                crate::socket::ClaudeAccount {
                    id: "acct-2".to_string(),
                    label: "사이오닉팀플랜".to_string(),
                },
                crate::socket::ClaudeAccount {
                    id: "acct-3".to_string(),
                    label: "개인계정".to_string(),
                },
            ];
            self.set_claude_account = String::new();
            // 리셋 시각을 심는다 — 이 목록은 「지금 옮길까 기다릴까」를 정하는
            // 자리라, 퍼센트만 있으면 90% 가 12분 뒤 풀리는 것인지 3시간 뒤인지
            // 구별이 안 된다(거노 2026-08-25).
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let mk = |dir: &str, w: Vec<(&str, f32)>, stale: bool, in_secs: u64| {
                crate::UsageBadge {
                    pct: w.iter().map(|(_, p)| *p).fold(0.0, f32::max),
                    label: "7d".to_string(),
                    stale,
                    account_dir: dir.to_string(),
                    resets_at: Some(now + in_secs),
                    windows: w.into_iter().map(|(l, p)| (l.to_string(), p)).collect(),
                }
            };
            let base = mk("", vec![("5h", 12.0), ("7d", 95.0)], false, 7980);
            if let Ok(mut g) = self.claude_usage.lock() {
                *g = Some(base.clone());
            }
            if let Ok(mut g) = self.claude_usage_all.lock() {
                g.insert(String::new(), base);
                if let Some(d) = crate::socket::claude_account_dir("acct-2") {
                    g.insert(
                        d.to_string_lossy().into_owned(),
                        mk("", vec![("5h", 68.0), ("7d", 41.0)], true, 2820),
                    );
                }
                // acct-3 은 일부러 안 넣는다.
            }
            self.chrome_dirty = true;
            return;
        }
        if step == 1 && want.starts_with("accounts") {
            match self.status_account_rect {
                Some(r) => {
                    self.account_menu = true;
                    self.account_menu_anchor = Some(r);
                    self.account_menu_provider = Some(crate::AccountProvider::Claude);
                    self.chrome_dirty = true;
                    eprintln!("[autoportpop] 계정 목록 anchor={r:?}");
                }
                None => eprintln!("[autoportpop] FAIL — 계정 칩이 아직 안 그려졌다"),
            }
            return;
        }
        if want.starts_with("accounts") {
            return;
        }
        if step == 0 && want == "account" {
            let badge = crate::UsageBadge {
                pct: 95.0,
                label: "7d".to_string(),
                stale: false,
                account_dir: String::new(),
                resets_at: Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs())
                        + 3 * 3600
                        + 54 * 60,
                ),
                windows: vec![("5h".to_string(), 12.0), ("7d".to_string(), 95.0)],
            };
            if let Ok(mut g) = self.claude_usage.lock() {
                *g = Some(badge.clone());
            }
            if let Ok(mut g) = self.claude_usage_all.lock() {
                g.insert(String::new(), badge);
            }
            self.chrome_dirty = true;
            return;
        }
        if step == 1 && want == "account" {
            match self.status_account_rect {
                Some(r) => {
                    self.account_menu = true;
                    self.account_menu_anchor = Some(r);
                    self.chrome_dirty = true;
                    eprintln!("[autoportpop] 계정 드롭다운 anchor={r:?}");
                }
                None => eprintln!("[autoportpop] FAIL — 계정 칩이 아직 안 그려졌다"),
            }
            return;
        }
        if want == "account" {
            return;
        }
        if step == 0 && tunnel {
            if want == "tunnel-off" {
                // 실제로 끄지 **않는다** — 이 기계에서 진짜 터널이 돌고 있으면
                // 검증이 그걸 내려버린다. 표시만 닫힌 것으로 세운다.
                self.statusbar.tunnel_on = Some(false);
                self.statusbar.tunnel_host = None;
                self.statusbar.tunnel_checked = Some(Instant::now());
                return;
            }
            // 실제로 문을 열지는 **않는다**. 켜진 화면(주소 줄·복사 버튼)을 보려고
            // 밖으로 나가는 문을 여는 건 검증이 치를 값이 아니다.
            self.statusbar.tunnel_on = Some(true);
            self.statusbar.tunnel_host = Some("kasaterm-probe.example.com".to_string());
            self.statusbar.tunnel_checked = Some(Instant::now());
            return;
        }
        if step == 0 {
            let row = |port: u16, pid: u32, repo: &str, site: &str, name: &str, label: &str,
                       orphan: bool, dead: bool| crate::info::PortRow {
                port,
                pid,
                kind: crate::info::port_kind(port, name),
                name: name.to_string(),
                orphan,
                pane: Some("%1".to_string()),
                label: label.to_string(),
                repo: repo.to_string(),
                site: site.to_string(),
                owner_dead: dead,
            };
            self.info.view.ports = vec![
                row(5173, 111, "tmuxify", "kasaterm 웹터미널", "node", "코하루", false, false),
                row(3000, 222, "tmuxify", "arona-ui", "npm", "코하루", true, false),
                row(8080, 333, "mission-control", "Mission Control", "next-server", "유우카", true, true),
                row(4000, 444, "", "(제목 없음)", "python3", "", true, false),
            ];
            self.info.view.outside = 22;
            if want.starts_with("usage") {
                // 격리 리그의 프로세스 트리는 서넛뿐이라 목록이 한 화면에 다
                // 들어가고, 그러면 정작 봐야 할 것(길 때 잘리는지·굴러가는지)이
                // 검증되지 않는다. 실기의 형태를 심는다 — 2026-08-27 실측에서
                // 이 앱 트리는 231개였고 상위 30개가 88% 를 차지했다.
                //
                // CPU 와 메모리의 **순서를 일부러 어긋나게** 심는다. 둘이 같은
                // 차례면 탭을 옮겨도 목록이 그대로라, 잣대별 정렬이 실제로
                // 도는지 화면으로 가릴 수가 없다(2026-08-29 탭 분리).
                self.statusbar.usage_top = (0..30)
                    .map(|i| {
                        (
                            1000 + i as u32,
                            ((i * 7) % 30) as f32 * 0.15,
                            (450 - i as u64 * 12) * 1024,
                            format!("proc-{i:02}"),
                        )
                    })
                    .collect();
                self.statusbar.usage_rows = 231;
                // 실측 비율을 그대로 재현한다(2026-08-27: 트리 233개 · 합 13.6G ·
                // 상위 30개 8.3G). 합계를 목록 합보다 작게 잡으면 「그 외」가 0 으로
                // 눌려, 정작 검증하려던 그 줄이 뜻 없는 값으로 찍힌다 — cpu 도 같아서
                // 목록 합(약 70%)이 총합 92% 안에 들어가도록 계수를 잡았다.
                self.statusbar.res = Some((92.0, 13_600_000_000));
            }
            return;
        }
        if step == 1 {
            // 앵커는 지난 프레임이 세워 둔 칩 사각형이다 — 그게 없으면 상태줄이
            // 아직 안 그려진 것이고, 그때 억지로 열면 팝오버가 엉뚱한 자리에 뜬다.
            let (kind, anchor) = match want.as_str() {
                w if w.starts_with("usage") => {
                    (crate::state::StatusbarPopover::Usage, self.statusbar.res_rect)
                }
                w if w.starts_with("tunnel") => {
                    (crate::state::StatusbarPopover::Tunnel, self.statusbar.tunnel_rect)
                }
                _ => (crate::state::StatusbarPopover::Ports, self.statusbar.port_rect),
            };
            match anchor {
                Some(r) => {
                    self.toggle_statusbar_popover(kind, r);
                    // 스크롤을 다음 step 으로 미루면 안 된다 — 팝오버가 열린 뒤
                    // 화면이 정적이면 프레임이 돌지 않아 그 step 이 영영 안 불린다
                    // (실측 2026-08-27: 열림만 찍히고 스크롤은 매번 누락).
                    if want.contains("-end") {
                        self.statusbar.popover_scroll = 9999.0;
                        eprintln!("[autoportpop] 스크롤 끝까지");
                    }
                    // 끄기 버튼은 겨눈 줄에만 뜨는데, 하네스는 마우스를 못
                    // 움직인다. 두 번째 클릭을 기다리는 상태를 직접 심어 그 줄이
                    // 무슨 말을 하는지 화면으로 확인한다 — 잘못 누르면 사람이
                    // 쓰던 앱이 닫히는 자리라 문구가 정확해야 한다.
                    // 어느 잣대로 폈는지. 경고가 없으면 마지막에 보던 탭이
                    // 남으므로, 검증에서는 찍어서 고정한다.
                    if want.contains("-cpu") {
                        self.statusbar.usage_tab = crate::state::UsageTab::Cpu;
                    } else if want.contains("-mem") {
                        self.statusbar.usage_tab = crate::state::UsageTab::Mem;
                    }
                    eprintln!("[autoportpop] 탭={:?}", self.statusbar.usage_tab);
                    // 바깥 앱 구역은 경고 구간에서만 펴진다. 검증 기계가 마침
                    // 한가하면 그 구역이 통째로 안 그려져, 거기 붙은 끄기 버튼도
                    // 잣대별 정렬도 화면으로 확인할 수가 없다 — 임계를 넘긴
                    // 상태를 심는다(값은 실기에서 온 그대로 쓴다).
                    if want.contains("-hog") {
                        for a in self.statusbar.usage_outside.iter_mut() {
                            a.hot = 99;
                        }
                    }
                    if want.contains("-armed") {
                        match self.statusbar.usage_outside.first() {
                            Some(a) => {
                                self.statusbar.usage_kill_armed = Some((a.pid, Instant::now()));
                                eprintln!("[autoportpop] 끄기 겨눔 pid={}", a.pid);
                            }
                            None => eprintln!("[autoportpop] FAIL — 바깥 앱 표본이 비었다"),
                        }
                    }
                    // 바깥 앱 판정은 화면에 몇 줄로만 남아서, 안 떴을 때 「값이
                    // 없는 것」과 「임계를 못 넘은 것」을 가릴 수가 없다.
                    for a in &self.statusbar.usage_outside {
                        eprintln!(
                            "[autoportpop] 바깥 {} cpu={:.0}% hot={} hog={} rss={:.1}G",
                            a.name,
                            a.cpu,
                            a.hot,
                            a.is_hog(),
                            a.rss as f32 / (1024.0 * 1024.0 * 1024.0)
                        );
                    }
                    eprintln!(
                        "[autoportpop] 우리 자신 cpu={:.0}% hot={} hot?={}",
                        self.statusbar.usage_self.0,
                        self.statusbar.usage_self.1,
                        crate::input::is_hot(self.statusbar.usage_self.1)
                    );
                    eprintln!("[autoportpop] 열림 {kind:?} anchor={r:?}");
                }
                None => eprintln!("[autoportpop] FAIL — 칩이 아직 안 그려졌다"),
            }
            return;
        }
        if want.starts_with("usage") {
            // 누를 것이 없는 팝오버라 여기서 끝난다(스크롤은 열 때 함께 했다).
            return;
        }
        if tunnel {
            // 복사 버튼 호버.
            match self
                .statusbar
                .popover_hits
                .iter()
                .find(|(h, _)| matches!(h, crate::state::StatusbarHit::CopyTunnelHost))
                .map(|(_, r)| *r)
            {
                Some(r) => {
                    self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                    self.chrome_dirty = true;
                    eprintln!("[autoportpop] 커서 → 복사 {:?}", self.cursor_px);
                }
                None => eprintln!("[autoportpop] FAIL — 복사 버튼이 없다"),
            }
            return;
        }
        // 호버 상태(×·열기 아이콘)는 커서가 행 위에 있어야만 그려진다. 좌표를
        // 지어내지 않고 **지난 프레임이 실제로 쌓은 히트렉트**에서 가져온다 —
        // 손으로 계산한 좌표는 틀려도 하네스만 통과시킨다.
        match self
            .statusbar
            .popover_hits
            .iter()
            .find(|(h, _)| matches!(h, crate::state::StatusbarHit::OpenPort(_)))
            .map(|(_, r)| *r)
        {
            Some(r) => {
                self.cursor_px = (r.0 + r.2 - 30.0, r.1 + r.3 / 2.0);
                self.chrome_dirty = true;
                eprintln!("[autoportpop] 커서 → {:?}", self.cursor_px);
            }
            None => eprintln!("[autoportpop] FAIL — 포트 행 히트렉트가 없다"),
        }
    }
}
