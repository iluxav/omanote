//! Pipe tables. Unlike everything else these need context from neighbouring
//! lines (column widths, alignment), so the editor builds a `Ctx` for the
//! table and each row is laid out against it.
//!
//! A concealed row is drawn as an aligned grid. A revealed row shows its raw
//! source but is padded with virtual cells, so it stays inside the grid while
//! being edited and the columns grow as you type.

use std::ops::Range;

use ratatui::style::Style;
use unicode_width::UnicodeWidthStr;

use crate::layout::{Cell, VRow};
use crate::look::{self, El};
use crate::markdown::{CharCell, indent, inline, marker};

const MIN_WIDTH: u16 = 3;
/// A column squeezed to fit the window never gets narrower than this.
const MIN_FITTED: u16 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Header,
    Separator,
    Body,
}

pub struct Ctx {
    /// Inner width of each column, including one space of padding either side.
    pub widths: Vec<u16>,
    pub aligns: Vec<Align>,
    pub role: Role,
    pub top: bool,
    pub bottom: bool,
    /// Draw a rule above this row (every body row after the first).
    pub divider: bool,
}

/// Columns of the unescaped pipes.
pub fn pipes(chars: &[char]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '|' => out.push(i),
            _ => {}
        }
        i += 1;
    }
    out
}

pub fn is_row(chars: &[char]) -> bool {
    chars.get(indent(chars)) == Some(&'|')
}

/// Source range of each cell's content: between pipes, plus a last cell that
/// has not been closed yet (common while typing).
pub fn cells(chars: &[char]) -> Vec<Range<usize>> {
    let p = pipes(chars);
    let mut out: Vec<Range<usize>> = p.windows(2).map(|w| w[0] + 1..w[1]).collect();
    if let Some(&last) = p.last() {
        if chars[last + 1..].iter().any(|c| !c.is_whitespace()) {
            out.push(last + 1..chars.len());
        }
    }
    out
}

/// A cell's content without surrounding whitespace. For a blank cell both ends
/// sit one space in, which is where the cursor belongs.
pub fn trim(chars: &[char], cell: &Range<usize>) -> (usize, usize) {
    let text = &chars[cell.clone()];
    match text.iter().position(|c| !c.is_whitespace()) {
        Some(first) => {
            let last = text.iter().rposition(|c| !c.is_whitespace()).unwrap_or(first);
            (cell.start + first, cell.start + last + 1)
        }
        None => {
            let at = cell.start + text.len().min(1);
            (at, at)
        }
    }
}

fn separator_align(chars: &[char], cell: &Range<usize>) -> Option<Align> {
    let (a, b) = trim(chars, cell);
    let text = &chars[a..b];
    let left = text.first() == Some(&':');
    let right = text.len() > 1 && text.last() == Some(&':');
    let dashes = &text[left as usize..text.len() - right as usize];
    if dashes.is_empty() || dashes.iter().any(|c| *c != '-') {
        return None;
    }
    Some(match (left, right) {
        (true, true) => Align::Center,
        (false, true) => Align::Right,
        _ => Align::Left,
    })
}

pub fn is_separator(chars: &[char]) -> bool {
    let cs = cells(chars);
    is_row(chars) && !cs.is_empty() && cs.iter().all(|c| separator_align(chars, c).is_some())
}

pub fn aligns(separator: &[char]) -> Vec<Align> {
    cells(separator).iter().map(|c| separator_align(separator, c).unwrap_or(Align::Left)).collect()
}

fn styled(chars: &[char], role: Role) -> Vec<CharCell> {
    let mut out = vec![CharCell::default(); chars.len()];
    let text = look::of(El::Text);
    let base = if role == Role::Header { text.patch(look::of(El::TableHeader)) } else { text };
    for cell in cells(chars) {
        if role == Role::Separator {
            for c in &mut out[cell] {
                c.style = marker();
            }
        } else {
            let (a, b) = trim(chars, &cell);
            inline(chars, a, b, base, &mut out);
        }
    }
    out
}

fn char_width(c: char) -> u16 {
    if c == '\t' { 4 } else { c.to_string().as_str().width() as u16 }
}

