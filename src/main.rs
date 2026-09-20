mod agents;
mod capture;
mod clipboard;
mod config;
mod desktop;
mod diacritics;
mod editor;
mod images;
mod layout;
mod markdown;
mod pane;
mod picker;
mod saveas;
mod sync;
mod table;
mod theme;
mod ui;
mod vaults;

use std::io::{Write, stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::SetCursorStyle;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture, Event, KeyCode,
    KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement,
    window_size,
};

use editor::{Editor, Pos};
use images::Images;
use picker::{Picker, Resolution};
use saveas::{After, SaveAs};

const DEMO: &str = include_str!("../demo.md");
const AUTOSAVE_IDLE: Duration = Duration::from_millis(1500);
const TOAST: Duration = Duration::from_secs(2);
const DOUBLE_CLICK: Duration = Duration::from_millis(350);
/// How long `omanote <name>` waits for GitHub before giving up and starting a new note.
const PULL_PATIENCE: Duration = Duration::from_secs(8);
/// Pasted images wider than this are scaled down before they are embedded.
const PASTE_MAX_WIDTH: u32 = 2000;

struct App {
    ed: Editor,
    picker: Option<Picker>,
    save_as: Option<SaveAs>,
    /// The assistant's terminal beside the note, and whether it has the keyboard.
    pane: Option<pane::Pane>,
    pane_focused: bool,
    chooser: Option<agents::Chooser>,
    /// An agent that quit the moment it started most likely printed why. Its
    /// pane then stays up until a key is pressed, so the reason can be read.
    pane_started: Instant,
    pane_failed: bool,
    /// Where the note ends and the pane begins, and where the assistant's
    /// screen is inside the pane, for routing the mouse.
    pane_x: u16,
    pane_screen: ratatui::layout::Rect,
    sync: sync::Sync,
    /// What was asked for on the command line when nothing matched: a name for the new note.
    wanted: Option<String>,
    /// `ed.save_count` as of the last time the sync was told about saves.
    seen_saves: u64,
    toast: Option<(String, Instant)>,
    last_click: Option<(Instant, Pos)>,
    enhanced_keys: bool,
    image_mode: images::Mode,
    settings: config::Config,
    /// Redraw every cell on the next frame, not just what changed.
    repaint: bool,
    /// `--keys`: show and log every key event, for diagnosing terminal quirks.
    key_log: Option<std::fs::File>,
    quit: bool,
}

impl App {
    fn say(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    /// The settings file was just saved from inside the editor: use it right away.
    /// A half-typed line during autosave changes nothing and says nothing; an
    /// explicit save reports what is wrong.
    fn settings_saved(&mut self, explicit: bool) {
        let file = config::path(&vaults::home());
        let same = |a: &std::path::Path, b: &std::path::Path| a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(a, b)| a == b);
        if !self.ed.path.as_deref().is_some_and(|p| same(p, &file)) {
            return;
        }
        let (settings, problems) = config::load(&vaults::home());
        match (problems.first(), explicit) {
            (None, _) => {
                self.settings = settings;
                self.repaint = true;
                if explicit {
                    self.say("Settings applied");
                }
            }
            (Some(problem), true) => self.say(format!("Not applied — {problem}")),
            (Some(_), false) => {}
        }
    }

    fn save(&mut self) {
        match self.ed.save() {
            Ok(true) => {
                self.say("Saved");
                self.settings_saved(true);
            }
            Ok(false) => self.ask_where(After::Stay),
            Err(e) => self.say(format!("Could not save: {e}")),
        }
    }

    /// Tell the sync about saves it has not heard of yet.
    fn note_saves(&mut self) {
        if self.ed.save_count != self.seen_saves {
            self.seen_saves = self.ed.save_count;
            if let Some(path) = &self.ed.path {
                self.sync.saved(path, &vaults::all(&vaults::home()));
            }
        }
    }

    /// Runs a few times a second: background sync, and noticing that the open
    /// note changed on disk (a pull, another program).
    fn tick(&mut self) {
        self.note_saves();
        if let Some(msg) = self.sync.tick() {
            self.say(msg);
        }
        let Some(path) = self.ed.path.clone() else { return };
        let on_disk = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if on_disk.is_none() || on_disk == self.ed.disk_mtime {
            return;
        }
        if self.ed.dirty {
            self.ed.disk_mtime = on_disk;
            return self.say("This note changed on disk while you were typing — keeping your version");
        }
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let (cursor, top, top_skip) = (self.ed.cursor, self.ed.top, self.ed.top_skip);
        self.ed = self.editor(&text, Some(path));
        self.seen_saves = 0;
        let row = cursor.row.min(self.ed.lines.len() - 1);
        self.ed.move_to(Pos { row, col: cursor.col.min(self.ed.lines[row].len()) }, false);
        (self.ed.top, self.ed.top_skip) = (top.min(self.ed.lines.len() - 1), top_skip);
        self.say("Note updated from disk");
    }

