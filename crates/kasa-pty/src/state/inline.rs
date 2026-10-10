//! 인라인 그림 — OSC 1337 셀 흐름 그림과 kitty 그림 프로토콜 기록, 뷰포트 배치로 환산해 프레임에 싣기.

use super::*;
use super::reader::find_subslice;
use super::vt::history_lines_for_cols;
#[cfg(test)]
use super::grid::snapshot;
#[cfg(test)]
use super::test_support::{ext_session, test_posix_shell, wait_text};
#[cfg(test)]
use super::vt::make_term;

/// Standard base64 decode (no external crate). Ignores non-alphabet bytes
/// (whitespace, `=` padding) so it tolerates wrapped iTerm payloads.
pub(crate) fn b64_decode(s: &[u8]) -> Vec<u8> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in s {
        let Some(v) = val(c) else { continue };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// MVP inline-image: a completed OSC 1337 body is `params:base64`. Decode
/// the payload, write a temp PNG/JPEG, and hand it to the existing image
/// pane viewer via the kasaspace `/open-image` endpoint. (True cell-flow
/// inline rendering is a later stage; this gets `imgcat`-style output
/// showing in kasaterm now.)
fn emit_inline_image(body: &[u8]) {
    let Some(colon) = body.iter().position(|&b| b == b':') else {
        return;
    };
    let bytes = b64_decode(&body[colon + 1..]);
    if bytes.len() < 16 {
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("kasaterm-inline-{nanos}.png"));
    if std::fs::write(&tmp, &bytes).is_err() {
        return;
    }
    let port = std::env::var("KASASPACE_MCP_PORT").unwrap_or_else(|_| "8765".into());
    let url = format!("http://127.0.0.1:{port}/open-image");
    let _ = std::process::Command::new("curl")
        .args([
            "-s",
            "--get",
            "--data-urlencode",
            &format!("path={}", tmp.display()),
            &url,
        ])
        .status();
}

/// 셀-흐름 인라인 이미지 모드인가. 기본 on — OSC 1337 이 도착한 그 자리에
/// 그린다. `KASATERM_INLINE_IMAGES=tab` 이면 옛 동작(temp PNG → 이미지 탭)으로
/// 돌아간다 — 셀 렌더가 실사용에서 검증될 때까지 남겨 둔 도피로(2026-08-13).
pub(super) fn inline_cell_flow() -> bool {
    static MODE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        std::env::var("KASATERM_INLINE_IMAGES").map(|v| v != "tab").unwrap_or(true)
    })
}

/// 셀-흐름 인라인 이미지 한 장의 PTY측 기록.
///
/// 앵커는 **내용 절대 줄**(`history_size + 화면 행`) — 새 출력이 밀어 올리는
/// 일반 스크롤과 높이 리사이즈(줄이 화면↔히스토리를 오가도 이 값은 그대로)에는
/// 안정하고, 가로 리사이즈(리플로우)·히스토리 캡 회전·clear 에서만 무너진다.
/// 그 셋은 `attach_inline_views` 가 전량 폐기한다 — 어긋난 자리에 그리는 것보다
/// 안 그리는 게 낫다.
struct InlineImg {
    id: u64,
    /// 재전송 판별 키 (name, size) — recall 은 그림 자리가 밀리면 같은 시퀀스를
    /// 새 커서 위치에서 다시 흘린다(recall 35b7f5e). 같은 키는 새 레코드가
    /// 아니라 **이동**이다.
    key: (String, u64),
    path: std::path::PathBuf,
    /// 원본 픽셀 수(가로×세로) — `INLINE_IMAGE_PIXEL_BUDGET` 셈용. 머리말을 못
    /// 읽는 형식은 0 이다(GUI 도 같은 디코더라 텍스처가 안 생긴다).
    pixels: u64,
    abs_line: i64,
    col: u16,
    cols: u16,
    rows: u16,
    /// 앵커 행 텍스트(앞 64자, trim). recall 은 그림 자리에 대체 텍스트
    /// (`[그림] name`)를 먼저 깔고 시퀀스를 덮으므로, 이 행이 다른 내용으로
    /// 바뀌면 그림이 그 자리를 떠난 것이다(스크롤 아웃·리페인트) — 남겨 두면
    /// 남의 글 위에 유령으로 뜬다. **기록 시점이 아니라 flush 뒤 첫 스냅샷에서
    /// 뜬다**(None=아직) — 기록은 동기 출력(2026) 한가운데라 그리드가 옛
    /// 프레임이고, 거기서 뜨면 다음 프레임에 반드시 어긋나 자기를 지운다.
    row_sig: Option<String>,
}

/// 한 pane 이 붙들어 두는 인라인 그림 장수. 넘으면 오래된 것부터 놓는다.
///
/// 한 장의 값은 PTY 쪽 임시 PNG 파일 하나와, 뷰포트에 보이는 동안의 GUI 텍스처
/// 하나다. 텍스처는 **원본 픽셀 그대로** RGBA8 로 올라가(`upload_image`) 장당
/// 가로×세로×4 바이트이고, 화면을 벗어나면 놓인다. 그래서 장수는 작은 그림을
/// 많이 그리는 화면을 위해 넉넉히 두고, 메모리는 아래 픽셀 합계가 묶는다.
/// 16 이던 시절 kasaslk 는 화면 전체에 12장만 그려 스레드 칸 프사·이모지가
/// 반블록으로 밀렸다(2026-10-02). 그 화면은 그림이 작아(셀 14×30px 기준
/// 프사 56×60, 본문 그림 많아야 0.7Mpx) 64장이 다 차도 픽셀 합계 근처에도
/// 안 간다. 값은 `KASATERM_INLINE_IMAGE_SLOTS` 로 자식에게도 알린다.
pub const INLINE_IMAGE_SLOTS: usize = 64;

