#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CursorShape {
    #[default]
    Block,
    Bar,
    Underline,
    Frame,
    Brackets,
    TwinRails,
    Topline,
    CornerMarks,
}

impl CursorShape {
    pub(crate) const ALL: [Self; 8] = [
        Self::Block,
        Self::Bar,
        Self::Underline,
        Self::Frame,
        Self::Brackets,
        Self::TwinRails,
        Self::Topline,
        Self::CornerMarks,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Bar => "bar",
            Self::Underline => "underline",
            Self::Frame => "frame",
            Self::Brackets => "brackets",
            Self::TwinRails => "twin-rails",
            Self::Topline => "topline",
            Self::CornerMarks => "corner-marks",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|shape| shape.as_str() == value)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct CursorQuad {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct CursorPrimitives {
    quads: [CursorQuad; 8],
    len: u8,
}

impl CursorPrimitives {
    pub(crate) fn as_slice(&self) -> &[CursorQuad] {
        &self.quads[..self.len as usize]
    }

    fn push(&mut self, x: f32, y: f32, width: f32, height: f32) {
        if width <= 0.0 || height <= 0.0 || self.len as usize >= self.quads.len() {
            return;
        }
        self.quads[self.len as usize] = CursorQuad {
            x,
            y,
            width,
            height,
        };
        self.len += 1;
    }
}

fn clamped_thickness(requested: f32, limit: f32) -> f32 {
    let requested = if requested.is_finite() {
        requested.max(1.0)
    } else {
        2.0
    };
    requested.min(limit.max(0.0))
}

pub(crate) fn cursor_primitives(
    shape: CursorShape,
    x: f32,
    y: f32,
    cell_width: f32,
    cell_height: f32,
    cell_span: u16,
    thickness: f32,
) -> CursorPrimitives {
    let mut out = CursorPrimitives::default();
    let width = cell_width * cell_span.max(1) as f32;
    if !x.is_finite()
        || !y.is_finite()
        || !width.is_finite()
        || !cell_height.is_finite()
        || width <= 0.0
        || cell_height <= 0.0
    {
        return out;
    }

    match shape {
        CursorShape::Block => out.push(x, y, width, cell_height),
        CursorShape::Bar => {
            let t = clamped_thickness(thickness, width / 2.0);
            out.push(x, y, t, cell_height);
        }
        CursorShape::Underline => {
            let t = clamped_thickness(thickness, cell_height / 2.0);
            out.push(x, y + cell_height - t, width, t);
        }
        CursorShape::Topline => {
            let t = clamped_thickness(thickness, cell_height / 2.0);
            out.push(x, y, width, t);
        }
        CursorShape::TwinRails => {
            let t = clamped_thickness(thickness, width / 3.0);
            out.push(x, y, t, cell_height);
            out.push(x + width - t, y, t, cell_height);
        }
        CursorShape::Frame => {
            let t = clamped_thickness(thickness, width.min(cell_height) / 3.0);
            out.push(x, y, width, t);
            out.push(x, y + cell_height - t, width, t);
            out.push(x, y + t, t, cell_height - t * 2.0);
            out.push(x + width - t, y + t, t, cell_height - t * 2.0);
        }
        CursorShape::Brackets => {
            let t = clamped_thickness(thickness, width.min(cell_height) / 3.0);
            let arm = (width * 0.32).max(t).min(width / 2.0);
            out.push(x, y, t, cell_height);
            out.push(x + width - t, y, t, cell_height);
            out.push(x + t, y, arm - t, t);
            out.push(x + t, y + cell_height - t, arm - t, t);
            out.push(x + width - arm, y, arm - t, t);
            out.push(x + width - arm, y + cell_height - t, arm - t, t);
        }
        CursorShape::CornerMarks => {
            let t = clamped_thickness(thickness, width.min(cell_height) / 3.0);
            let arm_w = (width * 0.30).max(t).min(width / 2.0);
            let arm_h = (cell_height * 0.28).max(t).min(cell_height / 2.0);
            out.push(x, y, arm_w, t);
            out.push(x, y + t, t, arm_h - t);
            out.push(x + width - arm_w, y, arm_w, t);
            out.push(x + width - t, y + t, t, arm_h - t);
            out.push(x, y + cell_height - t, arm_w, t);
            out.push(x, y + cell_height - arm_h, t, arm_h - t);
            out.push(x + width - arm_w, y + cell_height - t, arm_w, t);
            out.push(x + width - t, y + cell_height - arm_h, t, arm_h - t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_persisted_key_round_trips() {
        for shape in CursorShape::ALL {
            assert_eq!(CursorShape::from_str(shape.as_str()), Some(shape));
        }
        assert_eq!(CursorShape::from_str("pulse"), None);
    }

    #[test]
    fn legacy_shapes_keep_their_geometry() {
        let block = cursor_primitives(CursorShape::Block, 3.0, 5.0, 8.0, 18.0, 1, 2.0);
        assert_eq!(
            block.as_slice(),
            &[CursorQuad {
                x: 3.0,
                y: 5.0,
                width: 8.0,
                height: 18.0
            }]
        );

        let bar = cursor_primitives(CursorShape::Bar, 3.0, 5.0, 8.0, 18.0, 1, 2.0);
        assert_eq!(
            bar.as_slice(),
            &[CursorQuad {
                x: 3.0,
                y: 5.0,
                width: 2.0,
                height: 18.0
            }]
        );

        let underline = cursor_primitives(CursorShape::Underline, 3.0, 5.0, 8.0, 18.0, 1, 2.0);
        assert_eq!(
            underline.as_slice(),
            &[CursorQuad {
                x: 3.0,
                y: 21.0,
                width: 8.0,
                height: 2.0
            }]
        );
    }

    #[test]
    fn wide_cells_double_the_complete_shape() {
        for shape in CursorShape::ALL {
            let narrow = cursor_primitives(shape, 10.0, 20.0, 7.5, 18.0, 1, 2.0);
            let wide = cursor_primitives(shape, 10.0, 20.0, 7.5, 18.0, 2, 2.0);
            assert!(!wide.as_slice().is_empty(), "{}", shape.as_str());
            for quad in wide.as_slice() {
                assert!(quad.x >= 10.0, "{}", shape.as_str());
                assert!(
                    quad.x + quad.width <= 25.0 + f32::EPSILON,
                    "{}",
                    shape.as_str()
                );
                assert!(quad.y >= 20.0, "{}", shape.as_str());
                assert!(
                    quad.y + quad.height <= 38.0 + f32::EPSILON,
                    "{}",
                    shape.as_str()
                );
            }
            let narrow_right = narrow
                .as_slice()
                .iter()
                .map(|q| q.x + q.width)
                .fold(f32::NEG_INFINITY, f32::max);
            let wide_right = wide
                .as_slice()
                .iter()
                .map(|q| q.x + q.width)
                .fold(f32::NEG_INFINITY, f32::max);
            if shape == CursorShape::Bar {
                assert_eq!(wide, narrow);
            } else {
                assert_eq!(wide_right - narrow_right, 7.5, "{}", shape.as_str());
            }
        }
    }

    #[test]
    fn thickness_never_escapes_or_erases_a_narrow_cell() {
        for requested in [0.0, 99.0, f32::NAN] {
            for shape in CursorShape::ALL {
                let quads = cursor_primitives(shape, 0.0, 0.0, 3.0, 4.0, 1, requested);
                assert!(!quads.as_slice().is_empty(), "{}", shape.as_str());
                assert!(quads
                    .as_slice()
                    .iter()
                    .all(|q| q.width > 0.0 && q.height > 0.0));
                assert!(quads.as_slice().iter().all(|q| {
                    q.x >= 0.0 && q.y >= 0.0 && q.x + q.width <= 3.0 && q.y + q.height <= 4.0
                }));
            }
        }
    }
}
