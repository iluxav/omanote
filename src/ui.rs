//! Drawing: a centred text column, a status line and a nano-style hint bar.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::agents::Chooser;
use crate::config::{Align, Config};
use crate::editor::{Editor, Pos};
use crate::find::Find;
use crate::layout::{VRow, locate};
use crate::mention::Mention;
use crate::pane::Pane;
use crate::picker::{Note, Picker, Row, age};
use crate::saveas::{After, SaveAs};
use crate::theme;
use crate::vaults::tilde;

const PICKER_ROWS: usize = 8;
const MENTION_ROWS: usize = 6;

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
    Some((area.x + x, 1, w, area.height - 4, area.width - margin - x))
}

/// Narrowest a note may get beside another note, and the agent beside notes.
const NOTE_MIN: u16 = 42;
const AGENT_MIN: u16 = 48;

/// How the window is shared out. `right` is the second note's column (its
/// left edge is a rule, at `rule`). When there is no room for everything, the
/// note that does not have the keyboard is the one left out.
pub struct Panes {
    pub left: Rect,
    pub right: Option<Rect>,
    pub rule: Option<u16>,
    pub agent: Option<Rect>,
}

impl Panes {
    /// Everything the notes cover: what a popup is centred on.
    pub fn notes(&self) -> Rect {
        let end = self.right.map_or(self.left.x + self.left.width, |r| r.x + r.width);
        Rect::new(self.left.x, self.left.y, end - self.left.x, self.left.height)
    }
}

pub fn arrange(full: Rect, two_notes: bool, pane_open: bool, pane_focused: bool) -> Panes {
    let needed = 2 * NOTE_MIN + 1 + if pane_open { AGENT_MIN } else { 0 };
    if !two_notes || full.width < needed {
        let (left, agent) = split(full, pane_open, pane_focused);
        return Panes { left, right: None, rule: None, agent };
    }
    let agent_w = if pane_open { (full.width * 36 / 100).max(AGENT_MIN) } else { 0 };
    let notes_w = full.width - agent_w;
    let left_w = notes_w / 2;
    Panes {
        left: Rect::new(full.x, full.y, left_w, full.height),
        right: Some(Rect::new(full.x + left_w + 1, full.y, notes_w - left_w - 1, full.height)),
        rule: Some(full.x + left_w),
        agent: pane_open.then(|| Rect::new(full.x + notes_w, full.y, agent_w, full.height)),
    }
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
        (0, false) => "^G to the AI chat".to_string(),
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
/// What `draw` shows besides the focused note.
pub struct Scene<'a> {
    pub panes: &'a Panes,
    /// The note that does not have the keyboard, and whether the focused one is on the right.
    pub other: Option<&'a mut Editor>,
    pub on_right: bool,
    pub picker: Option<&'a Picker>,
    pub save_as: Option<&'a SaveAs>,
    pub toast: Option<&'a str>,
    pub sync: Option<&'a str>,
    pub config: &'a Config,
    pub assistant: Option<(&'a Pane, bool)>,
    pub chooser: Option<&'a Chooser>,
    pub mention: Option<&'a Mention>,
    pub find: Option<&'a Find>,
    pub back: Option<&'a str>,
    /// Where each note is (`vault/folder/`): the focused one's, then the other's.
    pub places: (&'a str, &'a str),
}

pub fn draw(f: &mut Frame, ed: &mut Editor, scene: Scene) {
    let Scene { panes, other, on_right, picker, save_as, toast, sync, config, assistant, chooser, mention, find, back, places } = scene;
    if let (Some((pane, focused)), Some(rect)) = (assistant, panes.agent) {
        if let Some(xy) = draw_pane(f, pane, rect, focused) {
            f.set_cursor_position(xy);
        }
    }
    // The notes may have no room at all (a narrow window with the assistant in front).
    if panes.left.width == 0 {
        return;
    }
    let typing_in_pane = assistant.is_some_and(|(_, focused)| focused);
    let (mine, theirs) = match (panes.right, on_right) {
        (Some(right), true) => (right, Some(panes.left)),
        (Some(right), false) => (panes.left, Some(right)),
        (None, _) => (panes.left, None),
    };
    // Even with no room to show it, the other note is there: ^Q closes, F6 changes over.
    let split = other.is_some();
    if let (Some(other), Some(area)) = (other, theirs) {
        draw_note(f, other, area, config, places.1, None);
    }
    if let Some(x) = panes.rule {
        let rule: Vec<Line> = (0..panes.left.height).map(|_| Line::styled("│", theme::get().faint())).collect();
        f.render_widget(Paragraph::new(rule), Rect::new(x, panes.left.y, 1, panes.left.height));
    }
    let focus = Focus { toast, sync, back, split, find, cursor: !typing_in_pane };
    let cursor_xy = draw_note(f, ed, mine, config, places.0, Some(focus));

    let area = panes.notes();
    if let Some(chooser) = chooser {
        draw_chooser(f, chooser, area);
    } else if let Some(prompt) = save_as {
        draw_save_as(f, prompt, toast, area);
    } else if let Some(picker) = picker {
        draw_picker(f, picker, area);
    } else if let (Some(mention), Some(xy)) = (mention, cursor_xy.filter(|_| !typing_in_pane)) {
        draw_mention(f, mention, xy, mine);
    }
}

/// What only the note with the keyboard shows: messages, the cursor, its keys.
struct Focus<'a> {
    toast: Option<&'a str>,
    sync: Option<&'a str>,
    back: Option<&'a str>,
    split: bool,
    find: Option<&'a Find>,
    cursor: bool,
}