/// 한 pane 이 붙들어 두는 인라인 그림의 원본 픽셀 합. 넘으면 장수와 같은 순서
/// (오래된 것부터)로 놓되, 방금 온 한 장은 크기와 관계없이 남긴다.
///
/// 장수만으로는 메모리가 안 묶인다 — 작은 칸(`width=8`)에 1200만 화소 사진을
/// 실으면 세 줄짜리 그림이 텍스처 48MB 다. 리그 실측(2026-10-02, 4032×3024
/// PNG 다섯 장을 한 화면에): GPU 몫 footprint 가 +239MB(장당 가로×세로×4)
/// 오르고, 스크롤로 화면에서 걷히면 그만큼 돌아온다(네 번 오가도 바닥이 안
/// 자랐다). 6400만 화소(텍스처 256MB)면 휴대폰 사진 다섯 장·레티나 스크린샷
/// 열한 장이 한 pane 에 함께 산다. 기본 폭(칸의 60%) 사진은 한 장이 스무 줄을
/// 넘어 한 화면에 두세 장뿐이라 평소엔 이 선에 닿지 않는다.
const INLINE_IMAGE_PIXEL_BUDGET: u64 = 64_000_000;

/// 그림 머리말만 읽은 원본 픽셀 수. GUI 가 쓰는 것과 같은 디코더 집합이다.
fn image_pixels(bytes: &[u8]) -> u64 {
    if let Some((w, h)) = png_size(bytes) {
        return w as u64 * h as u64;
    }
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .map_or(0, |(w, h)| w as u64 * h as u64)
}

#[derive(Default)]
pub(super) struct InlineImgs {
    imgs: Vec<InlineImg>,
    /// kitty 그림 프로토콜로 받은 그림·놓기. `next_id` 순번을 함께 써서 장수·픽셀
    /// 한도와 「오래된 것부터」가 OSC 1337 기록과 한 줄로 선다.
    kitty: crate::kitty::KittyStore,
    next_id: u64,
    /// 리플로우·clear·alt 전환 감지용 직전 프레임 상태.
    last_cols: u16,
    last_rows: u16,
    last_hist: i64,
    last_alt: bool,
}

impl InlineImgs {
    /// 장수·픽셀 한도를 넘는 동안 가장 오래된 것부터 파일째 놓는다. OSC 1337
    /// 기록과 kitty 그림을 한 셈으로 친다 — 순번(`next_id`)이 같은 줄이다.
    fn evict_over_budget(&mut self) {
        loop {
            let count = self.imgs.len() + self.kitty.images.len();
            let pixels: u64 = self.imgs.iter().map(|i| i.pixels).sum::<u64>()
                + self.kitty.images.iter().map(|i| i.pixels).sum::<u64>();
            if count <= 1 || (count <= INLINE_IMAGE_SLOTS && pixels <= INLINE_IMAGE_PIXEL_BUDGET) {
                return;
            }
            let iterm = self.imgs.first().map(|i| i.id);
            let kitty = self.kitty.images.iter().map(|i| i.uid).min();
            match (iterm, kitty) {
                (Some(a), Some(b)) if b < a => self.kitty.evict(b),
                (Some(_), _) => {
                    let old = self.imgs.remove(0);
                    let _ = std::fs::remove_file(&old.path);
                }
                (None, Some(b)) => self.kitty.evict(b),
                (None, None) => return,
            }
        }
    }
}

/// pane 이 닫히면 받은 그림 파일도 함께 지운다 — 안 지우면 임시 폴더에 쌓인다.
impl Drop for InlineImgs {
    fn drop(&mut self) {
        for im in self.imgs.drain(..) {
            let _ = std::fs::remove_file(&im.path);
        }
        self.kitty.clear();
    }
}

/// 리더 스레드의 그림 시퀀스 캡처 상태(OSC 1337 셀-흐름 경로·kitty APC).
/// 페이로드가 read 여러 번에 걸칠 수 있어 buf/capturing 이 프레임을 넘어 살고,
/// anchor 는 머리를 만난 순간의 커서(= 그림 좌상단)다.
#[derive(Default)]
pub(super) struct InlineScan {
    buf: Vec<u8>,
    capturing: Option<Capture>,
    anchor: Option<(i64, u16)>,
    /// read 경계에 걸린 kitty 머리(`ESC`·`ESC _`). 다음 배치 앞에 붙여 다시 본다 —
    /// 조각 전송은 4KB 마다 머리가 서서 64KB read 경계에 자주 걸린다.
    carry: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq)]
enum Capture {
    Iterm,
    Kitty,
}

impl InlineScan {
    pub(super) fn busy(&self) -> bool {
        self.capturing.is_some() || !self.carry.is_empty()
    }
}

/// PNG IHDR 의 (width, height). PNG 가 아니면 None — 그때 줄수는 기본값으로.
pub(crate) fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

/// 접두 바이트에서 **마지막** 절대 커서 이동(CUP — `ESC[{row};{col}H|f`)의
/// 0-기반 (row, col). 동기 출력(2026) 중엔 Term 커서를 못 믿으므로, 시퀀스를
/// 그 자리에 심으려던 송신측의 CUP 이 앵커의 정본이 된다.
fn last_cup(bytes: &[u8]) -> Option<(u16, u16)> {
    let mut found = None;
    let mut i = 0;
    while let Some(rel) = find_subslice(&bytes[i..], b"\x1b[") {
        let start = i + rel + 2;
        let mut j = start;
        while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
            j += 1;
        }
        if j < bytes.len() && (bytes[j] == b'H' || bytes[j] == b'f') {
            let params = std::str::from_utf8(&bytes[start..j]).unwrap_or("");
            let mut it = params.split(';');
            let row = it.next().and_then(|s| s.parse::<u16>().ok()).unwrap_or(1);
            let col = it.next().and_then(|s| s.parse::<u16>().ok()).unwrap_or(1);
            found = Some((row.saturating_sub(1), col.saturating_sub(1)));
        }
        i = start;
    }
    found
}

/// 그리드 한 줄의 앞부분 텍스트(trim, 최대 64자) — 인라인 이미지 생존 신호용.
/// `line` 은 화면 좌표(0=화면 첫 줄, 음수=히스토리).
fn grid_line_sig(term: &Term<PtyEventForwarder>, line: i32) -> String {
    let grid = term.grid();
    if line < -(grid.history_size() as i32) || line >= grid.screen_lines() as i32 {
        return String::new();
    }
    let cols = grid.columns().min(64);
    let mut s = String::with_capacity(cols);
    for c in 0..cols {
        let ch = grid[Point::new(alacritty_terminal::index::Line(line), alacritty_terminal::index::Column(c))].c;
        s.push(if ch == '\0' { ' ' } else { ch });
    }
    s.trim().to_string()
}

