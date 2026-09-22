//! Turns one source line into wrapped visual rows, and maps between source
//! columns and screen x positions. Concealment lives here: a revealed line
//! shows every source character, any other line drops hidden cells and swaps
//! in replacement glyphs.

use ratatui::style::Style;
use unicode_width::UnicodeWidthStr;

use crate::markdown::{Block, style_line};

pub struct Cell {
    /// Source column this cell was produced from.
    pub col: usize,
    pub text: String,
    pub width: u16,
    pub style: Style,
    /// Stands for exactly one source character. Padding and table rules are not
    /// solid: they repeat a column number and must not attract the cursor.
    pub solid: bool,
}

pub struct VRow {
    /// Virtual text before the cells: code gutter, hanging indent.
    pub lead: Vec<(String, Style)>,
    pub lead_w: u16,
    pub cells: Vec<Cell>,
    /// Source columns `[start, end)` that belong to this row.
    pub start: usize,
    pub end: usize,
    pub last: bool,
    /// Decoration with no source behind it (table borders); the cursor never lands here.
    pub virt: bool,
}

impl VRow {
    pub fn x_of(&self, col: usize) -> u16 {
        self.lead_w + self.cells.iter().filter(|c| c.col < col).map(|c| c.width).sum::<u16>()
    }

    /// The cell under screen offset `x`, if any.
    pub fn hit(&self, x: u16) -> Option<usize> {
        let mut acc = self.lead_w;
        for cell in &self.cells {
            if x >= acc && x < acc + cell.width {
                return Some(cell.col);
            }
            acc += cell.width;
        }
        None
    }

    /// Nearest cursor column for screen offset `x`.
    pub fn col_at(&self, x: u16, line_len: usize) -> usize {
        if x < self.lead_w {
            return self.start;
        }
        match self.hit(x) {
            Some(col) => col,
            None if self.last => line_len,
            // Wrapped rows end in the space they broke on; sit just before it.
            None => self.cells.last().map_or(self.start, |c| c.col),
        }
    }
}

/// Index of the visual row that holds cursor column `col`: the row showing
/// that character, or else the nearest character before it. (Not simply "the
/// row whose range contains it": a wrapped table row interleaves the ranges of
/// its cells across its visual rows.)
pub fn locate(rows: &[VRow], col: usize) -> usize {
    let mut best: Option<(usize, usize)> = None;
    for (i, row) in rows.iter().enumerate().filter(|(_, r)| !r.virt) {
        for cell in row.cells.iter().filter(|c| c.solid && c.col <= col) {
            if best.is_none_or(|(at, _)| cell.col > at) {
                best = Some((cell.col, i));
            }
        }
    }
    best.map(|(_, i)| i).or_else(|| rows.iter().position(|r| !r.virt)).unwrap_or(0)
}

