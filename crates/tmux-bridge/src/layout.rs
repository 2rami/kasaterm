//! tmux layout-string parser.
//!
//! Format (per `tmux layout` and `%layout-change` events):
//! ```text
//! LAYOUT     := CHECKSUM ',' BLOCK
//! BLOCK      := SIZE_POS ',' PANE_ID                  // leaf
//!             | SIZE_POS '{' BLOCK (',' BLOCK)* '}'   // horizontal split (left|right)
//!             | SIZE_POS '[' BLOCK (',' BLOCK)* ']'   // vertical split (top/bottom)
//! SIZE_POS   := W 'x' H ',' X ',' Y
//! ```
//! Example: `c1d8,80x24,0,0,1` — single pane, id 1.
//! Example: `9adb,80x24,0,0[80x12,0,0,1,80x11,0,13,2]` — vertical split.

use anyhow::{anyhow, bail, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Layout {
    Pane {
        id: u32,
        w: u16,
        h: u16,
        x: u16,
        y: u16,
    },
    /// Horizontal split: children laid out left-to-right.
    HSplit {
        w: u16,
        h: u16,
        x: u16,
        y: u16,
        children: Vec<Layout>,
    },
    /// Vertical split: children laid out top-to-bottom.
    VSplit {
        w: u16,
        h: u16,
        x: u16,
        y: u16,
        children: Vec<Layout>,
    },
}

impl Layout {
    pub fn rect(&self) -> (u16, u16, u16, u16) {
        match self {
            Layout::Pane { x, y, w, h, .. }
            | Layout::HSplit { x, y, w, h, .. }
            | Layout::VSplit { x, y, w, h, .. } => (*x, *y, *w, *h),
        }
    }

    /// All leaf panes in document order.
    pub fn leaves(&self) -> Vec<&Layout> {
        let mut out = Vec::new();
        fn walk<'a>(node: &'a Layout, out: &mut Vec<&'a Layout>) {
            match node {
                Layout::Pane { .. } => out.push(node),
                Layout::HSplit { children, .. } | Layout::VSplit { children, .. } => {
                    for c in children {
                        walk(c, out);
                    }
                }
            }
        }
        walk(self, &mut out);
        out
    }
}

pub fn parse_layout(s: &str) -> Result<Layout> {
    // Strip `<checksum>,` prefix if present.
    let body = match s.find(',') {
        Some(i) if is_hex_checksum(&s[..i]) => &s[i + 1..],
        _ => s,
    };
    let mut p = Parser::new(body);
    let layout = p.parse_block()?;
    if !p.eof() {
        bail!("trailing input at byte {}: {:?}", p.pos, &p.s[p.pos..]);
    }
    Ok(layout)
}

fn is_hex_checksum(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 8
        && s.chars().all(|c| c.is_ascii_hexdigit())
}

