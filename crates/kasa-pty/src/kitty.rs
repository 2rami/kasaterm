//! kitty 그림 프로토콜(APC `ESC _ G <키=값,…> ; <본문> ESC \`).
//!
//! 정본은 <https://sw.kovidgoyal.net/kitty/graphics-protocol/>. 여기서 하는 일:
//! 전송(`t=d` 본문·`t=f` 파일·`t=t` 임시 파일·`t=s` 공유 메모리, `f=100` PNG·
//! `f=32`/`f=24` 날 픽셀, `o=z` 압축, `m=1` 조각), 놓기(`a=T`·`a=p` — 글자 칸
//! 자리표시 `U=1` 과 커서 자리 놓기), 지우기(`a=d`), 질의(`a=q`).
//!
//! 그림 데이터는 OSC 1337 과 같은 길을 탄다 — PNG 임시 파일 하나를 GUI 가 읽어
//! 텍스처로 올린다. 그래서 장수·픽셀 한도도 OSC 1337 기록과 한 셈으로 묶인다
//! (`InlineImgs::evict_over_budget`).
//!
//! 자리표시(`U+10EEEE` + 행·열 결합 문자, 그림 id 는 글자색)는 **글자**라서
//! 스크롤·리플로우·대체 화면을 글과 똑같이 따라간다. 화면을 뜰 때마다 보이는
//! 칸에서 자리표시를 찾아 그 자리에 그림 조각을 싣는다(`placeholder_views`) —
//! 앵커를 따로 들고 다니지 않으니 어긋날 일이 없다. Claude Code 의 mod `Image`
//! 가 이 방식이다.

use std::io::Read;
use std::path::{Path, PathBuf};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Cell as TermCell;
use alacritty_terminal::vte::ansi::Color as VtColor;
use alacritty_terminal::Term;
use kasa_bridge::screen::{CellClip, InlineImageView};

pub(crate) const PLACEHOLDER: char = '\u{10EEEE}';

/// 그림 한 장의 해독 뒤 바이트 상한. 날 RGBA 4096×4096 이 64MB 라 그 위는 받지 않는다.
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
/// 날 픽셀 그림의 가로·세로 상한(kitty 와 Claude Code 가 쓰는 1만).
const MAX_SIDE: u32 = 10_000;
/// 놓은 상자 한 변의 칸 수 상한 — 잘못된 `c=`·`r=` 가 커서를 수만 줄 내리지 않게.
const MAX_BOX_CELLS: u32 = 1_000;

/// 행·열 번호 결합 문자(kitty `rowcolumn-diacritics.txt`). 순번이 곧 번호다.
/// 전부 결합 등급 230 이라 NFC 정규화(리더가 배치마다 돌린다)가 순서를 안 바꾼다.
static DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A,
    0x034B, 0x034C, 0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365,
    0x0366, 0x0367, 0x0368, 0x0369, 0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F,
    0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592, 0x0593, 0x0594, 0x0595, 0x0597,
    0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1, 0x05A8, 0x05A9,
    0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6,
    0x06D7, 0x06D8, 0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2,
    0x06E4, 0x06E7, 0x06E8, 0x06EB, 0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736,
    0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743, 0x0745, 0x0747, 0x0749, 0x074A,
    0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3, 0x0816, 0x0817,
    0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951,
    0x0953, 0x0954, 0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD,
    0x193A, 0x1A17, 0x1A75, 0x1A76, 0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C,
    0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1,
    0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4, 0x1DC5, 0x1DC6,
    0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF,
    0x1DE0, 0x1DE1, 0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1,
    0x20D4, 0x20D5, 0x20D6, 0x20D7, 0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0,
    0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2, 0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6,
    0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE, 0x2DEF, 0x2DF0,
    0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1,
    0xA8E0, 0xA8E1, 0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9,
    0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED, 0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2,
    0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1, 0xFE20, 0xFE21, 0xFE22, 0xFE23,
    0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187, 0x1D188, 0x1D189,
    0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

fn diacritic_index(c: char) -> Option<u32> {
    DIACRITICS.binary_search(&(c as u32)).ok().map(|i| i as u32)
}

/// GUI 가 알려 준 셀 픽셀 크기(물리 px). 크기 없이 놓은 그림의 칸 수와
/// `TIOCSWINSZ`·`CSI 14 t` 응답이 이걸 쓴다. 0 이면 아직 모른다.
static CELL_PX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 셀 한 칸의 물리 픽셀 크기를 알린다(글꼴·배율이 바뀔 때마다).
pub fn set_cell_pixels(w: u32, h: u32) {
    CELL_PX.store(((w as u64) << 32) | h as u64, std::sync::atomic::Ordering::Relaxed);
}

/// 알려진 셀 픽셀 크기, 모르면 `None`.
pub(crate) fn cell_pixels() -> Option<(u32, u32)> {
    let v = CELL_PX.load(std::sync::atomic::Ordering::Relaxed);
    let (w, h) = ((v >> 32) as u32, v as u32);
    (w > 0 && h > 0).then_some((w, h))
}

/// 해독한 제어 키들. 없는 키는 kitty 기본값이다.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Command {
    action: Option<u8>,
    quiet: u8,
    id: u32,
    number: u32,
    placement: u32,
    format: u32,
    medium: Option<u8>,
    compressed: bool,
    width: u32,
    height: u32,
    size: u64,
    offset: u64,
    more: bool,
    cols: u32,
    rows: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    no_move: bool,
    virtual_: bool,
    delete: Option<u8>,
    z: i32,
    parent: u32,
}

impl Command {
    pub(crate) fn parse(control: &[u8]) -> Self {
        let mut cmd = Command::default();
        for kv in control.split(|&b| b == b',') {
            let Some(eq) = kv.iter().position(|&b| b == b'=') else { continue };
            let (k, v) = (&kv[..eq], &kv[eq + 1..]);
            if k.len() != 1 {
                continue;
            }
            let num = || std::str::from_utf8(v).ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
            let n32 = || num().min(u32::MAX as u64) as u32;
            match k[0] {
                b'a' => cmd.action = v.first().copied(),
                b'q' => cmd.quiet = n32() as u8,
                b'i' => cmd.id = n32(),
                b'I' => cmd.number = n32(),
                b'p' => cmd.placement = n32(),
                b'f' => cmd.format = n32(),
                b't' => cmd.medium = v.first().copied(),
                b'o' => cmd.compressed = v.first() == Some(&b'z'),
                b's' => cmd.width = n32(),
                b'v' => cmd.height = n32(),
                b'S' => cmd.size = num(),
                b'O' => cmd.offset = num(),
                b'm' => cmd.more = v.first() == Some(&b'1'),
                b'c' => cmd.cols = n32(),
                b'r' => cmd.rows = n32(),
                b'x' => cmd.x = n32(),
                b'y' => cmd.y = n32(),
                b'w' => cmd.w = n32(),
                b'h' => cmd.h = n32(),
                b'C' => cmd.no_move = v.first() == Some(&b'1'),
                b'U' => cmd.virtual_ = v.first() == Some(&b'1'),
                b'd' => cmd.delete = v.first().copied(),
                b'z' => {
                    cmd.z = std::str::from_utf8(v).ok().and_then(|s| s.parse().ok()).unwrap_or(0)
                }
                b'P' => cmd.parent = n32(),
                _ => {}
            }
        }
        cmd
    }