/// One note in its column: the text, and its footer. Returns where the cursor is.
fn draw_note(f: &mut Frame, ed: &mut Editor, area: Rect, config: &Config, place: &str, focus: Option<Focus>) -> Option<(u16, u16)> {
    let (x, _, w, h, table_w) = text_area(area, config)?;
    let right_edge = area.x + area.width;
    ed.table_w = table_w;
    ed.set_view(x, 1, w, h);

    let focused = focus.is_some();
    let find = focus.as_ref().and_then(|f| f.find);
    let sel = ed.selection();
    let mut out: Vec<Line> = Vec::new();
    let mut map = Vec::new();
    let mut cursor_xy = None;
    let mut row = ed.top;
    let mut skip = ed.top_skip;
    while row < ed.lines.len() && out.len() < h as usize {
        // Markdown shows through on the line being edited, and nothing is being edited over there.
        let vrows = ed.rows(row, focused && ed.is_revealed(row));
        let cursor_vi = (row == ed.cursor.row).then(|| locate(&vrows, ed.cursor.col));
        for (vi, vr) in vrows.iter().enumerate().skip(std::mem::take(&mut skip)) {
            if out.len() >= h as usize {
                break;
            }
            if cursor_vi == Some(vi) {
                let cx = (x + vr.x_of(ed.cursor.col)).min(right_edge - 1);
                cursor_xy = Some((cx, 1 + out.len() as u16));
            }
            let marks: Vec<(usize, usize)> = find.map(|f| f.on_row(row).collect()).unwrap_or_default();
            out.push(render_row(vr, row, ed.lines[row].len(), sel, &marks, find.and_then(|f| f.here(row))));
            map.push((row, vi));
        }
        row += 1;
    }
    ed.view.rows = map;

    if ed.lines.len() == 1 && ed.lines[0].is_empty() {
        out[0] = Line::styled("Start writing…", theme::get().muted());
    }

    f.render_widget(Paragraph::new(out), Rect::new(x, 1, right_edge - x, h));

    // The footer: a band across the column, set apart from the page.
    let look = theme::get();
    f.render_widget(Block::new().style(look.surface()), Rect::new(area.x, area.height - 2, area.width, 2));
    if !look.rich {
        let rule = Line::styled("─".repeat(w as usize), look.faint());
        f.render_widget(Paragraph::new(rule), Rect::new(x, area.height - 3, w, 1));
    }
    let (status, hints) = (Rect::new(x, area.height - 2, w, 1), Rect::new(x, area.height - 1, w, 1));
    match focus {
        Some(Focus { find: Some(find), .. }) => {
            ed.view.back = None;
            draw_find(f, find, status, hints);
            return None;
        }
        Some(focus) => {
            if let Some(xy) = cursor_xy.filter(|_| focus.cursor) {
                f.set_cursor_position(xy);
            }
            ed.view.back = draw_status(f, ed, place, focus.toast, focus.sync, focus.back, true, status);
            draw_hints(f, focus.split, ed.markdown, hints);
        }
        None => {
            ed.view.back = None;
            draw_status(f, ed, place, None, None, None, false, status);
            let key = Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD);
            let line = Line::from(vec![Span::styled("F6", key), Span::styled(" or a click to write here", look.muted())]);
            f.render_widget(Paragraph::new(line).style(look.surface()), hints);
        }
    }
    cursor_xy
}