    /// A note without a file that has something in it worth keeping.
    fn unsaved_draft(&self) -> bool {
        self.ed.path.is_none() && self.ed.dirty && self.ed.lines.iter().any(|l| l.iter().any(|c| !c.is_whitespace()))
    }

    fn ask_where(&mut self, after: After) {
        self.picker = None;
        self.toast = None;
        let here = std::env::current_dir().ok();
        self.save_as = Some(SaveAs::new(&self.ed.lines, vaults::all(&vaults::home()), here, self.wanted.as_deref(), after));
    }

    /// Ctrl+G: open an assistant beside the note, or move the keyboard between
    /// the two. Which assistant is not built in: the settings may name one,
    /// otherwise the agents installed on this machine are offered.
    fn assistant(&mut self) {
        if self.pane.is_some() {
            self.pane_focused = !self.pane_focused;
            return;
        }
        if self.ed.path.is_none() {
            self.say("Save the note first (Ctrl+S): the assistant works on the file");
            return self.ask_where(After::Stay);
        }
        let agents = agents::available(&self.settings.agents);
        if let Some(fixed) = &self.settings.assistant {
            return self.open_assistant(agents::named(fixed, &agents));
        }
        match agents.len() {
            0 => self.say("No AI agent found (claude, codex, gemini, …). Install one, or add yours in the settings: omanote --config"),
            1 => self.open_assistant(agents[0].clone()),
            _ => {
                self.picker = None;
                self.toast = None;
                self.chooser = Some(agents::Chooser::new(agents, agents::last_used(&vaults::home()).as_deref()));
            }
        }
    }

    fn chooser_key(&mut self, key: KeyEvent) {
        let Some(chooser) = &mut self.chooser else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let pick = |app: &mut Self| {
            if let Some(agent) = app.chooser.take().and_then(|c| c.chosen().cloned()) {
                agents::remember(&vaults::home(), &agent.name);
                app.open_assistant(agent);
            }
        };
        match key.code {
            KeyCode::Esc => self.chooser = None,
            KeyCode::Char('c' | 'g') if ctrl => self.chooser = None,
            KeyCode::Up | KeyCode::BackTab => chooser.step(-1),
            KeyCode::Down | KeyCode::Tab => chooser.step(1),
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < chooser.agents.len() => {
                chooser.selected = c as usize - '1' as usize;
                pick(self);
            }
            KeyCode::Enter => pick(self),
            _ => {}
        }
    }

    /// Start `agent` in a pane, in the note's vault, told what you are working
    /// on. The note is saved first, since the file is what the agent sees.
    fn open_assistant(&mut self, agent: agents::Agent) {
        let Some(path) = self.ed.path.clone() else { return };
        if self.ed.dirty {
            if let Err(e) = self.ed.save() {
                return self.say(format!("Could not save: {e}"));
            }
        }
        let path = path.canonicalize().unwrap_or(path);
        let all = vaults::all(&vaults::home());
        // The vault the note is in, so the agent can see its neighbours; a
        // loose file gets its own folder.
        let in_vault = all.iter().map(|v| &v.path).filter(|v| path.starts_with(v)).max_by_key(|v| v.as_os_str().len());
        let dir = in_vault.cloned().or_else(|| path.parent().map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("."));
        let file = path.strip_prefix(&dir).unwrap_or(&path).to_string_lossy().into_owned();

        let mut context = format!(
            "The user is writing a markdown note in omanote, a terminal editor, and you are in a pane beside it. \
             The note is {file} (full path: {}); their cursor is on line {}. omanote saves as they type and reloads \
             the file when it changes on disk, so when asked to change the text, edit the file directly. \
             Your pane is narrow: keep replies short.",
            path.display(),
            self.ed.cursor.row + 1
        );
        if let Some(selected) = self.ed.selected_text().filter(|t| !t.trim().is_empty()) {
            let selected: String = selected.chars().take(4000).collect();
            context.push_str(&format!("\n\nThey have this text selected:\n{selected}"));
        }
        let command = agent
            .command
            .replace("{context}", "\"$OMANOTE_CONTEXT\"")
            .replace("{file}", "\"$OMANOTE_FILE\"")
            .replace("{dir}", "\"$OMANOTE_DIR\"");
        let env = [("OMANOTE_CONTEXT", context), ("OMANOTE_FILE", file), ("OMANOTE_DIR", dir.to_string_lossy().into_owned())];
        match pane::Pane::spawn(&command, &dir, &env, 24, 60) {
            Ok(mut pane) => {
                // Say which note it was given: the context itself is invisible.
                let note = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                pane.label = format!("{} · {note}", agent.name);
                self.pane = Some(pane);
                self.pane_started = Instant::now();
                self.pane_failed = false;
                self.pane_focused = true;
                self.repaint = true;
            }
            Err(e) => self.say(e),
        }
    }