    fn action(&self) -> u8 {
        self.action.unwrap_or(b't')
    }
}

/// 받은 그림 한 장. 파일은 이 기록이 소유한다.
pub(crate) struct KittyImage {
    pub(crate) id: u32,
    number: u32,
    /// GUI 텍스처의 정체 — 같은 id 로 다시 보내면 새 값이 돼 텍스처를 다시 뜬다.
    /// 놓아 줄 순서(오래된 것부터)도 이 값이다.
    pub(crate) uid: u64,
    pub(crate) path: PathBuf,
    pub(crate) pixels: u64,
    width: u32,
    height: u32,
}

/// 그림 하나를 놓은 자리. 자리표시(`U=1`)는 상자 크기만 쥐고 위치는 글자가
/// 정한다. 커서 자리 놓기는 내용 절대 줄 앵커를 쥔다(OSC 1337 기록과 같은 뜻).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Placement {
    image: u32,
    id: u32,
    cols: u16,
    rows: u16,
    virtual_: bool,
    abs_line: i64,
    col: u16,
    z: i32,
}

struct Pending {
    cmd: Command,
    at: Option<At>,
    data: Vec<u8>,
}

#[derive(Default)]
pub(crate) struct KittyStore {
    pub(crate) images: Vec<KittyImage>,
    placements: Vec<Placement>,
    pending: Option<Pending>,
    next_auto_id: u32,
}

/// 명령이 온 순간의 커서(내용 절대 줄, 열). 동기 출력 중이라 모르면 `None`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct At {
    pub(crate) abs_line: i64,
    pub(crate) col: u16,
}

pub(crate) struct Env {
    /// 파일·공유 메모리를 읽어도 되나. 원격 거울의 파서는 남의 기기 경로를 받으므로
    /// 끈다 — 그 경로는 이 기기에서 뜻이 없고, `t=t` 는 지우기까지 한다.
    pub(crate) local_media: bool,
    pub(crate) grid_cols: u16,
    /// 화면 첫 줄의 내용 절대 줄(= 히스토리 줄 수). 지우기의 `x`·`y` 를 앵커와 견준다.
    pub(crate) hist: i64,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Outcome {
    pub(crate) reply: Option<Vec<u8>>,
    /// 커서 자리 놓기 뒤 커서를 내릴 줄 수와 놓을 열(kitty: 오른쪽으로 상자 폭,
    /// 아래로 높이-1, 오른쪽 끝을 넘으면 다음 줄 첫 칸).
    pub(crate) cursor: Option<(u16, u16)>,
}

const EBADF: &str = "EBADF:Failed to read image file";

impl KittyStore {
    pub(crate) fn is_empty(&self) -> bool {
        self.images.is_empty() && self.pending.is_none()
    }

    /// 화면 좌표가 통째로 무너지는 사건(리플로우·대체 화면 전환·히스토리 지움)
    /// — 앵커를 쥔 놓기만 버린다. 자리표시는 글자가 정본이라 그대로 산다.
    pub(crate) fn drop_anchored(&mut self) {
        self.placements.retain(|p| p.virtual_);
    }

    /// 가장 오래 산 그림 하나를 파일째 놓는다(한도 넘김).
    pub(crate) fn evict(&mut self, uid: u64) {
        if let Some(i) = self.images.iter().position(|im| im.uid == uid) {
            let im = self.images.remove(i);
            let _ = std::fs::remove_file(&im.path);
            self.placements.retain(|p| p.image != im.id);
        }
    }

    pub(crate) fn clear(&mut self) {
        for im in self.images.drain(..) {
            let _ = std::fs::remove_file(&im.path);
        }
        self.placements.clear();
        self.pending = None;
    }

    fn image(&self, id: u32) -> Option<&KittyImage> {
        self.images.iter().find(|im| im.id == id)
    }

    fn newest_numbered(&self, number: u32) -> Option<u32> {
        self.images.iter().filter(|im| im.number == number).max_by_key(|im| im.uid).map(|im| im.id)
    }

    fn fresh_id(&mut self) -> u32 {
        // 클라이언트가 고르는 작은 수와 안 겹치게 위쪽 절반에서 나눠 준다.
        loop {
            self.next_auto_id = self.next_auto_id.wrapping_add(1);
            let id = 0x8000_0000 | self.next_auto_id;
            if self.image(id).is_none() {
                return id;
            }
        }
    }