/// 그림 시퀀스(OSC 1337·kitty APC)가 섞인 배치를 파서에 먹이면서 그림을
/// **그 자리에서** 뜬다.
///
/// 마커 직전까지 파서를 먼저 돌려야 커서가 이미지 자리에 가 있다 — 배치를
/// 통째로 advance 한 뒤 커서를 읽으면 이미지 뒤에 온 출력(recall 의 절대좌표
/// 리페인트)이 커서를 이미 딴 데로 옮긴 뒤다. 시퀀스 본문은 파서에 안 먹인다:
/// alacritty 는 어차피 버리고, vte OSC 버퍼에 MB 급 base64 를 밀 이유가 없다.
/// kitty 질의 응답도 이 순서 덕에 뒤따르는 DA1 응답보다 먼저 나간다 — 질의를
/// 보낸 쪽은 DA1 이 먼저 오면 「지원 안 함」으로 읽는다.
/// OSC 1337 마커가 read 경계에 걸치면 이번 배치는 놓친다 — 기존 scan_inline_image
/// 와 같은 트레이드. kitty 머리는 `carry` 로 잇는다.
pub(super) fn advance_scanning_inline(
    processor: &mut Processor<StdSyncHandler>,
    term: &mut Term<PtyEventForwarder>,
    bytes: &[u8],
    st: &mut InlineScan,
    imgs: &Mutex<InlineImgs>,
    responder: &PtyEventForwarder,
) {
    const ITERM: &[u8] = b"\x1b]1337;File=";
    const KITTY: &[u8] = b"\x1b_G";
    let joined;
    let mut data: &[u8] = if st.carry.is_empty() {
        bytes
    } else {
        let mut v = std::mem::take(&mut st.carry);
        v.extend_from_slice(bytes);
        joined = v;
        &joined
    };
    loop {
        if let Some(kind) = st.capturing {
            // kitty 는 ST 로만 닫힌다. OSC 1337 은 BEL 도 받는다.
            let stx = find_subslice(data, b"\x1b\\");
            let end = match kind {
                Capture::Kitty => stx.map(|e| (e, 2)),
                Capture::Iterm => match (data.iter().position(|&b| b == 0x07), stx) {
                    (Some(a), Some(b)) if a < b => Some((a, 1)),
                    (Some(a), None) => Some((a, 1)),
                    (_, Some(b)) => Some((b, 2)),
                    (None, None) => None,
                },
            };
            // ST 가 read 경계에서 `ESC` | `\\` 로 갈렸다.
            let end = if st.buf.last() == Some(&0x1b) && data.first() == Some(&b'\\') {
                st.buf.pop();
                Some((0, 1))
            } else {
                end
            };
            match end {
                Some((e, term_len)) => {
                    st.buf.extend_from_slice(&data[..e]);
                    match kind {
                        Capture::Iterm => record_inline_image(term, st, imgs),
                        Capture::Kitty => record_kitty(processor, term, st, imgs, responder),
                    }
                    st.buf.clear();
                    st.capturing = None;
                    data = &data[(e + term_len).min(data.len())..];
                }
                None => {
                    // 말라 죽은 스트림 무한 성장 방지. kitty 는 조각 하나가 그림
                    // 한 장일 수도 있어(조각 없이 보내는 클라이언트) 그림 상한만큼 둔다.
                    let cap = match kind {
                        Capture::Iterm => 8 * 1024 * 1024,
                        Capture::Kitty => 96 * 1024 * 1024,
                    };
                    if st.buf.len() < cap {
                        st.buf.extend_from_slice(data);
                    } else {
                        st.buf.clear();
                        st.capturing = None;
                        st.anchor = None;
                    }
                    return;
                }
            }
        } else {
            let iterm = if inline_cell_flow() { find_subslice(data, ITERM) } else { None };
            let kitty = find_subslice(data, KITTY);
            let next = match (iterm, kitty) {
                (Some(a), Some(b)) if b < a => Some((b, Capture::Kitty, KITTY.len())),
                (Some(a), _) => Some((a, Capture::Iterm, ITERM.len())),
                (None, Some(b)) => Some((b, Capture::Kitty, KITTY.len())),
                (None, None) => None,
            };
            match next {
                Some((m, kind, len)) => {
                    processor.advance(term, &data[..m]);
                    // 동기 출력(DECSET 2026) 안이면 방금 먹인 바이트가 파서
                    // 버퍼에만 쌓여 커서가 옛 자리(입력줄)에 있다 — recall 은
                    // 프레임 전체를 2026 으로 감싼다(2026-08-13 실측 137쌍).
                    // 그때는 마커 직전의 절대 커서 이동(CUP)에서 앵커를 읽는다.
                    // CUP 도 없으면 앵커 불명 — 페이로드는 그대로 소비하되
                    // 기록은 버린다(엉뚱한 자리에 그리는 것보다 낫다).
                    let hist = term.grid().history_size() as i64;
                    st.anchor = if processor.sync_bytes_count() > 0 {
                        last_cup(&data[..m]).map(|(row, col)| (hist + row as i64, col))
                    } else {
                        let cur = term.grid().cursor.point;
                        Some((hist + cur.line.0.max(0) as i64, cur.column.0 as u16))
                    };
                    st.capturing = Some(kind);
                    data = &data[m + len..];
                }
                None => {
                    let keep = if data.ends_with(b"\x1b_") {
                        2
                    } else if data.ends_with(b"\x1b") {
                        1
                    } else {
                        0
                    };
                    processor.advance(term, &data[..data.len() - keep]);
                    st.carry = data[data.len() - keep..].to_vec();
                    return;
                }
            }
        }
    }
}

