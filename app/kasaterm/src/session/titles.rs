//! 칸 제목 — 에이전트 대화 기록 꼬리에서 세션 제목과 effort 를 읽어 칸 이름에 싣는다.
//! 복원이 띄운 model·effort 도 여기 적어 두어 저장이 이어받는다.
use super::*;

impl App {
    /// claude 가 자기 세션에 붙인 이름(`/rename`)을 그 pane 의 제목으로 옮긴다.
    ///
    /// pane 제목이 읽는 것은 두 가지뿐이었다 — 터미널이 쏘는 OSC 와 그것을 따라가는
    /// GUI 사본. claude 안에서 `/rename` 을 치면 이름은 **transcript 의 `custom-title`
    /// 레코드**로만 남고 OSC 로는 안 나가므로, 탭에는 옛 이름이 그대로 남았다
    /// (사용자 2026-08-15 「소환할때 /rename 안되는거」). 실측으로 그 갈림을 확인했다:
    /// `/rename` 뒤에도 OSC 는 활동 요약이었고, 새 이름은 transcript 의 마지막
    /// custom-title 에만 있었다.
    ///
    /// **핀을 세워서** 옮긴다. 안 세우면 claude 가 계속 쏘는 활동 제목이 다음 프레임에
    /// 그대로 덮어, 이름이 한 번 깜빡이고 사라진다.
    ///
    /// ★ **사람이 정한 이름이 이긴다.** 지금 제목이 우리가 마지막에 심은 값과 다르면
    /// 그 사이 누군가(`surface.rename`·파일 탭)가 직접 정한 것이라
    /// 비켜선다. 처음 보는 pane 에 이미 핀이 서 있으면 그것도 남의 것이다.
    ///
    /// 같은 꼬리에서 **ultracode 상태**도 함께 뽑는다(`ultra_verdict_in`). 두 사실이
    /// 같은 파일의 같은 512KB 에 있어서, 따로 읽으면 같은 바이트를 두 번 읽는다.
    pub(crate) fn sync_session_titles(&mut self) {
        // transcript 꼬리를 읽는 일이라 프레임 박자로 돌 것이 아니다. rename 도
        // `/effort` 도 사람이 한 번 치는 사건이라 이 정도면 「치자마자」로 읽힌다.
        {
            let mut last = session_title_scan().lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2)) {
                return;
            }
            *last = Some(Instant::now());
        }
        self.sync_session_titles_now();
    }

    /// 박자를 무시한 한 바퀴 — 하네스가 한 프레임 안에서 시나리오를 이어 돌린다.
    /// 제품 경로는 언제나 `sync_session_titles` 로 들어온다.
    pub(crate) fn sync_session_titles_now(&mut self) {
        const WINDOW: u64 = 512 * 1024;
        let panes: Vec<(String, String)> = self
            .pane_claude_sid
            .iter()
            .filter(|(id, _)| self.pty.contains_key(id.as_str()))
            .map(|(id, sid)| (id.clone(), sid.clone()))
            .collect();
        let mut codex_panes: Vec<(String, String)> = Vec::new();
        for (pane_id, sid) in panes {
            // 갈래는 **지금 도는 하네스**로 가른다. 기록이 어느 창고에 있는지로
            // 추론하면, 하네스를 갈아 낀 자리(옛 번호가 남은 자리)가 엉뚱한 창고를
            // 읽는다.
            if self
                .pty
                .get(&pane_id)
                .and_then(|s| s.active_agent())
                .is_some_and(|k| matches!(k, kasa_pty::AgentKind::Codex))
            {
                codex_panes.push((pane_id, sid));
                continue;
            }
            let Some(path) = crate::socket::transcript_path_for_session(&sid) else {
                continue;
            };
            let meta = std::fs::metadata(&path).ok();
            let mtime = meta.as_ref().and_then(|m| m.modified().ok());
            let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            {
                let seen = session_titles().lock().unwrap();
                // 파일이 그대로면 이름도 상태도 그대로다 — 안 읽는다. 노는 pane 은
                // 여기서 전부 걸러지고, 읽는 것은 지금 말하고 있는 pane 뿐이다.
                if mtime.is_some()
                    && seen
                        .get(&pane_id)
                        .is_some_and(|e| e.mtime == mtime && e.sid == sid)
                {
                    continue;
                }
            }
            // 512KB — `pane_bg_active` 가 같은 파일에 쓰는 창과 같은 값이다. 두 곳이
            // 따로 읽는 건 중복이지만, 캐시를 하나로 묶으면 `struct App` 에 필드가
            // 붙는다(병렬 작업 핫스팟, CLAUDE.md). 같은 파일이라 페이지 캐시가 받는다.
            let (tail, _) = crate::socket::read_tail(&path, WINDOW);
            // 이름표는 **사람이 붙인 것만** 얹는다(2026-09-14 지시 「자동으로 붙는거 아예
            // 없애도돼」). 앱이 스스로 이름을 고르던 두 갈래를 여기서 걷었다:
            //
            // ① 명부(`~/.claude/sessions/<pid>.json`)의 세션 이름 — 사람이 친 `/rename` 을
            //    지키려고 1순위로 올렸던 것인데(2026-09-08), 그 이름의 **기본값이 부팅 때
            //    자동으로 붙는 세션 주소**(`yuzu-p0-4iz` 꼴)라서, 아무도 이름을 안 붙인
            //    창은 헤더가 통째로 주소가 됐다. 세션 이름은 원래 헤더가 아니라 입력박스
            //    보더 우측이 드는 겹이다(2026-08-27 확정: 헤더 = 캐릭터 + pane 번호 + 작업명).
            //    `/rename` 은 전사본에도 `custom-title` 로 쓰므로 아래 한 줄이 그대로 잡는다.
            // ② claude 가 스스로 지은 제목(`ai-title`) — 사람이 고른 말이 아니다.
            //
            // 파일에 아무것도 쓰지 않는다. 이름을 적는 것은 사람이 붙일 때뿐이다.
            let found = tail
                .lines()
                .filter_map(kasa_socket::sessions::custom_title_of_line)
                .last();
            let mut seen = session_titles().lock().unwrap();
            let entry = seen.entry(pane_id.clone()).or_default();
            // pane 이 다른 세션을 물면 기준선을 새로 잡는다 — 옛 대화의 마커를 이
            // 세션 것으로 읽으면 안 된다.
            if entry.sid != sid {
                *entry = PaneTranscript {
                    sid: sid.clone(),
                    ..Default::default()
                };
            }
            entry.mtime = mtime;
            // 이 세션을 처음 본 순간의 파일 크기가 기준선이다. 그 앞의 effort 마커는
            // **지난 실행**이 남긴 것이다 — 같은 jsonl 에 `--resume` 이 이어 쓰기
            // 때문이다. 훅(ultracode-mark.py)은 프로세스 시작 시각으로 같은 선을
            // 긋는데, 앱은 자기가 이 세션을 언제 물었는지 아니 파일 크기로 곧장
            // 자를 수 있다.
            let baseline = *entry.baseline.get_or_insert(len);
            // 기준선 이후로 자란 부분만 본다. 꼬리가 기준선보다 앞에서 시작하면 그
            // 앞부분을 잘라낸다.
            let tail_start = len.saturating_sub(WINDOW);
            let cut = usize::try_from(baseline.saturating_sub(tail_start)).unwrap_or(usize::MAX);
            if let Some(fresh) = tail
                .get(cut..)
                .or_else(|| (cut >= tail.len()).then_some(""))
            {
                // 마커가 없으면 이전 판정을 지운다 — 세션을 갈아탄 뒤라면 옛 상태를
                // 물려받으면 안 된다. 마커가 있으면 그것이 훅 표식보다 최신이다.
                entry.ultra = ultra_verdict_in(fresh);
            }
            // 꼬리 밖으로 밀려 안 보이는 것은 「이름이 없어졌다」가 아니다 — 대화가
            // 길어지면 옛 스탬프는 512KB 밖으로 나간다. 심어 둔 이름을 걷지 않는다.
            let Some(name) = found else { continue };
            let known = entry.title.clone();
            drop(seen);
            if let Some(adopted) = self.adopt_scanned_title(&pane_id, name, &known) {
                session_titles()
                    .lock()
                    .unwrap()
                    .entry(pane_id)
                    .or_default()
                    .title = Some(adopted);
            }
        }
        self.sync_codex_titles(&codex_panes);
    }

    /// 스캔이 찾은 이름을 pane 제목으로 얹는다. 하네스마다 이름이 적히는 자리는
    /// 다르지만(claude 는 transcript, codex 는 상태 db) **얹는 규칙은 하나여야 한다**
    /// — 갈리면 한쪽에서만 사람이 손수 붙인 이름이 덮인다.
    ///
    /// 사람이 붙인 이름은 지킨다: 핀이 섰는데 그 값이 우리가 심어 둔 것과 다르면 그건
    /// 사람 손이다. 반환은 캐시에 적어 둘 이름이고, 지켜야 할 자리면 `None`.
    pub(super) fn adopt_scanned_title(
        &mut self,
        pane_id: &str,
        name: String,
        known: &Option<String>,
    ) -> Option<String> {
        {
            let mut ws = self.ws.lock().unwrap();
            let pane = ws.panes.get_mut(pane_id)?;
            if pane.title.as_deref() == Some(name.as_str()) {
                return Some(name);
            }
            if pane.title_pinned && pane.title != *known {
                return None;
            }
            pane.title = Some(name.clone());
            pane.title_pinned = true;
        }
        self.chrome_dirty = true;
        Some(name)
    }

    /// codex 자리의 이름표. `/rename` 도 codex 가 스스로 짓는 제목도 상태 db 의
    /// `threads.name` 에만 적히고 rollout 에는 흔적이 없다(2026-09-05 실측) — 앱이
    /// 거기를 안 읽는 동안 codex 창의 헤더는 폴더 이름에 머물러, 창이 여럿일 때 어느
    /// 것이 무슨 일인지 가릴 수가 없었다(2026-09-05 지적).
    pub(super) fn sync_codex_titles(&mut self, panes: &[(String, String)]) {
        if panes.is_empty() {
            return;
        }
        // db 는 codex pane 전부가 함께 쓰는 파일이라, 아무도 이름을 안 건드렸으면
        // stat 한 번으로 전부 건너뛴다. claude 쪽이 transcript mtime 으로 노는 pane 을
        // 거르는 것과 같은 자리다.
        let mtime = crate::socket::codex_state_db_mtime();
        let want: Vec<(String, String)> = {
            let seen = session_titles().lock().unwrap();
            panes
                .iter()
                .filter(|(pane, sid)| {
                    mtime.is_none()
                        || !seen
                            .get(pane.as_str())
                            .is_some_and(|e| e.mtime == mtime && e.sid == *sid)
                })
                .cloned()
                .collect()
        };
        if want.is_empty() {
            return;
        }
        let names =
            crate::socket::codex_thread_names(&want.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>());
        for (pane_id, sid) in want {
            let known = {
                let mut seen = session_titles().lock().unwrap();
                let entry = seen.entry(pane_id.clone()).or_default();
                // pane 이 다른 세션을 물면 기준선을 새로 잡는다 — claude 경로와 같다.
                if entry.sid != sid {
                    *entry = PaneTranscript {
                        sid: sid.clone(),
                        ..Default::default()
                    };
                }
                entry.mtime = mtime;
                entry.title.clone()
            };
            let Some(name) = names.get(&sid) else {
                continue;
            };
            if let Some(adopted) = self.adopt_scanned_title(&pane_id, name.clone(), &known) {
                session_titles()
                    .lock()
                    .unwrap()
                    .entry(pane_id)
                    .or_default()
                    .title = Some(adopted);
            }
        }
    }

    /// 꼬리 스캔이 내린 ultracode 판정 — `refresh_pane_ultracode` 가 훅 표식과 합칠 때
    /// 쓴다. 표가 작아(살아 있는 claude pane 수) 통째로 복제하는 편이 락을 들고
    /// 다니는 것보다 낫다.
    pub(crate) fn scanned_ultracode(&self) -> HashMap<String, bool> {
        session_titles()
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(pane, e)| e.ultra.map(|v| (pane.clone(), v)))
            .collect()
    }

    /// 이 pane 을 `--effort ultracode` 로 되살렸다고 표시한다. 제품 경로는
    /// `restore_leaf` 하나뿐이고, 하네스가 같은 문을 써야 재는 것과 도는 것이 같다.
    pub(crate) fn mark_restored_ultracode(&self, pane: &str) {
        restored_ultracode()
            .lock()
            .unwrap()
            .insert(pane.to_string());
    }

    /// 복원이 `--effort ultracode` 로 되살린 pane 들 — 아직 아무 마커도 안 생긴
    /// 구간의 기준선이다. 사라진 pane 은 여기서 함께 걷는다(id 는 재사용된다).
    pub(crate) fn restored_ultracode_panes(&self) -> std::collections::HashSet<String> {
        let mut set = restored_ultracode().lock().unwrap();
        set.retain(|id| self.pty.contains_key(id));
        set.clone()
    }

    /// 복원이 이 pane 을 띄울 때 쓴 모델·effort 를 적어 둔다.
    ///
    /// 세션 저장이 담는 값(`reported_agent_cfg`)은 **statusline 이 보고한 것**이라,
    /// 훅이 아직 안 돈 pane 은 비어 있다. 그 상태로 저장되면 그 창은 다음 복원에서
    /// 기본 모델로 뜬다 — 사용자가 고른 값이 재시작 한 번에 조용히 풀린다. 방금
    /// 그 값으로 띄웠다는 사실은 앱만 알고 있으므로 여기 남긴다.
    pub(crate) fn mark_restored_agent_cfg(&self, pane: &str, model: &str, effort: &str) {
        if model.is_empty() && effort.is_empty() {
            return;
        }
        restored_agent_cfg()
            .lock()
            .unwrap()
            .insert(pane.to_string(), (model.to_string(), effort.to_string()));
    }
}