    /// 완성된 APC 본문(`G` 뒤부터 ST 앞까지) 하나를 처리한다. `next_uid` 는
    /// OSC 1337 기록과 함께 쓰는 순번이다.
    pub(crate) fn handle(&mut self, body: &[u8], at: Option<At>, env: &Env, next_uid: &mut u64) -> Outcome {
        let semi = body.iter().position(|&b| b == b';');
        let (control, payload) = match semi {
            Some(i) => (&body[..i], &body[i + 1..]),
            None => (body, &body[body.len()..]),
        };
        let mut cmd = Command::parse(control);
        let mut at = at;
        let mut data: Vec<u8>;
        // 이어 받는 조각은 `m`(과 `q`)만 싣는다. 동작 키가 있으면 받던 것을 버린
        // 새 명령이고, 지우기는 받던 것을 버린 뒤 지운다.
        match cmd.action {
            Some(b'd') => return self.delete(&cmd, at, env),
            Some(_) => self.pending = None,
            None => {}
        }
        if let Some(mut p) = self.pending.take() {
            if p.data.len() + payload.len() > MAX_IMAGE_BYTES * 4 / 3 + 4 {
                return reply(&p.cmd, p.cmd.id, p.cmd.number, 0, "EFBIG:image too large");
            }
            p.data.extend_from_slice(payload);
            if cmd.more {
                self.pending = Some(p);
                return Outcome::default();
            }
            if cmd.quiet != 0 {
                p.cmd.quiet = cmd.quiet;
            }
            cmd = p.cmd;
            at = p.at;
            data = p.data;
        } else {
            if cmd.more && matches!(cmd.action(), b't' | b'T' | b'q') && cmd.medium.unwrap_or(b'd') == b'd' {
                self.pending = Some(Pending { cmd, at, data: payload.to_vec() });
                return Outcome::default();
            }
            data = payload.to_vec();
        }
        if cmd.id != 0 && cmd.number != 0 {
            return reply(&cmd, cmd.id, cmd.number, 0, "EINVAL:i and I are mutually exclusive");
        }
        match cmd.action() {
            b'q' => {
                let msg = match load(&cmd, &data, env) {
                    Ok(_) => "OK".to_string(),
                    Err(e) => e,
                };
                reply(&cmd, cmd.id, cmd.number, 0, &msg)
            }
            b't' | b'T' => {
                let loaded = match load(&cmd, &data, env) {
                    Ok(l) => l,
                    Err(e) => return reply(&cmd, cmd.id, cmd.number, 0, &e),
                };
                data.clear();
                let id = if cmd.id != 0 { cmd.id } else { self.fresh_id() };
                let uid = *next_uid;
                *next_uid += 1;
                let Some(path) = write_png(&loaded.png) else {
                    return reply(&cmd, cmd.id, cmd.number, 0, "ENOSPC:could not store image");
                };
                let image = KittyImage {
                    id,
                    number: cmd.number,
                    uid,
                    path,
                    pixels: loaded.width as u64 * loaded.height as u64,
                    width: loaded.width,
                    height: loaded.height,
                };
                // 같은 id 로 다시 보내면 그림만 갈아 끼운다 — 자리표시·놓기는 id 로
                // 그림을 찾으므로 그대로 새 그림을 보여 준다.
                if let Some(i) = self.images.iter().position(|im| im.id == id) {
                    let old = std::mem::replace(&mut self.images[i], image);
                    let _ = std::fs::remove_file(&old.path);
                } else {
                    self.images.push(image);
                }
                if cmd.action() == b'T' {
                    return self.put(&cmd, id, at, env);
                }
                reply(&cmd, id, cmd.number, 0, "OK")
            }
            b'p' => {
                let id = if cmd.id != 0 { Some(cmd.id) } else { self.newest_numbered(cmd.number) };
                match id.filter(|id| self.image(*id).is_some()) {
                    Some(id) => self.put(&cmd, id, at, env),
                    None => reply(&cmd, cmd.id, cmd.number, cmd.placement, "ENOENT:image not found"),
                }
            }
            _ => reply(&cmd, cmd.id, cmd.number, 0, "EINVAL:unsupported action"),
        }
    }

    fn put(&mut self, cmd: &Command, id: u32, at: Option<At>, env: &Env) -> Outcome {
        if cmd.parent != 0 {
            return reply(cmd, id, cmd.number, cmd.placement, "EINVAL:relative placements are not supported");
        }
        let Some(im) = self.image(id) else {
            return reply(cmd, id, cmd.number, cmd.placement, "ENOENT:image not found");
        };
        let (cols, rows) = box_cells(cmd, im.width, im.height);
        let placement = Placement {
            image: id,
            id: cmd.placement,
            cols,
            rows,
            virtual_: cmd.virtual_,
            abs_line: at.map_or(0, |a| a.abs_line),
            col: at.map_or(0, |a| a.col),
            z: cmd.z,
        };
        if placement.virtual_ {
            self.placements.retain(|p| !(p.virtual_ && p.image == id && p.id == cmd.placement));
            self.placements.push(placement);
            return reply(cmd, id, cmd.number, cmd.placement, "OK");
        }
        let Some(at) = at else {
            // 동기 출력 한가운데 커서 이동도 없이 온 놓기 — 자리를 모른다. 엉뚱한
            // 곳에 그리느니 놓지 않는다(OSC 1337 과 같은 판단).
            return reply(cmd, id, cmd.number, cmd.placement, "OK");
        };
        if cmd.placement != 0 {
            self.placements.retain(|p| !(p.image == id && p.id == cmd.placement));
        }
        self.placements.push(placement);
        let mut out = reply(cmd, id, cmd.number, cmd.placement, "OK");
        if !cmd.no_move {
            let right = at.col as u32 + cols as u32;
            out.cursor = Some(if right >= env.grid_cols as u32 {
                (rows, 0)
            } else {
                (rows - 1, right as u16)
            });
        }
        out
    }