    /// The assistant exited (or never started): give the window back to the note.
    fn reap_pane(&mut self) {
        if self.pane_failed || !self.pane.as_mut().is_some_and(|p| p.exited()) {
            return;
        }
        if self.pane_started.elapsed() < Duration::from_secs(4) {
            self.pane_failed = true;
            self.pane_focused = true;
            if let Some(pane) = &mut self.pane {
                pane.label = format!("{} — exited straight away · any key closes", pane.label);
            }
            self.repaint = true;
            return;
        }
        self.close_pane();
    }

    fn close_pane(&mut self) {
        {
            self.pane = None;
            self.pane_failed = false;
            self.pane_focused = false;
            self.pane_x = u16::MAX;
            self.repaint = true;
            self.say("Assistant closed");
        }
    }

    /// F2: move, rename or copy the note. One without a file yet just gets saved.
    fn relocate(&mut self) {
        let Some(path) = self.ed.path.clone() else { return self.ask_where(After::Stay) };
        self.picker = None;
        self.toast = None;
        self.save_as = Some(SaveAs::relocate(&path, vaults::all(&vaults::home()), std::env::current_dir().ok()));
    }

    fn carry_on(&mut self, after: After) {
        match after {
            After::Stay => {}
            After::Quit => self.quit(),
            After::Open(path) => self.open(path),
            After::New => self.new_note(),
        }
    }

