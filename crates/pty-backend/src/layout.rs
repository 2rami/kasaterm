//! BSP layout tree for multi-pane PTY mode.
//!
//! tmux owns its layout server-side and broadcasts it to us as a
//! `%layout-change` event. Without tmux we run our own tree: a recursive
//! BSP where every internal node is a horizontal or vertical split with
//! a ratio, and every leaf carries a pane id.
//!
//! The renderer consumes `tmux_bridge::layout::Layout`, so we expose
//! `to_tmux_layout()` that walks our tree and emits the same shape with
//! pane rectangles already laid out for the given window size. Pane ids
//! are formatted "%N" by convention; conversion strips the prefix to
//! the u32 that tmux's Layout expects.
//!
//! Naming note: `Horizontal` here matches tmux — children sit side by
//! side with a *vertical* divider between them. That is the layout
//! Terminal.app calls "Cmd+D / vertical split". `Vertical` stacks
//! children with a horizontal divider, matching Cmd+Shift+D.

use tmux_bridge::layout::Layout;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SplitDir {
    /// Children laid out left-to-right (vertical divider line).
    Horizontal,
    /// Children laid out top-to-bottom (horizontal divider line).
    Vertical,
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
                    // Reserve one cell of divider between children so the
                    // visual gutter the renderer draws doesn't double up
                    // on a pane's own cells.
                    let aw = ((w as f32) * ratio).round() as u16;
                    let aw = aw.min(w.saturating_sub(1)).max(1);
                    let bw = w.saturating_sub(aw);
                    a.walk_rects(x, y, aw, h, out);
                    b.walk_rects(x + aw, y, bw, h, out);
                }
                SplitDir::Vertical => {
                    let ah = ((h as f32) * ratio).round() as u16;
                    let ah = ah.min(h.saturating_sub(1)).max(1);
                    let bh = h.saturating_sub(ah);
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
                    let aw = ((w as f32) * ratio).round() as u16;
                    let aw = aw.min(w.saturating_sub(1)).max(1);
                    let bw = w.saturating_sub(aw);
                    let children = vec![
                        a.build_layout(x, y, aw, h),
                        b.build_layout(x + aw, y, bw, h),
                    ];
                    Layout::HSplit { w, h, x, y, children }
                }
                SplitDir::Vertical => {
                    let ah = ((h as f32) * ratio).round() as u16;
                    let ah = ah.min(h.saturating_sub(1)).max(1);
                    let bh = h.saturating_sub(ah);
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
                    let aw = ((w as f32) * ratio).round() as u16;
                    let aw = aw.min(w.saturating_sub(1)).max(1);
                    let bw = w.saturating_sub(aw);
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
                    let ah = ((h as f32) * ratio).round() as u16;
                    let ah = ah.min(h.saturating_sub(1)).max(1);
                    let bh = h.saturating_sub(ah);
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
                let aw = ((w as f32) * *ratio).round() as u16;
                let aw = aw.min(w.saturating_sub(1)).max(1);
                let bw = w.saturating_sub(aw);
                if *head == 0 {
                    a.resize_at(tail, pos, x, y, aw, h)
                } else {
                    b.resize_at(tail, pos, x + aw, y, bw, h)
                }
            }
            SplitDir::Vertical => {
                let ah = ((h as f32) * *ratio).round() as u16;
                let ah = ah.min(h.saturating_sub(1)).max(1);
                let bh = h.saturating_sub(ah);
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
}