    fn delete(&mut self, cmd: &Command, at: Option<At>, env: &Env) -> Outcome {
        self.pending = None;
        let what = cmd.delete.unwrap_or(b'a');
        let free = what.is_ascii_uppercase();
        // 화면 칸(1부터) → 내용 절대 줄·열.
        let cell_line = |y: u32| env.hist + y.max(1) as i64 - 1;
        let cell_col = |x: u32| x.max(1) as i64 - 1;
        let hits = |p: &Placement, line: Option<i64>, col: Option<i64>| {
            !p.virtual_
                && line.is_none_or(|l| l >= p.abs_line && l < p.abs_line + p.rows as i64)
                && col.is_none_or(|c| c >= p.col as i64 && c < p.col as i64 + p.cols as i64)
        };
        let mut touched: Vec<u32> = Vec::new();
        let mut drop_images: Vec<u32> = Vec::new();
        let mut remove = |placements: &mut Vec<Placement>, pred: &dyn Fn(&Placement) -> bool| {
            placements.retain(|p| {
                let hit = pred(p);
                if hit {
                    touched.push(p.image);
                }
                !hit
            });
        };
        match what.to_ascii_lowercase() {
            b'a' => remove(&mut self.placements, &|p| !p.virtual_),
            b'i' => {
                let (id, pid) = (cmd.id, cmd.placement);
                remove(&mut self.placements, &|p| p.image == id && (pid == 0 || p.id == pid));
                if free && pid == 0 {
                    drop_images.push(id);
                }
            }
            b'n' => {
                if let Some(id) = self.newest_numbered(cmd.number) {
                    let pid = cmd.placement;
                    remove(&mut self.placements, &|p| p.image == id && (pid == 0 || p.id == pid));
                    if free && pid == 0 {
                        drop_images.push(id);
                    }
                }
            }
            b'c' => {
                if let Some(at) = at {
                    remove(&mut self.placements, &|p| hits(p, Some(at.abs_line), Some(at.col as i64)));
                }
            }
            b'p' => {
                let (l, c) = (cell_line(cmd.y), cell_col(cmd.x));
                remove(&mut self.placements, &|p| hits(p, Some(l), Some(c)));
            }
            b'q' => {
                let (l, c, z) = (cell_line(cmd.y), cell_col(cmd.x), cmd.z);
                remove(&mut self.placements, &|p| hits(p, Some(l), Some(c)) && p.z == z);
            }
            b'x' => {
                let c = cell_col(cmd.x);
                remove(&mut self.placements, &|p| hits(p, None, Some(c)));
            }
            b'y' => {
                let l = cell_line(cmd.y);
                remove(&mut self.placements, &|p| hits(p, Some(l), None));
            }
            b'z' => {
                let z = cmd.z;
                remove(&mut self.placements, &|p| !p.virtual_ && p.z == z);
            }
            b'r' => {
                let (lo, hi) = (cmd.x, cmd.y);
                remove(&mut self.placements, &|p| p.image >= lo && p.image <= hi);
                if free {
                    drop_images.extend(self.images.iter().map(|im| im.id).filter(|id| *id >= lo && *id <= hi));
                }
            }
            _ => {}
        }
        if free {
            // 대문자: 놓기를 잃고 더는 아무 데도 안 놓인 그림은 데이터째 놓는다.
            for id in touched {
                if !self.placements.iter().any(|p| p.image == id) {
                    drop_images.push(id);
                }
            }
            for id in drop_images {
                self.placements.retain(|p| p.image != id);
                if let Some(i) = self.images.iter().position(|im| im.id == id) {
                    let im = self.images.remove(i);
                    let _ = std::fs::remove_file(&im.path);
                }
            }
        }
        Outcome::default()
    }
}

/// 놓을 상자의 칸 수. 둘 다 주면 그대로, 하나만 주면 그림 비율로 나머지를,
/// 둘 다 없으면 원본 픽셀 크기를 셀 픽셀로 나눈다(셀 크기를 모르면 흔한 8×16).
fn box_cells(cmd: &Command, width: u32, height: u32) -> (u16, u16) {
    let (sw, sh) = (
        if cmd.w > 0 { cmd.w.min(width) } else { width }.max(1) as f64,
        if cmd.h > 0 { cmd.h.min(height) } else { height }.max(1) as f64,
    );
    let (cw, ch) = cell_pixels().map_or((8.0, 16.0), |(w, h)| (w as f64, h as f64));
    let (cols, rows) = match (cmd.cols, cmd.rows) {
        (0, 0) => ((sw / cw).ceil(), (sh / ch).ceil()),
        (c, 0) => (c as f64, (c as f64 * cw * sh / sw / ch).ceil()),
        (0, r) => ((r as f64 * ch * sw / sh / cw).ceil(), r as f64),
        (c, r) => (c as f64, r as f64),
    };
    let clamp = |v: f64| v.clamp(1.0, MAX_BOX_CELLS as f64) as u16;
    (clamp(cols), clamp(rows))
}

fn reply(cmd: &Command, id: u32, number: u32, placement: u32, msg: &str) -> Outcome {
    let ok = msg == "OK";
    // 번호도 id 도 없으면 kitty 는 답하지 않는다. q=1 은 OK 를, q=2 는 전부를 삼킨다.
    if (id == 0 && number == 0) || cmd.quiet >= 2 || (ok && cmd.quiet == 1) {
        return Outcome::default();
    }
    let mut keys = String::new();
    if id != 0 {
        keys.push_str(&format!("i={id}"));
    }
    if number != 0 {
        if !keys.is_empty() {
            keys.push(',');
        }
        keys.push_str(&format!("I={number}"));
    }
    if placement != 0 {
        keys.push_str(&format!(",p={placement}"));
    }
    Outcome { reply: Some(format!("\x1b_G{keys};{msg}\x1b\\").into_bytes()), cursor: None }
}

struct Loaded {
    png: Vec<u8>,
    width: u32,
    height: u32,
}

fn load(cmd: &Command, payload: &[u8], env: &Env) -> Result<Loaded, String> {
    let raw = match cmd.medium.unwrap_or(b'd') {
        b'd' => crate::state::b64_decode(payload),
        medium @ (b'f' | b't' | b's') => {
            if !env.local_media {
                return Err(EBADF.into());
            }
            let name = String::from_utf8(crate::state::b64_decode(payload)).map_err(|_| EBADF.to_string())?;
            let read = match medium {
                b'f' => read_file(&name, cmd.offset, cmd.size, false),
                b't' => read_file(&name, cmd.offset, cmd.size, true),
                _ => read_shm(&name, cmd.offset, cmd.size),
            };
            read.ok_or_else(|| EBADF.to_string())?
        }
        _ => return Err("EINVAL:unknown transmission medium".into()),
    };
    let data = if cmd.compressed {
        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&raw, MAX_IMAGE_BYTES)
            .map_err(|_| "EINVAL:zlib decompression failed".to_string())?
    } else {
        raw
    };
    decode(cmd, data)
}

fn decode(cmd: &Command, data: Vec<u8>) -> Result<Loaded, String> {
    let format = if cmd.format == 0 { 32 } else { cmd.format };
    match format {
        100 => {
            let (width, height) = crate::state::png_size(&data)
                .filter(|(w, h)| *w > 0 && *h > 0)
                .ok_or_else(|| "EBADPNG:not a PNG image".to_string())?;
            if width > MAX_SIDE || height > MAX_SIDE {
                return Err("EFBIG:image dimensions too large".into());
            }
            Ok(Loaded { png: data, width, height })
        }
        32 | 24 => {
            let (width, height) = (cmd.width, cmd.height);
            if width == 0 || height == 0 {
                return Err("EINVAL:s and v are required for raw pixel data".into());
            }
            if width > MAX_SIDE || height > MAX_SIDE {
                return Err("EFBIG:image dimensions too large".into());
            }
            let bpp = (format / 8) as usize;
            let need = width as usize * height as usize * bpp;
            if data.len() < need {
                return Err("ENODATA:insufficient image data".into());
            }
            let color = if bpp == 4 {
                image::ExtendedColorType::Rgba8
            } else {
                image::ExtendedColorType::Rgb8
            };
            let mut png = Vec::new();
            // 날 픽셀은 PNG 로 옮겨 OSC 1337 과 같은 파일 길을 태운다. 매 프레임
            // 갈아 끼우는 그림(`$.ui.blit`)도 있어 압축은 가장 빠른 쪽으로.
            use image::ImageEncoder;
            image::codecs::png::PngEncoder::new_with_quality(
                &mut png,
                image::codecs::png::CompressionType::Fast,
                image::codecs::png::FilterType::NoFilter,
            )
            .write_image(&data[..need], width, height, color)
            .map_err(|_| "EINVAL:could not encode image".to_string())?;
            Ok(Loaded { png, width, height })
        }
        _ => Err("EINVAL:unknown image format".into()),
    }
}

