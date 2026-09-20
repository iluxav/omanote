//! Drawing: a centred text column, a status line and a nano-style hint bar.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::agents::Chooser;
use crate::config::{Align, Config};
use crate::editor::{Editor, Pos};
use crate::layout::{VRow, locate};
use crate::pane::Pane;
use crate::picker::{Picker, Row, age};
use crate::saveas::{After, SaveAs};
use crate::theme;
use crate::vaults::tilde;

const PICKER_ROWS: usize = 8;

/// Where the text column goes: (x, y, width, height), and how wide a table
/// may get (it can spill past the column into free window on its right).
/// `None` if the terminal is too small.
pub fn text_area(area: Rect, config: &Config) -> Option<(u16, u16, u16, u16, u16)> {
    if area.height < 5 || area.width < 24 {
        return None;
    }
    let margin = config.margin.min(area.width / 4);
    let room = area.width - 2 * margin;
    let w = if config.width == 0 { room } else { config.width.clamp(20, room.max(20)).min(room) };
    let x = match config.align {
        Align::Left => margin,
        Align::Center => (area.width - w) / 2,
        Align::Right => area.width - margin - w,
    };
    // Rows: 1 blank on top, the text, 1 to breathe, status, hints.
    Some((x, 1, w, area.height - 4, area.width - margin - x))
}

/// With the assistant open the window is two columns: the note, and the
/// assistant's terminal. Too narrow for both, and whichever has the keyboard
/// gets the whole window. Returns (note area, pane area).
pub fn split(full: Rect, pane_open: bool, pane_focused: bool) -> (Rect, Option<Rect>) {
    if !pane_open {
        return (full, None);
    }
    if full.width < 96 {
        let nothing = Rect::new(full.x, full.y, 0, full.height);
        return if pane_focused { (nothing, Some(full)) } else { (full, None) };
    }
    let left = full.width * 56 / 100;
    (Rect::new(full.x, full.y, left, full.height), Some(Rect::new(full.x + left, full.y, full.width - left, full.height)))
}

/// Where the assistant's terminal itself goes inside its pane: under the title
/// row, right of the rule.
pub fn pane_screen(pane: Rect) -> Rect {
    Rect::new(pane.x + 2, pane.y + 1, pane.width.saturating_sub(3), pane.height.saturating_sub(1))
}

fn draw_pane(f: &mut Frame, pane: &Pane, area: Rect, focused: bool) -> Option<(u16, u16)> {
    let look = theme::get();
    f.render_widget(Clear, area);
    let rule = Style::new().fg(if focused { Color::Blue } else { Color::DarkGray });
    for y in area.y..area.y + area.height {
        f.render_widget(Paragraph::new(Line::styled("│", if focused { rule } else { look.faint() })), Rect::new(area.x, y, 1, 1));
    }
    let hint = match (pane.scrolled(), focused) {
        (0, true) => "^G back to the note".to_string(),
        (0, false) => "^G to the assistant".to_string(),
        (n, _) => format!("↑ {n} lines back · any key returns"),
    };
    let title = Line::from(vec![
        Span::styled(format!(" {} ", pane.label), Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(hint, look.muted()),
    ]);
    f.render_widget(Paragraph::new(title), Rect::new(area.x + 1, area.y, area.width.saturating_sub(1), 1));
    let cursor = pane.draw(f.buffer_mut(), pane_screen(area));
    cursor.filter(|_| focused)
}

#[allow(clippy::too_many_arguments)]
pub fn draw(
    f: &mut Frame,
    ed: &mut Editor,
    picker: Option<&Picker>,
    save_as: Option<&SaveAs>,
    toast: Option<&str>,
    sync: Option<&str>,
    config: &Config,
    enhanced_keys: bool,
    assistant: Option<(&Pane, bool)>,
    chooser: Option<&Chooser>,
) {
    let (area, pane_area) = split(f.area(), assistant.is_some(), assistant.is_some_and(|(_, focused)| focused));
    if let (Some((pane, focused)), Some(rect)) = (assistant, pane_area) {
        if let Some(xy) = draw_pane(f, pane, rect, focused) {
            f.set_cursor_position(xy);
        }
    }
    // The note may have no room at all (a narrow window with the assistant in front).
    if area.width == 0 {
        return;
    }
    let typing_in_pane = assistant.is_some_and(|(_, focused)| focused);
    let Some((x, _, w, h, table_w)) = text_area(area, config) else { return };
    ed.table_w = table_w;
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
        out[0] = Line::styled("Start writing…", theme::get().muted());
    }

    f.render_widget(Paragraph::new(out), Rect::new(x, 1, area.width - x, h));
    if let Some(xy) = cursor_xy.filter(|_| !typing_in_pane) {
        f.set_cursor_position(xy);
    }

    // The footer: a band across the whole window, set apart from the page.
    let look = theme::get();
    f.render_widget(Block::new().style(look.surface()), Rect::new(0, area.height - 2, area.width, 2));
    if !look.rich {
        let rule = Line::styled("─".repeat(w as usize), look.faint());
        f.render_widget(Paragraph::new(rule), Rect::new(x, area.height - 3, w, 1));
    }
    draw_status(f, ed, toast, sync, Rect::new(x, area.height - 2, w, 1));
    let hints = match (save_as, picker) {
        _ if chooser.is_some() => Some(vec![("↑↓", "select"), ("Enter", "open"), ("1-9", "pick"), ("Esc", "cancel")]),
        (Some(p), _) if p.moving.is_some() && p.keep_original => Some(vec![("Enter", "copy"), ("^K", "move instead"), ("↑↓", "where"), ("Esc", "cancel")]),
        (Some(p), _) if p.moving.is_some() => Some(vec![("Enter", "move"), ("^K", "copy instead"), ("↑↓", "where"), ("Esc", "cancel")]),
        (Some(p), _) if matches!(p.after, After::Stay) => Some(vec![("Enter", "save"), ("↑↓", "vault"), ("Esc", "cancel")]),
        (Some(_), _) => Some(vec![("Enter", "save"), ("↑↓", "vault"), ("^D", "discard"), ("Esc", "cancel")]),
        (None, Some(_)) => Some(vec![("↑↓", "select"), ("Enter", "open"), ("Esc", "cancel"), ("^U", "clear")]),
        (None, None) => None,
    };
    draw_hints(f, hints, enhanced_keys && ed.markdown, ed.markdown, Rect::new(x, area.height - 1, w, 1));
    if let Some(chooser) = chooser {
        draw_chooser(f, chooser, x, w, area.height - 1, area.width);
    } else if let Some(prompt) = save_as {
        draw_save_as(f, prompt, toast, x, w, area.height - 1, area.width);
    } else if let Some(picker) = picker {
        draw_picker(f, picker, x, w, area.height - 1, area.width);
    }
}