/// Push everything in `area` into the background, so the popup drawn next
/// stands out. Cells that are part of an image are left alone: a Kitty image's
/// identity rides in its cells' foreground colour, and a block image's pixels
/// are its colours, so recolouring either would wreck the picture.
fn scrim(f: &mut Frame, area: Rect) {
    let look = theme::get();
    let faint = look.faint().fg;
    let buf = f.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            let Some(cell) = buf.cell_mut((x, y)) else { continue };
            let symbol = cell.symbol();
            let image = symbol.starts_with('\u{10EEEE}') || ((symbol == "▀" || symbol == "▄") && matches!(cell.fg, Color::Rgb(..)));
            if image {
                continue;
            }
            match (look.rich, faint) {
                (true, Some(color)) => cell.fg = color,
                _ => cell.modifier.insert(Modifier::DIM),
            }
            cell.modifier.remove(Modifier::BOLD | Modifier::REVERSED);
        }
    }
}

/// A popup over the note: rounded frame, the title set into its top edge with
/// a quiet note at the right, and its own key hints in the bottom edge. It
/// sits in the upper part of the note's area, where the eye already is.
/// Returns where the contents go.
fn popup(f: &mut Frame, area: Rect, title: &str, note: &str, hints: &[(&str, &str)], rows: u16, width: u16) -> Rect {
    scrim(f, area);

    let w = width.min(area.width.saturating_sub(4)).max(24.min(area.width));
    let rows = rows.min(area.height.saturating_sub(6)).max(1);
    // Frame, a row of air above and below the contents, then the contents.
    let h = rows + 4;
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height.saturating_sub(h) / 4).max(1);
    frame(f, area, Rect::new(x, y, w, h), title, note, hints);
    Rect::new(x + 2, y + 2, w.saturating_sub(4), rows)
}

/// A rounded box with a title in its top edge, key hints in its bottom edge
/// and a shadow. `area` is what the shadow may fall on.
fn frame(f: &mut Frame, area: Rect, frame: Rect, title: &str, note: &str, hints: &[(&str, &str)]) {
    let look = theme::get();
    let Rect { x, y, width: w, height: h } = frame;

    // A soft shadow, down and to the right, lifts it off the page.
    if let Some(shade) = look.shadow() {
        let buf = f.buffer_mut();
        for sy in y + 1..(y + h + 1).min(area.y + area.height) {
            for sx in x + 2..(x + w + 2).min(area.x + area.width) {
                if let Some(cell) = buf.cell_mut((sx, sy)).filter(|c| !c.symbol().starts_with('\u{10EEEE}')) {
                    cell.bg = shade;
                }
            }
        }
    }
    f.render_widget(Clear, frame);
    f.render_widget(Block::new().style(look.surface()), frame);

    let edge = look.muted();
    let inner_w = w as usize - 2;
    // ╭─ Title ─────────────── note ─╮
    let note = if note.is_empty() { String::new() } else { format!(" {note} ") };
    let fill = inner_w.saturating_sub(title.chars().count() + note.chars().count() + 4);
    let top = Line::from(vec![
        Span::styled("╭─ ", edge),
        Span::styled(title.to_string(), Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {}", "─".repeat(fill)), edge),
        Span::styled(note, look.muted()),
        Span::styled("─╮", edge),
    ]);
    f.render_widget(Paragraph::new(top).style(look.surface()), Rect::new(x, y, w, 1));

    for row in 1..h - 1 {
        f.render_widget(Paragraph::new(Line::styled("│", edge)).style(look.surface()), Rect::new(x, y + row, 1, 1));
        f.render_widget(Paragraph::new(Line::styled("│", edge)).style(look.surface()), Rect::new(x + w - 1, y + row, 1, 1));
    }

    // ╰─ Enter open  Esc cancel ─────╯  (hints that do not fit are dropped whole)
    let key = Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD);
    let mut bottom = vec![Span::styled("╰─ ", edge)];
    let mut used = 0usize;
    for (name, label) in hints {
        let len = name.chars().count() + label.chars().count() + 3;
        if used + len + 2 > inner_w.saturating_sub(2) {
            break;
        }
        used += len;
        bottom.push(Span::styled(name.to_string(), key));
        bottom.push(Span::styled(format!(" {label}  "), look.muted()));
    }
    bottom.push(Span::styled(format!("{}╯", "─".repeat(inner_w.saturating_sub(used + 2))), edge));
    f.render_widget(Paragraph::new(Line::from(bottom)).style(look.surface()), Rect::new(x, y + h - 1, w, 1));
}