fn write_png(bytes: &[u8]) -> Option<PathBuf> {
    // `uid` 는 pane 마다 0 부터 세고 macOS 시계는 마이크로초 단위라, 두 pane 이 같은
    // 순간 첫 그림을 받으면 이름이 겹쳐 한쪽이 남의 파일을 지운다. 프로세스 전역
    // 순번과 pid 로 가른다.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("kasaterm-kitty-{}-{seq}.png", std::process::id()));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

/// 커널 가짜 파일·장치가 사는 곳. 정규 파일 검사로도 대부분 막히지만 `/proc`
/// 아래에는 정규 파일 행세를 하는 것이 있어 경로로도 막는다.
fn forbidden(path: &Path) -> bool {
    ["/proc", "/sys", "/dev"].iter().any(|p| path.starts_with(p))
}

/// `t=t` 로 받은 파일을 지워도 되는 임시 폴더인가.
fn in_temp_dir(path: &Path) -> bool {
    let mut roots: Vec<PathBuf> = vec![PathBuf::from("/tmp"), PathBuf::from("/dev/shm")];
    roots.push(std::env::temp_dir());
    roots.iter().any(|r| {
        path.starts_with(r) || std::fs::canonicalize(r).is_ok_and(|c| path.starts_with(c))
    })
}

/// 사람 권한으로 사람이 가리킨 파일을 읽는다 — 정규 파일만, 가짜 파일 시스템은
/// 거부, 크기 상한. 실패는 이유와 관계없이 한 가지 오류로 답한다(kitty 규칙 —
/// 다르게 답하면 파일이 있는지 없는지를 캐내는 길이 된다).
fn read_file(name: &str, offset: u64, size: u64, temp: bool) -> Option<Vec<u8>> {
    if name.is_empty() || name.contains('\0') || name.len() > 4096 {
        return None;
    }
    let path = Path::new(name);
    if !path.is_absolute() || forbidden(path) {
        return None;
    }
    let real = std::fs::canonicalize(path).ok()?;
    if forbidden(&real) {
        return None;
    }
    let meta = std::fs::metadata(&real).ok()?;
    if !meta.is_file() {
        return None;
    }
    let len = meta.len();
    if offset > len {
        return None;
    }
    let want = if size > 0 { size.min(len - offset) } else { len - offset };
    if want > MAX_IMAGE_BYTES as u64 {
        return None;
    }
    let mut f = std::fs::File::open(&real).ok()?;
    if offset > 0 {
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Start(offset)).ok()?;
    }
    let mut buf = Vec::with_capacity(want as usize);
    f.take(want).read_to_end(&mut buf).ok()?;
    // 임시 파일은 다 읽은 뒤 지운다 — 단 이름에 약속된 표지가 있고 임시 폴더
    // 안일 때만. 아무 경로나 지우게 두면 출력 한 줄로 사람 파일이 사라진다.
    if temp && name.contains("tty-graphics-protocol") && in_temp_dir(&real) {
        let _ = std::fs::remove_file(&real);
    }
    Some(buf)
}

#[cfg(unix)]
fn read_shm(name: &str, offset: u64, size: u64) -> Option<Vec<u8>> {
    // POSIX 이름은 `/` 하나로 시작하고 그 뒤엔 `/` 가 없다. `kitten icat` 은 앞의
    // `/` 없이 만들고 보낸다 — macOS 는 둘을 다른 이름으로 보므로 받은 그대로 먼저
    // 열고, 안 열리면 `/` 를 붙여 본다.
    let bare = name.strip_prefix('/').unwrap_or(name);
    if bare.is_empty() || bare.contains('/') || bare.len() > 254 || bare.contains('\0') {
        return None;
    }
    let open = |n: &str| {
        let c = std::ffi::CString::new(n).ok()?;
        // SAFETY: 이름은 NUL 없는 C 문자열이고, 연 fd 는 이 함수 안에서 닫는다.
        let fd = unsafe { libc::shm_open(c.as_ptr(), libc::O_RDONLY, 0 as libc::c_uint) };
        (fd >= 0).then_some((fd, c))
    };
    let (fd, cname) = open(name).or_else(|| open(&format!("/{bare}")))?;
    let out = (|| {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } != 0 {
            return None;
        }
        let len = st.st_size as u64;
        if offset > len {
            return None;
        }
        let want = if size > 0 { size.min(len - offset) } else { len - offset };
        if want == 0 || want > MAX_IMAGE_BYTES as u64 {
            return None;
        }
        let map_len = (offset + want) as usize;
        // SAFETY: 읽기 전용 공유 사상, 길이는 fstat 크기 안. 복사 뒤 바로 푼다.
        let ptr = unsafe {
            libc::mmap(std::ptr::null_mut(), map_len, libc::PROT_READ, libc::MAP_SHARED, fd, 0)
        };
        if ptr == libc::MAP_FAILED {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, map_len) }[offset as usize..].to_vec();
        unsafe { libc::munmap(ptr, map_len) };
        Some(bytes)
    })();
    // 읽었든 못 읽었든 이름은 지운다 — 보낸 쪽은 한 번 쓰고 잊는다(kitty 규칙).
    unsafe {
        libc::close(fd);
        libc::shm_unlink(cname.as_ptr());
    }
    out
}

#[cfg(not(unix))]
fn read_shm(_name: &str, _offset: u64, _size: u64) -> Option<Vec<u8>> {
    None
}

/// 자리표시 한 칸에서 읽은 것.
#[derive(Clone, Copy, PartialEq)]
struct Mark {
    id_low: u32,
    placement: u32,
    row: u32,
    col: u32,
    msb: u32,
}

fn color_id(color: Option<VtColor>) -> u32 {
    match color {
        Some(VtColor::Indexed(i)) => i as u32,
        Some(VtColor::Spec(rgb)) => (rgb.r as u32) << 16 | (rgb.g as u32) << 8 | rgb.b as u32,
        _ => 0,
    }
}