/// 완성된 kitty APC 본문 하나를 처리한다 — 응답은 PTY 로, 커서 자리 놓기의
/// 커서 이동은 파서로(동기 출력 중이면 그 버퍼 뒤에 줄을 서 순서가 지켜진다).
fn record_kitty(
    processor: &mut Processor<StdSyncHandler>,
    term: &mut Term<PtyEventForwarder>,
    st: &mut InlineScan,
    imgs: &Mutex<InlineImgs>,
    responder: &PtyEventForwarder,
) {
    let at = st
        .anchor
        .take()
        .map(|(abs_line, col)| crate::kitty::At { abs_line, col });
    let env = crate::kitty::Env {
        local_media: responder.respond,
        grid_cols: term.grid().columns() as u16,
        hist: term.grid().history_size() as i64,
    };
    let out = {
        let mut lock = imgs.lock().unwrap();
        let InlineImgs { kitty, next_id, .. } = &mut *lock;
        let out = kitty.handle(&st.buf, at, &env, next_id);
        lock.evict_over_budget();
        out
    };
    if let Some(reply) = out.reply {
        responder.write_to_pty(&reply);
    }
    if let Some((down, col)) = out.cursor {
        let mut seq = b"\x1bD".repeat(down as usize);
        seq.extend_from_slice(format!("\x1b[{}G", col as u32 + 1).as_bytes());
        processor.advance(term, &seq);
    }
}

/// 완성된 OSC 1337 본문(`params:base64`)을 temp 파일로 떨구고 앵커에 건다.
fn record_inline_image(
    term: &Term<PtyEventForwarder>,
    st: &mut InlineScan,
    imgs: &Mutex<InlineImgs>,
) {
    let Some((abs_line, col)) = st.anchor.take() else { return };
    let body: &[u8] = &st.buf;
    let Some(colon) = body.iter().position(|&b| b == b':') else { return };
    let params = String::from_utf8_lossy(&body[..colon]).into_owned();
    let bytes = b64_decode(&body[colon + 1..]);
    if bytes.len() < 16 {
        return;
    }
    let grid_cols = term.grid().columns() as u16;
    // 히스토리 캡에 닿으면 이후 줄들이 회전해 절대 줄 번호가 조용히 밀린다 —
    // 그 상태에선 새 앵커도 못 믿으므로 받지 않는다(기존 것은
    // attach_inline_views 가 이미 전량 폐기했다).
    if term.grid().history_size() >= history_lines_for_cols(grid_cols.max(1)) {
        return;
    }
    let mut name = String::new();
    let mut size: u64 = bytes.len() as u64;
    let mut width_cells: Option<u16> = None;
    for kv in params.split(';') {
        if let Some((k, v)) = kv.split_once('=') {
            match k {
                "name" => {
                    name = String::from_utf8_lossy(&b64_decode(v.as_bytes())).into_owned()
                }
                "size" => size = v.parse().unwrap_or(size),
                // iTerm 스펙은 N(셀)·Npx·N%·auto — 셀 수만 다룬다(recall 이 보내는
                // 형태). 나머지는 기본폭으로 떨어진다.
                "width" => width_cells = v.parse().ok(),
                _ => {}
            }
        }
    }
    let cols = width_cells
        .unwrap_or_else(|| (grid_cols as u32 * 6 / 10).clamp(20, 100) as u16)
        .clamp(1, grid_cols.saturating_sub(col).max(1));
    // 셀은 세로가 가로의 두 배쯤. recall 의 자리 예약 계산(image_rows 의 2.1)과
    // 같은 비율이어야 recall 이 비워 둔 줄 수와 우리가 덮는 줄 수가 맞아떨어진다.
    // 실제 폰트 비율과의 오차는 GUI 의 contain-fit 이 레터박스로 흡수한다.
    const CELL_ASPECT: f64 = 2.1;
    let rows = match png_size(&bytes) {
        Some((w, h)) if w > 0 => {
            (((cols as f64) * h as f64 / w as f64) / CELL_ASPECT).ceil() as u16
        }
        _ => 12,
    }
    .max(1);
    let key = (name, size);
    let mut lock = imgs.lock().unwrap();
    if let Some(existing) = lock.imgs.iter_mut().find(|i| i.key == key) {
        // 같은 그림의 재전송 = 자리 이동. 파일과 id(=GUI 텍스처)는 그대로 산다.
        existing.abs_line = abs_line;
        existing.col = col;
        existing.cols = cols;
        existing.rows = rows;
        existing.row_sig = None;
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("kasaterm-inline-{nanos}.png"));
    if std::fs::write(&tmp, &bytes).is_err() {
        return;
    }
    let id = lock.next_id;
    lock.next_id += 1;
    let pixels = image_pixels(&bytes);
    lock.imgs.push(InlineImg { id, key, path: tmp, pixels, abs_line, col, cols, rows, row_sig: None });
    lock.evict_over_budget();
}

/// 스냅샷에 인라인 이미지의 이번 프레임 뷰포트 배치를 싣는다. 앵커가 무너지는
/// 사건은 여기서 감지해 통째로 버린다.
pub(super) fn attach_inline_views(
    update: &mut ScreenUpdate,
    term: &Term<PtyEventForwarder>,
    imgs: &Mutex<InlineImgs>,
) {
    attach_inline_views_at_offset(update, term, imgs, term.grid().display_offset());
}

