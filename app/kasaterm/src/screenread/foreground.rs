use super::*;

struct Region {
    top: usize,
    left: usize,
    cells: Vec<Vec<GridCell>>,
}

#[derive(Default)]
pub(crate) struct TuiForeground {
    regions: Vec<Region>,
    original: Vec<Vec<GridCell>>,
}

impl TuiForeground {
    pub(crate) fn capture(rows: &[Vec<GridCell>]) -> Self {
        let mut regions = Vec::new();
        for (top, row) in rows.iter().enumerate() {
            for (left, cell) in row.iter().enumerate() {
                let (tr, bl, br, horizontal, vertical) = match cell.ch {
                    '╭' => ('╮', '╰', '╯', '─', '│'),
                    '┌' => ('┐', '└', '┘', '─', '│'),
                    '╔' => ('╗', '╚', '╝', '═', '║'),
                    _ => continue,
                };
                let Some(right) = row[left + 1..].iter().position(|c| c.ch == tr)
                    .map(|offset| left + 1 + offset) else { continue };
                if right - left < 3 || row[left + 1].ch != horizontal || row[right - 1].ch != horizontal {
                    continue;
                }
                let mut bottom = None;
                for (y, row) in rows.iter().enumerate().skip(top + 1) {
                    if row.len() <= right {
                        break;
                    }
                    if row[left].ch == bl && row[right].ch == br
                        && row[left + 1..right].iter().all(|c| c.ch == horizontal)
                    {
                        bottom = Some(y);
                        break;
                    }
                    if row[left].ch != vertical || row[right].ch != vertical {
                        break;
                    }
                }
                let Some(bottom) = bottom.filter(|&bottom| bottom > top + 1) else { continue };
                // 시작 배너는 사용자에게 보여 주는 학생 그림의 원래 자리다.
                let codex_welcome = rows[top + 1..bottom].iter().take(2).any(|row| {
                    let text: String = row[left + 1..right].iter().map(|cell| cell.ch).collect();
                    let heading = text.trim();
                    heading.strip_prefix(">_ OpenAI Codex (v").is_some_and(|version|
                        version.ends_with(')') && version[..version.len() - 1].chars()
                            .all(|ch| ch.is_ascii_digit() || ch == '.'))
                });
                let header: String = row[left + 1..right].iter().map(|cell| cell.ch).collect();
                let sprite_welcome = (header.contains("Claude Code") && !find_clawd_banners(&rows[top + 1..bottom]).is_empty())
                    || (header.contains("Antigravity") && !find_agy_banners(&rows[top + 1..bottom]).is_empty());
                if codex_welcome || sprite_welcome {
                    continue;
                }
                regions.push(Region {
                    top,
                    left,
                    cells: rows[top..=bottom].iter().map(|row| row[left..=right].to_vec()).collect(),
                });
            }
        }
        let original = if regions.is_empty() { Vec::new() } else { rows.to_vec() };
        Self { regions, original }
    }

    pub(crate) fn project(
        &self,
        rows: &[Vec<GridCell>],
        source_map: &[Vec<Option<(usize, usize)>>],
    ) -> Self {
        let contains = |row, col| self.regions.iter().any(|region|
            (region.top..region.top + region.cells.len()).contains(&row)
                && (region.left..region.left + region.cells[0].len()).contains(&col));
        let mut regions = Vec::new();
        for (top, (row, map)) in rows.iter().zip(source_map).enumerate() {
            let mut col = 0;
            while col < row.len().min(map.len()) {
                if !map[col].is_some_and(|(r, c)| contains(r, c)) {
                    col += 1;
                    continue;
                }
                let left = col;
                while col < row.len().min(map.len()) && map[col].is_some_and(|(r, c)| contains(r, c)) {
                    col += 1;
                }
                regions.push(Region { top, left, cells: vec![row[left..col].to_vec()] });
            }
        }
        let original = if regions.is_empty() { Vec::new() } else { rows.to_vec() };
        Self { regions, original }
    }

    pub(crate) fn prefix_end(&self, row: usize, cols: usize) -> usize {
        self.regions.iter()
            .filter(|region| (region.top..region.top + region.cells.len()).contains(&row))
            .map(|region| region.left)
            .min().unwrap_or(cols).min(cols)
    }

    pub(crate) fn tint_row(&self, index: usize, row: &mut [GridCell], accent: [u8; 4]) {
        let end = self.prefix_end(index, row.len());
        super::tint_row(&mut row[..end], accent);
    }

    pub(crate) fn restore(&self, rows: &mut [Vec<GridCell>]) {
        for region in &self.regions {
            for (offset, original) in region.cells.iter().enumerate() {
                if let Some(row) = rows.get_mut(region.top + offset) {
                    if let Some(cells) = row.get_mut(region.left..region.left + original.len()) {
                        cells.clone_from_slice(original);
                    }
                }
            }
        }
    }

    pub(crate) fn restore_art(
        &self,
        rows: &mut [Vec<GridCell>],
        rect: (f32, f32, f32, f32),
        origin: (f32, f32),
        cell: (f32, f32),
    ) {
        if cell.0 <= 0.0 || cell.1 <= 0.0 {
            return;
        }
        let left = ((rect.0 - origin.0) / cell.0).floor().max(0.0) as usize;
        let right = ((rect.0 + rect.2 - origin.0) / cell.0).ceil().max(0.0) as usize;
        let top = ((rect.1 - origin.1) / cell.1).floor().max(0.0) as usize;
        let bottom = ((rect.1 + rect.3 - origin.1) / cell.1).ceil().max(0.0) as usize;
        for (row, original) in rows.iter_mut().zip(&self.original).take(bottom).skip(top) {
            let end = right.min(row.len()).min(original.len());
            if left < end {
                row[left..end].clone_from_slice(&original[left..end]);
            }
        }
    }