/// 빠진 결합 문자는 왼쪽 칸에서 잇는다(kitty 규칙 셋).
fn read_mark(cell: &TermCell, left: Option<Mark>) -> Mark {
    let id_low = color_id(Some(cell.fg));
    let placement = color_id(cell.underline_color());
    let zw = cell.zerowidth().unwrap_or(&[]);
    let d = |i: usize| zw.get(i).and_then(|c| diacritic_index(*c));
    let same = left.filter(|l| l.id_low == id_low && l.placement == placement);
    let (row, col, msb) = match (d(0), d(1), d(2)) {
        (None, _, _) => match same {
            Some(l) => (l.row, l.col + 1, l.msb),
            None => (0, 0, 0),
        },
        (Some(r), None, _) => match same.filter(|l| l.row == r) {
            Some(l) => (r, l.col + 1, l.msb),
            None => (r, 0, 0),
        },
        (Some(r), Some(c), None) => match same.filter(|l| l.row == r && l.col + 1 == c) {
            Some(l) => (r, c, l.msb),
            None => (r, c, 0),
        },
        (Some(r), Some(c), Some(m)) => (r, c, m),
    };
    Mark { id_low, placement, row, col, msb }
}

/// 이번 뷰포트에서 자리표시가 덮은 칸마다 그림 조각을 싣는다.
///
/// 한 줄 안에서 같은 그림·같은 그림 줄·이어지는 열을 한 조각으로 묶고, 바로
/// 윗줄 조각과 상자·열 범위가 같으면 세로로 합친다(4칸×2줄 얼굴이 한 장).
/// 자리표시는 글자 그대로 grid 에 있으니 스크롤·리플로우·대체 화면은 따로
/// 셈할 것이 없다.
pub(crate) fn placeholder_views<T: EventListener>(
    term: &Term<T>,
    store: &KittyStore,
    display_offset: usize,
    out: &mut Vec<InlineImageView>,
) {
    if !store.placements.iter().any(|p| p.virtual_) {
        return;
    }
    let grid = term.grid();
    let (rows, cols) = (grid.screen_lines(), grid.columns());
    let first = out.len();
    for vr in 0..rows {
        let line = Line(vr as i32 - display_offset as i32);
        let mut left: Option<Mark> = None;
        // (그림 id, 놓기 id, 그림 줄, 시작 화면 열, 시작 그림 열, 길이)
        let mut run: Option<(u32, u32, u32, usize, u32, usize)> = None;
        for c in 0..=cols {
            let mark = (c < cols)
                .then(|| &grid[line][Column(c)])
                .filter(|cell| cell.c == PLACEHOLDER)
                .map(|cell| read_mark(cell, left));
            left = mark;
            let mark = mark.map(|m| (m.msb << 24 | m.id_low, m));
            if let (Some(r), Some((id, m))) = (run.as_mut(), mark) {
                if r.0 == id && r.1 == m.placement && r.2 == m.row && r.4 + r.5 as u32 == m.col {
                    r.5 += 1;
                    continue;
                }
            }
            if let Some(r) = run.take() {
                push_run(store, vr as i32, r, out, first);
            }
            run = mark.map(|(id, m)| (id, m.placement, m.row, c, m.col, 1));
        }
    }
}

fn push_run(
    store: &KittyStore,
    vr: i32,
    (id, pid, img_row, col, img_col, len): (u32, u32, u32, usize, u32, usize),
    out: &mut Vec<InlineImageView>,
    first: usize,
) {
    let Some(im) = store.image(id) else { return };
    // 놓기 id 가 맞는 것을 먼저, 없으면 그 그림의 아무 자리표시 놓기(id 0 의 뜻).
    let virtuals = || store.placements.iter().filter(move |p| p.virtual_ && p.image == id);
    let Some(p) = virtuals().find(|p| p.id == pid).or_else(|| virtuals().next()) else { return };
    let Ok(box_col) = u16::try_from(col as i64 - img_col as i64) else { return };
    let view = InlineImageView {
        id: im.uid,
        path: im.path.display().to_string(),
        row: vr - img_row as i32,
        col: box_col,
        cols: p.cols,
        rows: p.rows,
        clip: Some(CellClip { row: vr, col: col as u16, cols: len as u16, rows: 1 }),
    };
    if let Some(prev) = out[first..].iter_mut().rev().find(|v| {
        v.id == view.id
            && v.row == view.row
            && v.col == view.col
            && v.clip.is_some_and(|c| c.col == col as u16 && c.cols == len as u16 && c.row + c.rows as i32 == vr)
    }) {
        if let Some(c) = prev.clip.as_mut() {
            c.rows += 1;
        }
        return;
    }
    out.push(view);
}

/// 커서 자리에 놓은 그림들의 이번 뷰포트 배치.
pub(crate) fn anchored_views(store: &KittyStore, top_abs: i64, rows: u16, out: &mut Vec<InlineImageView>) {
    for p in store.placements.iter().filter(|p| !p.virtual_) {
        let Some(im) = store.image(p.image) else { continue };
        let row = p.abs_line - top_abs;
        if row + p.rows as i64 <= 0 || row >= rows as i64 {
            continue;
        }
        out.push(InlineImageView {
            id: im.uid,
            path: im.path.display().to_string(),
            row: row as i32,
            col: p.col,
            cols: p.cols,
            rows: p.rows,
            clip: Some(CellClip { row: row as i32, col: p.col, cols: p.cols, rows: p.rows }),
        });
    }
}

