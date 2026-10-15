//! Info 「내 프롬프트」 — 지금 보는 칸에서 사람이 직접 친 말만 모아 늘어놓고, 누르면 그 질문 자리로 옮긴다.
//!
//! 글은 그 칸에 묶인 대화 기록(claude transcript·codex rollout)을 `chat_view::parse` 와 같은 규칙으로 읽은 것이고,
//! 쪽지·완료 보고처럼 다른 쪽이 입력칸에 넣은 말은 `parse::relayed` 로 뺀다. 대화 보기는 첫
//! 화면만 보면 되니 꼬리만 읽지만, 여기는 모아보기라 파일을 처음부터 읽는다 — 사람 턴만 JSON 으로 푼다.
//!
//! 기록에는 화면 자리가 없다. 자리는 칸이 이미 가진 정본에서 글로 맞춰 찾는다 — 풀스크린 claude 는 칸 안 mod 가
//! 잰 행(`prompt_nav`), 거울은 받은 화면의 앵커. mod 가 없는 풀스크린 codex 와 풀스크린 원본의 거울은 자리를
//! 아는 곳이 없어 휠을 굴려 그 질문이 화면 맨 위에 설 때까지 찾아간다. 질문 사이를 옮겨 다니는 길은 이것
//! 하나다 — 칸 맨 위에 띄우던 질문 띠는 걷었다. 붙여넣기 접힘·그림 자리표시처럼 화면에 다른 글로 남는 말은 앞뒤로
//! 맞은 두 질문 사이의 차례로 메운다.
//!
//! 상태는 GUI 스레드의 지역 통에 둔다 — 그리기(`draw_info_col`)는 App 을 못 보고, App·InfoState 를 안
//! 늘리려는 것이다(`prompt_nav` 의 `NAV` 와 같은 방식).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::chat_view::parse;
use crate::{gpu, theme, UserEvent};

const POLL_EVERY: Duration = Duration::from_millis(1000);
/// 화면 정본과 다시 맞추는 주기 — 스크롤하면 「지금 보는 질문」이 바뀐다.
const PLACE_EVERY: Duration = Duration::from_millis(250);
/// 말풍선에 싣는 전문 상한. 붙여넣은 로그 전체를 띄우면 창을 덮는다.
const TIP_CHARS: usize = 600;
/// 맞추기 표(질문 × 화면 줄) 상한 — 넘으면 뒤에서부터 차례대로만 맞춘다.
const PLACE_TABLE_MAX: usize = 4_000_000;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Prompt {
    pub(crate) text: String,
    /// 기록 줄의 시각(유닉스 초).
    pub(crate) at: Option<u64>,
    /// 묻는 말(AskUserQuestion·codex 질문)에 고른 답이면 그 질문. 화면에 질문 줄로 서지 않아 자리 맞추기에서 뺀다.
    pub(crate) asked: Option<String>,
}

enum Source {
    File(PathBuf),
    Remote(String, String),
}

impl Source {
    fn key(&self) -> String {
        match self {
            Source::File(p) => p.to_string_lossy().into_owned(),
            Source::Remote(base, rid) => format!("remote:{base}|{rid}"),
        }
    }

    /// `(사람 턴 줄만 모은 글, 다음 자리, 처음부터 다시 읽었나, 원본 파일 이름)`. 거울은 원본이 주는 꼬리만
    /// 오고, `held` 는 지금 든 자리가 어느 파일의 것인지다(`TranscriptChunk::source`).
    fn read(&self, offset: u64, held: &str) -> Result<Option<(String, u64, bool, String)>, String> {
        match self {
            Source::File(p) => read_wanted(p, offset).map(|(raw, next, reset)| Some((raw, next, reset, String::new()))).map_err(|e| e.to_string()),
            Source::Remote(base, rid) => crate::chat_view::history::remote(base, rid, offset, held)
                .map(|c| c.map(|c| (keep_wanted(&c.raw), c.offset, c.reset || offset == 0, c.source)))
                .map_err(|e| e.to_string()),
        }
    }
}