    pub(crate) fn overlaps(
        &self,
        rect: (f32, f32, f32, f32),
        origin: (f32, f32),
        cell: (f32, f32),
    ) -> bool {
        self.regions.iter().any(|region| {
            let x = origin.0 + region.left as f32 * cell.0;
            let y = origin.1 + region.top as f32 * cell.1;
            let w = region.cells[0].len() as f32 * cell.0;
            let h = region.cells.len() as f32 * cell.1;
            rect.0 < x + w && rect.0 + rect.2 > x && rect.1 < y + h && rect.1 + rect.3 > y
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasa_bridge::screen::Color;

    fn row(text: &str) -> Vec<GridCell> {
        text.chars().map(|ch| GridCell { ch, ..GridCell::blank() }).collect()
    }

    fn popup() -> Vec<Vec<GridCell>> {
        let mut rows = [
            "message      ╭──────────╮",
            "body text    │ toast    │",
            "more text    │ original │",
            "body end     ╰──────────╯",
        ].map(row).to_vec();
        for row in &mut rows {
            for cell in &mut row[13..] {
                cell.fg = Color::Rgb(140, 160, 210);
                cell.bg = Color::Rgb(35, 40, 48);
            }
        }
        rows
    }

    #[test]
    fn foreground_preserves_cells_and_geometry_after_message_styling() {
        let mut rows = popup();
        let original = rows.clone();
        let foreground = TuiForeground::capture(&rows);
        assert_eq!(foreground.prefix_end(2, rows[2].len()), 13);
        for row in &mut rows {
            tint_row(row, [80, 200, 130, 255]);
            row[15].ch = 'x';
        }
        foreground.restore(&mut rows);
        for (actual, expected) in rows.iter().zip(&original) {
            assert_eq!(actual[13..], expected[13..]);
        }
        assert_ne!(rows[1][0].fg, original[1][0].fg);
    }

    #[test]
    fn foreground_checks_full_pixel_extent_of_decorations() {
        let foreground = TuiForeground::capture(&popup());
        let origin = (30.0, 40.0);
        let cell = (9.5, 22.0);
        let left = origin.0 + 13.0 * cell.0;
        assert!(foreground.overlaps((left - 10.0, 10.0, 20.0, 60.0), origin, cell));
        assert!(!foreground.overlaps((left - 20.0, 40.0, 20.0, 22.0), origin, cell));
        assert!(!foreground.overlaps((left, 128.0, 20.0, 22.0), origin, cell));
    }

    #[test]
    fn foreground_requires_closed_edges_and_preserves_welcome_art() {
        let mut broken = popup();
        broken[1][13].ch = ' ';
        assert_eq!(TuiForeground::capture(&broken).prefix_end(1, 25), 25);
        let mention = ["╭─────────────╮", "│ Claude Code │", "╰─────────────╯"].map(row);
        assert_eq!(TuiForeground::capture(&mention).prefix_end(1, 15), 0);
        let dialog = ["┌─────────┐", "│ approve │", "└─────────┘"].map(row);
        assert_eq!(TuiForeground::capture(&dialog).prefix_end(1, 11), 0);
        let labelled = ["╭─ Claude Code ─╮", "│ approve       │", "╰───────────────╯"].map(row);
        assert_eq!(TuiForeground::capture(&labelled).prefix_end(1, 17), 0);
    }

    #[test]
    fn foreground_follows_source_cells_through_narrow_mirror_reflow() {
        let rows = popup();
        let source = TuiForeground::capture(&rows);
        let view = crate::mirror_view::project(&rows, (3, 0), 10, 24, None, None);
        let foreground = source.project(&view.rows, &view.source_map);
        let mut styled = view.rows.clone();
        for (r, row) in styled.iter_mut().enumerate() {
            foreground.tint_row(r, row, [80, 200, 130, 255]);
        }
        foreground.restore(&mut styled);
        let mut checked = 0;
        for (r, map) in view.source_map.iter().enumerate() {
            for (c, source) in map.iter().enumerate() {
                if source.is_some_and(|(_, c)| c >= 13) {
                    assert_eq!(styled[r][c], view.rows[r][c]);
                    checked += 1;
                }
            }
        }
        assert!(checked > 30);
    }

    #[test]
    fn dropping_partly_occluded_art_restores_its_original_glyph_outside_popup() {
        let mut rows = popup();
        rows.push(row("            ✻ working   "));
        let foreground = TuiForeground::capture(&rows);
        let original = rows[4][12].clone();
        rows[4][12] = GridCell::blank();
        let rect = (12.0, 3.0, 2.0, 2.0);
        assert!(foreground.overlaps(rect, (0.0, 0.0), (1.0, 1.0)));
        foreground.restore_art(&mut rows, rect, (0.0, 0.0), (1.0, 1.0));
        foreground.restore(&mut rows);
        assert_eq!(rows[4][12], original);
    }
}