/// Ctrl+G with more than one agent installed: which one?
fn draw_chooser(f: &mut Frame, c: &Chooser, area: Rect) {
    let look = theme::get();
    let shown = c.agents.len().clamp(1, PICKER_ROWS).min(area.height.saturating_sub(6) as usize).max(1);
    let hints = [("↑↓", "select"), ("Enter", "open"), ("1-9", "pick"), ("Esc", "cancel")];
    let inner = popup(f, area, "Assistant", "opens beside the note", &hints, shown as u16, 58);
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
            list_row(i == c.selected, vec![Span::styled(key, look.muted()), Span::raw(agent.name.clone())], runs, inner.width)
        })
        .collect();
    f.render_widget(Paragraph::new(lines).style(look.surface()), inner);
}

/// One row of a popup's list: marker, text, and a quiet note at the right edge.
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

/// Where does this note go: the name being typed, then the places on offer.
fn draw_save_as(f: &mut Frame, p: &SaveAs, toast: Option<&str>, area: Rect) {
    let look = theme::get();
    let shown = p.vaults.len().clamp(1, PICKER_ROWS).min(area.height.saturating_sub(8) as usize).max(1);
    let title = match p.after {
        After::Stay if p.moving.is_some() && p.keep_original => "Copy note",
        After::Stay if p.moving.is_some() => "Move note",
        After::Stay => "Save note",
        After::Quit => "Save before quitting?",
        After::Open(_) | After::New => "Save this note first?",
    };
    let note = if p.moving.is_some() && p.keep_original { "the original stays" } else { "" };
    let hints: Vec<(&str, &str)> = match (&p.moving, &p.after) {
        (Some(_), _) if p.keep_original => vec![("Enter", "copy"), ("^K", "move instead"), ("↑↓", "where"), ("Esc", "cancel")],
        (Some(_), _) => vec![("Enter", "move"), ("^K", "copy instead"), ("↑↓", "where"), ("Esc", "cancel")],
        (None, After::Stay) => vec![("Enter", "save"), ("↑↓", "vault"), ("Esc", "cancel")],
        (None, _) => vec![("Enter", "save"), ("↑↓", "vault"), ("^D", "discard"), ("Esc", "cancel")],
    };
    let inner = popup(f, area, title, note, &hints, shown as u16 + 3, 76);
    let w = inner.width;

    let label = " Name  ";
    let mut lines = vec![
        Line::from(vec![Span::styled(label, look.muted()), Span::raw(p.name.clone()), Span::styled(format!(".{}", p.ext), look.muted())]),
        Line::default(),
        match toast {
            Some(msg) => Line::styled(format!(" {msg}"), Style::new().fg(Color::Yellow)),
            None => Line::styled(if p.moving.is_some() { " To" } else { " In" }, look.muted()),
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
    f.render_widget(Paragraph::new(lines).style(look.surface()), inner);
    let cx = inner.x + label.chars().count() as u16 + unicode_width::UnicodeWidthStr::width(p.name.as_str()) as u16;
    f.set_cursor_position((cx.min(inner.x + w.saturating_sub(1)), inner.y));
}

/// The note finder: what you typed, then the matches.
/// A note's name in a list: the folder part is quiet, the name is not, and
/// what matched the query stands out.
fn note_name(note: &Note, hits: &[usize]) -> Vec<Span<'static>> {
    let look = theme::get();
    let name_start = note.name.iter().rposition(|c| *c == '/').map_or(0, |k| k + 1);
    note.name
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
        .collect()
}

/// The `@` suggestions: a short list hanging from the cursor. The note keeps
/// the keyboard, so nothing is dimmed and the cursor stays where it is.
fn draw_mention(f: &mut Frame, m: &Mention, cursor: (u16, u16), area: Rect) {
    let look = theme::get();
    let p = &m.picker;
    let shown = p.len().clamp(1, MENTION_ROWS) as u16;
    let (w, h) = (48u16.min(area.width.saturating_sub(2)), shown + 2);
    // The text ends where the footer begins.
    let floor = area.height.saturating_sub(2);
    if w < 20 || h + 2 > floor {
        return;
    }
    // Under the line being typed; above it when that would run into the footer.
    let y = if cursor.1 + 1 + h <= floor { cursor.1 + 1 } else { cursor.1.saturating_sub(h).max(1) };
    let x = cursor.0.saturating_sub(4).clamp(area.x, area.x + area.width - w - 1);
    let hints = [("↑↓", "select"), ("Enter", "link"), ("Esc", "close")];
    frame(f, area, Rect::new(x, y, w, h), "Link to", "", &hints);

    let first = p.selected.saturating_sub(shown as usize - 1);
    let mut lines = Vec::new();
    for i in first..first + shown as usize {
        let (text, note) = match p.row(i) {
            Some(Row::Note(note, hits)) => (note_name(note, hits), age(note.modified)),
            Some(Row::Create(name)) => (vec![Span::styled(format!("+ Create “{name}.md”"), Style::new().fg(Color::Green))], String::new()),
            None => (vec![Span::styled("No notes yet: type a name", look.muted())], String::new()),
        };
        lines.push(list_row(i == p.selected && p.row(i).is_some(), text, note, w - 2));
    }
    f.render_widget(Paragraph::new(lines).style(look.surface()), Rect::new(x + 1, y + 1, w - 2, shown));
}

fn draw_picker(f: &mut Frame, p: &Picker, area: Rect) {
    let look = theme::get();
    let shown = p.len().clamp(1, PICKER_ROWS).min(area.height.saturating_sub(7) as usize).max(1);
    let notes = match p.total() {
        1 => "1 note".to_string(),
        n => format!("{n} notes"),
    };
    let width = 76u16.min(area.width.saturating_sub(4));
    // A long vault path loses its front, not the note count.
    let room = (width as usize).saturating_sub(notes.chars().count() + 26).max(8);
    let place = p.title();
    let count = place.chars().count();
    let place = if count > room { format!("…{}", place.chars().skip(count - room + 1).collect::<String>()) } else { place };
    let hints = [("↑↓", "select"), ("Enter", "open"), ("Esc", "cancel"), ("^U", "clear")];
    let inner = popup(f, area, "Open note", &format!("{place} · {notes}"), &hints, shown as u16 + 2, width);
    let w = inner.width;

    let prompt = " ›  ";
    let mut lines = vec![
        Line::from(vec![Span::styled(prompt, Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)), Span::raw(p.query.clone())]),
        Line::styled("─".repeat(w as usize), look.faint()),
    ];

    // Keep the selection inside the window of visible rows.
    let first = p.selected.saturating_sub(shown - 1);
    for i in first..first + shown {
        let (text, note) = match p.row(i) {
            Some(Row::Note(note, hits)) => (note_name(note, hits), age(note.modified)),
            Some(Row::Create(name)) => (vec![Span::styled(format!("+ New note “{name}”"), Style::new().fg(Color::Green))], String::new()),
            None => {
                let msg = if p.total() == 0 { "No notes yet — type a name and press Enter" } else { "Nothing matches" };
                (vec![Span::styled(msg, look.muted())], String::new())
            }
        };
        lines.push(list_row(i == p.selected && p.row(i).is_some(), text, note, w));
    }
    f.render_widget(Paragraph::new(lines).style(look.surface()), inner);
    let cx = inner.x + prompt.chars().count() as u16 + unicode_width::UnicodeWidthStr::width(p.query.as_str()) as u16;
    f.set_cursor_position((cx.min(inner.x + w.saturating_sub(1)), inner.y));
}