/// Visible cells for source columns `a..b`.
fn content(chars: &[char], cc: &[CharCell], a: usize, b: usize, revealed: bool) -> Vec<Cell> {
    (a..b)
        .filter(|&col| revealed || !cc[col].hidden)
        .map(|col| {
            let text = match (chars[col], cc[col].repl) {
                ('\t', _) => "    ".to_string(),
                (_, Some(repl)) if !revealed => repl.to_string(),
                (c, _) => c.to_string(),
            };
            Cell { col, width: text.as_str().width() as u16, text, style: cc[col].style, solid: true }
        })
        .collect()
}

/// Inner width each cell of this row needs.
pub fn measure(chars: &[char], role: Role, revealed: bool) -> Vec<u16> {
    let cc = styled(chars, role);
    cells(chars)
        .iter()
        .map(|cell| {
            let w = if revealed {
                chars[cell.clone()].iter().map(|c| char_width(*c)).sum()
            } else if role == Role::Separator {
                0
            } else {
                let (a, b) = trim(chars, cell);
                2 + content(chars, &cc, a, b, false).iter().map(|c| c.width).sum::<u16>()
            };
            w.max(MIN_WIDTH)
        })
        .collect()
}

pub fn empty_row(chars_like: &[char], columns: usize) -> Vec<char> {
    let lead: String = chars_like[..indent(chars_like)].iter().collect();
    format!("{lead}|{}", "  |".repeat(columns)).chars().collect()
}

pub fn separator_row(chars_like: &[char], columns: usize) -> Vec<char> {
    let lead: String = chars_like[..indent(chars_like)].iter().collect();
    format!("{lead}|{}", " --- |".repeat(columns)).chars().collect()
}

/// The row with an empty column inserted before or after cell `k`. Short rows
/// are filled out and an unclosed last cell is closed first, so the new cell
/// lands in the same column on every row.
pub fn add_column(chars: &[char], k: usize, after: bool, separator: bool) -> Vec<char> {
    let filler = if separator { " --- |" } else { "  |" };
    let mut line: Vec<char> = chars.to_vec();
    while line.last().is_some_and(|c| c.is_whitespace()) {
        line.pop();
    }
    if line.last() != Some(&'|') {
        line.extend(" |".chars());
    }
    while cells(&line).len() <= k {
        line.extend(filler.chars());
    }
    let cell = &cells(&line)[k];
    let at = if after { cell.end + 1 } else { cell.start };
    line.splice(at..at, filler.chars());
    line
}

fn pad(col: usize, n: u16, fill: &str, style: Style) -> Cell {
    Cell { col, text: fill.repeat(n as usize), width: n, style, solid: false }
}

fn border(chars: &[char], ctx: &Ctx, left: &str, mid: &str, right: &str) -> VRow {
    let bars: Vec<String> = ctx.widths.iter().map(|w| "─".repeat(*w as usize)).collect();
    let text = format!("{}{left}{}{right}", " ".repeat(indent(chars)), bars.join(mid));
    let lead_w = text.as_str().width() as u16;
    VRow { lead: vec![(text, look::of(El::TableBorder))], lead_w, cells: Vec::new(), start: 0, end: 0, last: false, virt: true }
}

/// Shrink columns so the table fits in `avail` cells of content (borders not
/// counted). Narrow columns keep their width; the wide ones share what is left.
pub fn fit(natural: &[u16], avail: u16) -> Vec<u16> {
    let mut widths = natural.to_vec();
    if natural.iter().map(|w| *w as u32).sum::<u32>() <= avail as u32 {
        return widths;
    }
    let mut order: Vec<usize> = (0..natural.len()).collect();
    order.sort_by_key(|&i| natural[i]);
    let mut left = avail;
    for (done, &i) in order.iter().enumerate() {
        let share = left / (natural.len() - done) as u16;
        widths[i] = natural[i].min(share.max(MIN_FITTED));
        left = left.saturating_sub(widths[i]);
    }
    widths
}

