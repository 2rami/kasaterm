//! 첫 실행 안내가 네이티브 설정 방으로 들어오는 최소 경계.
//!
//! 터미널 프로필 가져오기와 로그인 단계 전체는 다음 이행에서 붙인다. 이 단계는
//! 첫 실행도 Wry 화면으로 빠지지 않고 설정 방 안에서 현재 상태와 끝내기 손잡이를
//! 갖게 한다.

use super::*;

pub(crate) type Rect = (f32, f32, f32, f32);

/// 반환값은 「기본값으로 시작」 버튼의 히트 영역.
pub(crate) fn paint(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    w: f32,
    cursor: (f32, f32),
) -> (f32, Rect) {
    let h = 94.0;
    round_rect(g, x, y, w, h, theme::radius_md(), theme::surface());
    g.rect(x, y, 3.0, h, theme::accent());
    g.draw_text(
        x + 20.0,
        y + 17.0,
        "처음 설정",
        gpu::DrawOpts {
            font_size: 16.0,
            color: theme::text(),
            bold: true,
            italic: false,
        },
    );
    g.draw_text(
        x + 20.0,
        y + 45.0,
        "필요한 값은 아래에서 바로 고를 수 있어요. 나머지는 기본값으로 시작합니다.",
        gpu::DrawOpts {
            font_size: 12.0,
            color: theme::text_dim(),
            bold: false,
            italic: false,
        },
    );
    let bw = 124.0;
    let r = (x + w - bw - 16.0, y + 15.0, bw, 34.0);
    let hover = contains(r, cursor);
    g.hover_pointer |= hover;
    round_rect(
        g,
        r.0,
        r.1,
        r.2,
        r.3,
        theme::radius_md(),
        if hover {
            theme::surface_active()
        } else {
            theme::surface_hover()
        },
    );
    g.draw_text(
        r.0 + 15.0,
        r.1 + 9.0,
        "기본값으로 시작",
        gpu::DrawOpts {
            font_size: 12.0,
            color: theme::text(),
            bold: true,
            italic: false,
        },
    );
    (h, r)
}

fn contains(r: Rect, p: (f32, f32)) -> bool {
    p.0 >= r.0 && p.0 <= r.0 + r.2 && p.1 >= r.1 && p.1 <= r.1 + r.3
}