pub fn layout(chars: &[char], block: Block, revealed: bool, width: u16) -> Vec<VRow> {
    let width = width.max(8);
    let n = chars.len();
    let sl = style_line(chars, block);
    let prefix: Vec<(String, Style)> = sl.prefix.iter().map(|(t, s)| (t.to_string(), *s)).collect();
    let prefix_w = sl.prefix.map_or(0, |(t, _)| t.width() as u16);

    if sl.rule && !revealed {
        let cell = Cell { col: 0, text: "─".repeat(width as usize), width, style: crate::look::of(crate::look::El::Rule), solid: true };
        return vec![VRow { lead: prefix, lead_w: prefix_w, cells: vec![cell], start: 0, end: n, last: true, virt: false }];
    }

    let mut cells = Vec::with_capacity(n);
    for (col, &ch) in chars.iter().enumerate() {
        let cc = &sl.cells[col];
        if !revealed && cc.hidden {
            continue;
        }
        let text = match (ch, cc.repl) {
            ('\t', _) => "    ".to_string(),
            (_, Some(repl)) if !revealed => repl.to_string(),
            _ => ch.to_string(),
        };
        let width = text.width() as u16;
        cells.push(Cell { col, text, width, style: cc.style, solid: true });
    }

    let mut hang_w: u16 = cells.iter().filter(|c| c.col < sl.hang_col).map(|c| c.width).sum();
    if prefix_w + hang_w + 8 > width {
        hang_w = 0;
    }

    // Greedy word wrap. Spaces never trigger a break, so a row may overhang by
    // its trailing whitespace rather than start the next row with a space.
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut cur: Vec<Cell> = Vec::new();
    let mut cur_w = 0u16;
    let mut brk: Option<usize> = None;
    for cell in cells {
        let is_space = cell.text == " ";
        let avail = width - prefix_w - if rows.is_empty() { 0 } else { hang_w };
        if !is_space && !cur.is_empty() && cur_w + cell.width > avail {
            let tail = brk.map(|b| cur.split_off(b + 1)).unwrap_or_default();
            rows.push(std::mem::replace(&mut cur, tail));
            cur_w = cur.iter().map(|c| c.width).sum();
            brk = None;
        }
        if is_space {
            brk = Some(cur.len());
        }
        cur_w += cell.width;
        cur.push(cell);
    }
    rows.push(cur);

    let starts: Vec<usize> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| if i == 0 { 0 } else { row.first().map_or(n, |c| c.col) })
        .collect();
    let count = rows.len();
    rows.into_iter()
        .enumerate()
        .map(|(i, cells)| {
            let mut lead = prefix.clone();
            let mut lead_w = prefix_w;
            if i > 0 && hang_w > 0 {
                if sl.quote && !revealed {
                    lead.push(("▎".to_string(), crate::look::of(crate::look::El::QuoteBar)));
                    lead.push((" ".repeat(hang_w as usize - 1), Style::default()));
                } else {
                    lead.push((" ".repeat(hang_w as usize), Style::default()));
                }
                lead_w += hang_w;
            }
            VRow {
                lead,
                lead_w,
                cells,
                start: starts[i],
                end: starts.get(i + 1).copied().unwrap_or(n),
                last: i + 1 == count,
                virt: false,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(src: &str, revealed: bool, width: u16) -> Vec<VRow> {
        layout(&src.chars().collect::<Vec<_>>(), Block::Normal, revealed, width)
    }

    fn text(row: &VRow) -> String {
        row.cells.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn conceals_only_when_not_revealed() {
        assert_eq!(text(&rows("# Hi **you**", false, 40)[0]), "Hi you");
        assert_eq!(text(&rows("# Hi **you**", true, 40)[0]), "# Hi **you**");
    }

    #[test]
    fn wraps_at_words_and_covers_every_column() {
        let src = "alpha beta gamma delta epsilon";
        let r = rows(src, true, 12);
        assert_eq!(r.iter().map(text).collect::<Vec<_>>(), ["alpha beta ", "gamma delta ", "epsilon"]);
        assert_eq!(r[0].start, 0);
        assert_eq!(r[1].start, 11);
        assert_eq!(r[2].end, src.len());
        assert!(r[2].last && !r[0].last);
    }

    #[test]
    fn cursor_mapping_round_trips_on_revealed_rows() {
        let src = "alpha beta gamma delta epsilon";
        let r = rows(src, true, 12);
        for col in 0..=src.len() {
            let row = &r[locate(&r, col)];
            assert_eq!(row.col_at(row.x_of(col), src.len()), col, "col {col}");
        }
    }

    #[test]
    fn list_rows_hang_under_their_text() {
        let r = rows("- one two three four five six", false, 14);
        assert!(r.len() > 1);
        assert_eq!(r[0].lead_w, 0);
        assert_eq!(r[1].lead_w, 2);
    }

    #[test]
    fn click_on_concealed_line_maps_to_source_column() {
        // "a [link](u) b" renders as "a link b"; x=2 is the 'l' at source col 3.
        let r = rows("a [link](u) b", false, 40);
        assert_eq!(r[0].col_at(2, 13), 3);
        assert_eq!(r[0].col_at(99, 13), 13);
    }

    #[test]
    fn empty_and_rule_lines() {
        let r = rows("", false, 20);
        assert_eq!((r.len(), r[0].cells.len(), r[0].end), (1, 0, 0));
        assert_eq!(rows("---", false, 20)[0].cells[0].width, 20);
        assert_eq!(text(&rows("---", true, 20)[0]), "---");
    }
}