/// Break a cell's text into lines, at spaces where it can: the first of at
/// most `first` cells, the others of `rest`. The space a line breaks on is not
/// drawn (it is still in the source).
fn wrap(cells: Vec<Cell>, first: u16, rest: u16) -> Vec<Vec<Cell>> {
    let mut lines: Vec<Vec<Cell>> = Vec::new();
    let mut cur: Vec<Cell> = Vec::new();
    let mut cur_w = 0u16;
    let mut brk: Option<usize> = None;
    for cell in cells {
        let space = cell.text == " ";
        let width = if lines.is_empty() { first } else { rest }.max(1);
        if !cur.is_empty() && cur_w + cell.width > width {
            let tail = match (space, brk) {
                (true, _) | (false, None) => Vec::new(),
                (false, Some(b)) => cur.split_off(b).split_off(1),
            };
            lines.push(std::mem::replace(&mut cur, tail));
            cur_w = cur.iter().map(|c| c.width).sum();
            brk = None;
            if space {
                continue;
            }
        }
        if space {
            brk = Some(cur.len());
        }
        cur_w += cell.width;
        cur.push(cell);
    }
    // A dropped trailing space must not leave an empty line behind.
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

pub fn layout_row(chars: &[char], ctx: &Ctx, revealed: bool) -> Vec<VRow> {
    let n = chars.len();
    let p = pipes(chars);
    let cs = cells(chars);
    let cc = styled(chars, ctx.role);
    let columns = ctx.widths.len();
    let ruled = ctx.role == Role::Separator && !revealed;
    let glyph = |slot: usize| match (ruled, slot) {
        (false, _) => "│",
        (true, 0) => "├",
        (true, s) if s == columns => "┤",
        (true, _) => "┼",
    };
    let bar = |slot: usize| {
        let col = p.get(slot).copied().unwrap_or(n);
        Cell { col, text: glyph(slot).to_string(), width: 1, style: look::of(El::TableBorder), solid: false }
    };

    // Each column as lines of cells, every line exactly the column's width.
    let mut columns_lines: Vec<Vec<Vec<Cell>>> = Vec::with_capacity(columns);
    for (k, &w) in ctx.widths.iter().enumerate() {
        let lines = match cs.get(k) {
            None => vec![vec![pad(n, w, if ruled { "─" } else { " " }, look::of(El::TableBorder))]],
            Some(cell) if ruled => vec![vec![pad(cell.start, w, "─", look::of(El::TableBorder))]],
            Some(cell) if revealed => {
                // Raw source, then virtual padding out to the column edge.
                // The source brings its own leading space; continuation lines get
                // a virtual one so they do not sit against the grid line.
                let lines = wrap(content(chars, &cc, cell.start, cell.end, true), w, w.saturating_sub(1));
                let count = lines.len();
                lines
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut line)| {
                        if i > 0 {
                            // It stands in for the space the line broke on, one column back.
                            let at = line.first().map_or(cell.end, |c| c.col).saturating_sub(1);
                            line.insert(0, pad(at, 1, " ", Style::default()));
                        }
                        let used: u16 = line.iter().map(|c| c.width).sum();
                        // Padding sits at the line's last character (the cell's
                        // end on the last line), so a click on it lands there.
                        let at = if i + 1 == count { cell.end } else { line.last().map_or(cell.end, |c| c.col) };
                        line.push(pad(at, w.saturating_sub(used), " ", Style::default()));
                        line
                    })
                    .collect()
            }
            Some(cell) => {
                let (a, b) = trim(chars, cell);
                let lines = wrap(content(chars, &cc, a, b, false), w.saturating_sub(2), w.saturating_sub(2));
                let count = lines.len();
                lines
                    .into_iter()
                    .enumerate()
                    .map(|(i, text)| {
                        let free = w.saturating_sub(2 + text.iter().map(|c| c.width).sum::<u16>());
                        let left = match ctx.aligns.get(k) {
                            Some(Align::Right) => free,
                            Some(Align::Center) => free / 2,
                            _ => 0,
                        };
                        let first = text.first().map_or(a, |c| c.col);
                        let last = if i + 1 == count { b } else { text.last().map_or(b, |c| c.col) };
                        let mut line = vec![pad(first, 1 + left, " ", Style::default())];
                        line.extend(text);
                        line.push(pad(last, 1 + free - left, " ", Style::default()));
                        line
                    })
                    .collect()
            }
        };
        columns_lines.push(lines);
    }

    let height = columns_lines.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let mut rows = Vec::new();
    if ctx.top {
        rows.push(border(chars, ctx, "╭", "┬", "╮"));
    }
    if ctx.divider {
        rows.push(border(chars, ctx, "├", "┼", "┤"));
    }
    for r in 0..height {
        let mut out = content(chars, &cc, 0, p.first().copied().unwrap_or(0), true);
        for (k, lines) in columns_lines.iter_mut().enumerate() {
            out.push(bar(k));
            match lines.get_mut(r) {
                Some(line) => out.append(line),
                None => out.push(pad(cs.get(k).map_or(n, |c| c.end), ctx.widths[k], " ", Style::default())),
            }
        }
        out.push(bar(columns));
        if let (true, 0, Some(&close)) = (revealed, r, p.get(columns)) {
            out.extend(content(chars, &cc, close + 1, n, true));
        }
        out.retain(|c| c.width > 0);
        rows.push(VRow { lead: Vec::new(), lead_w: 0, cells: out, start: 0, end: n, last: r + 1 == height, virt: false });
    }
    if ctx.bottom {
        rows.push(border(chars, ctx, "╰", "┴", "╯"));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn text(row: &VRow) -> String {
        let lead: String = row.lead.iter().map(|(t, _)| t.as_str()).collect();
        lead + &row.cells.iter().map(|c| c.text.as_str()).collect::<String>()
    }

    #[test]
    fn recognises_rows_and_separators() {
        assert!(is_separator(&ch("|---|:-:|--:|")));
        assert!(is_separator(&ch("  | --- | --- |")));
        assert!(!is_separator(&ch("| a | b |")));
        assert!(!is_separator(&ch("|")));
        assert_eq!(aligns(&ch("|---|:-:|--:|")), [Align::Left, Align::Center, Align::Right]);
        assert_eq!(cells(&ch("| a \\| b | c")).len(), 2);
    }

    fn table(src: &[&str], revealed: Option<usize>) -> Vec<String> {
        let lines: Vec<Vec<char>> = src.iter().map(|s| ch(s)).collect();
        let role = |i| match i {
            0 => Role::Header,
            1 => Role::Separator,
            _ => Role::Body,
        };
        let mut widths: Vec<u16> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            for (k, w) in measure(line, role(i), revealed == Some(i)).into_iter().enumerate() {
                if k == widths.len() {
                    widths.push(w);
                }
                widths[k] = widths[k].max(w);
            }
        }
        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let ctx = Ctx { widths: widths.clone(), aligns: aligns(&lines[1]), role: role(i), top: i == 0, bottom: i + 1 == lines.len(), divider: i > 2 };
            out.extend(layout_row(line, &ctx, revealed == Some(i)).iter().map(text));
        }
        out
    }

    #[test]
    fn adds_columns_on_every_kind_of_row() {
        let add = |src: &str, k, after, sep| add_column(&ch(src), k, after, sep).iter().collect::<String>();
        assert_eq!(add("| a | b |", 0, true, false), "| a |  | b |");
        assert_eq!(add("| a | b |", 0, false, false), "|  | a | b |");
        assert_eq!(add("| a | b |", 1, true, false), "| a | b |  |");
        assert_eq!(add("|---|:-:|", 1, true, true), "|---|:-:| --- |");
        assert_eq!(add("| a | unclosed", 1, true, false), "| a | unclosed |  |");
        assert_eq!(add("| short |", 2, true, false), "| short |  |  |  |");
    }

    #[test]
    fn renders_an_aligned_grid() {
        let got = table(&["| Key | Action |", "|---|--:|", "| `^P` | **open** |", "| x |"], None);
        assert_eq!(
            got,
            [
                "╭─────┬────────╮",
                "│ Key │ Action │",
                "├─────┼────────┤",
                "│ ^P  │   open │",
                "├─────┼────────┤",
                "│ x   │        │",
                "╰─────┴────────╯",
            ]
        );
    }

    #[test]
    fn narrow_columns_keep_their_width_and_wide_ones_share() {
        assert_eq!(fit(&[10, 20, 30], 80), [10, 20, 30], "fits: untouched");
        assert_eq!(fit(&[10, 60, 60], 70), [10, 30, 30]);
        assert_eq!(fit(&[5, 100], 45), [5, 40]);
        assert_eq!(fit(&[40, 40, 40], 12), [8, 8, 8], "never below the minimum, even if that overflows");
    }

    fn squeezed(src: &[&str], widths: Vec<u16>, row: usize, revealed: bool) -> Vec<VRow> {
        let lines: Vec<Vec<char>> = src.iter().map(|s| ch(s)).collect();
        let role = match row {
            0 => Role::Header,
            1 => Role::Separator,
            _ => Role::Body,
        };
        let ctx = Ctx { widths, aligns: aligns(&lines[1]), role, top: false, bottom: false, divider: false };
        layout_row(&lines[row], &ctx, revealed)
    }

    const WIDE: [&str; 3] = ["| Part | Notes |", "|---|---|", "| CPU | six cores and twelve threads in one socket |"];

    #[test]
    fn long_cells_wrap_inside_their_column() {
        let rows = squeezed(&WIDE, vec![6, 18], 2, false);
        assert_eq!(
            rows.iter().map(text).collect::<Vec<_>>(),
            [
                "│ CPU  │ six cores and    │",
                "│      │ twelve threads   │",
                "│      │ in one socket    │",
            ]
        );
        assert!(rows[2].last && !rows[0].last);

        // The line being edited wraps too, and keeps the same outline.
        let raw = squeezed(&WIDE, vec![6, 18], 2, true);
        assert_eq!(raw[0].cells.iter().map(|c| c.text.as_str()).collect::<String>(), "│ CPU  │ six cores and    │");
        assert!(raw.iter().all(|r| text(r).chars().count() == 27), "{:?}", raw.iter().map(text).collect::<Vec<_>>());
        // A word longer than the column is cut rather than breaking the grid.
        let cut = squeezed(&["| a |", "|---|", "| Supercalifragilistic |"], vec![10], 2, false);
        assert_eq!(cut.iter().map(text).collect::<Vec<_>>(), ["│ Supercal │", "│ ifragili │", "│ stic     │"]);
    }

    #[test]
    fn the_cursor_finds_its_line_inside_a_wrapped_cell() {
        use crate::layout::locate;
        let src = WIDE[2];
        let rows = squeezed(&WIDE, vec![6, 18], 2, true);
        let at = |word: &str| src.find(word).unwrap();

        assert_eq!(locate(&rows, at("CPU")), 0);
        assert_eq!(locate(&rows, at("six")), 0);
        assert_eq!(locate(&rows, at("twelve")), 1);
        assert_eq!(locate(&rows, at("socket")), 2);
        assert_eq!(locate(&rows, src.len()), 2, "the end of the line is the end of the last cell");
        // x positions line up with what is drawn: "│ CPU  │ " is 9 cells.
        assert_eq!(rows[1].x_of(at("twelve")), 9);
        // (the raw line is a character wider than the rendered one, so it breaks differently)
        assert_eq!(rows[2].cells.iter().filter(|c| c.solid).map(|c| c.text.as_str()).collect::<String>().trim(), "one socket");
        assert_eq!(rows[2].x_of(at("socket")), 9 + "one ".len() as u16);
        // Clicking text lands on it; clicking the padding after a wrapped line stays on that line.
        assert_eq!(rows[1].col_at(9, src.len()), at("twelve"));
        let past_line_one = rows[0].col_at(25, src.len());
        assert_eq!(locate(&rows, past_line_one), 0);
        // Every character of the source is reachable and maps back to itself.
        for col in 0..src.len() {
            let row = &rows[locate(&rows, col)];
            if row.cells.iter().any(|c| c.solid && c.col == col) {
                assert_eq!(row.col_at(row.x_of(col), src.len()), col, "col {col}");
            }
        }
    }

    #[test]
    fn revealed_row_stays_in_the_grid() {
        let got = table(&["| Key | Action |", "|---|---|", "| `^P` | **open** |"], Some(2));
        assert_eq!(got[3], "│ `^P` │ **open** │");
        let widths: Vec<usize> = got.iter().map(|r| r.chars().count()).collect();
        assert!(widths.iter().all(|w| *w == widths[0]), "{got:?}");
    }

    #[test]
    fn cursor_maps_through_virtual_padding() {
        let lines = [ch("| a | b |"), ch("|---|---|"), ch("| long cell | x |")];
        let widths = vec![11, 3];
        let ctx = Ctx { widths, aligns: aligns(&lines[1]), role: Role::Header, top: false, bottom: false, divider: false };
        let row = &layout_row(&lines[0], &ctx, true)[0];
        // "│ a         │ b │": col 4 is the second pipe, col 5 the space after it.
        assert_eq!(row.x_of(3), 3);
        assert_eq!(row.x_of(4), 4);
        assert_eq!(row.x_of(5), 13);
        assert_eq!(row.col_at(8, 9), 4);
        assert_eq!(row.col_at(13, 9), 5);
    }
}