pub(super) fn attach_inline_views_at_offset(
    update: &mut ScreenUpdate,
    term: &Term<PtyEventForwarder>,
    imgs: &Mutex<InlineImgs>,
    display_offset: usize,
) {
    let mut lock = imgs.lock().unwrap();
    update.inline_images.clear();
    let grid = term.grid();
    let cols = grid.columns() as u16;
    let rows = grid.screen_lines() as u16;
    let hist = grid.history_size() as i64;
    let alt = term.mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN);
    let clear_all =
        // 가로 리사이즈 = 리플로우. 절대 줄이 전부 다시 감긴다.
        (lock.last_cols != 0 && cols != lock.last_cols)
        // alt 화면 전환 = 좌표 공간이 통째로 바뀐다. alt 안의 내용이 화면에서
        // 사라지는 것과 같은 운명이라 그림도 함께 사라지는 게 일관적이다.
        || alt != lock.last_alt
        // 높이 변화 없이 히스토리가 줄었다 = clear_history. (높이가 커질 때는
        // 히스토리 줄이 화면으로 복귀하며 줄어드는데, 그건 내용 이동이라
        // 절대 줄 앵커가 산다 — 함께 버리면 안 된다.)
        || (hist < lock.last_hist && rows == lock.last_rows)
        // 캡 도달 = 이후 줄들이 회전해 번호가 조용히 밀린다.
        || hist >= history_lines_for_cols(cols.max(1)) as i64;
    if clear_all && !lock.imgs.is_empty() {
        for im in lock.imgs.drain(..) {
            let _ = std::fs::remove_file(&im.path);
        }
    }
    if clear_all {
        lock.kitty.drop_anchored();
    }
    lock.last_cols = cols;
    lock.last_rows = rows;
    lock.last_hist = hist;
    lock.last_alt = alt;
    let top_abs = hist - display_offset as i64;
    if !lock.kitty.is_empty() {
        crate::kitty::anchored_views(&lock.kitty, top_abs, rows, &mut update.inline_images);
        crate::kitty::placeholder_views(term, &lock.kitty, display_offset, &mut update.inline_images);
    }
    if lock.imgs.is_empty() {
        return;
    }
    // 앵커 행이 다른 내용으로 바뀐 그림은 그 자리를 떠났다(recall 이 스크롤로
    // 걷어 갔거나 다른 글이 덮었다) — 유령으로 남기지 않는다. 서명이 아직
    // 없는 그림(기록 직후)은 지금 그리드에서 처음 뜬다 — 동기 출력이 flush 된
    // 첫 스냅샷이 여기라서다.
    lock.imgs.retain_mut(|im| {
        let sig_now = grid_line_sig(term, (im.abs_line - hist) as i32);
        let alive = match &im.row_sig {
            None => {
                im.row_sig = Some(sig_now);
                true
            }
            Some(sig) => *sig == sig_now,
        };
        if !alive {
            let _ = std::fs::remove_file(&im.path);
        }
        alive
    });
    let iterm: Vec<_> = lock
        .imgs
        .iter()
        .filter_map(|im| {
            let row = im.abs_line - top_abs;
            // 뷰포트와 겹치는 것만 — GUI 는 받은 것만 그리고, 안 온 그림의
            // 텍스처는 놓는다(스크롤로 벗어난 그림의 GPU 메모리 회수).
            (row + (im.rows as i64) > 0 && row < rows as i64).then(|| {
                kasa_screen::screen::InlineImageView {
                    id: im.id,
                    path: im.path.display().to_string(),
                    row: row as i32,
                    col: im.col,
                    cols: im.cols,
                    rows: im.rows,
                    clip: None,
                }
            })
        })
        .collect();
    update.inline_images.extend(iterm);
}

/// Capture an iTerm OSC 1337 inline-image sequence that may span several
/// PTY reads. `buf`/`capturing` persist across calls. Marker
/// `ESC ] 1337 ; File=` … terminator BEL or ST. alacritty parses the OSC
/// and drops it (unhandled), so the base64 never reaches the grid — we
/// sniff the raw batch in parallel to grab the payload.
pub(super) fn scan_inline_image(bytes: &[u8], buf: &mut Vec<u8>, capturing: &mut bool) {
    const MARKER: &[u8] = b"\x1b]1337;File=";
    let mut data = bytes;
    loop {
        if *capturing {
            let bel = data.iter().position(|&b| b == 0x07);
            let st = find_subslice(data, b"\x1b\\");
            let end = match (bel, st) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            match end {
                Some(e) => {
                    buf.extend_from_slice(&data[..e]);
                    emit_inline_image(buf);
                    buf.clear();
                    *capturing = false;
                    let term_len = if data.get(e) == Some(&0x07) { 1 } else { 2 };
                    data = &data[(e + term_len).min(data.len())..];
                }
                None => {
                    // Guard against unbounded growth on a malformed stream.
                    if buf.len() < 8 * 1024 * 1024 {
                        buf.extend_from_slice(data);
                    } else {
                        buf.clear();
                        *capturing = false;
                    }
                    return;
                }
            }
        } else {
            match find_subslice(data, MARKER) {
                Some(start) => {
                    *capturing = true;
                    data = &data[start + MARKER.len()..];
                }
                None => return,
            }
        }
    }
}