struct Parser<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Self { s, pos: 0 }
    }

    fn eof(&self) -> bool {
        self.pos >= self.s.len()
    }

    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn expect(&mut self, b: u8) -> Result<()> {
        match self.bump() {
            Some(c) if c == b => Ok(()),
            other => Err(anyhow!(
                "expected {:?} at byte {}, got {:?}",
                b as char,
                self.pos - 1,
                other.map(|c| c as char)
            )),
        }
    }

    fn parse_u16(&mut self) -> Result<u16> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if !b.is_ascii_digit() {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            bail!("expected number at byte {}", start);
        }
        self.s[start..self.pos]
            .parse::<u16>()
            .map_err(|e| anyhow!("u16 parse: {e}"))
    }

    fn parse_u32(&mut self) -> Result<u32> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if !b.is_ascii_digit() {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            bail!("expected number at byte {}", start);
        }
        self.s[start..self.pos]
            .parse::<u32>()
            .map_err(|e| anyhow!("u32 parse: {e}"))
    }

    /// Parse `WxH,X,Y` and return (w,h,x,y). Leaves cursor at the comma
    /// before the leaf id, or at `{`/`[` for splits.
    fn parse_size_pos(&mut self) -> Result<(u16, u16, u16, u16)> {
        let w = self.parse_u16()?;
        self.expect(b'x')?;
        let h = self.parse_u16()?;
        self.expect(b',')?;
        let x = self.parse_u16()?;
        self.expect(b',')?;
        let y = self.parse_u16()?;
        Ok((w, h, x, y))
    }

    fn parse_block(&mut self) -> Result<Layout> {
        let (w, h, x, y) = self.parse_size_pos()?;
        match self.peek() {
            Some(b'{') => {
                self.bump();
                let children = self.parse_children(b'}')?;
                Ok(Layout::HSplit { w, h, x, y, children })
            }
            Some(b'[') => {
                self.bump();
                let children = self.parse_children(b']')?;
                Ok(Layout::VSplit { w, h, x, y, children })
            }
            Some(b',') => {
                self.bump();
                let id = self.parse_u32()?;
                Ok(Layout::Pane { id, w, h, x, y })
            }
            other => Err(anyhow!(
                "expected ',' / '{{' / '[' after size+pos at byte {}, got {:?}",
                self.pos,
                other.map(|c| c as char)
            )),
        }
    }

    fn parse_children(&mut self, close: u8) -> Result<Vec<Layout>> {
        let mut out = Vec::new();
        loop {
            out.push(self.parse_block()?);
            match self.peek() {
                Some(b',') => {
                    self.bump();
                }
                Some(c) if c == close => {
                    self.bump();
                    return Ok(out);
                }
                other => bail!(
                    "expected ',' or {:?} at byte {}, got {:?}",
                    close as char,
                    self.pos,
                    other.map(|c| c as char)
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_pane_with_checksum() {
        let l = parse_layout("c1d8,80x24,0,0,1").unwrap();
        assert_eq!(
            l,
            Layout::Pane {
                id: 1,
                w: 80,
                h: 24,
                x: 0,
                y: 0,
            }
        );
    }

    #[test]
    fn single_pane_without_checksum() {
        let l = parse_layout("80x24,0,0,1").unwrap();
        assert!(matches!(l, Layout::Pane { id: 1, .. }));
    }

    #[test]
    fn vertical_split() {
        let l = parse_layout("9adb,80x24,0,0[80x12,0,0,1,80x11,0,13,2]").unwrap();
        let Layout::VSplit { children, w, h, .. } = l else {
            panic!("expected VSplit, got {l:?}");
        };
        assert_eq!((w, h), (80, 24));
        assert_eq!(children.len(), 2);
        assert!(matches!(children[0], Layout::Pane { id: 1, h: 12, .. }));
        assert!(matches!(children[1], Layout::Pane { id: 2, h: 11, y: 13, .. }));
    }

    #[test]
    fn horizontal_split() {
        let l = parse_layout("abcd,80x24,0,0{40x24,0,0,1,39x24,41,0,2}").unwrap();
        let Layout::HSplit { children, .. } = l else {
            panic!("expected HSplit");
        };
        assert_eq!(children.len(), 2);
        assert!(matches!(children[0], Layout::Pane { id: 1, w: 40, .. }));
        assert!(matches!(children[1], Layout::Pane { id: 2, w: 39, x: 41, .. }));
    }

    #[test]
    fn nested_split() {
        // 좌측 단일 + 우측 상하 분할
        let l = parse_layout("dead,80x24,0,0{40x24,0,0,1,39x24,41,0[39x12,41,0,2,39x11,41,13,3]}")
            .unwrap();
        let Layout::HSplit { children, .. } = l else {
            panic!()
        };
        assert_eq!(children.len(), 2);
        assert!(matches!(children[0], Layout::Pane { id: 1, .. }));
        let Layout::VSplit { children: r, .. } = &children[1] else {
            panic!("expected nested VSplit, got {:?}", children[1])
        };
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn leaves_in_order() {
        let l = parse_layout("dead,80x24,0,0{40x24,0,0,1,39x24,41,0[39x12,41,0,2,39x11,41,13,3]}")
            .unwrap();
        let ids: Vec<u32> = l
            .leaves()
            .iter()
            .map(|n| match n {
                Layout::Pane { id, .. } => *id,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }
}