/// `marks`: what the find bar matched on this line, in yellow; `here`: the
/// match the search is on, in light green so it stands out from the rest.
fn render_row(vr: &VRow, row: usize, line_len: usize, sel: Option<(Pos, Pos)>, marks: &[(usize, usize)], here: Option<(usize, usize)>) -> Line<'static> {
    let selected = |col: usize| sel.is_some_and(|(s, e)| s <= Pos { row, col } && Pos { row, col } < e);
    let marked = |col: usize| marks.iter().any(|&(from, to)| (from..to).contains(&col));
    let current = |col: usize| here.is_some_and(|(from, to)| (from..to).contains(&col));
    let lit = Style::new().fg(Color::Black).bg(Color::Yellow);
    let lit_here = Style::new().fg(Color::Black).bg(Color::LightGreen);
    let mut spans: Vec<Span> = vr.lead.iter().map(|(t, s)| Span::styled(t.clone(), *s)).collect();
    let mut run = String::new();
    let mut run_style = Style::default();
    for cell in &vr.cells {
        let style = if cell.solid && current(cell.col) {
            cell.style.patch(lit_here)
        } else if selected(cell.col) {
            cell.style.add_modifier(Modifier::REVERSED)
        } else if cell.solid && marked(cell.col) {
            cell.style.patch(lit)
        } else {
            cell.style
        };
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

/// Ctrl+F: the search takes the footer's place, so the text stays in view.
fn draw_find(f: &mut Frame, find: &Find, status: Rect, hints: Rect) {
    let look = theme::get();
    let key = Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD);
    let prompt = "Find  ";
    let count = find.count();
    let count_style = if find.current.is_none() && !find.query.is_empty() { Style::new().fg(Color::Red) } else { look.muted() };
    // A long query shows its end, where the typing is.
    let room = (status.width as usize).saturating_sub(prompt.len() + count.chars().count() + 3);
    let typed = find.query.chars().count();
    let query: String = if typed > room { find.query.chars().skip(typed - room).collect() } else { find.query.clone() };
    let shown = unicode_width::UnicodeWidthStr::width(query.as_str());
    let line = Line::from(vec![Span::styled(prompt, Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)), Span::raw(query)]);
    f.render_widget(Paragraph::new(line).style(look.surface()), status);
    let count_w = count.chars().count() as u16;
    f.render_widget(Paragraph::new(Line::styled(count, count_style)).style(look.surface()), Rect::new(status.x + status.width.saturating_sub(count_w), status.y, count_w.min(status.width), 1));
    f.set_cursor_position(((status.x + (prompt.len() + shown) as u16).min(status.x + status.width.saturating_sub(1)), status.y));

    let mut spans = Vec::new();
    for (name, label) in [("Enter", "next"), ("↑↓", "previous / next"), ("^U", "clear"), ("Esc", "done")] {
        spans.push(Span::styled(name, key));
        spans.push(Span::styled(format!(" {label}   "), look.muted()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(look.surface()), hints);
}

/// Returns where the "go back" label landed, so a click can find it.
#[allow(clippy::too_many_arguments)]
fn draw_status(f: &mut Frame, ed: &Editor, place: &str, toast: Option<&str>, sync: Option<&str>, back: Option<&str>, focused: bool, area: Rect) -> Option<(u16, u16, u16)> {
    let look = theme::get();
    let name = match &ed.path {
        Some(p) => p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()),
        None => "untitled".to_string(),
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
    if focused && ed.link_at(ed.cursor).is_some() {
        right.extend([Span::styled("^O", Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD)), Span::styled(" open link", look.muted()), gap()]);
    }
    right.push(Span::styled(format!("Ln {}, Col {}", ed.cursor.row + 1, ed.cursor.col + 1), look.muted()));
    right.extend([gap(), Span::styled(format!("{} words", ed.word_count()), look.muted())]);
    let right_w: usize = right.iter().map(|s| s.content.chars().count()).sum();

    let width = |spans: &[Span]| spans.iter().map(|s| s.content.chars().count()).sum::<usize>();
    let mut back_at = None;
    let left = match toast {
        Some(msg) => Line::from(Span::styled(msg.to_string(), Style::new().fg(Color::Yellow))),
        None => {
            let mut rest = vec![
                Span::styled(name, if focused { Style::new().add_modifier(Modifier::BOLD) } else { look.muted() }),
                Span::styled(if ed.dirty { "  ● unsaved" } else { "" }, Style::new().fg(Color::Yellow)),
            ];
            let back_label = back.map(|b| format!(" {b}"));
            let back_w = back_label.as_ref().map_or(0, |l| 3 + 5 + l.chars().count());
            // Which vault and folder this is, in whatever room the rest leaves;
            // a long one loses its front, the end being what tells notes apart.
            let room = (area.width as usize).saturating_sub(width(&rest) + back_w + right_w + 2);
            let count = place.chars().count();
            let place = match count {
                0 => String::new(),
                n if n <= room => place.to_string(),
                _ if room >= 8 => format!("…{}", place.chars().skip(count - room + 1).collect::<String>()),
                _ => String::new(),
            };
            let mut spans = vec![Span::styled(place, if focused { look.muted() } else { look.faint() })];
            spans.append(&mut rest);
            // The way back to the note this one was reached from.
            if let Some(label) = back_label {
                let from = area.x + width(&spans) as u16 + 3;
                back_at = Some((area.y, from, from + 5 + label.chars().count() as u16));
                spans.push(Span::raw("   "));
                spans.push(Span::styled("Alt+←", Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD)));
                spans.push(Span::styled(label, look.muted()));
            }
            Line::from(spans)
        }
    };

    let left_w = width(&left.spans);
    // The facts give way to a message or the way back, not the other way round.
    let crowded = left_w + right_w + 2 > area.width as usize;
    f.render_widget(Paragraph::new(left).style(look.surface()), area);
    if !crowded {
        let at = Rect::new(area.x + area.width.saturating_sub(right_w as u16), area.y, (right_w as u16).min(area.width), 1);
        f.render_widget(Paragraph::new(Line::from(right)).style(look.surface()), at);
    }
    back_at
}

