//! In-app text selection for the chat transcript (mouse drag → copy), ported
//! from pekchat-tui.
//!
//! An anchor is recorded on mouse-down (no visual yet), dragging activates the
//! highlight, and the release copies the text. Coordinates are absolute
//! content-line indices so the selection stays put while the chat scrolls.

/// A content position: `(line index, character column)`.
pub type Pos = (usize, usize);

#[derive(Clone)]
pub struct Selection {
    anchor: Pos,
    cursor: Pos,
    /// Becomes true once the mouse has dragged away from the anchor; a plain
    /// click (down + up with no movement) never activates.
    active: bool,
}

impl Selection {
    pub fn new(pos: Pos) -> Self {
        Self {
            anchor: pos,
            cursor: pos,
            active: false,
        }
    }

    /// Extend the selection to `pos`, marking it visible/active.
    pub fn drag(&mut self, pos: Pos) {
        self.cursor = pos;
        self.active = true;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Ordered `(start, end)` range, or `None` if not yet active.
    pub fn range(&self) -> Option<(Pos, Pos)> {
        if !self.active {
            return None;
        }
        if self.anchor <= self.cursor {
            Some((self.anchor, self.cursor))
        } else {
            Some((self.cursor, self.anchor))
        }
    }

    /// Extract the selected text from the rendered `lines` using stream
    /// (linewise-flowing) semantics: the first line from its column to the end,
    /// whole lines in between, and the last line up to its column.
    pub fn extract(&self, lines: &[String]) -> String {
        let Some((start, end)) = self.range() else {
            return String::new();
        };

        let mut out = String::new();
        for line_index in start.0..=end.0 {
            let Some(line) = lines.get(line_index) else {
                break;
            };
            let chars: Vec<char> = line.chars().collect();
            let len = chars.len();

            let (from, to) = if start.0 == end.0 {
                (start.1, end.1)
            } else if line_index == start.0 {
                (start.1, len)
            } else if line_index == end.0 {
                (0, end.1)
            } else {
                (0, len)
            };
            let from = from.min(len);
            let to = to.min(len);

            if line_index != start.0 {
                out.push('\n');
            }
            if from < to {
                out.extend(&chars[from..to]);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines() -> Vec<String> {
        vec!["hello world".into(), "second line".into(), "third".into()]
    }

    #[test]
    fn plain_click_is_not_active() {
        let sel = Selection::new((0, 3));
        assert!(!sel.is_active());
        assert_eq!(sel.range(), None);
        assert_eq!(sel.extract(&lines()), "");
    }

    #[test]
    fn single_line_selection_extracts_substring() {
        let mut sel = Selection::new((0, 0));
        sel.drag((0, 5));
        assert!(sel.is_active());
        assert_eq!(sel.extract(&lines()), "hello");
    }

    #[test]
    fn multi_line_selection_flows_across_lines() {
        let mut sel = Selection::new((0, 6));
        sel.drag((2, 3));
        assert_eq!(sel.extract(&lines()), "world\nsecond line\nthi");
    }

    #[test]
    fn reversed_drag_is_normalized() {
        let mut sel = Selection::new((1, 7));
        sel.drag((0, 6));
        assert_eq!(sel.range(), Some(((0, 6), (1, 7))));
        assert_eq!(sel.extract(&lines()), "world\nsecond ");
    }
}