/// pane 하나에 대해 transcript 꼬리에서 알아낸 것들.
#[derive(Default, Clone)]
pub(super) struct PaneTranscript {
    /// 이 값이 그대로면 파일을 다시 안 읽는다.
    pub(super) mtime: Option<std::time::SystemTime>,
    /// 우리가 마지막으로 심은 제목 — 남이 바꿨는지 가르는 기준.
    pub(super) title: Option<String>,
    /// 이 세션을 처음 봤을 때의 파일 크기. 그 앞의 effort 마커는 지난 실행 것이다.
    pub(super) baseline: Option<u64>,
    /// 기준선 이후 마지막 effort 마커. `None` = 이번 실행 구간엔 마커가 없다.
    pub(super) ultra: Option<bool>,
    /// 기준선을 잡을 때의 세션 id — 바뀌면 위 값들을 통째로 버린다.
    pub(super) sid: String,
}

/// pane 별 transcript 파생 상태.
///
/// `struct App` 이 아니라 모듈 static 인 이유는 그 struct 가 병렬 작업의 충돌
/// 핫스팟이기 때문이다(CLAUDE.md).
pub(super) fn session_titles() -> &'static std::sync::Mutex<HashMap<String, PaneTranscript>> {
    static C: std::sync::OnceLock<std::sync::Mutex<HashMap<String, PaneTranscript>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// 마지막으로 훑은 시각 — 꼬리 읽기의 박자를 잡는다.
