//! Drawing: a centred text column, a status line and a nano-style hint bar.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::editor::{Editor, Pos};
use crate::layout::{VRow, locate};
use crate::picker::{Picker, Row, age};

const MAX_WIDTH: u16 = 84;
const PICKER_ROWS: usize = 8;

/// Where the text column goes: (x, y, width, height). `None` if the terminal is too small.
pub fn text_area(area: Rect) -> Option<(u16, u16, u16, u16)> {
    if area.height < 5 || area.width < 24 {
        return None;
    }
    let w = (area.width - 4).min(MAX_WIDTH);
    Some(((area.width - w) / 2, 1, w, area.height - 3))
}

pub fn draw(f: &mut Frame, ed: &mut Editor, picker: Option<&Picker>, toast: Option<&str>, enhanced_keys: bool) {
    let area = f.area();
    let Some((x, _, w, h)) = text_area(area) else { return };
    ed.set_view(x, 1, w, h);

    let sel = ed.selection();
    let mut out: Vec<Line> = Vec::new();
    let mut map = Vec::new();
    let mut cursor_xy = None;
    let mut row = ed.top;
    let mut skip = ed.top_skip;
    while row < ed.lines.len() && out.len() < h as usize {
        let vrows = ed.rows(row, ed.is_revealed(row));
        let cursor_vi = (row == ed.cursor.row).then(|| locate(&vrows, ed.cursor.col));
        for (vi, vr) in vrows.iter().enumerate().skip(std::mem::take(&mut skip)) {
            if out.len() >= h as usize {
                break;
            }
            if cursor_vi == Some(vi) {
                let cx = (x + vr.x_of(ed.cursor.col)).min(area.width - 1);
                cursor_xy = Some((cx, 1 + out.len() as u16));
            }
            out.push(render_row(vr, row, ed.lines[row].len(), sel));
            map.push((row, vi));
        }
        row += 1;
    }
    ed.view.rows = map;

    if ed.lines.len() == 1 && ed.lines[0].is_empty() {
        out[0] = Line::styled("Start writing…", Style::new().fg(Color::DarkGray));
    }

    f.render_widget(Paragraph::new(out), Rect::new(x, 1, area.width - x, h));
    if let Some(xy) = cursor_xy {
        f.set_cursor_position(xy);
    }

    draw_status(f, ed, toast, Rect::new(x, area.height - 2, w, 1));
    draw_hints(f, picker.is_some(), enhanced_keys, Rect::new(x, area.height - 1, w, 1));
    if let Some(picker) = picker {
        draw_picker(f, picker, x, w, area.height - 1);
    }
}

