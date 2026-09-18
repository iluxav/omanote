//! The document, cursor, selection and every editing operation. Knows nothing
//! about the terminal; the UI tells it the text area size so that vertical
//! movement can follow wrapped rows.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::images::Images;
use crate::layout::{VRow, layout, locate};
use crate::markdown::{Block, classify, continuation, data_definition, task_mark};
use crate::table::{self, Role};

const UNDO_LIMIT: usize = 500;
const COALESCE: Duration = Duration::from_millis(1000);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub row: usize,
    pub col: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edit {
    Type,
    Erase,
    Other,
}

struct Snapshot {
    lines: Vec<Vec<char>>,
    cursor: Pos,
    anchor: Option<Pos>,
}

/// Where the text area was last drawn, for mouse hit-testing.
#[derive(Default)]
pub struct View {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// (line, visual row within the line) for each screen row.
    pub rows: Vec<(usize, usize)>,
}

pub struct Editor {
    pub lines: Vec<Vec<char>>,
    pub blocks: Vec<Block>,
    pub cursor: Pos,
    pub anchor: Option<Pos>,
    /// First line on screen, and how many of its visual rows are scrolled off.
    pub top: usize,
    pub top_skip: usize,
    pub images: Images,
    /// Images embedded in the note as `[label]: data:…` definitions. They are
    /// kept out of `lines` (one can be a megabyte of base64) and written back
    /// at the end of the file on save, for as long as the text refers to them.
    pub embeds: Vec<(String, String)>,
    /// (count, bytes of base64) of the embeds the text currently uses.
    pub embedded: (usize, usize),
    pub view: View,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    /// How many times this note has been written, and the file's timestamp as
    /// we last knew it (to notice when something else changes it).
    pub save_count: u64,
    pub disk_mtime: Option<std::time::SystemTime>,
    pub last_change: Instant,
    pub clipboard: String,
    goal_x: Option<u16>,
    follow: bool,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Option<(Edit, Instant, Pos)>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Editor {
    pub fn new(text: &str, path: Option<PathBuf>) -> Self {
        let mut embeds = Vec::new();
        let mut lines: Vec<Vec<char>> = Vec::new();
        for line in text.split('\n').map(|l| l.trim_end_matches('\r')) {
            match data_definition(line) {
                Some((label, uri)) => embeds.push((label, uri.to_string())),
                None => lines.push(line.chars().collect()),
            }
        }
        if lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        // The blank lines that set the definitions apart are ours, not the note's.
        while !embeds.is_empty() && lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        let blocks = classify(&lines);
        let mut editor = Editor {
            lines,
            blocks,
            cursor: Pos::default(),
            anchor: None,
            top: 0,
            top_skip: 0,
            images: Images::off(),
            embeds,
            embedded: (0, 0),
            view: View { w: 80, h: 24, ..View::default() },
            disk_mtime: path.as_ref().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok()),
            path,
            dirty: false,
            save_count: 0,
            last_change: Instant::now(),
            clipboard: String::new(),
            goal_x: None,
            follow: true,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
        };
        editor.count_embeds();
        editor
    }

    /// Embeds some `![alt][label]` in the text still points at.
    fn used_embeds(&self) -> Vec<&(String, String)> {
        let text: Vec<String> = self.lines.iter().filter(|l| l.contains(&'[')).map(|l| l.iter().collect::<String>().to_lowercase()).collect();
        self.embeds.iter().filter(|(label, _)| text.iter().any(|l| l.contains(&format!("][{label}]")))).collect()
    }

    fn count_embeds(&mut self) {
        let used = self.used_embeds();
        self.embedded = (used.len(), used.iter().map(|(_, uri)| uri.len()).sum());
    }

    /// Put an image into the note: a short `![pasted image][imgN]` line here,
    /// the data itself out of sight. Returns the label.
    pub fn paste_image(&mut self, data_uri: String) -> String {
        let taken = |label: &str| {
            self.embeds.iter().any(|(l, _)| l == label)
                || self.lines.iter().any(|l| l.first() == Some(&'[') && l.iter().collect::<String>().to_lowercase().starts_with(&format!("[{label}]:")))
        };
        let label = (1..).map(|n| format!("img{n}")).find(|l| !taken(l)).unwrap_or_default();
        self.embeds.push((label.clone(), data_uri));

        self.checkpoint(Edit::Other);
        self.remove_selection();
        let row = self.cursor.row;
        let marker: Vec<char> = format!("![pasted image][{label}]").chars().collect();
        // On an empty line the image takes it; otherwise it goes underneath. Either
        // way the cursor ends up on a fresh line below, ready for more text.
        let at = if self.lines[row].iter().all(|c| c.is_whitespace()) {
            self.lines[row] = marker;
            row
        } else {
            self.lines.insert(row + 1, marker);
            row + 1
        };
        self.lines.insert(at + 1, Vec::new());
        self.cursor = Pos { row: at + 1, col: 0 };
        self.edited(Edit::Other);
        label
    }

