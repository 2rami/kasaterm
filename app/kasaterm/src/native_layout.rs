pub(crate) type Rect = (f32, f32, f32, f32);
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Align {
    Start,
    Center,
    End,
}

pub(crate) fn fit_width(slot: Rect, desired: f32, align: Align) -> Rect {
    let width = desired.max(0.0).min(slot.2.max(0.0));
    let spare = (slot.2 - width).max(0.0);
    let offset = match align {
        Align::Start => 0.0,
        Align::Center => spare / 2.0,
        Align::End => spare,
    };
    (slot.0 + offset, slot.1, width, slot.3.max(0.0))
}

pub(crate) fn flow(
    widths: &[f32],
    area: Rect,
    height: f32,
    gap: f32,
    align: Align,
) -> (Vec<Rect>, f32) {
    let width = area.2.max(0.0);
    let height = height.max(0.0);
    let gap = gap.max(0.0);
    let mut rows: Vec<Vec<f32>> = Vec::new();
    let mut line = Vec::new();
    let mut used = 0.0;
    for desired in widths {
        let next = desired.max(0.0).min(width);
        if !line.is_empty() && used + gap + next > width + 0.01 {
            rows.push(std::mem::take(&mut line));
            used = 0.0;
        }
        if !line.is_empty() {
            used += gap;
        }
        used += next;
        line.push(next);
    }
    if !line.is_empty() {
        rows.push(line);
    }
    let mut out = Vec::with_capacity(widths.len());
    for (i, line) in rows.iter().enumerate() {
        let used = line.iter().sum::<f32>() + gap * line.len().saturating_sub(1) as f32;
        let mut x = fit_width((area.0, area.1, width, height), used, align).0;
        for w in line {
            out.push((x, area.1 + i as f32 * (height + gap), *w, height));
            x += w + gap;
        }
    }
    let h = rows.len() as f32 * height + rows.len().saturating_sub(1) as f32 * gap;
    (out, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_buttons_keep_only_their_requested_width_and_alignment() {
        assert_eq!(
            fit_width((10.0, 20.0, 200.0, 32.0), 76.0, Align::End),
            (134.0, 20.0, 76.0, 32.0)
        );
        assert_eq!(
            fit_width((10.0, 20.0, 200.0, 32.0), 76.0, Align::Center),
            (72.0, 20.0, 76.0, 32.0)
        );
        assert_eq!(
            fit_width((10.0, 20.0, 40.0, 32.0), 76.0, Align::Start),
            (10.0, 20.0, 40.0, 32.0)
        );
    }
    #[test]
    fn translated_actions_wrap_without_overlap_or_crossing_the_container() {
        for width in [120.0, 240.0, 390.0, 800.0] {
            let (rects, h) = flow(
                &[64.0, 164.0, 88.0, 270.0],
                (16.0, 30.0, width, 500.0),
                32.0,
                8.0,
                Align::End,
            );
            assert_eq!(rects.len(), 4);
            assert!(h >= 32.0);
            for r in &rects {
                assert!(r.0 >= 16.0 && r.0 + r.2 <= 16.0 + width + 0.01);
            }
            for pair in rects.windows(2) {
                assert!(
                    pair[1].1 >= pair[0].1 + pair[0].3 || pair[1].0 >= pair[0].0 + pair[0].2 + 7.99
                );
            }
        }
        assert_eq!(
            flow(&[], (0.0, 0.0, 100.0, 100.0), 32.0, 8.0, Align::Start).1,
            0.0
        );
    }
}