    fn save_as_key(&mut self, key: KeyEvent) {
        let Some(prompt) = &mut self.save_as else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let delete_word = |name: &mut String| {
            let cut = name.trim_end().rfind(|c: char| !c.is_alphanumeric()).map_or(0, |i| i + 1);
            name.truncate(cut);
        };
        match key.code {
            KeyCode::Esc => self.save_as = None,
            KeyCode::Up | KeyCode::BackTab => prompt.step(-1),
            KeyCode::Down | KeyCode::Tab => prompt.step(1),
            KeyCode::Backspace if ctrl || alt => prompt.edit(delete_word),
            KeyCode::Backspace => prompt.edit(|n| {
                n.pop();
            }),
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                'c' | 'g' => self.save_as = None,
                'u' => prompt.edit(String::clear),
                'w' | 'h' => prompt.edit(delete_word),
                'k' if prompt.moving.is_some() => prompt.toggle_copy(),
                // Only offered when the prompt interrupted quitting or switching notes.
                'd' if !matches!(prompt.after, After::Stay) => {
                    let after = self.save_as.take().map(|p| p.after).unwrap_or(After::Stay);
                    self.ed.dirty = false;
                    self.carry_on(after);
                }
                _ => {}
            },
            KeyCode::Char(c) if !alt => prompt.edit(|n| n.push(c)),
            KeyCode::Enter => self.save_as_confirm(),
            _ => {}
        }
    }

    fn save_as_confirm(&mut self) {
        let Some(prompt) = &mut self.save_as else { return };
        let path = match prompt.target() {
            Ok(path) => path,
            Err(msg) => return self.say(msg),
        };
        if path.exists() && !prompt.confirm_replace {
            prompt.confirm_replace = true;
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            return self.say(format!("{name} exists — Enter again to replace it, or rename"));
        }
        let vault = prompt.vaults[prompt.selected].path.clone();
        let (from, keep) = (prompt.moving.clone(), prompt.keep_original);
        if from.as_ref() == Some(&path) {
            return self.say("That is where it already is — change the name or pick another place");
        }
        let before = self.ed.path.replace(path.clone());
        if let Err(e) = self.ed.save() {
            self.ed.path = before;
            return self.say(format!("Could not save there: {e}"));
        }
        self.ed.images.set_note(&path, vault);
        self.ed.renamed();
        let mut done = format!("Saved to {}", vaults::tilde(&path));
        if let Some(old) = from {
            done = format!("Copied to {} — now editing the copy", vaults::tilde(&path));
            if !keep {
                // The new file is safely written; only now does the old one go.
                match std::fs::remove_file(&old) {
                    Ok(()) => done = format!("Moved to {}", vaults::tilde(&path)),
                    Err(e) => done = format!("Saved to {}, but could not remove the original: {e}", vaults::tilde(&path)),
                }
            }
            // If it left a GitHub vault, that vault has a deletion to send.
            self.sync.saved(&old, &vaults::all(&vaults::home()));
        }
        self.say(done);
        let after = self.save_as.take().map(|p| p.after).unwrap_or(After::Stay);
        self.carry_on(after);
    }

    /// Ctrl+N: a fresh, empty note. What was open is saved first; if it has no
    /// file yet and there is text in it, the save prompt comes up instead and
    /// the new note follows once that is settled.
    fn new_note(&mut self) {
        if self.unsaved_draft() {
            return self.ask_where(After::New);
        }
        if self.ed.dirty && self.ed.path.is_some() {
            if let Err(e) = self.ed.save() {
                return self.say(format!("Could not save, staying here: {e}"));
            }
        }
        self.note_saves();
        self.sync.flush();
        self.picker = None;
        self.wanted = None;
        self.ed = self.editor("", None);
        self.seen_saves = 0;
        self.say("New note — Ctrl+S to choose where it goes");
    }

    /// Switch to another note, saving the current one first.
    fn open(&mut self, path: PathBuf) {
        if self.unsaved_draft() {
            return self.ask_where(After::Open(path));
        }
        if self.ed.dirty && self.ed.path.is_some() {
            if let Err(e) = self.ed.save() {
                return self.say(format!("Could not save, staying here: {e}"));
            }
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return self.say(format!("Could not open {}: {e}", path.display())),
        };
        // Send what the last note left behind, and freshen the new one's vault.
        self.note_saves();
        self.sync.flush();
        self.sync.opened(&path, &vaults::all(&vaults::home()));
        self.ed = self.editor(&text, Some(path));
        self.seen_saves = 0;
    }

    fn editor(&self, text: &str, path: Option<PathBuf>) -> Editor {
        let vault = vaults::root_of(path.as_deref(), &vaults::all(&vaults::home()));
        let images = Images::new(self.image_mode, cell_pixels(), path.as_deref(), vault);
        Editor::new(text, path).with_images(images)
    }

    fn picker_key(&mut self, key: KeyEvent) {
        let Some(p) = &mut self.picker else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Enter => {
                if let Some(path) = p.chosen() {
                    self.picker = None;
                    self.open(path);
                }
            }
            KeyCode::Up | KeyCode::BackTab => p.step(-1),
            KeyCode::Down | KeyCode::Tab => p.step(1),
            KeyCode::PageUp => p.step(-5),
            KeyCode::PageDown => p.step(5),
            KeyCode::Backspace if ctrl || alt => p.delete_word(),
            KeyCode::Backspace => p.backspace(),
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                'p' | 'c' | 'g' => self.picker = None,
                'q' => self.quit(),
                'u' => p.clear(),
                'w' | 'h' => p.delete_word(),
                'n' | 'j' => p.step(1),
                'k' => p.step(-1),
                _ => {}
            },
            KeyCode::Char(c) if !alt => p.push(c.encode_utf8(&mut [0; 4])),
            _ => {}
        }
    }

    fn quit(&mut self) {
        if self.unsaved_draft() {
            return self.ask_where(After::Quit);
        }
        if self.ed.dirty && self.ed.path.is_some() {
            self.save();
        }
        // Detached: the push carries on after we are gone.
        self.note_saves();
        self.sync.flush();
        self.quit = true;
    }

    fn key(&mut self, key: KeyEvent) {
        if let Some(log) = &mut self.key_log {
            use std::io::Write;
            let line = format!("{:?} {:?} {:?}", key.code, key.modifiers, key.kind);
            let _ = writeln!(log, "{line}");
            self.toast = Some((line, Instant::now()));
        }
        if key.kind == KeyEventKind::Release {
            return;
        }
        // Ctrl+G always means "the other side"; everything else goes to
        // whichever side has the keyboard.
        let ctrl_g = key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('g' | 'G'));
        if self.chooser.is_some() {
            return self.chooser_key(key);
        }
        if self.pane_failed {
            return self.close_pane();
        }
        if self.pane_focused && self.save_as.is_none() && self.picker.is_none() {
            if ctrl_g {
                self.pane_focused = false;
            } else if let Some(pane) = &mut self.pane {
                pane.send_key(key);
            }
            return;
        }
        if ctrl_g && self.save_as.is_none() && self.picker.is_none() {
            return self.assistant();
        }
        if self.save_as.is_some() {
            return self.save_as_key(key);
        }
        if self.picker.is_some() {
            return self.picker_key(key);
        }
        let ed = &mut self.ed;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                'q' => self.quit(),
                'p' => {
                    let all = vaults::all(&vaults::home());
                    self.sync.pull_all(&all);
                    self.picker = Some(Picker::open_with(&all, std::env::current_dir().ok().as_deref()));
                }
                's' if shift => self.relocate(),
                's' => self.save(),
                'z' if shift => self.redo(),
                'z' => {
                    if !ed.undo() {
                        self.say("Nothing to undo");
                    }
                }
                'y' => self.redo(),
                'a' => ed.select_all(),
                'n' => self.new_note(),
                'l' => self.repaint = true,
                'c' => {
                    clipboard::copy(&ed.copy());
                    self.say("Copied");
                }
                'x' => clipboard::copy(&ed.cut()),
                'v' => match clipboard::image() {
                    Some(_) if !ed.markdown => self.say("Images can be pasted into markdown notes only"),
                Some((mime, bytes)) => match images::embeddable(&mime, bytes, PASTE_MAX_WIDTH) {
                        Ok(uri) => {
                            let size = uri.len();
                            let label = ed.paste_image(uri);
                            self.say(format!("Image embedded as [{label}] · {}", ui::human(size)));
                        }
                        Err(e) => self.say(e),
                    },
                    None => {
                        let text = clipboard::paste().unwrap_or_else(|| ed.clipboard.clone());
                        ed.insert_str(&text);
                    }
                },
                'b' | 'i' | 't' if !ed.markdown => self.say("That is a markdown shortcut; this file is plain text"),
                'b' => self.wrap("**"),
                'i' => self.wrap("*"),
                't' => ed.toggle_task(ed.cursor.row),
                'h' | 'w' => ed.delete_word_back(),
                _ => {}
            },
            KeyCode::Char(c) if !alt => ed.insert_char(c),
            // Super+Shift is the natural chord, but most window managers keep Super
            // for themselves, so Alt+Shift does the same thing.
            KeyCode::Left | KeyCode::Right if shift && (alt || key.modifiers.contains(KeyModifiers::SUPER)) => {
                if !ed.add_table_column(key.code == KeyCode::Right) {
                    self.say("Put the cursor in a table to add a column");
                }
            }
            KeyCode::Enter if shift && ed.add_table_row() => {}
            KeyCode::Left if ctrl => ed.word_left(shift),
            KeyCode::Right if ctrl => ed.word_right(shift),
            KeyCode::Left => ed.left(shift),
            KeyCode::Right => ed.right(shift),
            KeyCode::Up => ed.up(shift),
            KeyCode::Down => ed.down(shift),
            KeyCode::PageUp => ed.page(false, shift),
            KeyCode::PageDown => ed.page(true, shift),
            KeyCode::Home if ctrl => ed.doc_start(shift),
            KeyCode::End if ctrl => ed.doc_end(shift),
            KeyCode::Home => ed.home(shift),
            KeyCode::End => ed.end(shift),
            KeyCode::Backspace if ctrl || alt => ed.delete_word_back(),
            KeyCode::Backspace => ed.backspace(),
            KeyCode::Delete if ctrl => ed.delete_word_forward(),
            KeyCode::Delete => ed.delete(),
            KeyCode::Enter => ed.enter(),
            KeyCode::Tab => ed.tab(),
            KeyCode::BackTab => ed.backtab(),
            KeyCode::F(2) => self.relocate(),
            KeyCode::Esc => ed.clear_selection(),
            _ => {}
        }
    }

    fn redo(&mut self) {
        if !self.ed.redo() {
            self.say("Nothing to redo");
        }
    }

    fn wrap(&mut self, delim: &str) {
        if let Err(msg) = self.ed.toggle_wrap(delim) {
            self.say(msg);
        }
    }

    fn mouse(&mut self, m: MouseEvent) {
        // A click picks the side. Inside the pane the mouse is ours, not the
        // assistant's: drag selects (copied on release), the wheel scrolls back.
        if self.pane.is_some() && self.save_as.is_none() && self.picker.is_none() {
            let in_pane = m.column >= self.pane_x;
            if matches!(m.kind, MouseEventKind::Down(_)) {
                self.pane_focused = in_pane;
            }
            let dragging = matches!(m.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)) && self.pane_focused;
            if in_pane || dragging {
                let screen = self.pane_screen;
                let row = m.row.saturating_sub(screen.y);
                let col = m.column.saturating_sub(screen.x);
                let mut copied = None;
                if let Some(pane) = &mut self.pane {
                    match m.kind {
                        MouseEventKind::ScrollUp => pane.scroll(3),
                        MouseEventKind::ScrollDown => pane.scroll(-3),
                        MouseEventKind::Down(MouseButton::Left) => pane.select_from(row, col),
                        MouseEventKind::Drag(MouseButton::Left) => pane.select_to(row, col),
                        MouseEventKind::Up(MouseButton::Left) => copied = pane.selected_text(),
                        _ => {}
                    }
                }
                if let Some(text) = copied {
                    clipboard::copy(&text);
                    self.ed.clipboard = text;
                    self.say("Copied");
                }
                return;
            }
        }
        if let Some(prompt) = &mut self.save_as {
            match m.kind {
                MouseEventKind::ScrollUp => prompt.step(-1),
                MouseEventKind::ScrollDown => prompt.step(1),
                _ => {}
            }
            return;
        }
        if let Some(p) = &mut self.picker {
            match m.kind {
                MouseEventKind::ScrollUp => p.step(-1),
                MouseEventKind::ScrollDown => p.step(1),
                _ => {}
            }
            return;
        }
        let ed = &mut self.ed;
        match m.kind {
            MouseEventKind::ScrollUp => ed.scroll(-3),
            MouseEventKind::ScrollDown => ed.scroll(3),
            MouseEventKind::Down(MouseButton::Left) => {
                let (pos, checkbox) = ed.pos_at(m.column, m.row);
                if checkbox {
                    return ed.toggle_task(pos.row);
                }
                let double = self.last_click.is_some_and(|(at, p)| at.elapsed() < DOUBLE_CLICK && p == pos);
                if double {
                    ed.select_word_at(pos);
                    self.last_click = None;
                } else {
                    ed.move_to(pos, m.modifiers.contains(KeyModifiers::SHIFT));
                    self.last_click = Some((Instant::now(), pos));
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let (pos, _) = ed.pos_at(m.column, m.row);
                if pos != ed.cursor {
                    ed.move_to(pos, true);
                }
            }
            _ => {}
        }
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Key(key) => self.key(key),
            Event::Mouse(m) => self.mouse(m),
            // A terminal may shuffle what is on screen while it is resized or out of
            // sight, and a resize that ends at the old size is invisible to the
            // renderer. Painting everything afresh costs nothing and cannot be wrong.
            Event::Resize(..) => {
                self.ed.images.set_cell(cell_pixels());
                self.repaint = true;
            }
            Event::FocusGained => self.repaint = true,
            Event::Paste(text) if self.pane_focused && self.pane.is_some() => {
                if let Some(pane) = &mut self.pane {
                    pane.paste(&text);
                }
            }
            Event::Paste(text) => match (&mut self.save_as, &mut self.picker) {
                (Some(prompt), _) => prompt.edit(|n| n.extend(text.chars().filter(|c| !c.is_control()))),
                (None, Some(p)) => p.push(&text),
                (None, None) => self.ed.insert_str(&text),
            },
            _ => {}
        }
    }
}