    pub fn with_images(mut self, images: Images) -> Self {
        self.images = images;
        self
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            out.extend(line.iter());
            out.push('\n');
        }
        let used = self.used_embeds();
        if !used.is_empty() {
            out.push('\n');
        }
        for (label, uri) in used {
            out.push_str(&format!("[{label}]: {uri}\n"));
        }
        out
    }

    pub fn save(&mut self) -> std::io::Result<bool> {
        let Some(path) = &self.path else { return Ok(false) };
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.text())?;
        self.disk_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        self.save_count += 1;
        self.dirty = false;
        Ok(true)
    }

    pub fn word_count(&self) -> usize {
        self.lines
            .iter()
            .map(|l| l.split(|c| c.is_whitespace()).filter(|w| w.iter().any(|c| c.is_alphanumeric())).count())
            .sum()
    }

    // ---- selection -------------------------------------------------------

    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        (a != self.cursor).then(|| (a.min(self.cursor), a.max(self.cursor)))
    }

    /// Lines showing raw markdown: the cursor's, plus any the selection touches.
    pub fn is_revealed(&self, row: usize) -> bool {
        row == self.cursor.row || self.selection().is_some_and(|(s, e)| s.row <= row && row <= e.row)
    }

    fn selected_text(&self) -> Option<String> {
        let (s, e) = self.selection()?;
        let mut out = String::new();
        for row in s.row..=e.row {
            let line = &self.lines[row];
            let from = if row == s.row { s.col } else { 0 };
            let to = if row == e.row { e.col } else { line.len() };
            out.extend(&line[from..to]);
            if row != e.row {
                out.push('\n');
            }
        }
        Some(out)
    }

    pub fn select_all(&mut self) {
        let last = self.lines.len() - 1;
        self.anchor = Some(Pos::default());
        self.cursor = Pos { row: last, col: self.lines[last].len() };
        self.moved();
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    // ---- movement --------------------------------------------------------

    fn moved(&mut self) {
        self.goal_x = None;
        self.follow = true;
        self.last_edit = None;
    }

    pub fn move_to(&mut self, pos: Pos, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = pos;
        self.moved();
    }

    fn len(&self, row: usize) -> usize {
        self.lines[row].len()
    }

    pub fn rows(&self, row: usize, revealed: bool) -> Vec<VRow> {
        match self.blocks[row] {
            Block::Table { start, end } => {
                table::layout_row(&self.lines[row], &self.table_ctx(row, start, end, revealed), revealed)
            }
            Block::Normal => {
                let mut rows = layout(&self.lines[row], Block::Normal, revealed, self.view.w);
                rows.extend(self.images.rows_for(&self.lines[row]));
                rows
            }
            block => layout(&self.lines[row], block, revealed, self.view.w),
        }
    }

    /// Rows the cursor can be on, i.e. without table borders.
    fn text_rows(&self, row: usize) -> Vec<VRow> {
        let mut rows = self.rows(row, true);
        rows.retain(|r| !r.virt);
        rows
    }

    fn table_role(row: usize, start: usize) -> Role {
        match row - start {
            0 => Role::Header,
            1 => Role::Separator,
            _ => Role::Body,
        }
    }

    /// Column widths come from every row of the table, each measured the way it
    /// is currently shown (raw if revealed, rendered otherwise).
    fn table_ctx(&self, row: usize, start: usize, end: usize, revealed: bool) -> table::Ctx {
        let mut widths: Vec<u16> = Vec::new();
        for r in start..end {
            let shown_raw = if r == row { revealed } else { self.is_revealed(r) };
            for (k, w) in table::measure(&self.lines[r], Self::table_role(r, start), shown_raw).into_iter().enumerate() {
                if k == widths.len() {
                    widths.push(w);
                }
                widths[k] = widths[k].max(w);
            }
        }
        table::Ctx {
            widths,
            aligns: table::aligns(&self.lines[start + 1]),
            role: Self::table_role(row, start),
            top: row == start,
            bottom: row + 1 == end,
            divider: row > start + 2,
        }
    }

    pub fn left(&mut self, extend: bool) {
        if let (false, Some((s, _))) = (extend, self.selection()) {
            return self.move_to(s, false);
        }
        let Pos { row, col } = self.cursor;
        let pos = if col > 0 {
            Pos { row, col: col - 1 }
        } else if row > 0 {
            Pos { row: row - 1, col: self.len(row - 1) }
        } else {
            self.cursor
        };
        self.move_to(pos, extend);
    }

    pub fn right(&mut self, extend: bool) {
        if let (false, Some((_, e))) = (extend, self.selection()) {
            return self.move_to(e, false);
        }
        let Pos { row, col } = self.cursor;
        let pos = if col < self.len(row) {
            Pos { row, col: col + 1 }
        } else if row + 1 < self.lines.len() {
            Pos { row: row + 1, col: 0 }
        } else {
            self.cursor
        };
        self.move_to(pos, extend);
    }

    fn word_left_pos(&self) -> Pos {
        let Pos { row, col } = self.cursor;
        if col == 0 {
            return if row > 0 { Pos { row: row - 1, col: self.len(row - 1) } } else { self.cursor };
        }
        let line = &self.lines[row];
        let mut j = col;
        while j > 0 && !is_word(line[j - 1]) {
            j -= 1;
        }
        while j > 0 && is_word(line[j - 1]) {
            j -= 1;
        }
        Pos { row, col: j }
    }

    fn word_right_pos(&self) -> Pos {
        let Pos { row, col } = self.cursor;
        let line = &self.lines[row];
        if col == line.len() {
            return if row + 1 < self.lines.len() { Pos { row: row + 1, col: 0 } } else { self.cursor };
        }
        let mut j = col;
        while j < line.len() && !is_word(line[j]) {
            j += 1;
        }
        while j < line.len() && is_word(line[j]) {
            j += 1;
        }
        Pos { row, col: j }
    }

    pub fn word_left(&mut self, extend: bool) {
        let pos = self.word_left_pos();
        self.move_to(pos, extend);
    }

    pub fn word_right(&mut self, extend: bool) {
        let pos = self.word_right_pos();
        self.move_to(pos, extend);
    }

    /// The line above/below for Up/Down. A table's `|---|` row is stepped over:
    /// it is structure, not content (click it to edit the alignment).
    fn line_beside(&self, row: usize, down: bool) -> Option<usize> {
        let step = |r: usize| if down { Some(r + 1).filter(|n| *n < self.lines.len()) } else { r.checked_sub(1) };
        let next = step(row)?;
        match self.blocks[next] {
            Block::Table { start, .. } if next == start + 1 => step(next),
            _ => Some(next),
        }
    }

    /// Move by wrapped rows, keeping the screen column the cursor started from.
    /// The destination line is laid out revealed, because it will be once we land.
    fn vertical(&mut self, down: bool, extend: bool) {
        let Pos { row, col } = self.cursor;
        let rows = self.text_rows(row);
        let vi = locate(&rows, col);
        let gx = self.goal_x.unwrap_or_else(|| rows[vi].x_of(col));
        let pos = if down {
            if vi + 1 < rows.len() {
                Pos { row, col: rows[vi + 1].col_at(gx, self.len(row)) }
            } else if let Some(next) = self.line_beside(row, true) {
                Pos { row: next, col: self.text_rows(next)[0].col_at(gx, self.len(next)) }
            } else {
                Pos { row, col: self.len(row) }
            }
        } else if vi > 0 {
            Pos { row, col: rows[vi - 1].col_at(gx, self.len(row)) }
        } else if let Some(prev) = self.line_beside(row, false) {
            let above = self.text_rows(prev);
            Pos { row: prev, col: above[above.len() - 1].col_at(gx, self.len(prev)) }
        } else {
            Pos::default()
        };
        self.move_to(pos, extend);
        self.goal_x = Some(gx);
    }

    pub fn up(&mut self, extend: bool) {
        self.vertical(false, extend);
    }

    pub fn down(&mut self, extend: bool) {
        self.vertical(true, extend);
    }

    pub fn page(&mut self, down: bool, extend: bool) {
        for _ in 0..self.view.h.saturating_sub(1).max(1) {
            self.vertical(down, extend);
        }
    }

    pub fn home(&mut self, extend: bool) {
        let rows = self.text_rows(self.cursor.row);
        let start = rows[locate(&rows, self.cursor.col)].start;
        self.move_to(Pos { row: self.cursor.row, col: start }, extend);
    }

    pub fn end(&mut self, extend: bool) {
        let rows = self.text_rows(self.cursor.row);
        let vr = &rows[locate(&rows, self.cursor.col)];
        let col = if vr.last { self.len(self.cursor.row) } else { vr.end - 1 };
        self.move_to(Pos { row: self.cursor.row, col }, extend);
    }

    pub fn doc_start(&mut self, extend: bool) {
        self.move_to(Pos::default(), extend);
    }

    pub fn doc_end(&mut self, extend: bool) {
        let last = self.lines.len() - 1;
        self.move_to(Pos { row: last, col: self.len(last) }, extend);
    }

    /// Scroll by visual rows, so tall lines (images, long paragraphs) glide past.
    pub fn scroll(&mut self, delta: isize) {
        for _ in 0..delta.unsigned_abs() {
            if delta > 0 {
                if self.top_skip + 1 < self.rows(self.top, self.is_revealed(self.top)).len() {
                    self.top_skip += 1;
                } else if self.top + 1 < self.lines.len() {
                    self.top += 1;
                    self.top_skip = 0;
                }
            } else if self.top_skip > 0 {
                self.top_skip -= 1;
            } else if self.top > 0 {
                self.top -= 1;
                self.top_skip = self.rows(self.top, self.is_revealed(self.top)).len() - 1;
            }
        }
        self.follow = false;
    }

    /// Called by the UI before drawing: record the text area and keep the cursor on screen.
    pub fn set_view(&mut self, x: u16, y: u16, w: u16, h: u16) {
        if w != self.view.w {
            self.goal_x = None;
        }
        self.view = View { x, y, w, h, rows: Vec::new() };
        self.images.prepare(&self.lines, &self.embeds, w, (h * 3 / 5).max(4));
        self.top = self.top.min(self.lines.len() - 1);
        self.top_skip = self.top_skip.min(self.rows(self.top, self.is_revealed(self.top)).len() - 1);
        if !self.follow {
            return;
        }
        let Pos { row, col } = self.cursor;
        let vi = locate(&self.rows(row, true), col);
        if (row, vi) < (self.top, self.top_skip) {
            (self.top, self.top_skip) = (row, vi);
            return;
        }
        // Walk upwards from the cursor until the screen is full: that is the
        // furthest up the view may start and still show the cursor.
        let h = h as usize;
        let mut used = vi + 1;
        let (mut t, mut skip) = (row, used.saturating_sub(h));
        while t > self.top && used < h {
            let above = self.rows(t - 1, self.is_revealed(t - 1)).len();
            skip = (used + above).saturating_sub(h);
            used += above;
            t -= 1;
        }
        if (self.top, self.top_skip) < (t, skip) {
            (self.top, self.top_skip) = (t, skip);
        }
    }

    // ---- mouse -----------------------------------------------------------

    /// Text position under a screen cell, and whether that cell is a rendered checkbox.
    pub fn pos_at(&self, sx: u16, sy: u16) -> (Pos, bool) {
        let ry = sy.saturating_sub(self.view.y) as usize;
        let Some(&(row, vi)) = self.view.rows.get(ry) else {
            let last = self.lines.len() - 1;
            return (Pos { row: last, col: self.len(last) }, false);
        };
        let revealed = self.is_revealed(row);
        let rows = self.rows(row, revealed);
        let mut vi = vi.min(rows.len() - 1);
        if rows[vi].virt {
            // Decoration (table rule, image): use the text row it belongs to.
            let below = rows[vi..].iter().position(|r| !r.virt).map(|k| vi + k);
            vi = below.or_else(|| rows[..vi].iter().rposition(|r| !r.virt)).unwrap_or(vi);
        }
        let vr = &rows[vi];
        let x = sx.saturating_sub(self.view.x);
        let checkbox = !revealed && vr.hit(x).is_some() && vr.hit(x) == task_mark(&self.lines[row]);
        (Pos { row, col: vr.col_at(x, self.len(row)) }, checkbox)
    }

    pub fn select_word_at(&mut self, pos: Pos) {
        let line = &self.lines[pos.row];
        let (mut s, mut e) = (pos.col.min(line.len()), pos.col.min(line.len()));
        while s > 0 && is_word(line[s - 1]) {
            s -= 1;
        }
        while e < line.len() && is_word(line[e]) {
            e += 1;
        }
        self.anchor = Some(Pos { row: pos.row, col: s });
        self.cursor = Pos { row: pos.row, col: e };
        self.moved();
    }

    // ---- undo ------------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot { lines: self.lines.clone(), cursor: self.cursor, anchor: self.anchor }
    }

    /// Record an undo point, unless this edit continues a run of typing or erasing.
    fn checkpoint(&mut self, kind: Edit) {
        let continues = matches!(self.last_edit, Some((k, at, pos))
            if k == kind && kind != Edit::Other && at.elapsed() < COALESCE && pos == self.cursor);
        if !continues {
            self.undo.push(self.snapshot());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
    }

    fn edited(&mut self, kind: Edit) {
        self.blocks = classify(&self.lines);
        self.images.mark_stale();
        self.count_embeds();
        self.dirty = true;
        self.last_change = Instant::now();
        self.goal_x = None;
        self.follow = true;
        self.last_edit = Some((kind, Instant::now(), self.cursor));
    }

    fn restore(&mut self, snap: Snapshot) {
        self.lines = snap.lines;
        self.cursor = snap.cursor;
        self.anchor = snap.anchor;
        self.edited(Edit::Other);
        self.last_edit = None;
    }

    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo.pop() else { return false };
        self.redo.push(self.snapshot());
        self.restore(snap);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snap) = self.redo.pop() else { return false };
        self.undo.push(self.snapshot());
        self.restore(snap);
        true
    }

    // ---- editing ---------------------------------------------------------

    fn remove_range(&mut self, s: Pos, e: Pos) {
        if s.row == e.row {
            self.lines[s.row].drain(s.col..e.col);
        } else {
            let tail = self.lines[e.row].split_off(e.col);
            self.lines[s.row].truncate(s.col);
            self.lines[s.row].extend(tail);
            self.lines.drain(s.row + 1..=e.row);
        }
        self.cursor = s;
        self.anchor = None;
    }

    fn remove_selection(&mut self) -> bool {
        match self.selection() {
            Some((s, e)) => {
                self.remove_range(s, e);
                true
            }
            None => false,
        }
    }

    fn insert_raw(&mut self, text: &str) {
        let Pos { mut row, col } = self.cursor;
        let tail = self.lines[row].split_off(col);
        let mut parts = text.split('\n');
        self.lines[row].extend(parts.next().unwrap_or("").chars());
        for part in parts {
            row += 1;
            self.lines.insert(row, part.chars().collect());
        }
        let col = self.lines[row].len();
        self.lines[row].extend(tail);
        self.cursor = Pos { row, col };
    }

    pub fn insert_char(&mut self, c: char) {
        let kind = if self.selection().is_some() { Edit::Other } else { Edit::Type };
        self.checkpoint(kind);
        self.remove_selection();
        self.lines[self.cursor.row].insert(self.cursor.col, c);
        self.cursor.col += 1;
        self.edited(kind);
        if c.is_whitespace() {
            // Undo goes back a word at a time.
            self.last_edit = None;
        }
    }

    pub fn insert_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.checkpoint(Edit::Other);
        self.remove_selection();
        self.insert_raw(&text.replace("\r\n", "\n").replace('\r', "\n"));
        self.edited(Edit::Other);
    }

    fn erase(&mut self, target: Pos) {
        let kind = if self.selection().is_some() { Edit::Other } else { Edit::Erase };
        let target = if kind == Edit::Erase { self.keep_table_intact(target) } else { target };
        if kind == Edit::Erase && target == self.cursor {
            return;
        }
        self.checkpoint(kind);
        if !self.remove_selection() {
            self.remove_range(target.min(self.cursor), target.max(self.cursor));
        }
        self.edited(kind);
    }

    /// Backspace/Delete never eat a table's pipes or join two of its rows:
    /// on the row being edited they look like grid lines, not characters, and
    /// losing one silently merges cells. Deleting a selection still can.
    fn keep_table_intact(&mut self, target: Pos) -> Pos {
        let Pos { row, col } = self.cursor;
        if self.table_at(row).is_none() {
            return target;
        }
        if target.row != row {
            return if self.table_at(target.row).is_some() { self.cursor } else { target };
        }
        let pipes = table::pipes(&self.lines[row]);
        if target.col < col {
            match pipes.iter().rfind(|p| (target.col..col).contains(*p)) {
                // Already at the start of the cell: step back into the previous one.
                Some(&p) if p + 1 == col => {
                    let k = self.cell_index(row, col);
                    if k > 0 && p > 0 {
                        self.goto_cell(row, k - 1);
                    }
                    self.cursor
                }
                Some(&p) => Pos { row, col: p + 1 },
                None => target,
            }
        } else {
            match pipes.iter().find(|p| (col..target.col).contains(*p)) {
                Some(&p) => Pos { row, col: p },
                None => target,
            }
        }
    }

    pub fn backspace(&mut self) {
        let Pos { row, col } = self.cursor;
        let target = if col > 0 {
            Pos { row, col: col - 1 }
        } else if row > 0 {
            Pos { row: row - 1, col: self.len(row - 1) }
        } else {
            self.cursor
        };
        self.erase(target);
    }

    pub fn delete(&mut self) {
        let Pos { row, col } = self.cursor;
        let target = if col < self.len(row) {
            Pos { row, col: col + 1 }
        } else if row + 1 < self.lines.len() {
            Pos { row: row + 1, col: 0 }
        } else {
            self.cursor
        };
        self.erase(target);
    }

    pub fn delete_word_back(&mut self) {
        let target = self.word_left_pos();
        self.erase(target);
    }

    pub fn delete_word_forward(&mut self) {
        let target = self.word_right_pos();
        self.erase(target);
    }

    /// Enter continues lists, numbering and quotes; on an empty item it ends the list instead.
    pub fn enter(&mut self) {
        self.checkpoint(Edit::Other);
        if self.remove_selection() {
            self.blocks = classify(&self.lines);
        }
        let Pos { row, col } = self.cursor;
        if self.enter_in_table(row, col) {
            return self.edited(Edit::Other);
        }
        match continuation(&self.lines[row]) {
            Some((_, start)) if col >= start && self.lines[row][start..].iter().all(|c| c.is_whitespace()) => {
                self.lines[row].clear();
                self.cursor.col = 0;
            }
            Some((marker, start)) if col >= start => {
                let tail = self.lines[row].split_off(col);
                let col = marker.len();
                self.lines.insert(row + 1, marker.into_iter().chain(tail).collect());
                self.cursor = Pos { row: row + 1, col };
            }
            _ => self.insert_raw("\n"),
        }
        self.edited(Edit::Other);
    }

    fn table_at(&self, row: usize) -> Option<(usize, usize)> {
        match self.blocks[row] {
            Block::Table { start, end } => Some((start, end)),
            _ => None,
        }
    }

    fn goto_cell(&mut self, row: usize, k: usize) {
        let line = &self.lines[row];
        let cells = table::cells(line);
        let col = cells.get(k).or(cells.last()).map_or(line.len(), |c| table::trim(line, c).1);
        self.move_to(Pos { row, col }, false);
    }

    fn insert_table_row(&mut self, at: usize, like: usize, columns: usize) {
        let row = table::empty_row(&self.lines[like], columns);
        self.lines.insert(at, row);
        self.blocks = classify(&self.lines);
        self.goto_cell(at, 0);
    }

    fn cell_index(&self, row: usize, col: usize) -> usize {
        table::cells(&self.lines[row]).iter().rposition(|c| c.start <= col).unwrap_or(0)
    }

    /// Enter inside a table works like a spreadsheet: down one row in the same
    /// column, adding a row at the bottom (or leaving the table from an empty
    /// last row). Enter after a lone `| a | b |` line turns it into a table.
    fn enter_in_table(&mut self, row: usize, col: usize) -> bool {
        let columns = table::cells(&self.lines[row]).len();
        if let Some((start, end)) = self.table_at(row) {
            let header_columns = table::cells(&self.lines[start]).len();
            let blank = self.lines[row].iter().all(|c| matches!(c, '|' | ' '));
            let below = if row == start { start + 2 } else { row + 1 };
            if blank && row + 1 == end && row > start + 1 {
                self.lines[row].clear();
                self.cursor.col = 0;
            } else if below < end {
                let k = self.cell_index(row, col);
                self.goto_cell(below, k);
            } else {
                self.insert_table_row(below, start, header_columns);
            }
            return true;
        }
        col == self.lines[row].len() && columns > 0 && self.start_table(row)
    }

    /// Turn a lone `| a | b |` line into a table: add the separator and a first
    /// empty row, and put the cursor in it.
    fn start_table(&mut self, row: usize) -> bool {
        let line = &self.lines[row];
        let columns = table::cells(line).len();
        let lone_header = table::is_row(line)
            && columns > 0
            && !table::is_separator(line)
            && (row == 0 || !table::is_row(&self.lines[row - 1]));
        if lone_header {
            let separator = table::separator_row(line, columns);
            self.lines.insert(row + 1, separator);
            self.insert_table_row(row + 2, row, columns);
        }
        lone_header
    }

    /// Shift+Enter: a new empty row right below this one, wherever we are in the table.
    /// On a `| a | b |` line that is not a table yet it makes it one, from any cursor position.
    pub fn add_table_row(&mut self) -> bool {
        let row = self.cursor.row;
        let Some((start, _)) = self.table_at(row) else {
            if !table::is_row(&self.lines[row]) {
                return false;
            }
            self.checkpoint(Edit::Other);
            self.anchor = None;
            let started = self.start_table(row);
            if started {
                self.edited(Edit::Other);
            }
            return started;
        };
        self.checkpoint(Edit::Other);
        self.anchor = None;
        let columns = table::cells(&self.lines[start]).len();
        self.insert_table_row(if row <= start + 1 { start + 2 } else { row + 1 }, start, columns);
        self.edited(Edit::Other);
        true
    }

    /// A new empty column beside the cursor's cell, on every row of the table.
    pub fn add_table_column(&mut self, after: bool) -> bool {
        let Pos { row, col } = self.cursor;
        let Some((start, end)) = self.table_at(row) else { return false };
        self.checkpoint(Edit::Other);
        self.anchor = None;
        let k = self.cell_index(row, col);
        for r in start..end {
            self.lines[r] = table::add_column(&self.lines[r], k, after, r == start + 1);
        }
        self.blocks = classify(&self.lines);
        self.goto_cell(row, if after { k + 1 } else { k });
        self.edited(Edit::Other);
        true
    }

    fn tab_in_table(&mut self, forward: bool) -> bool {
        let Pos { row, col } = self.cursor;
        let Some((start, end)) = self.table_at(row) else { return false };
        let cells = table::cells(&self.lines[row]);
        let k = self.cell_index(row, col);
        let skip = |r: usize| r == start + 1;
        if forward {
            let next = if skip(row + 1) { row + 2 } else { row + 1 };
            if k + 1 < cells.len() {
                self.goto_cell(row, k + 1);
            } else if next < end {
                self.goto_cell(next, 0);
            } else {
                self.checkpoint(Edit::Other);
                self.insert_table_row(end, start, table::cells(&self.lines[start]).len());
                self.edited(Edit::Other);
            }
        } else if k > 0 {
            self.goto_cell(row, k - 1);
        } else if row > start {
            let prev = if skip(row - 1) { row - 2 } else { row - 1 };
            self.goto_cell(prev, usize::MAX);
        }
        true
    }

    pub fn tab(&mut self) {
        if self.selection().is_none() && self.tab_in_table(true) {
            return;
        }
        let row = self.cursor.row;
        if self.selection().is_none() && continuation(&self.lines[row]).is_some() {
            self.checkpoint(Edit::Other);
            self.lines[row].splice(0..0, [' ', ' ']);
            self.cursor.col += 2;
            self.edited(Edit::Other);
        } else {
            self.insert_str("    ");
        }
    }

    pub fn backtab(&mut self) {
        if self.tab_in_table(false) {
            return;
        }
        let row = self.cursor.row;
        let n = self.lines[row].iter().take(2).take_while(|c| **c == ' ').count();
        if n == 0 {
            return;
        }
        self.checkpoint(Edit::Other);
        self.lines[row].drain(0..n);
        self.cursor.col = self.cursor.col.saturating_sub(n);
        self.anchor = None;
        self.edited(Edit::Other);
    }

    /// Wrap the selection in `delim` (or unwrap it if it already is). With no
    /// selection, insert an empty pair and put the cursor inside.
    pub fn toggle_wrap(&mut self, delim: &str) -> Result<(), &'static str> {
        let d: Vec<char> = delim.chars().collect();
        let n = d.len();
        let Some((s, e)) = self.selection() else {
            self.checkpoint(Edit::Other);
            self.insert_raw(&delim.repeat(2));
            self.cursor.col -= n;
            self.edited(Edit::Other);
            return Ok(());
        };
        if s.row != e.row {
            return Err("Formatting works within a single line");
        }
        self.checkpoint(Edit::Other);
        let line = &mut self.lines[s.row];
        let wrapped = s.col >= n && line[s.col - n..s.col] == d[..] && line[e.col..].starts_with(&d);
        let (s_col, e_col) = if wrapped {
            line.drain(e.col..e.col + n);
            line.drain(s.col - n..s.col);
            (s.col - n, e.col - n)
        } else {
            line.splice(e.col..e.col, d.iter().copied());
            line.splice(s.col..s.col, d.iter().copied());
            (s.col + n, e.col + n)
        };
        self.anchor = Some(Pos { row: s.row, col: s_col });
        self.cursor = Pos { row: s.row, col: e_col };
        self.edited(Edit::Other);
        Ok(())
    }

    /// Tick/untick a task; turn a bullet or plain line into a task.
    pub fn toggle_task(&mut self, row: usize) {
        self.checkpoint(Edit::Other);
        let line = &mut self.lines[row];
        let indent = line.iter().take_while(|c| **c == ' ' || **c == '\t').count();
        let added = if let Some(mark) = task_mark(line) {
            line[mark] = if line[mark] == ' ' { 'x' } else { ' ' };
            0
        } else if line.len() > indent + 1 && matches!(line[indent], '-' | '*' | '+') && line[indent + 1] == ' ' {
            line.splice(indent + 2..indent + 2, "[ ] ".chars());
            4
        } else {
            line.splice(indent..indent, "- [ ] ".chars());
            6
        };
        if row == self.cursor.row && self.cursor.col >= indent {
            self.cursor.col += added;
            self.anchor = None;
        }
        let keep_view = row != self.cursor.row;
        self.edited(Edit::Other);
        if keep_view {
            self.follow = false;
        }
    }

    // ---- clipboard -------------------------------------------------------

    /// Selected text, or the whole current line when nothing is selected.
    pub fn copy(&mut self) -> String {
        let text = self.selected_text().unwrap_or_else(|| {
            let mut line: String = self.lines[self.cursor.row].iter().collect();
            line.push('\n');
            line
        });
        self.clipboard = text.clone();
        text
    }

    pub fn cut(&mut self) -> String {
        let text = self.copy();
        self.checkpoint(Edit::Other);
        if !self.remove_selection() {
            let row = self.cursor.row;
            if self.lines.len() > 1 {
                self.lines.remove(row);
                self.cursor = Pos { row: row.min(self.lines.len() - 1), col: 0 };
            } else {
                self.lines[0].clear();
                self.cursor = Pos::default();
            }
        }
        self.edited(Edit::Other);
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        Editor::new(text, None)
    }

    fn type_str(e: &mut Editor, s: &str) {
        for c in s.chars() {
            e.insert_char(c);
        }
    }

    #[test]
    fn typing_undo_redo_goes_word_by_word() {
        let mut e = ed("");
        type_str(&mut e, "hello world");
        assert_eq!(e.text(), "hello world\n");
        assert!(e.undo());
        assert_eq!(e.text(), "hello \n");
        assert!(e.undo());
        assert_eq!(e.text(), "\n");
        assert!(!e.undo());
        assert!(e.redo() && e.redo());
        assert_eq!(e.text(), "hello world\n");
        assert_eq!(e.cursor, Pos { row: 0, col: 11 });
    }

    #[test]
    fn typing_replaces_selection_across_lines() {
        let mut e = ed("one\ntwo\nthree");
        e.move_to(Pos { row: 0, col: 1 }, false);
        e.move_to(Pos { row: 2, col: 2 }, true);
        e.insert_char('X');
        assert_eq!(e.text(), "oXree\n");
        e.undo();
        assert_eq!(e.text(), "one\ntwo\nthree\n");
    }

    #[test]
    fn backspace_joins_lines_and_paste_splits_them() {
        let mut e = ed("ab\ncd");
        e.move_to(Pos { row: 1, col: 0 }, false);
        e.backspace();
        assert_eq!((e.text().as_str(), e.cursor), ("abcd\n", Pos { row: 0, col: 2 }));
        e.insert_str("1\r\n2\n3");
        assert_eq!((e.text().as_str(), e.cursor), ("ab1\n2\n3cd\n", Pos { row: 2, col: 1 }));
    }

    #[test]
    fn enter_continues_and_ends_lists() {
        let mut e = ed("- [x] done");
        e.doc_end(false);
        e.enter();
        assert_eq!(e.text(), "- [x] done\n- [ ] \n");
        e.enter();
        assert_eq!(e.text(), "- [x] done\n\n");
        let mut e = ed("1. first");
        e.doc_end(false);
        e.enter();
        type_str(&mut e, "second");
        assert_eq!(e.text(), "1. first\n2. second\n");
    }

    #[test]
    fn bold_toggles_on_and_off() {
        let mut e = ed("make this loud");
        e.move_to(Pos { row: 0, col: 5 }, false);
        e.move_to(Pos { row: 0, col: 9 }, true);
        e.toggle_wrap("**").unwrap();
        assert_eq!(e.text(), "make **this** loud\n");
        assert_eq!(e.selection(), Some((Pos { row: 0, col: 7 }, Pos { row: 0, col: 11 })));
        e.toggle_wrap("**").unwrap();
        assert_eq!(e.text(), "make this loud\n");
    }

    #[test]
    fn task_toggle_cycles() {
        let mut e = ed("buy milk");
        e.toggle_task(0);
        assert_eq!(e.text(), "- [ ] buy milk\n");
        e.toggle_task(0);
        assert_eq!(e.text(), "- [x] buy milk\n");
        e.toggle_task(0);
        assert_eq!(e.text(), "- [ ] buy milk\n");
    }

    #[test]
    fn vertical_movement_follows_wrapped_rows() {
        let mut e = ed("alpha beta gamma delta epsilon\nshort");
        e.set_view(0, 0, 12, 10);
        e.move_to(Pos { row: 0, col: 3 }, false);
        e.down(false);
        assert_eq!(e.cursor, Pos { row: 0, col: 14 });
        e.down(false);
        e.down(false);
        assert_eq!(e.cursor, Pos { row: 1, col: 3 });
        e.up(false);
        assert_eq!(e.cursor, Pos { row: 0, col: 26 });
    }

    #[test]
    fn typing_a_header_row_and_enter_builds_a_table() {
        let mut e = ed("");
        type_str(&mut e, "| Name | Qty |");
        e.enter();
        assert_eq!(e.text(), "| Name | Qty |\n| --- | --- |\n|  |  |\n");
        assert_eq!(e.cursor, Pos { row: 2, col: 2 });
        type_str(&mut e, "tea");
        e.tab();
        type_str(&mut e, "2");
        e.tab();
        assert_eq!(e.text(), "| Name | Qty |\n| --- | --- |\n| tea | 2 |\n|  |  |\n");
        e.backtab();
        assert_eq!(e.cursor, Pos { row: 2, col: 9 });
        e.doc_end(false);
        e.enter();
        assert_eq!(e.text(), "| Name | Qty |\n| --- | --- |\n| tea | 2 |\n\n");
    }

    #[test]
    fn enter_moves_down_and_shift_enter_inserts_a_row() {
        let mut e = ed("| a | b |\n|---|---|\n| c | d |\n| e | f |");
        e.move_to(Pos { row: 0, col: 7 }, false);
        e.enter();
        assert_eq!(e.cursor, Pos { row: 2, col: 7 }, "header -> first body row, same column");
        e.enter();
        assert_eq!(e.cursor, Pos { row: 3, col: 7 });
        assert_eq!(e.lines.len(), 4, "moving down adds nothing");
        e.move_to(Pos { row: 2, col: 3 }, false);
        assert!(e.add_table_row());
        assert_eq!(e.text(), "| a | b |\n|---|---|\n| c | d |\n|  |  |\n| e | f |\n");
        assert_eq!(e.cursor, Pos { row: 3, col: 2 });
        e.undo();
        assert_eq!(e.lines.len(), 4);
        let mut plain = ed("not a table");
        assert!(!plain.add_table_row() && !plain.add_table_column(true));
    }

    #[test]
    fn shift_enter_with_the_cursor_anywhere_on_a_header_line() {
        // Not a table yet, cursor on the "a".
        let mut e = ed("| a | b |");
        e.move_to(Pos { row: 0, col: 2 }, false);
        assert!(e.add_table_row());
        assert_eq!(e.text(), "| a | b |\n| --- | --- |\n|  |  |\n");
        assert_eq!(e.cursor, Pos { row: 2, col: 2 });
        // Already a table, cursor on the header's "a": new first body row.
        let mut e = ed("| a | b |\n|---|---|\n| c | d |");
        e.move_to(Pos { row: 0, col: 2 }, false);
        assert!(e.add_table_row());
        assert_eq!(e.text(), "| a | b |\n|---|---|\n|  |  |\n| c | d |\n");
        assert_eq!(e.cursor, Pos { row: 2, col: 2 });
        // Plain Enter mid-line on a lone header still just splits the line.
        let mut e = ed("| a | b |");
        e.move_to(Pos { row: 0, col: 2 }, false);
        e.enter();
        assert_eq!(e.text(), "| \na | b |\n");
    }

    #[test]
    fn adds_a_column_beside_the_cursor_on_every_row() {
        let mut e = ed("| a | b |\n|---|--:|\n| c | d |\n| short |");
        e.move_to(Pos { row: 2, col: 3 }, false);
        assert!(e.add_table_column(true));
        assert_eq!(e.text(), "| a |  | b |\n|---| --- |--:|\n| c |  | d |\n| short |  |\n");
        assert_eq!(e.cursor, Pos { row: 2, col: 6 }, "cursor lands in the new cell");
        assert!(e.add_table_column(false));
        assert_eq!(e.lines[0].iter().collect::<String>(), "| a |  |  | b |");
        e.undo();
        e.undo();
        assert_eq!(e.text(), "| a | b |\n|---|--:|\n| c | d |\n| short |\n");
    }

    #[test]
    fn arrows_skip_table_borders_and_separator() {
        let mut e = ed("above\n| a | b |\n|---|---|\n| c | d |\nbelow");
        e.set_view(0, 0, 40, 20);
        assert_eq!(e.rows(1, false).len(), 2);
        assert_eq!(e.rows(3, false).len(), 2);
        let three = ed("| a |\n|---|\n| b |\n| c |\n| d |");
        assert_eq!(three.rows(2, false).len(), 1, "first body row sits right under the header rule");
        assert_eq!(three.rows(3, false).len(), 2, "divider + row");
        assert_eq!(three.rows(4, false).len(), 3, "divider + row + bottom border");
        e.move_to(Pos { row: 0, col: 2 }, false);
        for expected in [1, 3, 4] {
            e.down(false);
            assert_eq!(e.cursor.row, expected);
        }
        for expected in [3, 1, 0] {
            e.up(false);
            assert_eq!(e.cursor.row, expected);
        }
    }

    #[test]
    fn backspace_and_delete_cannot_break_a_table() {
        let src = "| ab |  |\n|---|---|\n| c | d |\n";
        let mut e = ed(src);
        // In the empty second cell: backspace all the way, the pipes survive and
        // the cursor ends up back in the first cell.
        e.move_to(Pos { row: 0, col: 7 }, false);
        for _ in 0..4 {
            e.backspace();
        }
        assert_eq!(e.lines[0].iter().collect::<String>(), "|  | |", "space, (hop to first cell), b, a");
        // Delete stops at the closing pipe of the cell.
        e.move_to(Pos { row: 2, col: 2 }, false);
        for _ in 0..5 {
            e.delete();
        }
        assert_eq!(e.lines[2].iter().collect::<String>(), "| | d |");
        // Word-delete is clamped to the cell too.
        e.move_to(Pos { row: 2, col: 4 }, false);
        e.delete_word_back();
        assert_eq!(e.lines[2].iter().collect::<String>(), "| |d |");
        // Rows do not join.
        e.move_to(Pos { row: 2, col: 0 }, false);
        e.backspace();
        e.move_to(Pos { row: 0, col: 7 }, false);
        e.delete();
        assert_eq!(e.lines.len(), 3);
        assert!(e.blocks.iter().all(|b| matches!(b, Block::Table { .. })));
    }

    #[test]
    fn view_scrolls_by_visual_rows() {
        let para = "word ".repeat(40);
        let mut e = ed(&format!("{para}\nlast"));
        e.set_view(0, 0, 20, 5);
        let tall = e.rows(0, true).len();
        assert!(tall > 6);
        e.doc_end(false);
        e.set_view(0, 0, 20, 5);
        assert_eq!((e.top, e.top_skip), (0, tall - 4), "the long line is only partly scrolled off");
        e.scroll(-2);
        assert_eq!((e.top, e.top_skip), (0, tall - 6));
        e.scroll(100);
        assert_eq!((e.top, e.top_skip), (1, 0));
        e.doc_start(false);
        e.set_view(0, 0, 20, 5);
        assert_eq!((e.top, e.top_skip), (0, 0));
    }

    const PIXEL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==";

    #[test]
    fn pasted_images_live_outside_the_text() {
        let mut e = ed("intro\n\noutro");
        e.move_to(Pos { row: 0, col: 2 }, false);
        assert_eq!(e.paste_image(PIXEL.into()), "img1");
        assert_eq!(e.lines.len(), 5, "marker and a fresh line go under the current one; the data is not a line");
        assert_eq!(e.cursor, Pos { row: 2, col: 0 });
        assert_eq!(e.text(), format!("intro\n![pasted image][img1]\n\n\noutro\n\n[img1]: {PIXEL}\n"));
        assert_eq!(e.embedded, (1, PIXEL.len()));

        // On an empty line the marker takes that line; labels do not repeat.
        assert_eq!(e.paste_image(PIXEL.into()), "img2");
        assert_eq!(e.lines[2].iter().collect::<String>(), "![pasted image][img2]");

        // Deleting the marker drops the image from the file; undo brings it back.
        e.undo();
        e.move_to(Pos { row: 1, col: 0 }, false);
        e.cut();
        assert_eq!(e.text(), "intro\n\n\noutro\n");
        assert_eq!(e.embedded, (0, 0));
        e.undo();
        assert!(e.text().ends_with(&format!("[img1]: {PIXEL}\n")));
    }

    #[test]
    fn embedded_images_round_trip_through_the_file() {
        let file = format!("# Note\n\n![shot][Img1]\n\ntext\n\n[img1]: {PIXEL}\n[logo]: ./logo.png\n");
        let mut e = ed(&file);
        let shown: Vec<String> = e.lines.iter().map(|l| l.iter().collect()).collect();
        assert_eq!(shown, ["# Note", "", "![shot][Img1]", "", "text", "", "[logo]: ./logo.png"], "only the data line is lifted out");
        assert_eq!(e.embedded.0, 1);
        assert_eq!(e.paste_image(PIXEL.into()), "img2", "img1 is taken");
        assert_eq!(e.text().matches("data:image/png").count(), 2);
        assert_eq!(ed(&e.text()).text(), e.text(), "saving and reopening changes nothing");
    }

    #[test]
    fn cut_without_selection_takes_the_line() {
        let mut e = ed("a\nb\nc");
        e.move_to(Pos { row: 1, col: 0 }, false);
        assert_eq!(e.cut(), "b\n");
        assert_eq!(e.text(), "a\nc\n");
    }
}