/// A panel growing up from the hint bar: title rule, query line, results.
fn draw_picker(f: &mut Frame, p: &Picker, x: u16, w: u16, bottom: u16) {
    let dim = Style::new().fg(Color::DarkGray);
    let shown = p.len().clamp(1, PICKER_ROWS).min(bottom.saturating_sub(3) as usize);
    let h = shown as u16 + 2;
    let area = Rect::new(x, bottom - h, w, h);
    f.render_widget(Clear, Rect::new(0, area.y, f.area().width, h));

    let root = p.title();
    // A long vault path loses its front, not the note count.
    let room = (w as usize).saturating_sub(24).max(8);
    let count = root.chars().count();
    let root = if count > room { format!("…{}", root.chars().skip(count - room + 1).collect::<String>()) } else { root };
    let title = format!("── Notes · {root} · {} ", p.total());
    let rule = "─".repeat((w as usize).saturating_sub(title.chars().count()));
    let mut lines = vec![
        Line::styled(format!("{title}{rule}"), dim),
        Line::from(vec![Span::styled("› ", Style::new().fg(Color::Magenta)), Span::raw(p.query.clone())]),
    ];

    // Keep the selection inside the window of visible rows.
    let first = p.selected.saturating_sub(shown - 1);
    for i in first..first + shown {
        let selected = i == p.selected;
        let pick = |style: Style| if selected { style.add_modifier(Modifier::REVERSED) } else { style };
        let mut spans = vec![Span::styled(if selected { " ▸ " } else { "   " }, pick(Style::default()))];
        let mut right = String::new();
        match p.row(i) {
            Some(Row::Note(note, hits)) => {
                let name_start = note.name.iter().rposition(|c| *c == '/').map_or(0, |k| k + 1);
                for (k, c) in note.name.iter().enumerate() {
                    let style = if hits.contains(&k) {
                        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    } else if k < name_start {
                        dim
                    } else {
                        Style::default()
                    };
                    spans.push(Span::styled(c.to_string(), pick(style)));
                }
                right = age(note.modified);
            }
            Some(Row::Create(name)) => {
                spans.push(Span::styled(format!("+ Create “{name}”"), pick(Style::new().fg(Color::Green))));
            }
            None => {
                let msg = if p.total() == 0 { "No notes here yet — type a name and press Enter" } else { "No matches" };
                spans.push(Span::styled(msg, dim));
            }
        }
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let fill = (w as usize).saturating_sub(used + right.chars().count() + 1);
        spans.push(Span::styled(" ".repeat(fill), pick(Style::default())));
        spans.push(Span::styled(format!("{right} "), pick(dim)));
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
    let cx = x + 2 + unicode_width::UnicodeWidthStr::width(p.query.as_str()) as u16;
    f.set_cursor_position((cx.min(x + w - 1), area.y + 1));
}

fn render_row(vr: &VRow, row: usize, line_len: usize, sel: Option<(Pos, Pos)>) -> Line<'static> {
    let selected = |col: usize| sel.is_some_and(|(s, e)| s <= Pos { row, col } && Pos { row, col } < e);
    let mut spans: Vec<Span> = vr.lead.iter().map(|(t, s)| Span::styled(t.clone(), *s)).collect();
    let mut run = String::new();
    let mut run_style = Style::default();
    for cell in &vr.cells {
        let style = if selected(cell.col) { cell.style.add_modifier(Modifier::REVERSED) } else { cell.style };
        if style != run_style && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), run_style));
        }
        run_style = style;
        run.push_str(&cell.text);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_style));
    }
    // Show a selected line break as one highlighted cell.
    if vr.last && selected(line_len) {
        spans.push(Span::styled(" ", Style::new().add_modifier(Modifier::REVERSED)));
    }
    Line::from(spans)
}

pub fn human(bytes: usize) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{} KB", bytes / 1024),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

fn draw_status(f: &mut Frame, ed: &Editor, toast: Option<&str>, area: Rect) {
    let dim = Style::new().fg(Color::DarkGray);
    let name = match &ed.path {
        Some(p) => p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()),
        None => "demo (not saved anywhere)".to_string(),
    };
    let left = match toast {
        Some(msg) => Line::from(Span::styled(msg.to_string(), Style::new().fg(Color::Yellow))),
        None => Line::from(vec![
            Span::styled(name, Style::new().add_modifier(Modifier::BOLD)),
            Span::styled(if ed.dirty { "  ● unsaved" } else { "" }, dim),
        ]),
    };
    let images = match ed.embedded {
        (0, _) => String::new(),
        (1, bytes) => format!("1 image, {}  ·  ", human(bytes)),
        (n, bytes) => format!("{n} images, {}  ·  ", human(bytes)),
    };
    let right = format!("{images}Ln {}, Col {}  ·  {} words", ed.cursor.row + 1, ed.cursor.col + 1, ed.word_count());
    f.render_widget(Paragraph::new(left), area);
    f.render_widget(Paragraph::new(Line::styled(right, dim)).right_aligned(), area);
}

fn draw_hints(f: &mut Frame, picking: bool, enhanced_keys: bool, area: Rect) {
    let mut hints = vec![("^Q", "Quit"), ("^P", "Open"), ("^S", "Save"), ("^Z", "Undo"), ("^Y", "Redo"), ("^C", "Copy"), ("^X", "Cut"), ("^V", "Paste"), ("^B", "Bold")];
    if enhanced_keys {
        // Without the kitty keyboard protocol Ctrl+I is indistinguishable from Tab.
        hints.push(("^I", "Italic"));
    }
    hints.push(("^T", "Task"));
    if picking {
        hints = vec![("↑↓", "Select"), ("Enter", "Open"), ("Esc", "Cancel"), ("^U", "Clear")];
    }
    let mut spans = Vec::new();
    let mut used = 0;
    for (key, label) in hints {
        // Drop whole hints that do not fit rather than cutting one in half.
        used += key.chars().count() + label.len() + 3;
        if used > area.width as usize + 2 {
            break;
        }
        spans.push(Span::styled(key, Style::new().add_modifier(Modifier::REVERSED)));
        spans.push(Span::styled(format!(" {label}  "), Style::new().fg(Color::DarkGray)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