/// Pixel size of one cell, for sizing images. Terminals that do not report it get a typical 1:2 cell.
fn cell_pixels() -> (u16, u16) {
    match window_size() {
        Ok(s) if s.width > 0 && s.height > 0 && s.columns > 0 && s.rows > 0 => (s.width / s.columns, s.height / s.rows),
        _ => (10, 20),
    }
}

fn restore_terminal(enhanced_keys: bool) {
    let mut out = stdout();
    let _ = out.write_all(b"\x1b_Ga=d,d=A,q=2\x1b\\");
    if enhanced_keys {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableFocusChange, DisableBracketedPaste, DisableMouseCapture, SetCursorStyle::DefaultUserShape, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_on_the_command_line_are_a_name_unless_they_are_a_path() {
        let t = |words: &[&str]| target(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>());
        assert_eq!(t(&[]), Target::Untitled);
        assert_eq!(t(&["welc"]), Target::Find("welc".into()));
        assert_eq!(t(&["readme"]), Target::Find("readme".into()));
        assert_eq!(t(&["my", "ideas"]), Target::Find("my ideas".into()));
        for file in ["notes/todo", "./draft", "~/test.md", "new-note.md", "LOG.TXT", "../x"] {
            assert_eq!(t(&[file]), Target::File(file.into()), "{file}");
        }
        // A real file wins even without an extension (run from the project root).
        assert_eq!(t(&["Makefile"]), Target::File("Makefile".into()));
    }
}

const HELP: &str = "\
omanote — a small markdown note editor

  omanote                     start a new note; Ctrl+S asks where to keep it
  omanote <name>              find a note by name in this folder and every vault:
                              one match opens, several are listed (Tab, Enter),
                              none starts a new note. Case and .md do not matter.
  omanote <path/to/file.md>   open (or start) that file
  omanote --demo              a note that shows off what the editor renders
  Ctrl+N inside the editor    start a new note (asks to save an unnamed one first)
  Ctrl+P inside the editor    fuzzy-find a note in any vault
  F2 inside the editor        move, rename or copy the note (another vault, another folder)
  Ctrl+G inside the editor    an AI agent in a pane beside the note: pick from the agent
                              CLIs you have installed (Claude Code, Codex, Gemini, …) or
                              your own from the settings. Ctrl+G again moves between the
                              two; quit the agent to close the pane

Vaults (where Ctrl+P looks; new notes go in ~/.omanote/docs):
  omanote --vl <folder>       add a folder of notes
  omanote --vlgh <owner/repo> clone a GitHub repo into ~/.omanote/vaults and add it
  omanote --vlrm <name>       forget a vault (folder, owner/repo or name); files are kept
  omanote --vls               list vaults
  omanote --sync              sync every GitHub vault now: pull, then push

Desktop:
  omanote --new [name]        a new note, even if one by that name exists
  omanote --find [words]      matching notes as JSON (what the Omarchy search popup asks)
  omanote --capture <text>    add a line to the inbox note without opening the editor
  omanote --omarchy           add omanote to the app launcher and the Omarchy menu

  omanote --config            edit the settings (text width, left/center/right, margins);
                              saving applies them straight away
  Ctrl+L inside the editor    repaint the screen

  --keys                      show every key event (for diagnosing a terminal)
  -h, --help                  this text
";

/// What the non-flag words on the command line ask for.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    Untitled,
    /// `--new`: an empty note, with a name to suggest when it is saved.
    New(String),
    File(PathBuf),
    /// A name to look for in the current folder and the vaults.
    Find(String),
}