/// 복원이 띄울 때 쓴 pane 별 (model, effort). `restored_ultracode` 와 같은 이유로
/// struct App 밖에 둔다(병렬 작업 규칙).
pub(super) fn restored_agent_cfg() -> &'static std::sync::Mutex<HashMap<String, (String, String)>> {
    static C: std::sync::OnceLock<std::sync::Mutex<HashMap<String, (String, String)>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

pub(super) fn session_title_scan() -> &'static std::sync::Mutex<Option<Instant>> {
    static C: std::sync::OnceLock<std::sync::Mutex<Option<Instant>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// 복원이 `--effort ultracode` 로 되살린 pane 들. 훅의 argv 기준선과 같은 구실이다
/// (`collab-hooks/ultracode-mark.py` 의 `_proc_start`) — 그쪽은 `ps` 로 argv 를 캐고
/// 이쪽은 자기가 띄운 명령을 그냥 안다.
pub(super) fn restored_ultracode() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static C: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// 이 구간에서 **가장 뒤에 있는** effort 마커의 판정. 없으면 `None`.
///
/// ⚠️ **니들 셋과 last-wins 규칙은 `collab-hooks/ultracode-mark.py` 의 `NEEDLES` 와
/// 같아야 한다.** 훅은 프롬프트를 보낼 때 돌고 이쪽은 앱이 훑는다 — 두 벌이 같은
/// 사실을 다르게 읽으면 글로우와 저장이 갈리고, 한쪽만 고친 날 조용히 어긋난다.
/// 훅이 python 이라 상수를 공유할 길이 없어 주석으로 못 박는다. 한쪽을 고치거든
/// 다른 쪽도 같이.
///
/// `Set effort level to` 는 `/effort` 가 화면에 뱉는 줄이라 **ultracode 가 아닌 값도
/// 잡아야 한다** — ultracode 에서 xhigh 로 내린 것도 이 줄로만 알 수 있다.
pub(super) fn ultra_verdict_in(text: &str) -> Option<bool> {
    const ENTER: &str = r#""type":"ultra_effort_enter""#;
    const EXIT: &str = r#""type":"ultra_effort_exit""#;
    const CMD: &str = r#""content":"<local-command-stdout>Set effort level to "#;
    let mut best: Option<(usize, bool)> = None;
    let mut take = |at: Option<usize>, on: bool| {
        if let Some(i) = at {
            if best.is_none_or(|(b, _)| i > b) {
                best = Some((i, on));
            }
        }
    };
    take(text.rfind(ENTER), true);
    take(text.rfind(EXIT), false);
    if let Some(i) = text.rfind(CMD) {
        take(Some(i), text[i + CMD.len()..].starts_with("ultracode"));
    }
    best.map(|(_, on)| on)
}
