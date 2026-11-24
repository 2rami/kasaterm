use super::*;

/// 칼럼이 이 프레임에 읽는 값. `g` 가 `self.gpu` 를 잡은 안쪽에서 부르므로
/// `&self` 로 구해야 하는 것은 부르는 쪽이 미리 풀어 넘긴다.
pub(super) struct Frame<'a> {
    pub x: f32,
    pub w: f32,
    pub bg: [u8; 4],
    pub viewport: (f32, f32),
    pub status_h: f32,
    pub git: crate::git_panel::paint::Frame<'a>,
}

pub(super) fn paint(g: &mut gpu::GpuRenderer, git: &mut state::GitState, info: &mut state::InfoState, f: &Frame<'_>) {
    // 상태줄은 늘 있으므로 조건 없이 함께 뺀다 — 안 빼면 칼럼 바닥이 그 띠 뒤로 들어가
    // 가려진다. 시저는 `push_clip` 을 세운 자리에만 걸리는데 여기는 그 바깥이라, 자리를
    // 미리 빼 두는 이 계산이 정본이다.
    let top = TITLE_HEIGHT;
    let bottom = (f.viewport.1 - f.status_h).max(top);
    g.rect(f.x, top, f.w, bottom - top, f.bg);
    g.rect(f.x, top, 1.0, bottom - top, theme::border());
    let body_top = info::draw_side_head(g, f.git.cursor, info, git, f.x, f.w, top);
    info::draw_info_col(g, info, git, &f.git, f.x, f.w, body_top, bottom);
    // 「—」 의 까닭·잘린 값의 전문 — 표를 다 그린 뒤 커서 아래 것 하나만.
    let (cx, cy) = f.git.cursor;
    if info.pane_menu.is_none() && !git.panel_menu_open() {
        if let Some((tip, r)) = info.tip_rects.iter()
            .find(|(_, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
        {
            App::draw_hover_tip(g, tip, r.0, r.1 + r.3, f.viewport.0, f.viewport.1);
        } else {
            crate::info_prompts::draw_card(g, f.x, (f.viewport.0, bottom));
        }
    }
}