/// A drawer rising from the hint bar, on the same surface as the footer so the
/// two read as one piece. Returns where its contents go.
fn drawer(f: &mut Frame, title: &str, note: &str, x: u16, w: u16, bottom: u16, rows: u16, band: u16) -> Rect {
    let look = theme::get();
    let h = rows + 1;
    let y = bottom.saturating_sub(h);
    let full = Rect::new(0, y, band, h);
    f.render_widget(Clear, full);
    f.render_widget(Block::new().style(look.surface()), full);

    // ── Title ─────────────────────────────── note ──
    let note = if note.is_empty() { String::new() } else { format!(" {note} ") };
    let used = title.chars().count() + note.chars().count() + 6;
    let rule = "─".repeat((w as usize).saturating_sub(used));
    let head = Line::from(vec![
        Span::styled("── ", look.faint()),
        Span::styled(title.to_string(), Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {rule}"), look.faint()),
        Span::styled(note, look.muted()),
        Span::styled("──", look.faint()),
    ]);
    f.render_widget(Paragraph::new(head).style(look.surface()), Rect::new(x, y, w, 1));
    Rect::new(x, y + 1, w, rows)
}

/// Ctrl+G with more than one agent installed: which one?
fn draw_chooser(f: &mut Frame, c: &Chooser, x: u16, w: u16, bottom: u16, band: u16) {
    let look = theme::get();
    let shown = c.agents.len().clamp(1, PICKER_ROWS).min(bottom.saturating_sub(4) as usize);
    let area = drawer(f, "Assistant", "opens beside the note", x, w, bottom, shown as u16, band);
    let first = c.selected.saturating_sub(shown - 1);
    let lines: Vec<Line> = c
        .agents
        .iter()
        .enumerate()
        .skip(first)
        .take(shown)
        .map(|(i, agent)| {
            let key = if i < 9 { format!("{}  ", i + 1) } else { "   ".to_string() };
            // The program it runs, without the placeholders, as a quiet reminder.
            let runs = agent.command.split_whitespace().next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
            list_row(i == c.selected, vec![Span::styled(key, look.muted()), Span::raw(agent.name.clone())], runs, w)
        })
        .collect();
    f.render_widget(Paragraph::new(lines).style(look.surface()), area);
}

/// One row of a drawer list: marker, text, and a quiet note at the right edge.
fn list_row(selected: bool, mut text: Vec<Span<'static>>, note: String, w: u16) -> Line<'static> {
    let look = theme::get();
    // On a reversed (fallback) selection, dimmed text would vanish: keep it plain.
    let pick = |style: Style| match (selected, look.rich) {
        (false, _) => style,
        (true, true) => style.patch(look.raised()),
        (true, false) => Style::new().add_modifier(style.add_modifier).patch(look.raised()),
    };
    let marker = if selected { Span::styled(" ▸ ", pick(Style::new().fg(Color::Magenta))) } else { Span::raw("   ") };
    for span in &mut text {
        span.style = pick(span.style);
    }
    let used: usize = 3 + text.iter().map(|s| s.content.chars().count()).sum::<usize>();
    let fill = (w as usize).saturating_sub(used + note.chars().count() + 1);
    let mut spans = vec![marker];
    spans.extend(text);
    spans.push(Span::styled(" ".repeat(fill), pick(Style::default())));
    spans.push(Span::styled(format!("{note} "), pick(look.muted())));
    Line::from(spans)
}

/// Same place and shape as the picker: title rule, the name being typed, the vaults.
fn draw_save_as(f: &mut Frame, p: &SaveAs, toast: Option<&str>, x: u16, w: u16, bottom: u16, band: u16) {
    let look = theme::get();
    let shown = p.vaults.len().clamp(1, PICKER_ROWS).min(bottom.saturating_sub(5) as usize);
    let title = match p.after {
        After::Stay if p.moving.is_some() && p.keep_original => "Copy note",
        After::Stay if p.moving.is_some() => "Move note",
        After::Stay => "Save note",
        After::Quit => "Save before quitting?",
        After::Open(_) | After::New => "Save this note first?",
    };
    let note = if p.moving.is_some() && p.keep_original { "the original stays" } else { "" };
    let area = drawer(f, title, note, x, w, bottom, shown as u16 + 2, band);

    let label = "   Name  ";
    let mut lines = vec![
        Line::from(vec![Span::styled(label, look.muted()), Span::raw(p.name.clone()), Span::styled(format!(".{}", p.ext), look.muted())]),
        match toast {
            Some(msg) => Line::styled(format!("   {msg}"), Style::new().fg(Color::Yellow)),
            None => Line::styled(if p.moving.is_some() { "   To" } else { "   In" }, look.muted()),
        },
    ];
    let first = p.selected.saturating_sub(shown - 1);
    for (i, vault) in p.vaults.iter().enumerate().skip(first).take(shown) {
        let kind = match (&vault.github, i) {
            _ if p.stays == Some(i) => "where it is now".to_string(),
            _ if p.here == Some(i) => "current folder".to_string(),
            (Some(repo), _) => format!("github · {repo}"),
            (None, 0) => "default".to_string(),
            (None, _) => String::new(),
        };
        // A long path loses its front: the end is what tells vaults apart.
        let room = (w as usize).saturating_sub(kind.chars().count() + 6).max(8);
        let path = tilde(&vault.path);
        let count = path.chars().count();
        let path = if count > room { format!("…{}", path.chars().skip(count - room + 1).collect::<String>()) } else { path };
        lines.push(list_row(i == p.selected, vec![Span::raw(path)], kind, w));
    }
    f.render_widget(Paragraph::new(lines).style(look.surface()), area);
    let cx = x + label.chars().count() as u16 + unicode_width::UnicodeWidthStr::width(p.name.as_str()) as u16;
    f.set_cursor_position((cx.min(x + w - 1), area.y));
}

/// The note finder: what you typed, then the matches.
fn draw_picker(f: &mut Frame, p: &Picker, x: u16, w: u16, bottom: u16, band: u16) {
    let look = theme::get();
    let shown = p.len().clamp(1, PICKER_ROWS).min(bottom.saturating_sub(4) as usize);
    let notes = match p.total() {
        1 => "1 note".to_string(),
        n => format!("{n} notes"),
    };
    // A long vault path loses its front, not the note count.
    let room = (w as usize).saturating_sub(notes.chars().count() + 24).max(8);
    let place = p.title();
    let count = place.chars().count();
    let place = if count > room { format!("…{}", place.chars().skip(count - room + 1).collect::<String>()) } else { place };
    let area = drawer(f, "Open note", &format!("{place} · {notes}"), x, w, bottom, shown as u16 + 1, band);

    let prompt = " ›  ";
    let mut lines = vec![Line::from(vec![Span::styled(prompt, Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)), Span::raw(p.query.clone())])];

    // Keep the selection inside the window of visible rows.
    let first = p.selected.saturating_sub(shown - 1);
    for i in first..first + shown {
        let (text, note) = match p.row(i) {
            Some(Row::Note(note, hits)) => {
                // The folder part is quiet, the name is not, what matched stands out.
                let name_start = note.name.iter().rposition(|c| *c == '/').map_or(0, |k| k + 1);
                let spans = note
                    .name
                    .iter()
                    .enumerate()
                    .map(|(k, c)| {
                        let style = if hits.contains(&k) {
                            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                        } else if k < name_start {
                            look.muted()
                        } else {
                            Style::default()
                        };
                        Span::styled(c.to_string(), style)
                    })
                    .collect();
                (spans, age(note.modified))
            }
            Some(Row::Create(name)) => (vec![Span::styled(format!("+ New note “{name}”"), Style::new().fg(Color::Green))], String::new()),
            None => {
                let msg = if p.total() == 0 { "No notes yet — type a name and press Enter" } else { "Nothing matches" };
                (vec![Span::styled(msg, look.muted())], String::new())
            }
        };
        lines.push(list_row(i == p.selected && p.row(i).is_some(), text, note, w));
    }
    f.render_widget(Paragraph::new(lines).style(look.surface()), area);
    let cx = x + prompt.chars().count() as u16 + unicode_width::UnicodeWidthStr::width(p.query.as_str()) as u16;
    f.set_cursor_position((cx.min(x + w - 1), area.y));
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

fn draw_status(f: &mut Frame, ed: &Editor, toast: Option<&str>, sync: Option<&str>, area: Rect) {
    let look = theme::get();
    let name = match &ed.path {
        Some(p) => p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()),
        None => "untitled".to_string(),
    };
    let left = match toast {
        Some(msg) => Line::from(Span::styled(msg.to_string(), Style::new().fg(Color::Yellow))),
        None => Line::from(vec![
            Span::styled(name, Style::new().add_modifier(Modifier::BOLD)),
            Span::styled(if ed.dirty { "  ● unsaved" } else { "" }, Style::new().fg(Color::Yellow)),
        ]),
    };

    // Right side: quiet facts, with the one that may need attention in colour.
    let gap = || Span::styled("   ", look.muted());
    let mut right: Vec<Span> = Vec::new();
    if let Some(state) = sync {
        let style = match state {
            "to sync" => Style::new().fg(Color::Yellow),
            "syncing…" => Style::new().fg(Color::Blue),
            _ => look.muted(),
        };
        right.extend([Span::styled(format!("⇅ {state}"), style), gap()]);
    }
    match ed.embedded {
        (0, _) => {}
        (1, bytes) => right.extend([Span::styled(format!("1 image, {}", human(bytes)), look.muted()), gap()]),
        (n, bytes) => right.extend([Span::styled(format!("{n} images, {}", human(bytes)), look.muted()), gap()]),
    }
    if !ed.markdown {
        right.extend([Span::styled("plain text", look.muted()), gap()]);
    }
    right.push(Span::styled(format!("Ln {}, Col {}", ed.cursor.row + 1, ed.cursor.col + 1), look.muted()));
    right.extend([gap(), Span::styled(format!("{} words", ed.word_count()), look.muted())]);

    let right_w: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let crowded = toast.is_some_and(|t| t.chars().count() + right_w + 2 > area.width as usize);
    f.render_widget(Paragraph::new(left).style(look.surface()), area);
    if !crowded {
        let at = Rect::new(area.x + area.width.saturating_sub(right_w as u16), area.y, (right_w as u16).min(area.width), 1);
        f.render_widget(Paragraph::new(Line::from(right)).style(look.surface()), at);
    }
}

fn draw_hints(f: &mut Frame, modal: Option<Vec<(&'static str, &'static str)>>, italic: bool, markdown: bool, area: Rect) {
    let look = theme::get();
    let mut hints = vec![("^Q", "quit"), ("^N", "new"), ("^P", "open"), ("^S", "save"), ("^G", "assistant"), ("F2", "move"), ("^Z", "undo"), ("^Y", "redo"), ("^C", "copy"), ("^X", "cut"), ("^V", "paste")];
    if markdown {
        hints.push(("^B", "bold"));
        // Without the kitty keyboard protocol Ctrl+I is indistinguishable from Tab.
        if italic {
            hints.push(("^I", "italic"));
        }
        hints.push(("^T", "task"));
    }
    if let Some(modal) = modal {
        hints = modal;
    }
    // The key carries the weight, the word beside it stays quiet.
    let key = Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    let mut used = 0;
    for (name, label) in hints {
        // Drop whole hints that do not fit rather than cutting one in half.
        used += name.chars().count() + label.chars().count() + 4;
        if used > area.width as usize + 3 {
            break;
        }
        spans.push(Span::styled(name, key));
        spans.push(Span::styled(format!(" {label}   "), look.muted()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(look.surface()), area);
}
