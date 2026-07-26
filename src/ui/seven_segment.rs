//! A tiny seven-segment font. Each glyph is drawn in a 3x3 cell using the
//! classic segment layout:
//!
//! ```text
//!  _        a
//! |_|     f g b
//! |_|     e d c
//! ```
//!
//! `render` turns a string into three equal-length rows of text you can drop
//! straight into the TUI (colour it green and you have a watch display).

/// Segment order: a, b, c, d, e, f, g.
type Segs = [bool; 7];

const fn segs(a: bool, b: bool, c: bool, d: bool, e: bool, f: bool, g: bool) -> Segs {
    [a, b, c, d, e, f, g]
}

/// Map an ASCII char to its seven-segment shape. Letters use the conventional
/// pseudo-seven-segment alphabet (some are lowercase-ish approximations, which
/// is exactly how cheap segment displays render them).
fn glyph(ch: char) -> Segs {
    let t = true;
    let f = false;
    match ch.to_ascii_uppercase() {
        '0' => segs(t, t, t, t, t, t, f),
        '1' => segs(f, t, t, f, f, f, f),
        '2' => segs(t, t, f, t, t, f, t),
        '3' => segs(t, t, t, t, f, f, t),
        '4' => segs(f, t, t, f, f, t, t),
        '5' => segs(t, f, t, t, f, t, t),
        '6' => segs(t, f, t, t, t, t, t),
        '7' => segs(t, t, t, f, f, f, f),
        '8' => segs(t, t, t, t, t, t, t),
        '9' => segs(t, t, t, t, f, t, t),
        'A' => segs(t, t, t, f, t, t, t),
        'B' => segs(f, f, t, t, t, t, t),
        'C' => segs(t, f, f, t, t, t, f),
        'D' => segs(f, t, t, t, t, f, t),
        'E' => segs(t, f, f, t, t, t, t),
        'F' => segs(t, f, f, f, t, t, t),
        'G' => segs(t, f, t, t, t, t, f),
        'H' => segs(f, t, t, f, t, t, t),
        'I' => segs(f, t, t, f, f, f, f),
        'J' => segs(f, t, t, t, t, f, f),
        'K' => segs(f, t, t, f, t, t, t),
        'L' => segs(f, f, f, t, t, t, f),
        'M' => segs(t, t, t, f, t, t, f),
        'N' => segs(f, f, t, f, t, f, t),
        'O' => segs(t, t, t, t, t, t, f),
        'P' => segs(t, t, f, f, t, t, t),
        'Q' => segs(t, t, t, f, f, t, t),
        'R' => segs(f, f, f, f, t, f, t),
        'S' => segs(t, f, t, t, f, t, t),
        'T' => segs(f, f, f, t, t, t, t),
        'U' => segs(f, t, t, t, t, t, f),
        'V' => segs(f, f, t, t, t, f, f),
        'W' => segs(f, t, t, t, t, t, f),
        'X' => segs(f, t, t, f, t, t, t),
        'Y' => segs(f, t, t, t, f, t, t),
        'Z' => segs(t, t, f, t, t, f, t),
        '-' => segs(f, f, f, f, f, f, t),
        '_' => segs(f, f, f, t, f, f, f),
        _ => segs(f, f, f, f, f, f, f), // space / unknown
    }
}

/// Render `text` into three rows. Every glyph is 3 columns wide with a single
/// blank column between glyphs, so all three returned strings have equal length.
pub fn render(text: &str) -> [String; 3] {
    let mut rows = [String::new(), String::new(), String::new()];
    for (i, ch) in text.chars().enumerate() {
        if i > 0 {
            for row in &mut rows {
                row.push(' ');
            }
        }
        let s = glyph(ch);
        // Row 0: top segment (a).
        rows[0].push(' ');
        rows[0].push(if s[0] { '_' } else { ' ' });
        rows[0].push(' ');
        // Row 1: f, g, b.
        rows[1].push(if s[5] { '|' } else { ' ' });
        rows[1].push(if s[6] { '_' } else { ' ' });
        rows[1].push(if s[1] { '|' } else { ' ' });
        // Row 2: e, d, c.
        rows[2].push(if s[4] { '|' } else { ' ' });
        rows[2].push(if s[3] { '_' } else { ' ' });
        rows[2].push(if s[2] { '|' } else { ' ' });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn rows_are_equal_length() {
        let rows = render("KLAWPLY");
        assert_eq!(rows[0].chars().count(), rows[1].chars().count());
        assert_eq!(rows[1].chars().count(), rows[2].chars().count());
    }

    #[test]
    fn print_banner() {
        for row in render("KLAWPLY") {
            println!("{row}");
        }
        for row in render("CONNECT") {
            println!("{row}");
        }
    }
}
