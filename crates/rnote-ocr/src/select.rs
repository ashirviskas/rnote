//! Taking the text out of an area of a document.

// Imports
use crate::{Bounds, CharBox, Line};

/// The part of a line that is inside the area.
struct Piece {
    /// The left edge of the first character taken.
    x: f64,
    /// The vertical middle of the line.
    middle: f64,
    /// The height of the line.
    height: f64,
    text: String,
}

impl Piece {
    /// Whether both pieces are on one row of text: two columns, or the pieces a recogniser splits a row into.
    fn same_row(&self, other: &Self) -> bool {
        (self.middle - other.middle).abs() < self.height.min(other.height) / 2.0
    }
}

/// The text of the lines inside the area, cut at its edges.
///
/// A line takes part when its vertical middle is inside the area, and gives the characters whose horizontal middle
/// is inside it. The lines are put top to bottom, the ones on one row left to right with a space between them.
pub fn text_in(lines: &[Line], area: Bounds) -> String {
    let inside = |middle: f64, start: f64, length: f64| (start..=start + length).contains(&middle);

    let mut pieces = lines
        .iter()
        .filter(|line| inside(line.bounds.y + line.bounds.h / 2.0, area.y, area.h))
        .filter_map(|line| {
            let chars = line
                .chars
                .iter()
                .filter(|c| inside((c.x0 + c.x1) / 2.0, area.x, area.w))
                .collect::<Vec<&CharBox>>();
            let text = chars
                .iter()
                .filter_map(|c| c.top())
                .collect::<String>()
                .trim()
                .to_string();
            (!text.is_empty()).then(|| Piece {
                x: chars[0].x0,
                middle: line.bounds.y + line.bounds.h / 2.0,
                height: line.bounds.h,
                text,
            })
        })
        .collect::<Vec<Piece>>();
    pieces.sort_by(|a, b| a.middle.total_cmp(&b.middle));

    let mut rows: Vec<Vec<Piece>> = Vec::new();
    for piece in pieces {
        match rows.last_mut() {
            Some(row) if row[0].same_row(&piece) => row.push(piece),
            _ => rows.push(vec![piece]),
        }
    }
    rows.into_iter()
        .map(|mut row| {
            row.sort_by(|a, b| a.x.total_cmp(&b.x));
            row.into_iter()
                .map(|piece| piece.text)
                .collect::<Vec<String>>()
                .join(" ")
        })
        .collect::<Vec<String>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line with characters 10 wide and 10 high.
    fn line(text: &str, x: f64, y: f64) -> Line {
        let chars = text
            .chars()
            .enumerate()
            .map(|(i, ch)| CharBox::exact(ch, x + i as f64 * 10.0, x + (i + 1) as f64 * 10.0))
            .collect::<Vec<CharBox>>();
        Line {
            bounds: Bounds {
                x,
                y,
                w: chars.len() as f64 * 10.0,
                h: 10.0,
            },
            chars,
        }
    }

    fn area(x: f64, y: f64, w: f64, h: f64) -> Bounds {
        Bounds { x, y, w, h }
    }

    /// `ABC` above `DEF`.
    fn abc_def() -> Vec<Line> {
        vec![line("ABC", 0.0, 0.0), line("DEF", 0.0, 20.0)]
    }

    #[test]
    fn lines_are_cut_at_the_edges_of_the_area() {
        // The right two columns. A is touched, but its middle is outside.
        assert_eq!(text_in(&abc_def(), area(8.0, -5.0, 30.0, 40.0)), "BC\nEF");
        // The left two columns of the lower line
        assert_eq!(text_in(&abc_def(), area(-5.0, 18.0, 22.0, 14.0)), "DE");
    }

    #[test]
    fn an_area_around_everything_gives_all_lines() {
        assert_eq!(
            text_in(&abc_def(), area(-5.0, -5.0, 100.0, 100.0)),
            "ABC\nDEF"
        );
    }

    #[test]
    fn an_area_that_touches_nothing_gives_no_text() {
        assert_eq!(text_in(&abc_def(), area(50.0, 0.0, 30.0, 30.0)), "");
        assert_eq!(text_in(&abc_def(), area(0.0, 40.0, 30.0, 30.0)), "");
        assert_eq!(text_in(&[], area(0.0, 0.0, 30.0, 30.0)), "");
    }

    #[test]
    fn a_line_takes_part_when_its_middle_is_in_the_area() {
        // Reaches from above down to just past the middle of the upper line
        assert_eq!(text_in(&abc_def(), area(-5.0, -5.0, 100.0, 11.0)), "ABC");
        // Covers a sliver at the top of it only
        assert_eq!(text_in(&abc_def(), area(-5.0, -5.0, 100.0, 7.0)), "");
    }

    #[test]
    fn lines_on_one_row_are_joined_left_to_right_with_a_space() {
        // Two columns, the right one a little lower, and given first
        let lines = vec![
            line("右", 100.0, 2.0),
            line("左", 0.0, 0.0),
            line("下", 0.0, 20.0),
        ];
        assert_eq!(text_in(&lines, area(-5.0, -5.0, 200.0, 100.0)), "左 右\n下");
    }

    #[test]
    fn lines_are_ordered_top_to_bottom() {
        let lines = vec![
            line("3", 0.0, 40.0),
            line("1", 0.0, 0.0),
            line("2", 0.0, 20.0),
        ];
        assert_eq!(text_in(&lines, area(-5.0, -5.0, 100.0, 100.0)), "1\n2\n3");
    }

    #[test]
    fn a_cut_line_is_trimmed_and_a_blank_one_dropped() {
        let lines = vec![line("A B  C", 0.0, 0.0), line("   ", 0.0, 20.0)];
        // From the first space to the two spaces
        assert_eq!(text_in(&lines, area(10.0, -5.0, 40.0, 40.0)), "B");
    }
}
