//! BSP layout tree for multi-pane PTY mode.
//!
//! tmux owns its layout server-side and broadcasts it to us as a
//! `%layout-change` event. Without tmux we run our own tree: a recursive
//! BSP where every internal node is a horizontal or vertical split with
//! a ratio, and every leaf carries a pane id.
//!
//! The renderer consumes `kasa_bridge::layout::Layout`, so we expose
//! `to_tmux_layout()` that walks our tree and emits the same shape with
//! pane rectangles already laid out for the given window size. Pane ids
//! are formatted "%N" by convention; conversion strips the prefix to
//! the u32 that tmux's Layout expects.
//!
//! Naming note: `Horizontal` here matches tmux — children sit side by
//! side with a *vertical* divider between them. That is the layout
//! Terminal.app calls "Cmd+D / vertical split". `Vertical` stacks
//! children with a horizontal divider, matching Cmd+Shift+D.

use kasa_bridge::layout::Layout;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SplitDir {
    /// Children laid out left-to-right (vertical divider line).
    Horizontal,
    /// Children laid out top-to-bottom (horizontal divider line).
    Vertical,
}

impl SplitDir {
    pub fn opposite(self) -> Self {
        match self {
            SplitDir::Horizontal => SplitDir::Vertical,
            SplitDir::Vertical => SplitDir::Horizontal,
        }
    }
}

/// 학생 여럿을 **한 번에** 앉히는 배치 — 호스트(부른 pane)가 크게 남고 학생들이
/// 반대 축으로 균등하게 서는 모양.
///
/// ```text
/// dir = Horizontal, host_ratio = 0.6
/// ┌────────────────┬────────┐
/// │                │  학생1  │
/// │   호스트         ├────────┤
/// │                │  학생2  │
/// │                ├────────┤
/// │                │  학생3  │
/// └────────────────┴────────┘
/// ```
///
/// **호스트를 루트 직계 자식에 두는 게 요점이다.** `set_leaf_ratio` 는 직계 부모의
/// ratio 만 바꾸므로(그 함수 주석 참고), 호스트가 더 깊이 있으면 「부른 쪽을 크게」가
/// 루트 하나로 안 잡힌다.
///
/// 그리고 학생 쪽은 **균형 이진트리가 아니라 「남은 개수의 역수」 사슬**이다. 0.5 로
/// 반씩 가르는 균형트리는 N 이 2의 거듭제곱일 때만 균등해서, N=3 이면 1/2·1/4·1/4 로
/// 어긋난다. 사슬은 모든 N 에서 정확히 균등하고, leaf 순서가 화면 순서(위→아래 또는
/// 왼→오른쪽)와 같아 `Cmd+[`/`Cmd+]` 순환이 눈에 보이는 순서를 따른다.
///
/// 학생이 없으면 호스트 혼자인 트리다(쪼갤 이유가 없다).
pub fn fleet(host: &str, students: &[String], dir: SplitDir, host_ratio: f32) -> PtyLayout {
    let Some((first, rest)) = students.split_first() else {
        return PtyLayout::single(host);
    };
    PtyLayout::Split {
        dir,
        ratio: host_ratio.clamp(0.1, 0.9),
        a: Box::new(PtyLayout::single(host)),
        b: Box::new(even_chain(first, rest, dir.opposite())),
    }
}

/// 한 축을 정확히 균등하게 나누는 사슬. 첫 조각이 `1/남은개수` 를 갖고 나머지가
/// 그 뒤를 같은 규칙으로 나눈다.
fn even_chain(first: &String, rest: &[String], dir: SplitDir) -> PtyLayout {
    let Some((next, tail)) = rest.split_first() else {
        return PtyLayout::single(first.clone());
    };
    PtyLayout::Split {
        dir,
        ratio: 1.0 / (rest.len() + 1) as f32,
        a: Box::new(PtyLayout::single(first.clone())),
        b: Box::new(even_chain(next, tail, dir)),
    }
}

/// split 하나가 부모 길이 `total` 을 두 조각으로 가르는 **정본 산술**. 트리를
/// 그리는 곳(`walk_rects`·`build_layout`·`collect_dividers`·`resize_at`)과 용량을
/// 재는 곳이 전부 이걸 쓴다.
///
/// 하나로 모은 이유가 있다. 처음엔 용량 쪽만 `total * (1 - ratio)` 로 따로 셌는데,
/// f32 에서 `1 - 0.6 = 0.39999998` 이라 200칸이 **79** 로 나왔다 — 그리는 쪽은 같은
/// 창에서 80 을 준다. 그 한 칸 차이로 「한 명도 못 앉힌다」고 답했다. 재는 산술과
/// 그리는 산술이 갈리면 용량은 언제든 조용히 거짓말한다.
///
/// 조각 사이에 칸 하나를 남기지 않는다 — `a` 를 `total-1` 로 묶어 두므로 `b` 는
/// 최소 한 칸을 갖고, 렌더러가 그 자리에 구분선을 그린다.
fn split_extent(total: u16, ratio: f32) -> (u16, u16) {
    let a = ((total as f32) * ratio).round() as u16;
    let a = a.min(total.saturating_sub(1)).max(1);
    (a, total.saturating_sub(a))
}