#[cfg(test)]
pub(crate) fn diacritic(n: u32) -> char {
    char::from_u32(DIACRITICS[n as usize]).unwrap()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn b64(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in bytes.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() {
                    out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 40 % 256) as u8, (y * 40 % 256) as u8, 200, 255]));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    fn env() -> Env {
        Env { local_media: true, grid_cols: 80, hist: 0 }
    }

    fn run(store: &mut KittyStore, apc: &str) -> Outcome {
        let mut uid = 1;
        store.handle(apc.as_bytes(), Some(At { abs_line: 0, col: 0 }), &env(), &mut uid)
    }

    fn reply_text(o: &Outcome) -> Option<String> {
        o.reply.as_ref().map(|r| String::from_utf8_lossy(r).into_owned())
    }

    #[test]
    fn parses_control_keys() {
        let c = Command::parse(b"a=T,U=1,q=2,f=100,i=33554474,c=4,r=2,t=f,m=1,z=-5");
        assert_eq!(c.action, Some(b'T'));
        assert!(c.virtual_ && c.more);
        assert_eq!((c.quiet, c.format, c.id, c.cols, c.rows, c.z), (2, 100, 33554474, 4, 2, -5));
        assert_eq!(c.medium, Some(b'f'));
    }

    /// Claude Code·`kitten icat` 이 보내는 지원 질의 — 1×1 RGB 를 받고 OK 로만 답한다.
    #[test]
    fn query_answers_ok_without_storing() {
        let mut s = KittyStore::default();
        let o = run(&mut s, "i=31,s=1,v=1,a=q,t=d,f=24;AAAA");
        assert_eq!(reply_text(&o).as_deref(), Some("\x1b_Gi=31;OK\x1b\\"));
        assert!(s.images.is_empty());
    }

    #[test]
    fn query_reports_errors_and_q2_silences_them() {
        let mut s = KittyStore::default();
        let o = run(&mut s, "i=9,a=q,t=d,f=100;AAAA");
        assert!(reply_text(&o).unwrap().starts_with("\x1b_Gi=9;EBADPNG"));
        assert_eq!(run(&mut s, "i=9,a=q,q=2,t=d,f=100;AAAA").reply, None);
        // id 도 번호도 없으면 kitty 는 답하지 않는다.
        assert_eq!(run(&mut s, "a=q,t=d,f=24,s=1,v=1;AAAA").reply, None);
    }

    #[test]
    fn chunked_png_lands_once_complete() {
        let mut s = KittyStore::default();
        let data = b64(&png(3, 2));
        let (a, b) = data.split_at(8);
        let mut uid = 1;
        let e = env();
        let first = format!("a=t,f=100,i=5,m=1;{a}");
        assert_eq!(s.handle(first.as_bytes(), None, &e, &mut uid), Outcome::default());
        assert!(s.images.is_empty(), "조각이 다 오기 전엔 그림이 아니다");
        let last = format!("m=0;{b}");
        let o = s.handle(last.as_bytes(), None, &e, &mut uid);
        assert_eq!(reply_text(&o).as_deref(), Some("\x1b_Gi=5;OK\x1b\\"));
        let im = s.image(5).unwrap();
        assert_eq!((im.width, im.height, im.pixels), (3, 2, 6));
        assert_eq!(std::fs::read(&im.path).unwrap(), png(3, 2));
        s.clear();
    }

    #[test]
    fn raw_pixels_become_a_decodable_png() {
        let mut s = KittyStore::default();
        let rgba: Vec<u8> = (0..2 * 2 * 4).map(|i| i as u8 * 9).collect();
        let o = run(&mut s, &format!("a=t,f=32,s=2,v=2,i=1;{}", b64(&rgba)));
        assert!(reply_text(&o).unwrap().ends_with(";OK\x1b\\"));
        let back = image::open(&s.image(1).unwrap().path).unwrap().to_rgba8();
        assert_eq!(back.into_raw(), rgba);
        // 모자라는 날 픽셀은 거절한다.
        let short = run(&mut s, &format!("a=t,f=24,s=4,v=4,i=2;{}", b64(&[0; 12])));
        assert!(reply_text(&short).unwrap().contains("ENODATA"));
        s.clear();
    }

    #[test]
    fn zlib_payload_is_inflated() {
        let mut s = KittyStore::default();
        let rgb = vec![7u8; 4 * 4 * 3];
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&rgb, 6);
        let o = run(&mut s, &format!("a=t,f=24,s=4,v=4,o=z,i=3;{}", b64(&z)));
        assert!(reply_text(&o).unwrap().ends_with(";OK\x1b\\"), "{:?}", reply_text(&o));
        s.clear();
    }

    #[test]
    fn oversized_dimensions_are_refused() {
        let mut s = KittyStore::default();
        let o = run(&mut s, "a=t,f=32,s=20000,v=1,i=4;AAAA");
        assert!(reply_text(&o).unwrap().contains("EFBIG"));
    }

    fn file_cmd(medium: char, path: &str, id: u32) -> String {
        format!("a=t,t={medium},f=100,i={id};{}", b64(path.as_bytes()))
    }

    #[cfg(unix)]
    #[test]
    fn file_medium_reads_regular_files_only() {
        let dir = std::env::temp_dir().join(format!("kasaterm-kitty-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("face.png");
        std::fs::write(&good, png(2, 2)).unwrap();
        let mut s = KittyStore::default();
        let ok = run(&mut s, &file_cmd('f', good.to_str().unwrap(), 1));
        assert!(reply_text(&ok).unwrap().ends_with(";OK\x1b\\"));
        assert!(good.exists(), "t=f 는 파일을 그대로 둔다");
        for bad in ["/dev/zero", "/dev/null", "/proc/self/environ", "/etc", "relative.png", ""] {
            let o = run(&mut s, &file_cmd('f', bad, 2));
            assert_eq!(reply_text(&o).as_deref(), Some("\x1b_Gi=2;EBADF:Failed to read image file\x1b\\"), "{bad}");
        }
        // 심볼릭 링크로 장치를 가리켜도 실제 경로에서 걸린다.
        let link = dir.join("sneaky.png");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
        let o = run(&mut s, &file_cmd('f', link.to_str().unwrap(), 3));
        assert!(reply_text(&o).unwrap().contains("EBADF"));
        // 원격 거울(남의 기기 경로)은 파일을 아예 안 읽는다.
        let mut uid = 9;
        let remote = Env { local_media: false, ..env() };
        let o = s.handle(file_cmd('f', good.to_str().unwrap(), 4).as_bytes(), None, &remote, &mut uid);
        assert!(reply_text(&o).unwrap().contains("EBADF"));
        s.clear();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn temp_file_is_deleted_only_with_the_marker_in_a_temp_dir() {
        let dir = std::env::temp_dir();
        let marked = dir.join(format!("tty-graphics-protocol-{}.png", std::process::id()));
        let plain = dir.join(format!("kasaterm-kitty-plain-{}.png", std::process::id()));
        std::fs::write(&marked, png(1, 1)).unwrap();
        std::fs::write(&plain, png(1, 1)).unwrap();
        let mut s = KittyStore::default();
        assert!(reply_text(&run(&mut s, &file_cmd('t', marked.to_str().unwrap(), 1))).unwrap().ends_with(";OK\x1b\\"));
        assert!(!marked.exists(), "표지 있는 임시 파일은 읽고 지운다");
        assert!(reply_text(&run(&mut s, &file_cmd('t', plain.to_str().unwrap(), 2))).unwrap().ends_with(";OK\x1b\\"));
        assert!(plain.exists(), "표지 없는 파일은 지우지 않는다");
        let _ = std::fs::remove_file(&plain);
        s.clear();
    }

    #[cfg(unix)]
    fn make_shm(name: &str, bytes: &[u8]) {
        let cname = std::ffi::CString::new(name).unwrap();
        unsafe {
            let fd = libc::shm_open(cname.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600 as libc::c_uint);
            assert!(fd >= 0);
            assert_eq!(libc::ftruncate(fd, bytes.len() as libc::off_t), 0);
            let p = libc::mmap(std::ptr::null_mut(), bytes.len(), libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
            libc::munmap(p, bytes.len());
            libc::close(fd);
        }
    }

    /// `kitten icat` 은 앞의 `/` 없이 만든 이름을 그대로 보낸다.
    #[cfg(unix)]
    #[test]
    fn shared_memory_name_without_slash() {
        let name = format!("kt-kitty-bare-{}", std::process::id());
        make_shm(&name, &png(1, 1));
        let mut s = KittyStore::default();
        let o = run(&mut s, &file_cmd('s', &name, 6));
        assert!(reply_text(&o).unwrap().ends_with(";OK\x1b\\"), "{:?}", reply_text(&o));
        s.clear();
    }

    #[cfg(unix)]
    #[test]
    fn shared_memory_is_read_then_unlinked() {
        let name = format!("/kt-kitty-{}", std::process::id());
        let bytes = png(2, 3);
        let cname = std::ffi::CString::new(name.clone()).unwrap();
        make_shm(&name, &bytes);
        let mut s = KittyStore::default();
        let o = run(&mut s, &file_cmd('s', &name, 6));
        assert!(reply_text(&o).unwrap().ends_with(";OK\x1b\\"), "{:?}", reply_text(&o));
        assert_eq!((s.image(6).unwrap().width, s.image(6).unwrap().height), (2, 3));
        let again = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDONLY, 0 as libc::c_uint) };
        assert!(again < 0, "읽은 공유 메모리는 지운다");
        s.clear();
    }

    #[test]
    fn virtual_put_needs_no_cursor_and_does_not_move_it() {
        let mut s = KittyStore::default();
        let mut uid = 1;
        let cmd = format!("a=T,U=1,q=2,f=100,i=7,c=4,r=2;{}", b64(&png(8, 8)));
        let o = s.handle(cmd.as_bytes(), None, &env(), &mut uid);
        assert_eq!(o, Outcome::default(), "q=2 는 OK 도 삼키고 자리표시 놓기는 커서를 안 옮긴다");
        assert_eq!(s.placements.len(), 1);
        // 같은 그림을 다시 보내면 놓기는 하나로 남고 그림만 갈린다.
        let first = s.image(7).unwrap().path.clone();
        s.handle(cmd.as_bytes(), None, &env(), &mut uid);
        assert_eq!(s.placements.len(), 1);
        assert!(!first.exists(), "갈린 옛 파일은 지운다");
        s.clear();
    }

    #[test]
    fn cursor_put_moves_like_kitty() {
        let mut s = KittyStore::default();
        let cmd = format!("a=T,f=100,i=1,c=3,r=2;{}", b64(&png(4, 4)));
        let o = run(&mut s, &cmd);
        assert_eq!(o.cursor, Some((1, 3)), "오른쪽으로 폭, 아래로 높이-1");
        let mut uid = 5;
        let edge = s.handle(cmd.as_bytes(), Some(At { abs_line: 0, col: 78 }), &env(), &mut uid);
        assert_eq!(edge.cursor, Some((2, 0)), "오른쪽 끝을 넘으면 다음 줄 첫 칸");
        let still = s.handle(format!("a=p,i=1,C=1,p=4").as_bytes(), Some(At { abs_line: 5, col: 0 }), &env(), &mut uid);
        assert_eq!(still.cursor, None);
        s.clear();
    }

    #[test]
    fn delete_by_id_frees_data_only_in_uppercase() {
        let mut s = KittyStore::default();
        run(&mut s, &format!("a=T,U=1,f=100,i=7,c=4,r=2,q=2;{}", b64(&png(2, 2))));
        let path = s.image(7).unwrap().path.clone();
        run(&mut s, "a=d,d=i,i=7");
        assert!(s.placements.is_empty() && s.image(7).is_some());
        run(&mut s, "a=d,d=I,i=7");
        assert!(s.image(7).is_none() && !path.exists());
    }

    /// 받던 조각을 버리고 새로 보낸 명령은 앞 조각에 붙지 않는다.
    #[test]
    fn a_new_command_abandons_a_half_upload() {
        let mut s = KittyStore::default();
        run(&mut s, "a=t,f=100,i=5,m=1;AAAA");
        let o = run(&mut s, &format!("a=t,f=100,i=6;{}", b64(&png(1, 1))));
        assert!(reply_text(&o).unwrap().starts_with("\x1b_Gi=6;OK"));
        assert!(s.image(5).is_none() && s.image(6).is_some());
        s.clear();
    }

    #[test]
    fn delete_all_spares_virtual_placements() {
        let mut s = KittyStore::default();
        run(&mut s, &format!("a=T,U=1,f=100,i=7,c=4,r=2,q=2;{}", b64(&png(2, 2))));
        run(&mut s, &format!("a=T,f=100,i=8,c=2,r=1,q=2;{}", b64(&png(2, 2))));
        run(&mut s, "a=d");
        assert_eq!(s.placements.len(), 1);
        assert!(s.placements[0].virtual_);
        s.clear();
    }

    #[test]
    fn box_follows_aspect_when_one_side_is_missing() {
        set_cell_pixels(10, 20);
        let c = Command { cols: 4, ..Default::default() };
        assert_eq!(box_cells(&c, 100, 100), (4, 2));
        let r = Command { rows: 3, ..Default::default() };
        assert_eq!(box_cells(&r, 100, 100), (6, 3));
        assert_eq!(box_cells(&Command::default(), 25, 41), (3, 3));
    }

    #[test]
    fn diacritics_are_numbered_in_order() {
        assert_eq!(diacritic_index('\u{0305}'), Some(0));
        assert_eq!(diacritic_index('\u{030D}'), Some(1));
        assert_eq!(diacritic_index('\u{030E}'), Some(2));
        assert_eq!(diacritic_index('a'), None);
    }
}