fn draw_hints(f: &mut Frame, split: bool, markdown: bool, area: Rect) {
    let look = theme::get();
    // Only what is omanote's own. Copy, paste, undo, bold and the like are the
    // keys they are everywhere, and listing them would crowd these out.
    // Beside another note, ^Q closes this one, and there is somewhere else to go.
    let mut hints = vec![if split { ("^Q", "close") } else { ("^Q", "quit") }, ("^N", "new"), ("^P", "open"), ("^S", "save"), ("^G", "ai chat"), ("F2", "move"), ("Alt+←", "back")];
    if markdown {
        // Typing an @ is how notes get linked; it sits with the other ways to reach a note.
        hints.insert(3, ("@", "link"));
        hints.push(("^T", "task"));
    }
    if split {
        hints.insert(1, ("F6", "other note"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_the_window_between_two_notes_and_the_agent() {
        let full = |w| Rect::new(0, 0, w, 40);
        // One note: exactly what it always was.
        let p = arrange(full(200), false, true, false);
        assert_eq!((p.left.width, p.right, p.rule, p.agent.map(|a| a.x)), (112, None, None, Some(112)));

        // Two notes share the window evenly, with a rule between them.
        let p = arrange(full(131), true, false, false);
        let right = p.right.unwrap();
        assert_eq!((p.left.width, p.rule, right.x, right.x + right.width, p.agent), (65, Some(65), 66, 131, None));
        assert_eq!(p.notes(), full(131));

        // Two notes and the agent, when all three have room.
        let p = arrange(full(200), true, true, false);
        let (right, agent) = (p.right.unwrap(), p.agent.unwrap());
        assert_eq!((p.left.width, right.x, right.width, agent.x, agent.width), (64, 65, 63, 128, 72));
        assert_eq!(p.notes().width, 128, "popups centre on the notes, not on the agent");

        // No room: the note without the keyboard is left out, not squeezed.
        let p = arrange(full(84), true, false, false);
        assert_eq!((p.left.width, p.right), (84, None));
        let p = arrange(full(130), true, true, false);
        assert_eq!((p.right, p.agent.is_some()), (None, true));
    }

    #[test]
    fn a_note_can_sit_anywhere_in_the_window() {
        let config = Config::default();
        let (x0, _, w0, _, _) = text_area(Rect::new(0, 0, 60, 30), &config).unwrap();
        let (x1, _, w1, _, _) = text_area(Rect::new(66, 0, 60, 30), &config).unwrap();
        assert_eq!((x1, w1), (x0 + 66, w0));
    }
}