/// `fleet` 로 앉힐 수 있는 **최대 인원**. 넘겨서 앉히면 쓸 수 없는 크기가 되므로
/// 부르는 쪽이 이걸로 자르고, 남는 인원은 탭으로 보내거나 사람에게 알려야 한다.
///
/// 지금 split 경로엔 이 가드가 없어서 아무리 좁아도 계속 쪼갠다 — 그래서 셋을
/// 부르면 마지막 학생이 글자 몇 줄만 남는다. 0 이 나오면 **한 명도 못 앉힌다**는
/// 뜻이다(창이 이미 너무 작다).
///
/// 학생들이 나눠 갖는 축만 인원으로 갈리고, 나머지 축은 다 같이 쓰므로 인원과
/// 무관하다 — 그 축이 이미 하한 밑이면 몇 명이든 안 된다.
pub fn fleet_capacity(
    dir: SplitDir,
    host_ratio: f32,
    cols: u16,
    rows: u16,
    min_cols: u16,
    min_rows: u16,
) -> usize {
    let r = host_ratio.clamp(0.1, 0.9);
    match dir {
        SplitDir::Horizontal => {
            // 학생 열은 폭을 호스트와 나눠 갖고(인원 무관), 높이를 인원끼리 나눈다.
            let (_, w) = split_extent(cols, r);
            if w < min_cols {
                return 0;
            }
            (rows / min_rows.max(1)) as usize
        }
        SplitDir::Vertical => {
            let (_, h) = split_extent(rows, r);
            if h < min_rows {
                return 0;
            }
            (cols / min_cols.max(1)) as usize
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PtyLayout {
    Leaf {
        pane_id: String,
    },
    Split {
        dir: SplitDir,
        /// Fraction of the parent's extent occupied by child `a`.
        /// `b` takes `1.0 - ratio`. New splits start at 0.5.
        ratio: f32,
        a: Box<PtyLayout>,
        b: Box<PtyLayout>,
    },
}

impl PtyLayout {
    pub fn single(pane_id: impl Into<String>) -> Self {
        PtyLayout::Leaf { pane_id: pane_id.into() }
    }

    /// Leaf pane ids in document order — same order tmux uses for
    /// `Cmd+[` / `Cmd+]` focus cycling.
    pub fn leaves(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            PtyLayout::Leaf { pane_id } => out.push(pane_id.as_str()),
            PtyLayout::Split { a, b, .. } => {
                a.collect_leaves(out);
                b.collect_leaves(out);
            }
        }
    }

    /// Replace the leaf with `target` pane id by a Split node containing
    /// the original leaf as `a` and a new leaf with `new_pane` as `b`.
    /// Returns true on success. New splits use a 0.5 ratio so panes
    /// start out equal.
    pub fn split_leaf(&mut self, target: &str, dir: SplitDir, new_pane: String) -> bool {
        match self {
            PtyLayout::Leaf { pane_id } if pane_id == target => {
                let a = PtyLayout::Leaf { pane_id: pane_id.clone() };
                let b = PtyLayout::Leaf { pane_id: new_pane };
                *self = PtyLayout::Split {
                    dir,
                    ratio: 0.5,
                    a: Box::new(a),
                    b: Box::new(b),
                };
                true
            }
            PtyLayout::Leaf { .. } => false,
            PtyLayout::Split { a, b, .. } => {
                a.split_leaf(target, dir, new_pane.clone()) || b.split_leaf(target, dir, new_pane)
            }
        }
    }

    /// True when two trees have the same shape — identical leaf ids in the same
    /// positions AND the same split directions — IGNORING split ratios. Lets the
    /// GUI skip a full re-adopt on a cwd-only poll (shape unchanged) while still
    /// catching a move/swap/split (shape changed). A plain leaves-Vec compare
    /// misses a move that keeps the same leaves but flips a split's direction
    /// (좌우 → 상하) — exactly drag-relocation. Ratios are excluded so a
    /// divider's drag-set ratio (the daemon doesn't store it) isn't a change.
    pub fn same_shape(&self, other: &PtyLayout) -> bool {
        match (self, other) {
            (PtyLayout::Leaf { pane_id: a }, PtyLayout::Leaf { pane_id: b }) => a == b,
            (
                PtyLayout::Split { dir: d1, a: a1, b: b1, .. },
                PtyLayout::Split { dir: d2, a: a2, b: b2, .. },
            ) => d1 == d2 && a1.same_shape(a2) && b1.same_shape(b2),
            _ => false,
        }
    }

    /// Remove the leaf with `target` pane id. Its sibling absorbs the
    /// space (the parent Split collapses into the surviving child).
    /// Returns true if removed. Removing the last leaf returns false —
    /// the caller should treat that as "close the window".
    pub fn remove_leaf(&mut self, target: &str) -> bool {
        // The replacement happens one level above the matching leaf, so
        // we recurse looking for a Split whose child is the target.
        match self {
            PtyLayout::Leaf { .. } => false, // root-level removal handled by caller
            PtyLayout::Split { a, b, .. } => {
                if let PtyLayout::Leaf { pane_id } = a.as_ref() {
                    if pane_id == target {
                        let survivor = std::mem::replace(
                            b.as_mut(),
                            PtyLayout::Leaf { pane_id: String::new() },
                        );
                        *self = survivor;
                        return true;
                    }
                }
                if let PtyLayout::Leaf { pane_id } = b.as_ref() {
                    if pane_id == target {
                        let survivor = std::mem::replace(
                            a.as_mut(),
                            PtyLayout::Leaf { pane_id: String::new() },
                        );
                        *self = survivor;
                        return true;
                    }
                }
                a.remove_leaf(target) || b.remove_leaf(target)
            }
        }
    }

    /// Swap the tree positions of two leaves by exchanging their pane
    /// ids in place. The renderer and PTY maps key off the id, so this
    /// moves pane A into B's slot and vice versa without touching the
    /// PTYs themselves. Returns false if either id is missing or equal.
    pub fn swap_leaves(&mut self, a: &str, b: &str) -> bool {
        if a == b {
            return false;
        }
        let leaves = self.leaves();
        if !leaves.iter().any(|&l| l == a) || !leaves.iter().any(|&l| l == b) {
            return false;
        }
        self.swap_ids(a, b);
        true
    }

    /// Set the split ratio of `pane_id`'s *immediate parent* so the pane
    /// takes `frac` of that container (its sibling subtree gets `1 - frac`).
    /// The orchestration knob behind `kasaterm-cli resize` — "make the orchestrator
    /// pane big" after a fleet regroup. Deliberately the direct container
    /// only, not the pane's share of the whole window: nested splits keep
    /// their own ratios. Returns false when the pane isn't a direct child
    /// of any split (single-pane window or id missing).
    pub fn set_leaf_ratio(&mut self, pane_id: &str, frac: f32) -> bool {
        match self {
            PtyLayout::Leaf { .. } => false,
            PtyLayout::Split { ratio, a, b, .. } => {
                if matches!(a.as_ref(), PtyLayout::Leaf { pane_id: p } if p == pane_id) {
                    *ratio = frac;
                    return true;
                }
                if matches!(b.as_ref(), PtyLayout::Leaf { pane_id: p } if p == pane_id) {
                    *ratio = 1.0 - frac;
                    return true;
                }
                a.set_leaf_ratio(pane_id, frac) || b.set_leaf_ratio(pane_id, frac)
            }
        }
    }

    fn swap_ids(&mut self, a: &str, b: &str) {
        match self {
            PtyLayout::Leaf { pane_id } => {
                if pane_id == a {
                    *pane_id = b.to_string();
                } else if pane_id == b {
                    *pane_id = a.to_string();
                }
            }
            PtyLayout::Split { a: ca, b: cb, .. } => {
                ca.swap_ids(a, b);
                cb.swap_ids(a, b);
            }
        }
    }

    /// Insert `moving` as a new sibling of the `target` leaf, splitting
    /// along `dir`. `before` places the moving pane on the left/top (child
    /// `a`); otherwise it lands on the right/bottom (child `b`). Used by
    /// drag-and-drop relocation: detach the moving leaf first (its PTY
    /// stays alive), then re-attach it beside the drop target. Returns
    /// true when the target leaf was found.
    pub fn insert_beside(
        &mut self,
        target: &str,
        dir: SplitDir,
        before: bool,
        moving: String,
    ) -> bool {
        match self {
            PtyLayout::Leaf { pane_id } if pane_id == target => {
                let target_leaf = PtyLayout::Leaf {
                    pane_id: pane_id.clone(),
                };
                let moving_leaf = PtyLayout::Leaf { pane_id: moving };
                let (a, b) = if before {
                    (moving_leaf, target_leaf)
                } else {
                    (target_leaf, moving_leaf)
                };
                *self = PtyLayout::Split {
                    dir,
                    ratio: 0.5,
                    a: Box::new(a),
                    b: Box::new(b),
                };
                true
            }
            PtyLayout::Leaf { .. } => false,
            PtyLayout::Split { a, b, .. } => {
                a.insert_beside(target, dir, before, moving.clone())
                    || b.insert_beside(target, dir, before, moving)
            }
        }
    }

    /// Walks the tree and produces a list of leaf rectangles. Each entry
    /// is `(pane_id, x, y, w, h)` in cell coordinates. Used by the
    /// resize path to SIGWINCH each PTY to its share.
    pub fn leaf_rects(&self, total_w: u16, total_h: u16) -> Vec<(String, u16, u16, u16, u16)> {
        let mut out = Vec::new();
        self.walk_rects(0, 0, total_w, total_h, &mut out);
        out
    }

    fn walk_rects(
        &self,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        out: &mut Vec<(String, u16, u16, u16, u16)>,
    ) {
        match self {
            PtyLayout::Leaf { pane_id } => {
                out.push((pane_id.clone(), x, y, w, h));
            }
            PtyLayout::Split { dir, ratio, a, b } => match dir {
                SplitDir::Horizontal => {
                    let (aw, bw) = split_extent(w, *ratio);
                    a.walk_rects(x, y, aw, h, out);
                    b.walk_rects(x + aw, y, bw, h, out);
                }
                SplitDir::Vertical => {
                    let (ah, bh) = split_extent(h, *ratio);
                    a.walk_rects(x, y, w, ah, out);
                    b.walk_rects(x, y + ah, w, bh, out);
                }
            },
        }
    }

    /// Convert into the tmux Layout shape the renderer already knows
    /// how to consume. Pane ids must be formatted "%N" — the numeric
    /// part feeds the `u32` slot in `Layout::Pane`.
    pub fn to_tmux_layout(&self, total_w: u16, total_h: u16) -> Layout {
        self.build_layout(0, 0, total_w, total_h)
    }

    fn build_layout(&self, x: u16, y: u16, w: u16, h: u16) -> Layout {
        match self {
            PtyLayout::Leaf { pane_id } => {
                let id = pane_id
                    .strip_prefix('%')
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                Layout::Pane { id, w, h, x, y }
            }
            PtyLayout::Split { dir, ratio, a, b } => match dir {
                SplitDir::Horizontal => {
                    let (aw, bw) = split_extent(w, *ratio);
                    let children = vec![
                        a.build_layout(x, y, aw, h),
                        b.build_layout(x + aw, y, bw, h),
                    ];
                    Layout::HSplit { w, h, x, y, children }
                }
                SplitDir::Vertical => {
                    let (ah, bh) = split_extent(h, *ratio);
                    let children = vec![
                        a.build_layout(x, y, w, ah),
                        b.build_layout(x, y + ah, w, bh),
                    ];
                    Layout::VSplit { w, h, x, y, children }
                }
            },
        }
    }

    /// Index of `target` in `leaves()` order, for focus cycling.
    pub fn index_of(&self, target: &str) -> Option<usize> {
        self.leaves().iter().position(|id| *id == target)
    }

    /// Every split seam in the tree, laid out for the given window size.
    /// The renderer/mouse layer uses these to hit-test "is the cursor on
    /// a divider?" and which split's `ratio` a drag should move.
    pub fn dividers(&self, total_w: u16, total_h: u16) -> Vec<Divider> {
        let mut out = Vec::new();
        let mut path = Vec::new();
        self.collect_dividers(&mut path, 0, 0, total_w, total_h, &mut out);
        out
    }

    fn collect_dividers(
        &self,
        path: &mut Vec<u8>,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        out: &mut Vec<Divider>,
    ) {
        if let PtyLayout::Split { dir, ratio, a, b } = self {
            match dir {
                SplitDir::Horizontal => {
                    let (aw, bw) = split_extent(w, *ratio);
                    out.push(Divider {
                        path: path.clone(),
                        dir: *dir,
                        edge: x + aw,
                        span_start: y,
                        span_len: h,
                    });
                    path.push(0);
                    a.collect_dividers(path, x, y, aw, h, out);
                    path.pop();
                    path.push(1);
                    b.collect_dividers(path, x + aw, y, bw, h, out);
                    path.pop();
                }
                SplitDir::Vertical => {
                    let (ah, bh) = split_extent(h, *ratio);
                    out.push(Divider {
                        path: path.clone(),
                        dir: *dir,
                        edge: y + ah,
                        span_start: x,
                        span_len: w,
                    });
                    path.push(0);
                    a.collect_dividers(path, x, y, w, ah, out);
                    path.pop();
                    path.push(1);
                    b.collect_dividers(path, x, y + ah, w, bh, out);
                    path.pop();
                }
            }
        }
    }

    /// Move the split identified by `path` so its seam lands at cell
    /// position `pos` (column for a Horizontal split, row for Vertical).
    /// The ratio is clamped to [0.1, 0.9] so no pane collapses to nothing.
    /// Returns true if the path resolved to a Split node.
    pub fn resize_divider(&mut self, path: &[u8], pos: u16, total_w: u16, total_h: u16) -> bool {
        self.resize_at(path, pos, 0, 0, total_w, total_h)
    }

    /// Read the split ratio at `path` (the seam the GUI just dragged). Used to
    /// forward a divider drag to the daemon as a resolution-independent ratio —
    /// the headless daemon has no pixel grid, so pos/cols would be meaningless.
    pub fn ratio_at(&self, path: &[u8]) -> Option<f32> {
        let PtyLayout::Split { ratio, a, b, .. } = self else {
            return None;
        };
        match path.split_first() {
            None => Some(*ratio),
            Some((head, tail)) => {
                if *head == 0 {
                    a.ratio_at(tail)
                } else {
                    b.ratio_at(tail)
                }
            }
        }
    }

    /// Set the split ratio at `path` directly (daemon side of a divider drag).
    /// Mirrors resize_at's 0.1..0.9 clamp so the seam can't collapse a pane.
    pub fn set_ratio_at(&mut self, path: &[u8], ratio: f32) -> bool {
        let PtyLayout::Split { ratio: r, a, b, .. } = self else {
            return false;
        };
        match path.split_first() {
            None => {
                *r = ratio.clamp(0.1, 0.9);
                true
            }
            Some((head, tail)) => {
                if *head == 0 {
                    a.set_ratio_at(tail, ratio)
                } else {
                    b.set_ratio_at(tail, ratio)
                }
            }
        }
    }

    /// Ctrl+드래그: `path`의 Horizontal split을 상하먼저로 재구조화해, 상하를
    /// 관통하던 세로선을 상/하 독립 경계로 쪼갠다. 대상 H split의 두 자식이 같은
    /// 비율(±0.02)의 Vertical split(=상단 정렬)일 때만 성공하고, 그 외엔 트리를
    /// 건드리지 않고 None(호출부가 일반 드래그로 폴백). 성공 시 새로 생긴 하단
    /// Horizontal split의 절대 path(= `path` + [1])를 돌려줘, 이어지는 드래그가
    /// 하단 경계만 움직이게 한다.
    ///
    /// ```text
    /// Split(H, r, Split(V,s,aT,aB), Split(V,s,bT,bB))
    ///   → Split(V, s, Split(H, r, aT, bT),   // 상단 좌우 (고정)
    ///                 Split(H, r, aB, bB))    // 하단 좌우 (독립)
    /// ```
    pub fn split_htov_at(&mut self, path: &[u8]) -> Option<Vec<u8>> {
        match path.split_first() {
            Some((head, tail)) => {
                let PtyLayout::Split { a, b, .. } = self else {
                    return None;
                };
                let child = if *head == 0 { a } else { b };
                let mut sub = child.split_htov_at(tail)?;
                sub.insert(0, *head);
                Some(sub)
            }
            None => {
                let PtyLayout::Split { dir: SplitDir::Horizontal, ratio, a, b } = self else {
                    return None;
                };
                let r = *ratio;
                let (sa, a_top, a_bot) = match a.as_ref() {
                    PtyLayout::Split { dir: SplitDir::Vertical, ratio, a, b } => {
                        (*ratio, a.clone(), b.clone())
                    }
                    _ => return None,
                };
                let (sb, b_top, b_bot) = match b.as_ref() {
                    PtyLayout::Split { dir: SplitDir::Vertical, ratio, a, b } => {
                        (*ratio, a.clone(), b.clone())
                    }
                    _ => return None,
                };
                if (sa - sb).abs() > 0.02 {
                    return None; // 상단 높이가 안 맞으면 재구조화 시 어긋난다
                }
                *self = PtyLayout::Split {
                    dir: SplitDir::Vertical,
                    ratio: sa,
                    a: Box::new(PtyLayout::Split {
                        dir: SplitDir::Horizontal,
                        ratio: r,
                        a: a_top,
                        b: b_top,
                    }),
                    b: Box::new(PtyLayout::Split {
                        dir: SplitDir::Horizontal,
                        ratio: r,
                        a: a_bot,
                        b: b_bot,
                    }),
                };
                Some(vec![1])
            }
        }
    }

    /// `split_htov_at` 의 역 — Ctrl+드래그로 상하 분리한 하단 세로선이 상단 세로선과
    /// 위치(ratio)가 맞춰지면 관통 세로선으로 재병합해 다시 같이 움직이게 한다(거노:
    /// 위에랑 맞춰지면 같이 움직이게). `path` = 분리된 상하 V split. 상단 H(a)·하단
    /// H(b)의 ratio 가 `snap` 이내면 관통 Split(H) 로 되돌리고 그 path(=path 자체) 반환;
    /// 아니면 트리 무변경 + None.
    /// ```text
    /// Split(V, s, Split(H,r1,aTL,aTR), Split(H,r2,bBL,bBR))   (r1≈r2)
    ///   → Split(H, r1, Split(V,s,aTL,bBL), Split(V,s,aTR,bBR))   관통 재병합
    /// ```
    pub fn merge_vtoh_at(&mut self, path: &[u8], snap: f32) -> Option<Vec<u8>> {
        match path.split_first() {
            Some((head, tail)) => {
                let PtyLayout::Split { a, b, .. } = self else {
                    return None;
                };
                let child = if *head == 0 { a } else { b };
                let mut sub = child.merge_vtoh_at(tail, snap)?;
                sub.insert(0, *head);
                Some(sub)
            }
            None => {
                let PtyLayout::Split { dir: SplitDir::Vertical, ratio: s, a, b } = self else {
                    return None;
                };
                let s = *s;
                let (r1, a_tl, a_tr) = match a.as_ref() {
                    PtyLayout::Split { dir: SplitDir::Horizontal, ratio, a, b } => {
                        (*ratio, a.clone(), b.clone())
                    }
                    _ => return None,
                };
                let (r2, b_bl, b_br) = match b.as_ref() {
                    PtyLayout::Split { dir: SplitDir::Horizontal, ratio, a, b } => {
                        (*ratio, a.clone(), b.clone())
                    }
                    _ => return None,
                };
                if (r1 - r2).abs() > snap {
                    return None; // 상/하 세로선이 아직 안 맞음 — 분리 유지
                }
                *self = PtyLayout::Split {
                    dir: SplitDir::Horizontal,
                    ratio: r1,
                    a: Box::new(PtyLayout::Split {
                        dir: SplitDir::Vertical,
                        ratio: s,
                        a: a_tl,
                        b: b_bl,
                    }),
                    b: Box::new(PtyLayout::Split {
                        dir: SplitDir::Vertical,
                        ratio: s,
                        a: a_tr,
                        b: b_br,
                    }),
                };
                Some(path.to_vec())
            }
        }
    }

    fn resize_at(&mut self, path: &[u8], pos: u16, x: u16, y: u16, w: u16, h: u16) -> bool {
        let PtyLayout::Split { dir, ratio, a, b } = self else {
            return false;
        };
        if path.is_empty() {
            let r = match dir {
                SplitDir::Horizontal => pos.saturating_sub(x) as f32 / w.max(1) as f32,
                SplitDir::Vertical => pos.saturating_sub(y) as f32 / h.max(1) as f32,
            };
            *ratio = r.clamp(0.1, 0.9);
            return true;
        }
        let (head, tail) = path.split_first().unwrap();
        match dir {
            SplitDir::Horizontal => {
                let (aw, bw) = split_extent(w, *ratio);
                if *head == 0 {
                    a.resize_at(tail, pos, x, y, aw, h)
                } else {
                    b.resize_at(tail, pos, x + aw, y, bw, h)
                }
            }
            SplitDir::Vertical => {
                let (ah, bh) = split_extent(h, *ratio);
                if *head == 0 {
                    a.resize_at(tail, pos, x, y, w, ah)
                } else {
                    b.resize_at(tail, pos, x, y + ah, w, bh)
                }
            }
        }
    }
}

/// A split seam, in cell coordinates, plus the tree `path` that leads to
/// the `Split` node owning it (0 = into child `a`, 1 = into child `b`).
#[derive(Debug, Clone)]
pub struct Divider {
    pub path: Vec<u8>,
    pub dir: SplitDir,
    /// Seam position: column x for Horizontal, row y for Vertical.
    pub edge: u16,
    /// Start of the seam's perpendicular extent (y for Horizontal,
    /// x for Vertical).
    pub span_start: u16,
    /// Length of that extent in cells.
    pub span_len: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_leaf_round_trip() {
        let t = PtyLayout::single("%0");
        assert_eq!(t.leaves(), vec!["%0"]);
        let l = t.to_tmux_layout(80, 24);
        match l {
            Layout::Pane { id, w, h, x, y } => {
                assert_eq!((id, w, h, x, y), (0, 80, 24, 0, 0));
            }
            _ => panic!("expected leaf"),
        }
    }

    #[test]
    fn split_then_leaves() {
        let mut t = PtyLayout::single("%0");
        assert!(t.split_leaf("%0", SplitDir::Horizontal, "%1".into()));
        assert_eq!(t.leaves(), vec!["%0", "%1"]);
        let rects = t.leaf_rects(80, 24);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].3 + rects[1].3, 80, "widths sum to total");
        assert_eq!(rects[0].4, 24);
        assert_eq!(rects[1].4, 24);
    }

    #[test]
    fn vertical_split_stacks() {
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Vertical, "%1".into());
        let rects = t.leaf_rects(80, 24);
        assert_eq!(rects[0].3, 80);
        assert_eq!(rects[1].3, 80);
        assert_eq!(rects[0].4 + rects[1].4, 24);
    }

    #[test]
    fn nested_split() {
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        t.split_leaf("%1", SplitDir::Vertical, "%2".into());
        assert_eq!(t.leaves(), vec!["%0", "%1", "%2"]);
    }

    #[test]
    fn set_leaf_ratio_direct_child() {
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        // %0 = child a: frac 그대로.
        assert!(t.set_leaf_ratio("%0", 0.7));
        match &t {
            PtyLayout::Split { ratio, .. } => assert!((ratio - 0.7).abs() < 1e-6),
            _ => panic!("expected split"),
        }
        // %1 = child b: 부모 ratio 는 1 - frac.
        assert!(t.set_leaf_ratio("%1", 0.6));
        match &t {
            PtyLayout::Split { ratio, .. } => assert!((ratio - 0.4).abs() < 1e-6),
            _ => panic!("expected split"),
        }
        let rects = t.leaf_rects(100, 24);
        assert!(rects[1].3 > rects[0].3, "%1 이 0.6 차지해 더 넓어야");
    }

    #[test]
    fn set_leaf_ratio_nested_and_missing() {
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        t.split_leaf("%1", SplitDir::Vertical, "%2".into());
        // %2 의 직계 부모(안쪽 Split)만 변해야 — 바깥 ratio 불변.
        let outer_before = match &t {
            PtyLayout::Split { ratio, .. } => *ratio,
            _ => panic!(),
        };
        assert!(t.set_leaf_ratio("%2", 0.8));
        match &t {
            PtyLayout::Split { ratio, b, .. } => {
                assert!((ratio - outer_before).abs() < 1e-6, "바깥 ratio 불변");
                match b.as_ref() {
                    PtyLayout::Split { ratio, .. } => assert!((ratio - 0.2).abs() < 1e-6),
                    _ => panic!("expected inner split"),
                }
            }
            _ => panic!(),
        }
        // 없는 pane / 단일 leaf → false.
        assert!(!t.set_leaf_ratio("%9", 0.5));
        let mut single = PtyLayout::single("%0");
        assert!(!single.set_leaf_ratio("%0", 0.5));
    }

    #[test]
    fn remove_leaf_collapses_parent() {
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        assert!(t.remove_leaf("%1"));
        assert_eq!(t.leaves(), vec!["%0"]);
        // Root collapsed back to a leaf.
        match t {
            PtyLayout::Leaf { pane_id } => assert_eq!(pane_id, "%0"),
            _ => panic!("expected leaf after collapse"),
        }
    }

    #[test]
    fn remove_nested_keeps_sibling() {
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        t.split_leaf("%1", SplitDir::Vertical, "%2".into());
        assert!(t.remove_leaf("%2"));
        assert_eq!(t.leaves(), vec!["%0", "%1"]);
    }

    #[test]
    fn move_pane_relocates_via_detach_insert() {
        // %0 | %1 side by side. Drag %1 to the top of %0: detach it
        // (PTY stays alive in the caller) then insert above %0.
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        assert!(t.remove_leaf("%1"));
        assert_eq!(t.leaves(), vec!["%0"]);
        assert!(t.insert_beside("%0", SplitDir::Vertical, true, "%1".into()));
        assert_eq!(t.leaves(), vec!["%1", "%0"]);
        let rects = t.leaf_rects(80, 24);
        let r1 = rects.iter().find(|(id, ..)| id == "%1").unwrap();
        let r0 = rects.iter().find(|(id, ..)| id == "%0").unwrap();
        assert_eq!(r1.3, 80, "%1 spans full width on top");
        assert_eq!(r0.3, 80, "%0 spans full width below");
        assert!(r1.2 < r0.2, "%1 sits above %0");
    }

    #[test]
    fn insert_beside_after_puts_moving_second() {
        let mut t = PtyLayout::single("%0");
        assert!(t.insert_beside("%0", SplitDir::Horizontal, false, "%9".into()));
        assert_eq!(t.leaves(), vec!["%0", "%9"]);
    }

    #[test]
    fn dividers_one_per_split() {
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        t.split_leaf("%1", SplitDir::Vertical, "%2".into());
        // Two splits in the tree → two seams.
        assert_eq!(t.dividers(80, 24).len(), 2);
    }

    #[test]
    fn resize_divider_moves_ratio() {
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        // Default 0.5 → 40 cols each on an 80-col window.
        assert_eq!(t.leaf_rects(80, 24)[0].3, 40);
        let path = t.dividers(80, 24)[0].path.clone();
        // Drag the seam to column 20 → left pane shrinks to 20.
        assert!(t.resize_divider(&path, 20, 80, 24));
        assert_eq!(t.leaf_rects(80, 24)[0].3, 20);
        assert_eq!(t.leaf_rects(80, 24)[1].3, 60);
    }

    #[test]
    fn ratio_roundtrip_through_path() {
        // The daemon path of a divider drag: GUI reads the ratio it just set,
        // ships it over RPC, daemon writes it back at the same path.
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        t.split_leaf("%1", SplitDir::Vertical, "%2".into());
        let vert = t
            .dividers(80, 24)
            .into_iter()
            .find(|d| d.dir == SplitDir::Vertical)
            .expect("vertical seam");
        assert!(t.set_ratio_at(&vert.path, 0.25));
        assert_eq!(t.ratio_at(&vert.path), Some(0.25));
        // Clamp matches resize_at so a pane can't collapse.
        assert!(t.set_ratio_at(&vert.path, 0.99));
        assert_eq!(t.ratio_at(&vert.path), Some(0.9));
        // A path that runs past a leaf is a no-op, never a panic. Child a is
        // Leaf %0, so descending into it (path [0, …]) finds no Split.
        assert!(!t.set_ratio_at(&[0, 0], 0.3));
        assert_eq!(t.ratio_at(&[0, 0]), None);
    }

    #[test]
    fn resize_divider_clamps() {
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        let path = t.dividers(80, 24)[0].path.clone();
        // Dragging past the edge clamps to 0.1, never collapsing a pane.
        t.resize_divider(&path, 0, 80, 24);
        let left = t.leaf_rects(80, 24)[0].3;
        assert!(left >= 8 && left < 40, "clamped left width = {left}");
    }

    #[test]
    fn resize_nested_divider() {
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        t.split_leaf("%1", SplitDir::Vertical, "%2".into());
        // The vertical seam lives inside the right half (b subtree).
        let vert = t
            .dividers(80, 24)
            .into_iter()
            .find(|d| d.dir == SplitDir::Vertical)
            .expect("vertical seam");
        assert!(t.resize_divider(&vert.path, 6, 80, 24));
        // %1 (top-right) now occupies 6 rows of the 24-row window.
        let r1 = t
            .leaf_rects(80, 24)
            .into_iter()
            .find(|(id, ..)| id == "%1")
            .unwrap();
        assert_eq!(r1.4, 6);
    }

    #[test]
    fn to_tmux_layout_emits_hsplit() {
        let mut t = PtyLayout::single("%0");
        t.split_leaf("%0", SplitDir::Horizontal, "%1".into());
        match t.to_tmux_layout(80, 24) {
            Layout::HSplit { children, w, h, .. } => {
                assert_eq!((w, h), (80, 24));
                assert_eq!(children.len(), 2);
            }
            _ => panic!("expected HSplit"),
        }
    }

    /// 학생 몫이 **반감하지 않는지**가 이 배치의 존재 이유다 — 옛 경로는 split 을
    /// N 번 연달아 불러 1/2 → 1/4 → 1/8 로 줄었고, 넷을 부르면 마지막이 1/16 이었다.
    #[test]
    fn fleet_students_are_equal_not_halving() {
        for n in 1..=6usize {
            let ids: Vec<String> = (1..=n).map(|i| format!("%{i}")).collect();
            let t = fleet("%0", &ids, SplitDir::Horizontal, 0.6);
            // 호스트는 루트 직계 자식 — 「부른 쪽을 크게」가 루트 ratio 하나로 잡힌다.
            match &t {
                PtyLayout::Split { dir: SplitDir::Horizontal, ratio, a, .. } => {
                    assert!((ratio - 0.6).abs() < 1e-6, "n={n}");
                    assert!(matches!(a.as_ref(), PtyLayout::Leaf { pane_id } if pane_id == "%0"));
                }
                _ => panic!("n={n}: 루트가 Horizontal split 이어야"),
            }
            // 화면 순서 = leaf 순서(포커스 순환이 눈을 따라가야 한다).
            let mut want = vec!["%0"];
            want.extend(ids.iter().map(|s| s.as_str()));
            assert_eq!(t.leaves(), want, "n={n}");

            let rects = t.leaf_rects(200, 60);
            let host = rects.iter().find(|r| r.0 == "%0").unwrap();
            assert_eq!(host.4, 60, "n={n}: 호스트는 전체 높이");
            let hs: Vec<u16> = ids
                .iter()
                .map(|id| rects.iter().find(|r| &r.0 == id).unwrap().4)
                .collect();
            let (lo, hi) = (*hs.iter().min().unwrap(), *hs.iter().max().unwrap());
            // 셀은 정수라 나머지가 한 칸씩 갈릴 수 있다 — 그 이상 벌어지면 반감이다.
            assert!(hi - lo <= 1, "n={n}: 학생 높이가 균등해야 (got {hs:?})");
            assert_eq!(hs.iter().sum::<u16>(), 60, "n={n}: 높이 합이 창 높이");
            // 폭은 전원이 호스트와 나눠 갖는 한 덩어리를 공유한다.
            for id in &ids {
                let r = rects.iter().find(|x| &x.0 == id).unwrap();
                assert_eq!(r.3, 200 - host.3, "n={n}: {id} 폭");
            }
        }
    }

    #[test]
    fn fleet_vertical_puts_students_side_by_side() {
        // 세로로 긴 창: 호스트가 위, 학생들이 좌우로 선다.
        let ids: Vec<String> = vec!["%1".into(), "%2".into(), "%3".into()];
        let t = fleet("%0", &ids, SplitDir::Vertical, 0.6);
        let rects = t.leaf_rects(90, 100);
        let host = rects.iter().find(|r| r.0 == "%0").unwrap();
        assert_eq!(host.3, 90, "호스트는 전체 폭");
        let ws: Vec<u16> = ids
            .iter()
            .map(|id| rects.iter().find(|r| &r.0 == id).unwrap().3)
            .collect();
        assert_eq!(ws, vec![30, 30, 30]);
        for id in &ids {
            let r = rects.iter().find(|x| &x.0 == id).unwrap();
            assert_eq!(r.4, 100 - host.4, "{id} 높이는 호스트가 남긴 만큼");
        }
    }

    #[test]
    fn fleet_empty_and_single() {
        // 학생이 없으면 쪼갤 이유가 없다.
        let t = fleet("%0", &[], SplitDir::Horizontal, 0.6);
        assert!(matches!(t, PtyLayout::Leaf { pane_id } if pane_id == "%0"));
        // 한 명이면 사슬 없이 split 하나.
        let t = fleet("%0", &["%1".into()], SplitDir::Horizontal, 0.6);
        assert_eq!(t.leaves(), vec!["%0", "%1"]);
        match &t {
            PtyLayout::Split { b, .. } => {
                assert!(matches!(b.as_ref(), PtyLayout::Leaf { .. }), "사슬이 없어야")
            }
            _ => panic!(),
        }
    }

    #[test]
    fn fleet_host_ratio_is_clamped() {
        // 0/1 을 넘기면 pane 이 사라진다 — 트리 쪽에서 먼저 막는다.
        let t = fleet("%0", &["%1".into()], SplitDir::Horizontal, 5.0);
        match &t {
            PtyLayout::Split { ratio, .. } => assert!((ratio - 0.9).abs() < 1e-6),
            _ => panic!(),
        }
        let t = fleet("%0", &["%1".into()], SplitDir::Horizontal, -1.0);
        match &t {
            PtyLayout::Split { ratio, .. } => assert!((ratio - 0.1).abs() < 1e-6),
            _ => panic!(),
        }
    }

    #[test]
    fn fleet_capacity_counts_the_divided_axis() {
        // 가로 배치: 학생 열은 높이를 나눠 갖는다 → 60줄 / 16줄 = 3명.
        assert_eq!(fleet_capacity(SplitDir::Horizontal, 0.6, 200, 60, 80, 16), 3);
        // 창이 커지면 인원도 늘고,
        assert_eq!(fleet_capacity(SplitDir::Horizontal, 0.6, 200, 96, 80, 16), 6);
        // 남은 폭이 하한 밑이면 **인원과 무관하게** 0 이다(다 같이 쓰는 축이라).
        assert_eq!(fleet_capacity(SplitDir::Horizontal, 0.6, 150, 60, 80, 16), 0);
        // 세로 배치는 갈리는 축이 폭이다.
        assert_eq!(fleet_capacity(SplitDir::Vertical, 0.6, 240, 100, 80, 16), 3);
        assert_eq!(fleet_capacity(SplitDir::Vertical, 0.9, 240, 100, 80, 16), 0);
    }

    /// 용량대로 앉히면 **정말로** 하한을 지키는지. 창 크기를 훑는 이유는 이 둘이
    /// 갈리는 게 반올림 한 칸에서 나기 때문이다 — 한 크기만 보면 통과한다(처음
    /// 79 vs 80 으로 어긋났을 때 실제로 그랬다).
    #[test]
    fn fleet_capacity_matches_actual_rects() {
        let (min_c, min_r) = (80u16, 16u16);
        for cols in [160u16, 161, 199, 200, 240, 320] {
            for rows in [32u16, 33, 47, 60, 64, 100] {
                for r in [0.5f32, 0.6, 0.75] {
                    for dir in [SplitDir::Horizontal, SplitDir::Vertical] {
                        let n = fleet_capacity(dir, r, cols, rows, min_c, min_r);
                        if n == 0 {
                            continue;
                        }
                        let ids: Vec<String> = (1..=n).map(|i| format!("%{i}")).collect();
                        let t = fleet("%0", &ids, dir, r);
                        let rects = t.leaf_rects(cols, rows);
                        for id in &ids {
                            let g = rects.iter().find(|x| &x.0 == id).unwrap();
                            assert!(
                                g.3 >= min_c && g.4 >= min_r,
                                "{cols}x{rows} r={r} {dir:?} n={n}: {id} = {}x{} 가 하한 밑",
                                g.3,
                                g.4
                            );
                        }
                    }
                }
            }
        }
    }

    // 좌우먼저: 각 컬럼 = 상단 leaf + 하단 좌우2, 상단 정렬(0.4).
    fn aligned_columns() -> PtyLayout {
        let col = |top: &str, bl: &str, br: &str| PtyLayout::Split {
            dir: SplitDir::Vertical,
            ratio: 0.4,
            a: Box::new(PtyLayout::single(top)),
            b: Box::new(PtyLayout::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.5,
                a: Box::new(PtyLayout::single(bl)),
                b: Box::new(PtyLayout::single(br)),
            }),
        };
        PtyLayout::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            a: Box::new(col("%1", "%5", "%4")),
            b: Box::new(col("%2", "%3", "%6")),
        }
    }

    #[test]
    fn split_htov_reparents_aligned_columns() {
        let mut t = aligned_columns();
        let xw = |t: &PtyLayout, id: &str| {
            let r = t.leaf_rects(100, 100).into_iter().find(|r| r.0 == id).unwrap();
            (r.1, r.3) // (x, w)
        };
        let top1 = xw(&t, "%1");
        let top2 = xw(&t, "%2");
        let bl_before = xw(&t, "%5");

        let bot = t.split_htov_at(&[]).expect("정렬된 컬럼 → 재구조화 성공");
        assert_eq!(bot, vec![1], "하단 Horizontal split 의 path");
        // 최상위가 Vertical(상하먼저)로 뒤바뀜, 상단 비율은 옛 컬럼 비율 승계.
        match &t {
            PtyLayout::Split { dir: SplitDir::Vertical, ratio, .. } => {
                assert!((ratio - 0.4).abs() < 1e-6)
            }
            _ => panic!("상하먼저로 재구조화돼야"),
        }
        assert_eq!(t.leaves(), vec!["%1", "%2", "%5", "%4", "%3", "%6"]);

        // 반환된 하단 path 로 경계만 이동 → 상단 %1,%2 가로 불변, 하단만 변함.
        t.set_ratio_at(&bot, 0.75);
        assert_eq!(xw(&t, "%1"), top1, "상단 %1 가로 불변");
        assert_eq!(xw(&t, "%2"), top2, "상단 %2 가로 불변");
        assert_ne!(xw(&t, "%5"), bl_before, "하단 경계는 이동");
    }

    #[test]
    fn merge_vtoh_roundtrip() {
        // split_htov_at 으로 상하 분리 → 분리 직후 상단·하단 세로선 ratio 는 둘 다
        // 원래 컬럼 비율(0.4)이라 정렬됨 → merge_vtoh_at 이 관통 Horizontal 로 재병합.
        let mut t = aligned_columns();
        let bot = t.split_htov_at(&[]).expect("분리");
        assert_eq!(bot, vec![1]);
        let merged = t.merge_vtoh_at(&[], 0.02).expect("정렬 → 재병합");
        assert_eq!(merged, vec![], "관통 Horizontal split = 루트");
        match &t {
            PtyLayout::Split { dir: SplitDir::Horizontal, ratio, .. } => {
                assert!((ratio - 0.5).abs() < 1e-6, "관통 세로선 비율(0.5) 복원")
            }
            _ => panic!("관통 Horizontal 로 재병합돼야"),
        }
    }

    #[test]
    fn merge_vtoh_rejects_unaligned() {
        // 하단 세로선만 옮겨 상단과 어긋나면 재병합 거부 — 상하 분리(Vertical) 유지.
        let mut t = aligned_columns();
        let bot = t.split_htov_at(&[]).expect("분리");
        t.set_ratio_at(&bot, 0.8); // 하단만 0.8, 상단은 0.4
        assert_eq!(t.merge_vtoh_at(&[], 0.02), None, "안 맞으면 거부");
        assert!(matches!(t, PtyLayout::Split { dir: SplitDir::Vertical, .. }));
    }

    #[test]
    fn split_htov_rejects_unaligned_or_leaf() {
        // 한쪽이 leaf(상하 분할 없음) → 재구조화 불가, 트리 무변경.
        let mut t = PtyLayout::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            a: Box::new(PtyLayout::single("%0")),
            b: Box::new(PtyLayout::single("%1")),
        };
        assert_eq!(t.split_htov_at(&[]), None);
        assert!(
            matches!(t, PtyLayout::Split { dir: SplitDir::Horizontal, .. }),
            "거부 시 원본 유지"
        );

        // 상단 비율 불일치(0.3 vs 0.6) → 정렬 안 됨 → 거부.
        let col = |s: f32, top: &str, bot: &str| PtyLayout::Split {
            dir: SplitDir::Vertical,
            ratio: s,
            a: Box::new(PtyLayout::single(top)),
            b: Box::new(PtyLayout::single(bot)),
        };
        let mut t2 = PtyLayout::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            a: Box::new(col(0.3, "%1", "%3")),
            b: Box::new(col(0.6, "%2", "%4")),
        };
        assert_eq!(t2.split_htov_at(&[]), None, "상단 정렬 안 됨 → 거부");
    }
}