/// One word that exists as a file, or is plainly a path, is a file. Anything
/// else is a name to search for: `omanote my ideas`, `omanote readme`.
fn target(words: &[String]) -> Target {
    match words {
        [] => Target::Untitled,
        [one] => {
            let path = PathBuf::from(one);
            let lower = one.to_lowercase();
            let pathlike = one.contains('/') || one.starts_with(['.', '~']) || [".md", ".markdown", ".txt"].iter().any(|e| lower.ends_with(e));
            if pathlike || path.is_file() { Target::File(path) } else { Target::Find(one.clone()) }
        }
        many => Target::Find(many.join(" ")),
    }
}

/// Handle the flags that do their job and exit. Returns what is left: the note to open.
fn cli(args: &[String]) -> Result<(Target, bool, bool), String> {
    let home = vaults::home();
    let (mut words, mut keys, mut demo) = (Vec::new(), false, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |what: &str| it.next().cloned().ok_or(format!("{arg} needs {what}"));
        let done = match arg.as_str() {
            "--vl" => vaults::add_local(&home, &value("a folder")?)?,
            "--vlgh" => vaults::add_github(&home, &value("a GitHub repo, like owner/repo")?)?,
            "--vlrm" => vaults::remove(&home, &value("the vault to remove")?)?,
            "--vls" => vaults::list(&home),
            "--omarchy" => desktop::integrate(),
            "--find" => {
                let query: Vec<String> = it.by_ref().cloned().collect();
                picker::find_json(&vaults::all(&home), &query.join(" "), 60)
            }
            "--new" => {
                // A new note no matter what exists; the words become its suggested name.
                let name: Vec<String> = it.by_ref().cloned().collect();
                return Ok((Target::New(name.join(" ")), keys, demo));
            }
            "--capture" => {
                // Everything after the flag is the note, quoted or not.
                let text: Vec<String> = it.by_ref().cloned().collect();
                let vault = vaults::all(&home).first().map(|v| v.path.clone()).unwrap_or_default();
                capture::capture(&vault, &text.join(" "))?
            }
            "--config" => {
                // Open the settings in the editor itself; saving applies them.
                let file = config::ensure(&home).map_err(|e| format!("cannot write the config: {e}"))?;
                return Ok((Target::File(file), keys, demo));
            }
            "--sync" => {
                let ok = sync::sync_all(&home, &vaults::all(&home));
                std::process::exit(if ok { 0 } else { 1 });
            }
            "-h" | "--help" => HELP.to_string(),
            "--keys" => {
                keys = true;
                continue;
            }
            "--demo" => {
                demo = true;
                continue;
            }
            flag if flag.starts_with('-') && flag.len() > 1 => return Err(format!("unknown option {flag} — see omanote --help")),
            word => {
                words.push(word.to_string());
                continue;
            }
        };
        println!("{}", done.trim_end());
        std::process::exit(0);
    }
    Ok((target(&words), keys, demo))
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let (target, keys, demo) = cli(&args).unwrap_or_else(|msg| {
        eprintln!("omanote: {msg}");
        std::process::exit(2);
    });
    let key_log = match keys {
        true => Some(std::fs::File::create(std::env::temp_dir().join("omanote-keys.log"))?),
        false => None,
    };
    // A name is looked up before the screen is taken over: one match is simply
    // the file to open; several leave the list up; none means a new note.
    let here = std::env::current_dir().ok();
    let mut early_sync = sync::Sync::new(vaults::home());
    let mut picker = None;
    let mut wanted = None;
    let mut greeting = None;
    let path = match target {
        Target::Untitled => None,
        Target::New(name) => {
            wanted = Some(name).filter(|n| !n.trim().is_empty());
            None
        }
        Target::File(path) => Some(path),
        Target::Find(name) => {
            let all = vaults::all(&vaults::home());
            let look = |all: &[vaults::Vault]| {
                let mut found = Picker::open_with(all, here.as_deref());
                found.push(&name);
                found
            };
            let mut found = look(&all);
            // Not here — but it may have been made on GitHub or another machine.
            // This is the one time waiting for a pull beats starting a new note.
            if found.resolve() == Resolution::Nothing && early_sync.has_github(&all) {
                eprintln!("“{name}” is not here yet — checking GitHub…");
                if !early_sync.pull_now(&all, PULL_PATIENCE) {
                    eprintln!("GitHub is slow to answer; carrying on (the pull continues in the background).");
                }
                found = look(&all);
            }
            match found.resolve() {
                Resolution::Open(path) => Some(path),
                Resolution::Choose => {
                    picker = Some(found);
                    None
                }
                Resolution::Nothing => {
                    greeting = Some(format!("No note called “{name}” — this is a new one"));
                    wanted = Some(name);
                    None
                }
            }
        }
    };
    let text = match &path {
        Some(p) if p.exists() => std::fs::read_to_string(p)?,
        Some(_) => String::new(),
        None if demo => DEMO.to_string(),
        None => String::new(),
    };

    enable_raw_mode()?;
    // First thing on the wire: its answer must not be mistaken for typing later.
    theme::init(theme::detect());
    let enhanced_keys = supports_keyboard_enhancement().unwrap_or(false);
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, EnableFocusChange, SetCursorStyle::SteadyBar)?;
    if enhanced_keys {
        execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
    }
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal(enhanced_keys);
        hook(info);
    }));

    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    let mut app = App {
        ed: Editor::new("", None),
        picker,
        save_as: None,
        wanted,
        pane: None,
        pane_focused: false,
        chooser: None,
        pane_started: Instant::now(),
        pane_failed: false,
        pane_x: u16::MAX,
        pane_screen: ratatui::layout::Rect::default(),
        sync: early_sync,
        seen_saves: 0,
        toast: None,
        last_click: None,
        enhanced_keys,
        image_mode: images::detect(),
        settings: config::Config::default(),
        repaint: false,
        key_log,
        quit: false,
    };
    // Freshen every GitHub vault as we start, in the background, so a note made
    // elsewhere is here by the time it is looked for.
    app.sync.pull_all(&vaults::all(&vaults::home()));
    app.ed = app.editor(&text, path);
    if let Some(msg) = greeting {
        app.say(msg);
    }

    let all_vaults = vaults::all(&vaults::home());
    let (settings, problems) = config::load(&vaults::home());
    app.settings = settings;
    if let Some(first) = problems.first() {
        app.say(format!("config.toml, {first}"));
    }
    let result = (|| -> std::io::Result<()> {
        let mut redraw = true;
        let mut drawn = Instant::now();
        while !app.quit {
            app.reap_pane();
            // The assistant prints whenever it likes, so with the pane open we
            // look often, but only draw when something actually changed.
            let printed = app.pane.as_ref().is_some_and(|p| p.take_dirty());
            if redraw || printed || drawn.elapsed() >= Duration::from_millis(250) {
                redraw = false;
                drawn = Instant::now();
                if app.toast.as_ref().is_some_and(|(_, at)| at.elapsed() > TOAST) {
                    app.toast = None;
                }
                app.tick();
                let toast = app.toast.as_ref().map(|(msg, _)| msg.clone());
                let syncing = app.ed.path.as_ref().and_then(|p| app.sync.state(p, &all_vaults));
                let size = terminal.size()?;
                if std::mem::take(&mut app.repaint) {
                    terminal.clear()?;
                }
                let full = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                let (note, pane_area) = ui::split(full, app.pane.is_some(), app.pane_focused);
                app.pane_x = pane_area.map_or(u16::MAX, |r| r.x);
                if let (Some(pane), Some(rect)) = (&mut app.pane, pane_area) {
                    let screen = ui::pane_screen(rect);
                    pane.resize(screen.height, screen.width);
                    app.pane_screen = screen;
                }
                // Images have to be in the terminal before the frame that refers to them.
                if let Some((x, y, w, h, table_w)) = ui::text_area(note, &app.settings).filter(|_| note.width > 0) {
                    app.ed.table_w = table_w;
                    app.ed.set_view(x, y, w, h);
                    let pending = app.ed.images.take_outbox();
                    if !pending.is_empty() {
                        let mut out = stdout();
                        out.write_all(&pending)?;
                        out.flush()?;
                    }
                }
                let assistant = app.pane.as_ref().map(|p| (p, app.pane_focused));
                terminal.draw(|f| ui::draw(f, &mut app.ed, app.picker.as_ref(), app.save_as.as_ref(), toast.as_deref(), syncing, &app.settings, app.enhanced_keys, assistant, app.chooser.as_ref()))?;
            }

            let wait = Duration::from_millis(if app.pane.is_some() { 25 } else { 250 });
            if event::poll(wait)? {
                // Drain everything pending so a burst of input costs one redraw.
                loop {
                    app.handle(event::read()?);
                    if app.quit || !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
                redraw = true;
            } else if app.ed.dirty && app.ed.path.is_some() && app.ed.last_change.elapsed() > AUTOSAVE_IDLE {
                match app.ed.save() {
                    Ok(_) => app.settings_saved(false),
                    Err(e) => app.say(format!("Could not save: {e}")),
                }
                redraw = true;
            }
        }
        Ok(())
    })();

    restore_terminal(enhanced_keys);
    result
}