/// 셀-흐름 인라인 이미지(OSC 1337) — 실제 PTY 로 전 경로(스캔→앵커→기록→뷰)를
/// 돈다. recall 이 kasaterm 에서 쓰는 형태(`width=<셀수>`, base64 PNG)를 그대로
/// 흘린다.
#[cfg(test)]
mod inline_image_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// 1x1 빨강 PNG. png_size 가 (1,1) 을 읽어 rows 계산까지 실경로를 탄다.
    const PNG_1X1: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn sh(pane_id: &str) -> PtySession {
        PtySession::start(PtyOptions {
            shell: Some(test_posix_shell()),
            cols: 40,
            rows: 10,
            pane_id: pane_id.into(),
            ..Default::default()
        })
        .expect("PTY 를 못 띄웠다")
    }

    fn emit(sess: &PtySession, name_b64: &str) {
        // printf 한 방이 recall 의 show_image 시퀀스와 같은 꼴이다. 뒤의 \n 은
        // 커서를 이미지 아래로 내린다(recall 은 자리를 스스로 예약한다).
        let cmd = format!(
            "printf '\\033]1337;File=name={name_b64};size=68;width=4;inline=1:{PNG_1X1}\\007\\n\\n\\n'\n"
        );
        sess.send_bytes(cmd.as_bytes()).unwrap();
    }

    fn wait_views(sess: &PtySession, want: usize) -> Vec<kasa_screen::screen::InlineImageView> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let v = sess.full_snapshot().inline_images;
            if v.len() == want || Instant::now() > deadline {
                return v;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn osc1337_lands_as_a_placed_view() {
        let sess = sh("test-inline");
        emit(&sess, "YS5wbmc="); // a.png
        let views = wait_views(&sess, 1);
        assert_eq!(views.len(), 1, "이미지가 뷰로 안 실렸다");
        let v = &views[0];
        assert_eq!(v.cols, 4, "width=4(셀) 가 그대로 와야 한다");
        // 1x1 픽셀 → rows = ceil(4/2.1) = 2.
        assert_eq!(v.rows, 2);
        assert!((0..10).contains(&v.row), "뷰포트 밖 배치: row={}", v.row);
        assert!(std::path::Path::new(&v.path).exists(), "temp 파일이 없다");
        let _ = std::fs::remove_file(&v.path);
    }

    /// 같은 (name, size) 재전송은 새 그림이 아니라 **이동**이다 — recall 은 그림
    /// 자리가 밀리면 같은 시퀀스를 다시 흘린다(recall 35b7f5e). 두 장으로 쌓이면
    /// 재전송마다 화면에 그림이 늘어난다.
    #[test]
    fn resend_of_same_image_moves_instead_of_duplicating() {
        let sess = sh("test-inline-move");
        emit(&sess, "Yi5wbmc="); // b.png
        let first = wait_views(&sess, 1);
        assert_eq!(first.len(), 1);
        emit(&sess, "Yi5wbmc=");
        // 재전송 완료를 화면 마커로 못박는다 — 이동 결과가 첫 배치와 같은 상대
        // 위치라, 뷰만 봐서는 「처리 전」과 「이동 후」가 구별되지 않는다.
        sess.send_bytes(b"printf 'RESEND-DONE\\n'\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !sess.visible_text(10).contains("RESEND-DONE") {
            assert!(Instant::now() < deadline, "마커가 10초 안에 안 떴다");
            std::thread::sleep(Duration::from_millis(50));
        }
        let v = sess.full_snapshot().inline_images;
        assert_eq!(v.len(), 1, "재전송이 두 장으로 쌓였다: {v:?}");
        assert_eq!(v[0].id, first[0].id, "이동인데 id 가 바뀌었다(텍스처 캐시가 죽는다)");
        assert_eq!(v[0].path, first[0].path, "이동인데 파일이 바뀌었다");
        let _ = std::fs::remove_file(&v[0].path);
    }

    /// 스크롤백으로 밀려난 그림은 그 프레임의 뷰에서 빠지고, 위로 올리면
    /// 돌아온다 — 「그림이 대화 기록에 남는다」의 실체다.
    #[test]
    fn scrolled_out_image_returns_when_scrolling_back() {
        let sess = sh("test-inline-scroll");
        emit(&sess, "Yy5wbmc="); // c.png
        let placed = wait_views(&sess, 1);
        assert_eq!(placed.len(), 1);
        // 화면(10행)보다 많이 밀어 그림을 히스토리로 보낸다.
        sess.send_bytes(b"printf '\\n\\n\\n\\n\\n\\n\\n\\n\\n\\n\\n\\n\\n\\n'\n").unwrap();
        let gone = wait_views(&sess, 0);
        assert!(gone.is_empty(), "밀려난 그림이 뷰에 남았다: {gone:?}");
        // 한 줄씩 올리며 찾는다 — 한 번에 크게 올리면 앵커를 지나칠 수 있고
        // (실측: scroll(20)이 정확히 한 줄 지나쳐 row=10 에 뒀다), 얼마나
        // 올려야 하는지는 배너·프롬프트 줄수에 따라 환경마다 다르다.
        let mut back = Vec::new();
        for _ in 0..40 {
            sess.scroll(1);
            back = sess.full_snapshot().inline_images;
            if !back.is_empty() {
                break;
            }
        }
        assert_eq!(back.len(), 1, "스크롤백을 다 올려도 그림이 안 돌아왔다");
        let _ = std::fs::remove_file(&back[0].path);
    }

    /// kasaslk 는 한 화면에 프사·이모지를 수십 장 그린다 — 옛 한도 16 을 넘는
    /// 그림이 말없이 버려지면 화면에 남은 그림이 빈 자리가 된다.
    #[test]
    fn pane_keeps_more_than_sixteen_images() {
        let sess = sh("test-inline-many");
        let want = 20;
        // size 만 달리해 서로 다른 그림(키)으로 만든다.
        let cmd = format!(
            "i=1; while [ $i -le {want} ]; do printf '\\033]1337;File=name=bi5wbmc=;size=%d;width=4;inline=1:{PNG_1X1}\\007\\n' $i; i=$((i+1)); done\n"
        );
        sess.send_bytes(cmd.as_bytes()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let kept = loop {
            let _ = sess.full_snapshot();
            let n = sess.inline_imgs.lock().unwrap().imgs.len();
            if n >= want || Instant::now() > deadline {
                break n;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let imgs: Vec<_> = sess.inline_imgs.lock().unwrap().imgs.drain(..).collect();
        for im in &imgs {
            let _ = std::fs::remove_file(&im.path);
        }
        assert_eq!(kept, want, "{want}장 중 {kept}장만 남았다");
    }

    fn fixture(id: u64, pixels: u64) -> InlineImg {
        InlineImg {
            id,
            key: (format!("f{id}"), id),
            path: std::path::PathBuf::from(format!("/nonexistent-kasaterm-inline-{id}.png")),
            pixels,
            abs_line: 0,
            col: 0,
            cols: 4,
            rows: 1,
            row_sig: None,
        }
    }

    #[test]
    fn slot_overflow_drops_the_oldest_first() {
        let mut imgs = InlineImgs::default();
        for id in 0..(INLINE_IMAGE_SLOTS as u64 + 3) {
            imgs.imgs.push(fixture(id, 56 * 60));
            imgs.evict_over_budget();
        }
        assert_eq!(imgs.imgs.len(), INLINE_IMAGE_SLOTS);
        assert_eq!(imgs.imgs[0].id, 3, "가장 오래된 셋이 먼저 나가야 한다");
        assert_eq!(imgs.imgs.last().unwrap().id, INLINE_IMAGE_SLOTS as u64 + 2);
    }

    /// 장수 안이어도 원본 픽셀 합이 넘으면 오래된 것부터 놓는다. 방금 온 한
    /// 장은 혼자 한도를 넘어도 남는다 — 그걸 버리면 아무것도 안 그려진다.
    #[test]
    fn pixel_budget_drops_the_oldest_but_keeps_the_newest() {
        let photo = 4032 * 3024;
        let mut imgs = InlineImgs::default();
        for id in 0..8 {
            imgs.imgs.push(fixture(id, photo));
            imgs.evict_over_budget();
        }
        let total: u64 = imgs.imgs.iter().map(|i| i.pixels).sum();
        assert!(total <= INLINE_IMAGE_PIXEL_BUDGET, "합계 {total} 가 한도를 넘었다");
        assert_eq!(imgs.imgs.len() as u64, INLINE_IMAGE_PIXEL_BUDGET / photo);
        assert_eq!(imgs.imgs.last().unwrap().id, 7);

        imgs.imgs.push(fixture(100, INLINE_IMAGE_PIXEL_BUDGET * 2));
        imgs.evict_over_budget();
        assert_eq!(imgs.imgs.len(), 1);
        assert_eq!(imgs.imgs[0].id, 100);
    }

    #[test]
    fn image_pixels_reads_png_and_other_headers() {
        let png = b64_decode(PNG_1X1.as_bytes());
        assert_eq!(image_pixels(&png), 1);
        assert_eq!(image_pixels(b"not an image at all"), 0);
    }

    #[test]
    fn child_env_carries_the_slot_count() {
        let sess = sh("test-inline-env");
        sess.send_bytes(b"echo \"SLOTS=$KASATERM_INLINE_IMAGE_SLOTS.\"\n").unwrap();
        let want = format!("SLOTS={INLINE_IMAGE_SLOTS}.");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !sess.visible_text(10).contains(&want) {
            assert!(Instant::now() < deadline, "자식 env 에 {want} 가 없다: {}", sess.visible_text(10));
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod kitty_graphics_tests {
    use super::*;
    use crate::kitty::diacritic;
    use crate::kitty::tests::{b64, png};
    use kasa_screen::screen::{CellClip, InlineImageView};

    struct Shared(Arc<Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// 리더 스레드가 하는 일(스캔하며 파서 먹이기 → 스냅샷에 그림 싣기)을 PTY 없이.
    struct Rig {
        term: Term<PtyEventForwarder>,
        proc: Processor<StdSyncHandler>,
        scan: InlineScan,
        imgs: Mutex<InlineImgs>,
        out: Arc<Mutex<Vec<u8>>>,
        responder: PtyEventForwarder,
    }

    fn rig(cols: u16, rows: u16, respond: bool) -> Rig {
        let out = Arc::new(Mutex::new(Vec::new()));
        let listener = PtyEventForwarder {
            respond,
            writer: Arc::new(Mutex::new(Box::new(Shared(out.clone())))),
            size: Arc::new(Mutex::new((cols, rows))),
            last_title: Arc::new(Mutex::new(None)),
        };
        Rig {
            responder: listener.clone(),
            term: make_term(cols, rows, listener),
            proc: Processor::new(),
            scan: InlineScan::default(),
            imgs: Mutex::new(InlineImgs::default()),
            out,
        }
    }

    impl Rig {
        fn feed(&mut self, bytes: &[u8]) {
            advance_scanning_inline(&mut self.proc, &mut self.term, bytes, &mut self.scan, &self.imgs, &self.responder);
        }
        fn views_at(&self, offset: usize) -> Vec<InlineImageView> {
            let mut u = ScreenUpdate::default();
            attach_inline_views_at_offset(&mut u, &self.term, &self.imgs, offset);
            u.inline_images
        }
        fn views(&self) -> Vec<InlineImageView> {
            self.views_at(0)
        }
        fn out(&self) -> String {
            String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned()
        }
    }

    const PROBE: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";

    /// `fg` 는 SGR 글자색 인자, `(line, col)` 은 상자 왼쪽 위(1부터) — 줄마다 CUP 로
    /// 그 열에 다시 선다(Claude Code 의 Ink 가 그리는 모양).
    fn placeholders(fg: &str, cols: u32, rows: u32, (line, col): (u32, u32)) -> String {
        (0..rows)
            .map(|r| {
                let mut s = format!("\x1b[{};{col}H\x1b[{fg}m", line + r);
                for c in 0..cols {
                    s.push(crate::kitty::PLACEHOLDER);
                    s.push(diacritic(r));
                    s.push(diacritic(c));
                }
                s + "\x1b[39m"
            })
            .collect()
    }

    fn transmit_virtual(r: &mut Rig, id: u32, cols: u32, rows: u32) {
        r.feed(format!("\x1b_Ga=T,U=1,q=2,f=100,i={id},c={cols},r={rows};{}\x1b\\", b64(&png(8, 8))).as_bytes());
    }

    /// 질의를 보낸 쪽은 뒤따른 DA1 이 먼저 오면 「지원 안 함」으로 읽는다.
    #[test]
    fn query_reply_precedes_da1() {
        let mut r = rig(40, 6, true);
        let mut bytes = PROBE.to_vec();
        bytes.extend_from_slice(b"\x1b[c");
        r.feed(&bytes);
        assert_eq!(r.out(), "\x1b_Gi=31;OK\x1b\\\x1b[?62;22c");
    }

    #[test]
    fn sequence_split_across_reads_is_still_whole() {
        let mut r = rig(40, 6, true);
        // 머리가 `ESC` | `_G…` 로, 끝(ST)이 `ESC` | `\\` 로 갈린다.
        r.feed(b"hi\x1b");
        r.feed(b"_Gi=31,s=1,v=1,a=q,t=d,f=24;AA");
        r.feed(b"AA\x1b");
        r.feed(b"\\ok");
        assert_eq!(r.out(), "\x1b_Gi=31;OK\x1b\\");
        assert_eq!(grid_line_sig(&r.term, 0), "hiok", "APC 가 화면에 글자로 새면 안 된다");
    }

    #[test]
    fn remote_mirror_parser_never_answers() {
        let mut r = rig(40, 6, false);
        r.feed(PROBE);
        assert_eq!(r.out(), "");
    }

    /// Claude Code mod `Image` 의 모양 — 4칸×2줄 얼굴 뒤에 이름.
    #[test]
    fn placeholders_become_one_clipped_view() {
        let mut r = rig(40, 6, true);
        transmit_virtual(&mut r, 7, 4, 2);
        r.feed(format!("ab{}", placeholders("38;5;7", 4, 2, (1, 3))).as_bytes());
        let v = r.views();
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!((v[0].row, v[0].col, v[0].cols, v[0].rows), (0, 2, 4, 2));
        assert_eq!(v[0].clip, Some(CellClip { row: 0, col: 2, cols: 4, rows: 2 }));
        // 자리표시 칸은 빈칸으로 넘어간다 — 그림이 없는 거울·폰도 자리만 남는다.
        let snap = snapshot(&mut r.term, 40, 6, "t", &Arc::new(Mutex::new(None)), true);
        let row0 = &snap.dirty.iter().find(|(i, _)| *i == 0).unwrap().1;
        let text: String = row0.iter().take(8).map(|c| c.ch).collect();
        assert_eq!(text, "ab      ");
    }

    #[test]
    fn scrolling_clips_like_text_and_scrollback_shows_it_again() {
        let mut r = rig(40, 4, true);
        transmit_virtual(&mut r, 7, 4, 2);
        r.feed(placeholders("38;5;7", 4, 2, (1, 1)).as_bytes());
        r.feed(b"\x1b[4;1H");
        // 맨 아래에서 한 줄 밀어 올리면 그림 첫 줄은 히스토리로, 둘째 줄만 화면
        // 맨 위에 남는다.
        r.feed(b"\r\nx");
        let v = r.views();
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!((v[0].row, v[0].clip), (-1, Some(CellClip { row: 0, col: 0, cols: 4, rows: 1 })));
        let back = r.views_at(1);
        assert_eq!(back[0].clip, Some(CellClip { row: 0, col: 0, cols: 4, rows: 2 }));
    }

    #[test]
    fn alt_screen_keeps_its_own_placeholders() {
        let mut r = rig(40, 6, true);
        transmit_virtual(&mut r, 7, 4, 2);
        r.feed(b"\x1b[?1049h");
        r.feed(placeholders("38;5;7", 4, 1, (3, 5)).as_bytes());
        let v = r.views();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].clip, Some(CellClip { row: 2, col: 4, cols: 4, rows: 1 }));
        r.feed(b"\x1b[?1049l");
        assert!(r.views().is_empty(), "대체 화면의 글자와 함께 그림도 사라진다");
        assert_eq!(r.imgs.lock().unwrap().kitty.images.len(), 1, "그림 데이터는 남는다");
    }

    #[test]
    fn truecolor_id_with_high_byte_and_inferred_cells() {
        let mut r = rig(40, 6, true);
        let id = 2u32 << 24 | 0x01_02_03;
        transmit_virtual(&mut r, id, 3, 1);
        // 첫 칸만 행·열·윗바이트를 싣고 나머지는 결합 문자 없이 — 왼쪽에서 잇는다.
        let mut line = String::from("\x1b[38;2;1;2;3m");
        line.push(crate::kitty::PLACEHOLDER);
        line.push(diacritic(0));
        line.push(diacritic(0));
        line.push(diacritic(2));
        line.push(crate::kitty::PLACEHOLDER);
        line.push(crate::kitty::PLACEHOLDER);
        r.feed(line.as_bytes());
        let v = r.views();
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].clip, Some(CellClip { row: 0, col: 0, cols: 3, rows: 1 }));
    }

    #[test]
    fn unknown_image_ids_draw_nothing() {
        let mut r = rig(40, 6, true);
        transmit_virtual(&mut r, 7, 4, 1);
        r.feed(placeholders("38;5;8", 4, 1, (1, 1)).as_bytes());
        assert!(r.views().is_empty());
    }

    #[test]
    fn cursor_put_moves_the_cursor_and_rides_the_scroll() {
        let mut r = rig(40, 5, true);
        r.feed(format!("x\x1b_Ga=T,f=100,i=1,c=3,r=2;{}\x1b\\y", b64(&png(4, 4))).as_bytes());
        assert_eq!(grid_line_sig(&r.term, 1), "y");
        let cur = r.term.grid().cursor.point;
        assert_eq!((cur.line.0, cur.column.0), (1, 5), "아래로 1줄, 오른쪽으로 3칸 뒤 y 한 글자");
        let v = r.views();
        assert_eq!((v[0].row, v[0].col, v[0].cols, v[0].rows), (0, 1, 3, 2));
        r.feed(b"\r\n\r\n\r\n\r\n");
        assert_eq!(r.views()[0].row, -1, "글과 함께 위로 밀린다");
        // 지우기(a=d, 화면 전부)로 놓기가 사라진다.
        r.feed(b"\x1b_Ga=d\x1b\\");
        assert!(r.views().is_empty());
    }

    #[test]
    fn budget_counts_iterm_and_kitty_together() {
        let mut imgs = InlineImgs::default();
        for id in 0..(INLINE_IMAGE_SLOTS as u64 - 1) {
            imgs.imgs.push(InlineImg {
                id,
                key: (format!("f{id}"), id),
                path: std::path::PathBuf::from(format!("/nonexistent-kasaterm-inline-{id}.png")),
                pixels: 1,
                abs_line: 0,
                col: 0,
                cols: 1,
                rows: 1,
                row_sig: None,
            });
        }
        imgs.next_id = INLINE_IMAGE_SLOTS as u64;
        let env = crate::kitty::Env { local_media: true, grid_cols: 80, hist: 0 };
        for i in 1..=2 {
            let body = format!("a=t,f=100,i={i},q=2;{}", b64(&png(2, 2)));
            let InlineImgs { kitty, next_id, .. } = &mut imgs;
            kitty.handle(body.as_bytes(), None, &env, next_id);
            imgs.evict_over_budget();
        }
        assert_eq!(imgs.imgs.len() + imgs.kitty.images.len(), INLINE_IMAGE_SLOTS);
        assert_eq!(imgs.imgs[0].id, 1, "가장 오래된 OSC 1337 기록이 먼저 나간다");
        assert_eq!(imgs.kitty.images.len(), 2);
    }
}

#[cfg(test)]
mod external_inline_tests {
    use super::*;

    #[test]
    fn live_inline_images_ignore_host_scrollback_offset() {
        let (sess, events, _writer, _resized) = ext_session(20, 5);
        let lines = (0..15).map(|line| format!("L{line:02}\r\n")).collect::<String>();
        events.send(ExtEvent::Bytes(lines.into_bytes())).unwrap();
        assert!(wait_text(&sess, "L14"));
        let history = sess.view_state().1 as i64;
        sess.inline_imgs.lock().unwrap().imgs.push(InlineImg {
            id: 1, key: ("fixture".into(), 0),
            path: std::path::PathBuf::from("/nonexistent-kasaterm-inline-fixture.png"),
            pixels: 0, abs_line: history + 1, col: 2, cols: 4, rows: 1, row_sig: None,
        });
        assert_eq!(sess.live_screen().inline_images[0].row, 1);
        sess.scroll(3);
        assert_eq!(sess.view_state().0, 3);
        assert_eq!(sess.full_snapshot().inline_images[0].row, 4);
        assert_eq!(sess.live_screen().inline_images[0].row, 1,
            "live cells and inline placement must use the same zero offset");
    }
}