/// 사람 턴일 수 있는 줄 — claude 의 user 줄(도구 결과 빼고)과 예약, codex 의 사람 말, 그리고 AskUserQuestion 의
/// 물음·답 한 쌍(답은 도구 결과라 물음 줄이 있어야 짝을 찾는다). 본문 안의 따옴표는 이스케이프돼 있어 이 열쇠들은
/// 실제 키에만 걸린다. 나머지(도구 출력이 대부분)는 JSON 으로 풀지도 않는다.
fn wanted(line: &str) -> bool {
    let answer = line.contains(r#""answers":{"#) && line.contains(r#""questions":["#);
    (line.contains(r#""type":"user""#) && (!line.contains(r#""tool_use_id""#) || answer))
        || line.contains(r#""name":"AskUserQuestion""#)
        || line.contains(r#""queue-operation""#)
        || line.contains(r#""user_message""#)
        || line.contains(r#""UserMessage""#)
}

fn keep_wanted(raw: &str) -> String {
    raw.split_inclusive('\n').filter(|l| wanted(l)).collect()
}

/// 끝의 쓰다 만 줄은 다음 바퀴로 미룬다. 파일이 줄었으면(세션 교체) 처음부터.
fn read_wanted(path: &Path, offset: u64) -> std::io::Result<(String, u64, bool)> {
    use std::io::{BufRead, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let reset = offset == 0 || offset > len;
    let mut at = if reset { 0 } else { offset };
    f.seek(SeekFrom::Start(at))?;
    let mut reader = std::io::BufReader::new(f);
    let mut kept = String::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        at += n as u64;
        if let Ok(line) = std::str::from_utf8(&buf) {
            if wanted(line) {
                kept.push_str(line);
            }
        }
    }
    Ok((kept, at, reset))
}

fn stamp(line: &str) -> Option<u64> {
    const KEY: &str = r#""timestamp":""#;
    let rest = &line[line.find(KEY)? + KEY.len()..];
    crate::socket::rfc3339_epoch(&rest[..rest.find('"')?])
}

#[derive(Default)]
struct Feed {
    offset: u64,
    /// `offset` 이 가리키는 원본 파일 — 거울의 원본 칸이 다른 파일로 옮겨 가면 새 꼬리가 `reset` 으로 온다.
    source: String,
    conv: parse::Conversation,
    /// `conv.items` 와 같은 차례의 시각.
    stamps: Vec<Option<u64>>,
    prompts: Vec<Prompt>,
    version: u64,
    loaded: bool,
    error: Option<String>,
    busy: bool,
}

impl Feed {
    /// 기록 조각 하나. 세션이 바뀐 것을 알아채면 false — 다음 바퀴가 처음부터 다시 읽는다.
    fn take(&mut self, raw: &str) -> bool {
        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            let asking: Vec<usize> = (0..self.conv.items.len())
                .filter(|&i| matches!(self.conv.items[i], parse::Item::Answered { pairs: None, .. }))
                .collect();
            if !self.conv.apply(line) {
                return false;
            }
            self.stamps.resize(self.conv.items.len(), stamp(line));
            // 답은 물은 때가 아니라 고른 때다 — 자리를 비운 사이 몇 시간 묵기도 한다.
            for i in asking {
                if matches!(self.conv.items[i], parse::Item::Answered { pairs: Some(_), .. }) {
                    self.stamps[i] = stamp(line);
                }
            }
        }
        self.prompts = own_prompts(&self.conv.items, &self.stamps);
        true
    }
}

/// 사람이 친 말과 묻는 말에 고른 답. claude 는 `parse` 가 이미 다른 쪽이 넣은 말에 `from` 을 달지만 codex
/// 쪽은 안 달아서 여기서 한 번 더 `relayed` 로 거른다. 예약(아직 안 보낸 말)·꺼내진 예약·슬래시 명령은 질문이
/// 아니다. 답은 claude 는 AskUserQuestion 결과로, codex 는 사람 턴 안의 `<send_user_message_question_reply>` 로 온다.
pub(crate) fn own_prompts(items: &[parse::Item], stamps: &[Option<u64>]) -> Vec<Prompt> {
    own_prompts_at(items, stamps).into_iter().map(|(_, p)| p).collect()
}

/// 같은 규칙으로 고른 말과 그 말이 선 칸 번호 — 대화 보기가 그 칸으로 옮긴다.
pub(crate) fn own_prompt_items(items: &[parse::Item]) -> Vec<(usize, Prompt)> {
    own_prompts_at(items, &[])
}

fn own_prompts_at(items: &[parse::Item], stamps: &[Option<u64>]) -> Vec<(usize, Prompt)> {
    let answers = |pairs: Vec<(String, String)>, at: Option<u64>| -> Vec<Prompt> {
        pairs
            .into_iter()
            .filter(|(_, a)| !a.trim().is_empty() && a != "—")
            .map(|(q, a)| Prompt { text: a, at, asked: Some(q) })
            .collect()
    };
    items
        .iter()
        .enumerate()
        .flat_map(|(i, item)| {
            let at = stamps.get(i).copied().flatten();
            let found = (|| -> Vec<Prompt> { match item {
                parse::Item::Bubble { user: true, from: None, queued: false, text, images } => {
                    let text = text.trim();
                    if let Some(pairs) = codex_answers(text) {
                        return answers(pairs, at);
                    }
                    if parse::relayed(text).is_some() {
                        return Vec::new();
                    }
                    let text = match (text.is_empty(), *images) {
                        (false, _) => text.to_string(),
                        (true, 0) => return Vec::new(),
                        (true, n) => format!("그림 {n}장"),
                    };
                    vec![Prompt { text, at, asked: None }]
                }
                parse::Item::Answered { pairs: Some(pairs), .. } => answers(pairs.clone(), at),
                _ => Vec::new(),
            } })();
            found.into_iter().map(move |p| (i, p))
        })
        .collect()
}

/// codex 가 묻고 사람이 고른 답 — `[{"question":…,"answer":…}]` 를 감싼 사람 턴. 못 읽어도 사람 말로 세지 않는다.
fn codex_answers(text: &str) -> Option<Vec<(String, String)>> {
    let body = text.strip_prefix("<send_user_message_question_reply>")?;
    let body = body.trim().strip_suffix("</send_user_message_question_reply>").unwrap_or(body);
    let list: Vec<serde_json::Value> = serde_json::from_str(body.trim()).unwrap_or_default();
    Some(
        list.iter()
            .filter_map(|v| Some((v["question"].as_str()?.to_string(), v["answer"].as_str()?.to_string())))
            .collect(),
    )
}

fn poll(feed: Arc<Mutex<Feed>>, source: Source, proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
    {
        let Ok(mut f) = feed.lock() else { return };
        if f.busy {
            return;
        }
        f.busy = true;
    }
    std::thread::spawn(move || {
        let (offset, held) = feed.lock().map(|f| (f.offset, f.source.clone())).unwrap_or_default();
        let chunk = source.read(offset, &held);
        let Ok(mut f) = feed.lock() else { return };
        f.busy = false;
        let before = (f.prompts.clone(), f.loaded, f.error.clone());
        match chunk {
            Ok(None) => f.error = None,
            Ok(Some((raw, next, reset, file))) => {
                if reset {
                    *f = Feed { version: f.version, ..Feed::default() };
                }
                if f.take(&raw) {
                    f.offset = next;
                    f.source = file;
                } else {
                    *f = Feed { version: f.version, ..Feed::default() };
                }
                f.error = None;
            }
            Err(e) => f.error = Some(e),
        }
        f.loaded = true;
        if (f.prompts.clone(), f.loaded, f.error.clone()) != before {
            f.version += 1;
            drop(f);
            let _ = proxy.send_event(UserEvent::Redraw);
        }
    });
}

// ── 화면 자리 맞추기 ─────────────────────────────────────────────────────────────

/// 맞춰 볼 머리글 — 첫 줄, 프롬프트 마커와 공백을 걷은 앞 40자. 화면 앵커는 감긴 첫 행뿐이고 mod 는 첫 줄
/// 80자까지만 들고 있어서, 앞부분끼리만 견준다.
fn head_key(s: &str) -> String {
    let first = s.trim().lines().next().unwrap_or("");
    first.trim_start_matches(['❯', '›', '>']).chars().filter(|c| !c.is_whitespace()).take(40).collect()
}

fn same(prompt: &str, shown: &str) -> bool {
    !prompt.is_empty() && !shown.is_empty() && (prompt.starts_with(shown) || shown.starts_with(prompt))
}

/// 화면 목록에 있어도 사람 질문이 아닌 줄 — 다른 쪽이 넣은 말, 슬래시 명령, claude 가 plugin 프롬프트
/// 위에 그리는 출처 줄. 차례로 메울 때 이 줄들을 세지 않는다.
fn not_own(shown: &str) -> bool {
    let t = shown.trim().trim_start_matches(['❯', '›', '>']).trim_start();
    t.starts_with('/') || t.starts_with("Prompt from the ") || parse::relayed(t).is_some()
}

/// 기록의 질문마다 화면 목록의 몇 번째인지. 글이 맞는 쌍을 차례를 지키며 가장 많이 잇고(짧은 「ㅇㅇ」
/// 이 엉뚱한 옛 줄에 붙어 뒤 질문을 다 밀어내지 않게 — 앞에서부터 욕심내면 그렇게 된다), 맞은 둘 사이에
/// 남은 질문과 남은 화면 줄의 수가 같으면 그 차례대로 메운다.
pub(crate) fn place(prompts: &[String], shown: &[String]) -> Vec<Option<usize>> {
    let pk: Vec<String> = prompts.iter().map(|s| head_key(s)).collect();
    let sk: Vec<String> = shown.iter().map(|s| head_key(s)).collect();
    let (n, m) = (pk.len(), sk.len());
    let mut out = vec![None; n];
    if n == 0 || m == 0 {
        return out;
    }
    if (n + 1).saturating_mul(m + 1) <= PLACE_TABLE_MAX {
        let w = m + 1;
        let mut dp = vec![0u16; (n + 1) * w];
        for i in 1..=n {
            for j in 1..=m {
                dp[i * w + j] = if same(&pk[i - 1], &sk[j - 1]) {
                    dp[(i - 1) * w + j - 1].saturating_add(1)
                } else {
                    dp[(i - 1) * w + j].max(dp[i * w + j - 1])
                };
            }
        }
        let (mut i, mut j) = (n, m);
        while i > 0 && j > 0 {
            if same(&pk[i - 1], &sk[j - 1]) && dp[i * w + j] == dp[(i - 1) * w + j - 1].saturating_add(1) {
                out[i - 1] = Some(j - 1);
                i -= 1;
                j -= 1;
            } else if dp[(i - 1) * w + j] >= dp[i * w + j - 1] {
                i -= 1;
            } else {
                j -= 1;
            }
        }
    } else {
        let mut hi = m;
        for i in (0..n).rev() {
            if let Some(j) = (0..hi).rev().find(|&j| same(&pk[i], &sk[j])) {
                out[i] = Some(j);
                hi = j;
            }
        }
    }
    // 맞은 쌍 사이의 틈 — 앞 경계는 (-1, -1), 뒤 경계는 (n, m) 이다.
    let mut fences: Vec<(isize, isize)> = vec![(-1, -1)];
    fences.extend(out.iter().enumerate().filter_map(|(i, j)| j.map(|j| (i as isize, j as isize))));
    fences.push((n as isize, m as isize));
    for pair in fences.windows(2) {
        let ((p0, s0), (p1, s1)) = (pair[0], pair[1]);
        let left: Vec<usize> = ((p0 + 1)..p1).map(|i| i as usize).collect();
        let free: Vec<usize> = ((s0 + 1)..s1).map(|j| j as usize).filter(|&j| !not_own(&shown[j])).collect();
        if !left.is_empty() && left.len() == free.len() {
            for (i, j) in left.into_iter().zip(free) {
                out[i] = Some(j);
            }
        }
    }
    out
}

// ── 지금 보는 칸의 목록 ─────────────────────────────────────────────────────────

/// 칸이 가진 질문 자리 목록 — 이동은 그 목록의 번호로 한다.
enum Shelf {
    /// 풀스크린 claude — 칸 안 mod 가 잰 행. 이동도 mod 에 시킨다.
    Mod(crate::prompt_nav::NavState),
    /// 거울 — 받은 화면이 이 기기 스크롤백에 쌓인다.
    Mirror(Vec<kasa_pty::PromptAnchor>, i64),
    /// mod 없는 풀스크린 칸(codex)과 풀스크린 원본의 거울. 대화를 TUI 가 통째로 쥐고 있어 자리를 모른다 —
    /// 휠을 굴려 그 질문이 화면 맨 위에 설 때까지 찾아간다(`Seek`). 값은 지금 화면에 보이는 질문 줄.
    Screen(Vec<String>),
}

impl Shelf {
    fn texts(&self) -> Vec<String> {
        match self {
            Shelf::Mod(s) => s.prompts.iter().map(|(_, t)| t.clone()).collect(),
            Shelf::Mirror(a, _) => a.iter().map(|a| a.text.clone()).collect(),
            Shelf::Screen(rows) => rows.clone(),
        }
    }

    fn current(&self) -> Option<usize> {
        match self {
            Shelf::Mod(s) => usize::try_from(s.current).ok(),
            Shelf::Mirror(a, top) => a.iter().rposition(|a| a.abs_line <= *top),
            Shelf::Screen(_) => None,
        }
    }
}

/// 풀스크린 TUI 의 질문 줄 — claude 는 `❯`, codex 는 `›` 로 연다.
fn prompt_row(row: &str) -> bool {
    row.trim_start().starts_with(['\u{276f}', '\u{203a}'])
}

fn row_text(row: &[kasa_bridge::screen::Cell]) -> String {
    row.iter().filter(|c| c.ch != '\0').map(|c| c.ch).collect::<String>().trim_end().to_string()
}

/// 휠로 찾아가는 중인 이동 하나.
#[derive(Clone)]
struct Seek {
    pane: String,
    want: String,
    down: bool,
    /// 목적지가 화면에 보이기 시작했다 — 한 노치씩 다가간다.
    near: bool,
    /// 한쪽 끝까지 가도 없어 반대쪽으로 돌았다. 그쪽 끝에서도 없으면 그만둔다.
    turned: bool,
    /// 원본 격자 좌표 — 거울도 받은 원본 화면의 가운데다.
    cell: (u16, u16),
    /// 거울 — 휠이 원본까지 갔다가 화면이 돌아오는 데 네트워크 한 바퀴가 걸린다.
    remote: bool,
    /// 거울에서 보낸 휠의 답을 기다리는 중 — (보낼 때 화면, 보낸 때).
    waiting: Option<(u64, Instant)>,
    /// 거울 — 지난번 보낸 노치 수와 그때 화면. 답이 오면 글이 몇 줄 옮겨 갔는지로 노치당 줄 수(`rate`)를 잰다.
    sent_notches: usize,
    sent_rows: Vec<String>,
    rate: Option<f32>,
    /// 거울 — 친 말 머리글 차례와 그 가운데 목적지. 화면에 보이는 질문의 차례로 목적지를 지나쳤는지 안다.
    keys: Vec<String>,
    index: usize,
    /// 거울 — 한 번에 가는 거리(화면 몇 장). 안 지나쳤으면 늘리고, 지나쳤으면(`back`) 돌아서서 화면보다 짧게만
    /// 걷는다 — 긴 답 한가운데에 내리면 질문이 안 보여 지나쳤는지 모르므로, 되돌아갈 때는 건너뛰지 않게 걷는다.
    screens: f32,
    back: bool,
    last: Instant,
    sent: u32,
    stall: u32,
    print: u64,
}

const SEEK_EVERY: Duration = Duration::from_millis(30);
const SEEK_MAX: u32 = 400;
/// 화면이 이만큼 연달아 그대로면 끝(맨 위·맨 아래)에 닿은 것이다.
const SEEK_STALL: u32 = 12;
/// 거울은 휠을 보내고 화면이 바뀌어 올 때까지 다음 휠을 미룬다 — 답을 안 보고 쏘아 대면 보이기 시작한
/// 목적지를 길에 있던 휠이 지나쳐 버린다. 이만큼 기다려도 그대로면 끝에 닿은 것으로 센다.
const REMOTE_WAIT: Duration = Duration::from_millis(900);
const REMOTE_STALL: u32 = 2;
/// 노치당 줄 수의 위 끝 — 잰 값이 이보다 크면 잘못 잰 것으로 보고 자른다.
const RATE_MAX: f32 = 12.0;
/// 거울 한 걸음의 거리(화면 장 수). 한 장보다 짧으면 목적지가 화면을 건너뛸 수 없다. 위 끝은 지나쳤을 때 되돌아
/// 걸을 거리의 상한이다.
const SCREENS_MIN: f32 = 0.6;
const SCREENS_MAX: f32 = 4.0;

thread_local! {
    static SEEK: RefCell<Option<Seek>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Line {
    /// 사람이 친 말(`asked` 없는 `Feed::prompts`) 가운데 몇 번째인지 — 누르면 이 번호로 다시 맞춰 옮긴다. 답 줄은
    /// 그 질문을 받은 턴, 곧 바로 앞의 친 말이다.
    pub(crate) index: usize,
    /// 체크 표시를 기억하는 열쇠(`check_key`).
    pub(crate) key: String,
    /// 묻는 말에 고른 답 줄 — 앞에 「답」 표지를 단다.
    pub(crate) answer: bool,
    pub(crate) head: String,
    pub(crate) tip: String,
    pub(crate) at: Option<u64>,
    pub(crate) reachable: bool,
    pub(crate) current: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Status {
    /// 대화 기록이 묶인 칸이 아니다(셸 등) — 묶음을 안 세운다.
    #[default]
    None,
    Loading,
    Error(String),
    Ready,
}

type Rect = (f32, f32, f32, f32);

#[derive(Default)]
struct State {
    seen: Option<(String, String)>,
    key: Option<String>,
    feed: Option<Arc<Mutex<Feed>>>,
    polled: Option<Instant>,
    placed: Option<Instant>,
    version: u64,
    status: Status,
    /// 새 것이 위.
    lines: Vec<Line>,
    /// (칸, 질문 번호, 그린 자리).
    hits: Vec<(String, usize, Rect)>,
    /// (체크 열쇠, 체크칸 자리).
    boxes: Vec<(String, Rect)>,
}

thread_local! {
    static PROMPTS: RefCell<State> = RefCell::new(State::default());
    static CHECKS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// 기억하는 체크 상한 — 넘으면 오래된 것부터 잊는다.
const CHECKS_MAX: usize = 2000;
/// 체크칸 그림 크기와 글이 비켜 서는 폭.
const BOX: f32 = 11.0;
const BOX_ADVANCE: f32 = 17.0;
/// 취소선 높이 — 11 글자 줄(`y + 5`)의 글자 가운데(실측: 줄 머리에서 글자 4.4~14.8).
const STRIKE_Y: f32 = 9.5;
const ANSWER_MARK: &str = "답";

/// 같은 말을 거울로 보든 그 기기에서 보든 같은 열쇠 — 친 시각과 머리글로 가른다(「ㅇㅇ」 같은 짧은 말이 겹쳐도
/// 초까지 같은 일은 드물다).
fn check_key(p: &Prompt) -> String {
    let asked = p.asked.as_deref().map(head_key).unwrap_or_default();
    format!("{}|{}|{}", p.at.unwrap_or(0), head_key(&p.text), asked)
}

/// 설정 파일 옆 — 검증용 앱은 설정 파일을 격리하므로 사람의 체크를 건드리지 않는다.
fn checks_path() -> Option<PathBuf> {
    Some(crate::socket::settings_file_path()?.with_file_name("prompt-checks.json"))
}

fn with_checks<R>(f: impl FnOnce(&mut Vec<String>) -> R) -> R {
    CHECKS.with(|c| {
        let mut c = c.borrow_mut();
        let list = c.get_or_insert_with(|| {
            checks_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        });
        f(list)
    })
}

fn checked(key: &str) -> bool {
    with_checks(|list| list.iter().any(|k| k == key))
}

fn toggle_check(key: &str) {
    let text = with_checks(|list| {
        match list.iter().position(|k| k == key) {
            Some(i) => {
                list.remove(i);
            }
            None => {
                list.push(key.to_string());
                let over = list.len().saturating_sub(CHECKS_MAX);
                list.drain(..over);
            }
        }
        serde_json::to_string(list).unwrap_or_default()
    });
    if let Some(path) = checks_path() {
        let _ = std::fs::write(path, text);
    }
}

/// 목록 줄, 새 것이 위. `spots`·`current` 는 친 말(`typed_texts`)의 화면 자리다. 첫 친 말보다 앞선 답은 갈 턴이
/// 없어(앞 세션에서 이어진 기록) 뺀다.
fn lines_for(prompts: &[Prompt], spots: &[Option<usize>], current: Option<usize>, anywhere: bool) -> Vec<Line> {
    let mut typed = 0usize;
    let mut lines: Vec<Line> = prompts
        .iter()
        .filter_map(|p| {
            if p.asked.is_none() {
                typed += 1;
            }
            let index = typed.checked_sub(1)?;
            let spot = spots.get(index).copied().flatten();
            let reachable = anywhere || spot.is_some();
            Some(Line {
                index,
                key: check_key(p),
                answer: p.asked.is_some(),
                head: first_line(&p.text),
                tip: tip_text(p, reachable),
                at: p.at,
                reachable,
                current: p.asked.is_none() && current.is_some() && spot == current,
            })
        })
        .collect();
    lines.reverse();
    lines
}

/// 화면에 질문 줄로 서는 말 — 자리 맞추기(`place`)와 이동은 이 목록의 번호로 한다.
fn typed_texts(prompts: &[Prompt]) -> Vec<String> {
    prompts.iter().filter(|p| p.asked.is_none()).map(|p| p.text.clone()).collect()
}

fn first_line(text: &str) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(200).collect()
}

fn tip_text(p: &Prompt, reachable: bool) -> String {
    let clip = |s: &str| {
        let mut out: String = s.trim().chars().take(TIP_CHARS).collect();
        if out.len() < s.trim().len() {
            out.push('…');
        }
        out
    };
    let mut tip = match &p.asked {
        Some(q) => format!("질문 · {}\n답 · {}", clip(q), clip(&p.text)),
        None => clip(&p.text),
    };
    tip.push_str(match (reachable, p.asked.is_some()) {
        (true, false) => "\n누르면 이 질문 자리로 가요",
        (true, true) => "\n누르면 이 질문을 받은 자리로 가요",
        (false, _) => "\n화면 기록에 없어 옮길 수 없어요",
    });
    tip
}

/// 친 날짜·시각 `10.12 22:46`(해가 다르면 앞에 해). 지역 시각은 OS 에 맡기고 못 구하면 나이로 물러난다.
fn clock(at: u64) -> String {
    #[cfg(target_os = "macos")]
    if let (Some(t), Some(now)) = (local_tm(at), local_tm(now_secs())) {
        let day = format!("{:02}.{:02} {:02}:{:02}", t.tm_mon + 1, t.tm_mday, t.tm_hour, t.tm_min);
        return if t.tm_year == now.tm_year { day } else { format!("{}.{day}", t.tm_year + 1900) };
    }
    age(at)
}

#[cfg(target_os = "macos")]
pub(crate) fn local_tm(secs: u64) -> Option<libc::tm> {
    let t = secs.min(libc::time_t::MAX as u64) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    (!unsafe { libc::localtime_r(&t, &mut tm) }.is_null()).then_some(tm)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn age(at: u64) -> String {
    let ago = now_secs().saturating_sub(at);
    if ago < 60 {
        "방금".into()
    } else if ago < 3600 {
        format!("{}분", ago / 60)
    } else if ago < 86400 {
        format!("{}시간", ago / 3600)
    } else {
        format!("{}일", ago / 86400)
    }
}

/// 거울에서 한 번에 보낼 노치. 답을 보고 나서야 다음을 보내므로 한 번에 멀리 가야 빠르다 — 목적지가 안 보이면
/// 화면 `screens` 장만큼(지나치면 돌아서며 줄인다), 보이면 그 줄을 맨 위로 올리는 만큼. 노치당 줄 수를 아직
/// 못 쟀으면 조금씩.
fn remote_notches(near: bool, at: Option<usize>, rate: Option<f32>, rows: usize, screens: f32) -> usize {
    match (near, at, rate) {
        (true, Some(r), Some(rate)) => ((r as f32 / rate).floor() as usize).max(1),
        (true, ..) => 1,
        (false, _, Some(rate)) => ((rows as f32 * screens / rate).floor() as usize).clamp(1, 400),
        (false, ..) => 4,
    }
}

/// 휠을 보낸 뒤 글이 옮겨 갔나 — 같은 자리에 같은 줄이 8할 넘게 남았으면 안 움직인 것이다(입력칸·상태줄은
/// 늘 제자리다).
fn moved(before: &[String], after: &[String]) -> bool {
    let rows: Vec<usize> = (0..after.len().min(before.len())).filter(|&i| !after[i].trim().is_empty()).collect();
    let kept = rows.iter().filter(|&&i| after[i] == before[i]).count();
    !rows.is_empty() && (kept as f32) < rows.len() as f32 * 0.8
}

/// 두 화면 사이에 글이 몇 줄 옮겨 갔나(아래로 밀렸으면 양수). 앞 화면에 한 번만 나오는 줄이 셋 이상 같은
/// 거리로 맞아야 믿는다 — 접힌 문단 꼬리·빈 줄·구분선처럼 되풀이되는 줄은 엉뚱한 거리에도 맞는다. 입력칸·
/// 상태줄은 제자리라 옮김 0 은 보지 않는다.
fn scroll_shift(before: &[String], after: &[String]) -> Option<isize> {
    let mut seen: std::collections::HashMap<&str, Option<usize>> = std::collections::HashMap::new();
    for (j, row) in before.iter().enumerate().filter(|(_, r)| !r.trim().is_empty()) {
        seen.entry(row.as_str()).and_modify(|at| *at = None).or_insert(Some(j));
    }
    let mut votes: std::collections::HashMap<isize, usize> = std::collections::HashMap::new();
    for (i, row) in after.iter().enumerate() {
        if let Some(&Some(j)) = seen.get(row.as_str()) {
            let d = i as isize - j as isize;
            if d != 0 {
                *votes.entry(d).or_default() += 1;
            }
        }
    }
    let (d, n) = votes.into_iter().max_by_key(|&(d, n)| (n, -d.abs()))?;
    (n >= 3).then_some(d)
}

/// 지금 보는 칸 `(칸, 보고 있는 탭)` — Info 카드와 같은 칸(활성 칸의 보고 있는 터미널 탭)이다. 기록은 탭에
/// 묶이고 이동은 칸으로 시킨다(탭이 여럿이면 둘이 다르다).
fn focused(ws: &crate::Workspace) -> Option<(String, String)> {
    let pane = ws.active_pane.clone()?;
    let p = ws.panes.get(&pane)?;
    let tab = p.tabs.get(p.active_tab)?;
    tab.term()?;
    Some((pane, tab.pid.clone()?))
}

impl crate::App {
    fn prompt_shelf(&mut self, pane: &str) -> Option<Shelf> {
        let pid = self.ws.lock().ok()?.active_tab_pid(pane);
        if kasa_mcp::remote::is_view_pane(&pid) {
            let sess = self.pty.get(&pid)?.clone();
            // 거울은 원본이 claude 든 codex 든 받은 화면 그대로다 — 마커로 가르지 않고 다 받는다. 출처 줄 같은
            // 군더더기는 맞추기(`place`)가 안 센다.
            // 풀스크린 원본은 지난 줄을 앱 안에만 둬서 받은 기록에 앵커가 없다 — 원본에 휠을 보내 찾아간다.
            if let Some(rows) = self.screen_rows(pane).filter(|_| self.mirror_takes_wheel(pane)) {
                return Some(Shelf::Screen(rows.into_iter().filter(|r| prompt_row(r)).collect()));
            }
            let anchors = sess.prompt_anchors();
            let top = self
                .pane_view_shift
                .get(pane)
                .and_then(|s| s.projection.as_ref())
                .and_then(|p| p.top_abs)
                .unwrap_or_else(|| {
                    let (off, hist) = sess.view_state();
                    hist as i64 - off as i64
                });
            return Some(Shelf::Mirror(anchors, top));
        }
        if let Some(state) = crate::prompt_nav::live_state(&pid) {
            return Some(Shelf::Mod(state));
        }
        let rows = self.screen_rows(pane)?;
        Some(Shelf::Screen(rows.into_iter().filter(|r| prompt_row(r)).collect()))
    }

    /// 거울의 원본이 SGR 휠을 받는 풀스크린 앱인가.
    fn mirror_takes_wheel(&self, pane: &str) -> bool {
        let Ok(ws) = self.ws.lock() else { return false };
        ws.panes.get(pane).and_then(|p| p.term()).is_some_and(|t| t.alt_screen && t.mouse_enabled && t.mouse_sgr)
    }

    /// 대체 화면 칸의 지금 화면 글자(줄마다). 대체 화면이 아니면 `None`.
    fn screen_rows(&self, pane: &str) -> Option<Vec<String>> {
        let ws = self.ws.lock().ok()?;
        let t = ws.panes.get(pane)?.term()?;
        t.alt_screen.then(|| t.cells.iter().map(|r| row_text(r)).collect())
    }

    /// 화면에 보이는 질문과 견줘 위·아래를 정하고 휠로 떠난다. 보이는 질문이 없으면(라이브 바닥의 긴 답)
    /// 위로 — 지난 질문은 위에 있다. 견줄 때는 글이 맞는 줄만 센다 — 차례 메우기(`place`)는 입력칸
    /// (`› Ask Codex…`)까지 질문으로 짝지어 방향을 거꾸로 잡는다.
    fn begin_screen_seek(&mut self, pane: &str, texts: &[String], index: usize) {
        let Some(rows) = self.screen_rows(pane) else { return };
        let keys: Vec<String> = texts.iter().map(|t| head_key(t)).collect();
        let last_seen = rows
            .iter()
            .filter(|r| prompt_row(r))
            .filter_map(|r| keys.iter().rposition(|k| same(k, &head_key(r))))
            .max();
        let down = last_seen.is_some_and(|last| index > last);
        let cols = self.ws.lock().ok().and_then(|ws| ws.panes.get(pane)?.term().map(|t| t.cells.first().map_or(0, Vec::len))).unwrap_or(0);
        let cell = ((cols / 2) as u16, (rows.len() / 2) as u16);
        let now = Instant::now();
        let remote = self.ws.lock().is_ok_and(|ws| kasa_mcp::remote::is_view_pane(&ws.active_tab_pid(pane)));
        let seek = Seek {
            pane: pane.to_string(),
            want: keys[index].clone(),
            down,
            near: false,
            turned: false,
            cell,
            remote,
            waiting: None,
            sent_notches: 0,
            sent_rows: Vec::new(),
            rate: None,
            keys: keys.clone(),
            index,
            screens: SCREENS_MIN,
            back: false,
            last: now.checked_sub(SEEK_EVERY).unwrap_or(now),
            sent: 0,
            stall: 0,
            print: 0,
        };
        SEEK.with(|s| *s.borrow_mut() = Some(seek));
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 한 틱 — 목적지가 맨 위 세 줄 안에 서면 끝, 아래쪽에 보이면 방향을 아래로 돌려 한 노치씩 올린다
    /// (화면의 줄을 위로 올리려면 아래로 굴려야 한다).
    fn pump_screen_seek(&mut self) {
        let Some(mut seek) = SEEK.with(|s| s.borrow().clone()) else { return };
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        if seek.last.elapsed() < SEEK_EVERY {
            return;
        }
        let Some(rows) = self.screen_rows(&seek.pane) else {
            SEEK.with(|s| *s.borrow_mut() = None);
            return;
        };
        // codex 는 맨 윗줄에 지금 보는 턴의 질문을 띠로 붙인다 — 거기 걸린 것은 아래에서 다가와 닿았을 때만
        // 진짜 그 줄이다. 처음부터 맨 윗줄에 있으면 그 턴의 꼬리를 보는 중이라 더 올라간다.
        let at = rows.iter().position(|r| prompt_row(r) && same(&head_key(r), &seek.want));
        if at.is_some_and(|r| (1..=2).contains(&r) || r == 0 && seek.near) {
            SEEK.with(|s| *s.borrow_mut() = None);
            return;
        }
        if at.is_some_and(|r| r > 2) {
            seek.near = true;
            seek.down = true;
        }
        let print = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            rows.hash(&mut h);
            h.finish()
        };
        if seek.remote {
            if let Some((sent_print, sent_at)) = seek.waiting {
                // 바뀌기 시작했으면 한 틱 더 그대로인지 본다 — 다시 그리는 중간 화면으로 판단하지 않는다.
                let settling = print != sent_print && print != seek.print;
                if (print == sent_print || settling) && sent_at.elapsed() < REMOTE_WAIT {
                    seek.print = print;
                    SEEK.with(|s| *s.borrow_mut() = Some(seek));
                    return;
                }
                seek.waiting = None;
                let moved = moved(&seek.sent_rows, &rows);
                seek.stall = if moved { 0 } else { seek.stall + 1 };
                if let Some(d) = moved.then(|| scroll_shift(&seek.sent_rows, &rows)).flatten() {
                    seek.rate = Some((d.unsigned_abs() as f32 / seek.sent_notches.max(1) as f32).clamp(1.0, RATE_MAX));
                }
                // 보이는 질문이 목적지보다 앞(위로 가는 중)이나 뒤(아래로 가는 중)면 지나친 것이다. 크게 걷다 끝에
                // 부딪혀도(마지막 질문은 바닥 화면에 안 보이기도 한다) 돌아서며 줄인다 — 헛돈 끝으로 세지 않는다.
                if !seek.near {
                    let seen = rows.iter().filter(|r| prompt_row(r)).filter_map(|r| seek.keys.iter().rposition(|k| same(k, &head_key(r))));
                    let passed = seen.into_iter().any(|k| if seek.down { k > seek.index } else { k < seek.index });
                    if passed || !moved && seek.screens > SCREENS_MIN {
                        seek.down = !seek.down;
                        seek.screens = SCREENS_MIN;
                        seek.stall = 0;
                        seek.back = true;
                    } else if moved && !seek.back {
                        seek.screens = (seek.screens * 2.0).min(SCREENS_MAX);
                    }
                }
            }
            // 한 노치에 옮기는 줄보다 가까우면 더 다가갈 수 없다 — 지나치느니 여기서 선다.
            if seek.near && at.zip(seek.rate).is_some_and(|(r, rate)| (r as f32) < rate) {
                SEEK.with(|s| *s.borrow_mut() = None);
                return;
            }
        } else {
            seek.stall = if print == seek.print { seek.stall + 1 } else { 0 };
        }
        seek.print = print;
        let stall_max = if seek.remote { REMOTE_STALL } else { SEEK_STALL };
        // 더 못 굴리는데 보이면 거기가 갈 수 있는 끝이다(마지막 질문은 바닥에서도 맨 위까지 못 온다).
        if seek.stall >= stall_max && at.is_some() {
            SEEK.with(|s| *s.borrow_mut() = None);
            return;
        }
        if seek.stall >= stall_max && !seek.turned {
            seek.turned = true;
            seek.down = !seek.down;
            seek.stall = 0;
        }
        if seek.stall >= stall_max || seek.sent >= SEEK_MAX {
            SEEK.with(|s| *s.borrow_mut() = None);
            self.set_toast("그 질문까지 못 찾았어요".into());
            return;
        }
        let button = if seek.down { 65 } else { 64 };
        let notches = if seek.remote { remote_notches(seek.near, at, seek.rate, rows.len(), seek.screens) } else if seek.near { 1 } else { 4 };
        if seek.remote {
            // 거울 화면에 비춘 자리로 옮기는 `send_mouse_sgr` 를 건너뛴다 — 칸은 이미 원본 격자 좌표다.
            let wheel = format!("\x1b[<{button};{};{}M", seek.cell.0 + 1, seek.cell.1 + 1).repeat(notches);
            if let Some(pty) = self.pty_for_pane(&seek.pane) {
                let _ = pty.send_bytes(wheel.as_bytes());
            }
            seek.waiting = Some((print, Instant::now()));
            seek.sent_notches = notches;
            seek.sent_rows = rows;
        } else {
            for _ in 0..notches {
                self.send_mouse_sgr(&seek.pane, button, seek.cell.0, seek.cell.1, true);
            }
        }
        seek.sent += 1;
        seek.last = Instant::now();
        SEEK.with(|s| *s.borrow_mut() = Some(seek));
    }

    /// render 매 틱 — Info 가 열려 있을 때만 지금 보는 칸의 기록을 따라 읽고 화면 자리와 맞춘다.
    pub(crate) fn pump_info_prompts(&mut self) {
        self.pump_screen_seek();
        let open = self.info.runtime_page() && self.git.col_visible;
        let seen = if open { self.ws.lock().ok().and_then(|ws| focused(&ws)) } else { None };
        let source = seen.as_ref().and_then(|(_, pid)| match kasa_mcp::remote::remote_meta(pid) {
            Some((base, rid)) => Some(Source::Remote(base, rid)),
            None => self.collab.hub.bound.lock().ok()?.get(pid).cloned().map(Source::File),
        });
        let key = source.as_ref().map(Source::key);
        let now = Instant::now();
        let (feed, due) = PROMPTS.with(|p| {
            let mut p = p.borrow_mut();
            if p.seen != seen || p.key != key {
                let had = !p.lines.is_empty() || p.status != Status::None;
                *p = State { seen: seen.clone(), key: key.clone(), ..State::default() };
                if key.is_some() {
                    p.feed = Some(Arc::new(Mutex::new(Feed::default())));
                    p.status = Status::Loading;
                }
                if had || key.is_some() {
                    self.chrome_dirty = true;
                }
            }
            let due = p.polled.is_none_or(|t| now.duration_since(t) >= POLL_EVERY);
            if due {
                p.polled = Some(now);
            }
            (p.feed.clone(), due)
        });
        let (Some((pane, _)), Some(feed)) = (seen, feed) else { return };
        if due {
            if let Some(source) = source {
                poll(Arc::clone(&feed), source, self.proxy.clone());
            }
        }
        let (version, loaded, error) = match feed.try_lock() {
            Ok(f) => (f.version, f.loaded, f.error.clone()),
            Err(_) => return,
        };
        let stale = PROMPTS.with(|p| {
            let p = p.borrow();
            p.version != version || p.placed.is_none_or(|t| now.duration_since(t) >= PLACE_EVERY)
        });
        if !stale {
            return;
        }
        let prompts = feed.lock().map(|f| f.prompts.clone()).unwrap_or_default();
        let texts = typed_texts(&prompts);
        let shelf = self.prompt_shelf(&pane);
        // 풀스크린 TUI 는 대화를 처음부터 쥐고 있어 어느 질문이든 휠로 닿는다. 대화 보기도 기록을 다 편다.
        let anywhere = matches!(shelf, Some(Shelf::Screen(_))) || self.chat_view_on(&pane);
        let (spots, current) = match &shelf {
            Some(shelf) => (place(&texts, &shelf.texts()), shelf.current()),
            None => (vec![None; texts.len()], None),
        };
        let lines = lines_for(&prompts, &spots, current, anywhere);
        let status = match error {
            Some(e) if !loaded || prompts.is_empty() => Status::Error(format!("기록을 못 읽었어요 · {e}")),
            _ if !loaded => Status::Loading,
            _ => Status::Ready,
        };
        PROMPTS.with(|p| {
            let mut p = p.borrow_mut();
            p.placed = Some(now);
            p.version = version;
            if p.lines != lines || p.status != status {
                p.lines = lines;
                p.status = status;
                self.chrome_dirty = true;
            }
        });
    }

    /// 목록 줄을 누른 것. 누른 순간의 화면 목록으로 다시 맞춘다 — 그린 뒤 질문이 늘었거나 스크롤백이
    /// 돌았으면 그린 때의 번호는 남의 자리다.
    pub(crate) fn info_prompt_press(&mut self, x: f32, y: f32) -> bool {
        let inside = |r: &Rect| x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3;
        if let Some(key) = PROMPTS.with(|p| p.borrow().boxes.iter().find(|(_, r)| inside(r)).map(|(k, _)| k.clone())) {
            toggle_check(&key);
            self.chrome_dirty = true;
            return true;
        }
        let hit = PROMPTS.with(|p| p.borrow().hits.iter().find(|(_, _, r)| inside(r)).map(|(pane, i, _)| (pane.clone(), *i)));
        let Some((pane, index)) = hit else { return false };
        let now = self.ws.lock().ok().and_then(|ws| focused(&ws));
        if now.is_none() || now != PROMPTS.with(|p| p.borrow().seen.clone()) {
            return true;
        }
        let texts = PROMPTS.with(|p| p.borrow().feed.as_ref().and_then(|f| f.lock().ok().map(|f| typed_texts(&f.prompts)))).unwrap_or_default();
        // 대화 보기 칸은 기록을 다 가졌다 — 그 보기에서 그 질문을 맨 위에 세운다.
        if self.chat_view_on(&pane) {
            if let Some(text) = texts.get(index) {
                let nth = texts[..index].iter().filter(|t| *t == text).count();
                if !self.chat_view_jump(&pane, text, nth) {
                    self.set_toast("이 질문을 대화 보기에서 못 찾았어요".into());
                }
            }
            PROMPTS.with(|p| p.borrow_mut().placed = None);
            self.chrome_dirty = true;
            return true;
        }
        let shelf = self.prompt_shelf(&pane);
        let spot = shelf.as_ref().and_then(|s| place(&texts, &s.texts()).get(index).copied().flatten());
        match (shelf, spot) {
            (Some(Shelf::Mod(_)), Some(i)) => {
                self.prompt_nav_request(&pane, crate::prompt_nav::NavOp::Prompt(i));
            }
            (Some(Shelf::Mirror(anchors, _)), Some(i)) => {
                self.jump_mirror_abs(&pane, anchors[i].abs_line);
            }
            (Some(Shelf::Screen(_)), _) if index < texts.len() => self.begin_screen_seek(&pane, &texts, index),
            _ => self.set_toast("이 질문은 화면 기록에 없어 옮길 수 없어요".into()),
        }
        PROMPTS.with(|p| p.borrow_mut().placed = None);
        self.chrome_dirty = true;
        true
    }
}

/// 「지금 보는 칸」 카드 아래 묶음의 높이 — 그리기 전에 스크롤 범위에 더한다.
pub(crate) fn height() -> f32 {
    PROMPTS.with(|p| {
        let p = p.borrow();
        let body = match p.status {
            Status::None => return 0.0,
            Status::Ready if !p.lines.is_empty() => p.lines.len() as f32 * crate::info::ROW_H,
            _ => crate::info::ROW_H,
        };
        crate::info::SEC_GAP + crate::info::DEV_H + body
    })
}

/// `docs/design.md` 「Info 탭」 — 묶음 머리 30 아래 질문 한 줄 22. 새 것이 위.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    tips: &mut Vec<(String, Rect)>,
    x: f32,
    w: f32,
    mut y: f32,
    top: f32,
    bottom: f32,
) {
    use crate::info::{fit_text, DEV_H as SECTION_H, ROW_H, SEC_GAP};
    PROMPTS.with(|p| {
        let mut p = p.borrow_mut();
        p.hits.clear();
        if p.status == Status::None {
            return;
        }
        let x0 = x + 14.0;
        let right = x + w - 12.0;
        y += SEC_GAP;
        let title = match p.status {
            Status::Ready => format!("내 프롬프트 · {}개", p.lines.len()),
            _ => "내 프롬프트".to_string(),
        };
        if y + SECTION_H > top && y < bottom {
            crate::info::draw_room_head(g, &title, x0 - 12.0, right, y + 3.0);
        }
        y += SECTION_H;
        let note = match &p.status {
            Status::Loading => Some(("대화 기록을 읽는 중이에요".to_string(), false)),
            Status::Error(e) => Some((e.clone(), true)),
            Status::Ready if p.lines.is_empty() => Some(("아직 직접 친 프롬프트가 없어요".to_string(), false)),
            _ => None,
        };
        if let Some((text, attention)) = note {
            if y + ROW_H > top && y < bottom {
                let text = fit_text(g, &text, (right - x0).max(0.0), 11.0, false);
                let color = if attention { theme::attention() } else { theme::text_dim() };
                g.draw_text(x0, y + 5.0, &text, gpu::DrawOpts { font_size: 11.0, color, bold: false, italic: false });
            }
            return;
        }
        let pane = p.seen.as_ref().map(|(pane, _)| pane.clone()).unwrap_or_default();
        let mut hits = Vec::new();
        let mut boxes = Vec::new();
        for line in &p.lines {
            if y + ROW_H > top && y < bottom {
                let rect = (x, y, w, ROW_H);
                let check_rect = (x, y, x0 + BOX_ADVANCE - x, ROW_H);
                let on_box = crate::info::hit(cursor, &check_rect);
                let hot = (line.reachable || on_box) && crate::info::hit(cursor, &rect);
                if hot {
                    g.rect(x, y, w, ROW_H, theme::surface());
                    g.hover_pointer = true;
                }
                if line.current {
                    g.rect(x + 6.0, y + 5.0, 2.0, ROW_H - 10.0, theme::accent());
                }
                let done = checked(&line.key);
                let (icon, ink) = if done { ("square-check", theme::success()) } else { ("square", theme::text_mute()) };
                g.queue_icon(icon, x0, y + (ROW_H - BOX) / 2.0, BOX, ink);
                let tx = x0 + BOX_ADVANCE;
                let when = line.at.map(clock).unwrap_or_default();
                let when_w = if when.is_empty() { 0.0 } else { g.measure_chrome_text(&when, 10.0, false) };
                if when_w > 0.0 {
                    g.draw_text(right - when_w, y + 6.0, &when, gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false });
                }
                let mark_w = if line.answer {
                    let mark_w = g.measure_chrome_text(ANSWER_MARK, 10.0, false);
                    g.draw_text(tx, y + 6.0, ANSWER_MARK, gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false });
                    mark_w + 6.0
                } else {
                    0.0
                };
                let room = (right - tx - mark_w - if when_w > 0.0 { when_w + 8.0 } else { 0.0 }).max(0.0);
                let head = fit_text(g, &line.head, room, 11.0, false);
                let color = if done {
                    theme::text_mute()
                } else if line.current {
                    theme::accent()
                } else if line.reachable {
                    theme::text()
                } else {
                    theme::text_dim()
                };
                g.draw_text(tx + mark_w, y + 5.0, &head, gpu::DrawOpts { font_size: 11.0, color, bold: false, italic: false });
                if done {
                    let head_w = g.measure_chrome_text(&head, 11.0, false);
                    g.rect(tx + mark_w, y + STRIKE_Y, head_w, 1.0, theme::text_mute());
                }
                // 말풍선은 먼저 담긴 자리가 이긴다 — 체크칸 설명을 줄 전문보다 앞에 둔다.
                if let Some(r) = g.clip_hit(check_rect) {
                    let tip = if done { "누르면 체크를 풀어요" } else { "누르면 끝낸 것으로 체크해요" };
                    tips.push((tip.to_string(), r));
                    boxes.push((line.key.clone(), r));
                }
                if let Some(r) = g.clip_hit(rect) {
                    tips.push((line.tip.clone(), r));
                    if line.reachable {
                        hits.push((pane.clone(), line.index, r));
                    }
                }
            }
            y += ROW_H;
        }
        p.hits = hits;
        p.boxes = boxes;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prompt_rows_show_the_date_and_time_they_were_typed() {
        let today = clock(now_secs());
        assert_eq!(today.len(), "10.12 22:46".len(), "{today}");
        assert!(clock(0).starts_with("19"), "다른 해에 친 말은 해를 앞에 단다: {}", clock(0));
    }

    #[test]
    fn prompts_find_their_rows_even_when_the_screen_holds_tells_and_commands() {
        let prompts = s(&["첫 질문", "두 번째 질문은 길어서\n여러 줄이다", "ㅇㅇ"]);
        let shown = s(&["❯ 첫 질문", "⟦미도리⟧ 브리프 간다", "/model", "두 번째 질문은 길", "ㅇㅇ"]);
        assert_eq!(place(&prompts, &shown), [Some(0), Some(3), Some(4)]);
    }

    #[test]
    fn a_short_reply_does_not_jump_to_an_older_same_reply_and_push_the_rest_out() {
        let prompts = s(&["고쳐 줘", "ㅇㅇ", "커밋해"]);
        let shown = s(&["ㅇㅇ", "옛 세션 질문", "고쳐 줘", "ㅇㅇ", "커밋해"]);
        assert_eq!(place(&prompts, &shown), [Some(2), Some(3), Some(4)]);
    }

    #[test]
    fn collapsed_pastes_fill_the_gap_between_matched_neighbours_in_order() {
        let prompts = s(&["앞 질문", "긴 로그를 붙여넣은 질문", "뒤 질문"]);
        let shown = s(&["앞 질문", "⟦유즈⟧ 쪽지", "[Pasted text #1 +40 lines]", "뒤 질문"]);
        assert_eq!(place(&prompts, &shown), [Some(0), Some(2), Some(3)]);
    }

    #[test]
    fn prompts_scrolled_out_of_history_stay_unplaced() {
        let prompts = s(&["사라진 질문 하나", "사라진 질문 둘", "남은 질문"]);
        let shown = s(&["남은 질문"]);
        assert_eq!(place(&prompts, &shown), [None, None, Some(0)]);
    }

    #[test]
    fn only_words_the_person_typed_are_collected_from_claude_and_codex() {
        let raw = [
            r#"{"type":"user","sessionId":"s","timestamp":"2026-10-13T08:00:00Z","message":{"content":"직접 친 질문"}}"#,
            r#"{"type":"user","sessionId":"s","message":{"content":"⟦미도리⟧ 쪽지 본문"}}"#,
            r#"{"type":"user","sessionId":"s","message":{"content":"[완료] 시로코(%27) — 끝"}}"#,
            r#"{"type":"user","sessionId":"s","isMeta":true,"message":{"content":"스킬 본문"}}"#,
            r#"{"type":"user","sessionId":"s","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"출력"}]}}"#,
            r#"{"type":"user","sessionId":"s","message":{"content":"<command-name>/model</command-name>"}}"#,
            r#"{"type":"assistant","sessionId":"s","timestamp":"2026-10-13T08:00:30Z","message":{"content":[{"type":"tool_use","id":"q1","name":"AskUserQuestion","input":{"questions":[{"question":"색은?"}]}}]}}"#,
            r#"{"type":"user","sessionId":"s","timestamp":"2026-10-13T08:05:00Z","message":{"content":[{"type":"tool_result","tool_use_id":"q1","content":"Your questions have been answered: \"색은?\"=\"파랑\". You can now continue."}]},"toolUseResult":{"questions":[{"question":"색은?"}],"answers":{"색은?":"파랑"}}}"#,
        ]
        .join("\n");
        let mut feed = Feed::default();
        assert!(feed.take(&keep_wanted(&(raw + "\n"))));
        assert_eq!(
            feed.prompts,
            [
                Prompt { text: "직접 친 질문".into(), at: Some(1791878400), asked: None },
                Prompt { text: "파랑".into(), at: Some(1791878700), asked: Some("색은?".into()) },
            ],
            "답의 시각은 물은 때가 아니라 고른 때다"
        );

        let codex = [
            r#"{"timestamp":"2026-10-13T08:01:00Z","type":"event_msg","payload":{"type":"user_message","message":"codex 에 친 말"}}"#,
            r#"{"timestamp":"2026-10-13T08:02:00Z","type":"event_msg","payload":{"type":"user_message","message":"⟦유즈⟧ 쪽지"}}"#,
            r#"{"timestamp":"2026-10-13T08:03:00Z","type":"event_msg","payload":{"type":"agent_message","message":"답"}}"#,
            r#"{"timestamp":"2026-10-13T08:04:00Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"<send_user_message_question_reply>\n[{\"answer\":\"고른 답\",\"question\":\"어느 쪽?\"}]\n</send_user_message_question_reply>"}]}}}"#,
        ]
        .join("\n");
        let mut feed = Feed::default();
        assert!(feed.take(&keep_wanted(&(codex + "\n"))));
        assert_eq!(
            feed.prompts,
            [
                Prompt { text: "codex 에 친 말".into(), at: Some(1791878460), asked: None },
                Prompt { text: "고른 답".into(), at: Some(1791878640), asked: Some("어느 쪽?".into()) },
            ]
        );
    }

    #[test]
    fn a_mirror_seek_measures_how_far_the_text_moved_ignoring_repeated_rows() {
        let screen = |top: usize| -> Vec<String> {
            let mut rows: Vec<String> = (top..top + 10).map(|i| if i % 3 == 0 { "  올라가요.".into() } else { format!("{i}번 줄") }).collect();
            rows.push("❯ ".into());
            rows
        };
        assert_eq!(scroll_shift(&screen(20), &screen(14)), Some(6), "위로 굴리면 글이 아래로 밀린다");
        assert_eq!(scroll_shift(&screen(20), &screen(23)), Some(-3));
        assert_eq!(scroll_shift(&screen(20), &screen(80)), None, "겹치는 줄이 없으면 모른다");
        assert!(moved(&screen(20), &screen(14)));
        assert!(!moved(&screen(20), &screen(20)), "끝에 닿아 그대로면 안 움직인 것이다");
    }

    #[test]
    fn a_mirror_seek_strides_by_screens_until_near_then_lifts_the_row_to_the_top() {
        assert_eq!(remote_notches(false, None, None, 48, SCREENS_MIN), 4, "줄 수를 못 쟀으면 조금씩");
        assert_eq!(remote_notches(false, None, Some(3.0), 48, 4.0), 64);
        assert_eq!(remote_notches(true, Some(31), Some(3.0), 48, 4.0), 10, "보이면 그 줄을 맨 위로 올리는 만큼");
        assert_eq!(remote_notches(true, Some(2), Some(3.0), 48, 4.0), 1);
    }

    #[test]
    fn an_answer_row_goes_to_the_turn_that_asked_and_stays_out_of_placing() {
        let p = |text: &str, asked: Option<&str>| Prompt { text: text.into(), at: None, asked: asked.map(Into::into) };
        let prompts = [p("앞 세션 답", Some("이어진 질문")), p("첫 질문", None), p("파랑", Some("색은?")), p("둘째 질문", None)];
        assert_eq!(typed_texts(&prompts), ["첫 질문", "둘째 질문"]);
        let lines = lines_for(&prompts, &[Some(4), None], Some(4), false);
        let got: Vec<(&str, usize, bool, bool, bool)> =
            lines.iter().map(|l| (l.head.as_str(), l.index, l.answer, l.reachable, l.current)).collect();
        assert_eq!(got, [("둘째 질문", 1, false, false, false), ("파랑", 0, true, true, false), ("첫 질문", 0, false, true, true)]);
        assert!(lines[1].tip.starts_with("질문 · 색은?\n답 · 파랑"));
    }

    #[test]
    fn the_whole_file_is_read_and_a_half_written_tail_waits() {
        let dir = std::env::temp_dir().join(format!("kt-info-prompts-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let line = |t: &str| format!(r#"{{"type":"user","sessionId":"s","message":{{"content":"{t}"}}}}"#);
        let filler = format!("{{\"type\":\"assistant\",\"message\":{{\"content\":\"{}\"}}}}\n", "x".repeat(1024 * 1024));
        std::fs::write(&path, format!("{}\n{filler}{}\n{}", line("맨 처음"), line("나중"), &line("쓰는 중")[..20])).unwrap();
        let (raw, next, reset) = read_wanted(&path, 0).unwrap();
        assert!(reset);
        let mut feed = Feed::default();
        assert!(feed.take(&raw));
        assert_eq!(feed.prompts.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), ["맨 처음", "나중"]);
        let (raw, _, reset) = read_wanted(&path, next).unwrap();
        assert!(raw.is_empty() && !reset);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
