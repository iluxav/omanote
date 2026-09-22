mod agents;
mod capture;
mod clipboard;
mod commands;
mod config;
mod desktop;
mod diacritics;
mod editor;
mod find;
mod grammar;
mod images;
mod layout;
mod look;
mod markdown;
mod mention;
mod now;
mod pane;
mod picker;
mod remind;
mod saveas;
mod spell;
mod sync;
mod table;
mod theme;
mod ui;
mod vaults;

use std::io::{Write, stdout};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
use mention::{Dest, Mention};
use picker::{Picker, Resolution};
use saveas::{After, SaveAs};

const DEMO: &str = include_str!("../demo.md");
const AUTOSAVE_IDLE: Duration = Duration::from_millis(1500);
const TOAST: Duration = Duration::from_secs(2);
/// How long the typing has to pause before the spelling is checked.
const SPELL_SETTLE: Duration = Duration::from_millis(400);
const DOUBLE_CLICK: Duration = Duration::from_millis(350);
/// How long `omanote <name>` waits for GitHub before giving up and starting a new note.
const PULL_PATIENCE: Duration = Duration::from_secs(8);
/// Pasted images wider than this are scaled down before they are embedded.
const PASTE_MAX_WIDTH: u32 = 2000;

/// The note that does not have the keyboard, when two are open side by side.
/// The one being written in is always `App::ed`; changing sides swaps them,
/// so everything the editor can do works the same on either side.
struct Parked {
    ed: Editor,
    history: Vec<(PathBuf, Pos)>,
    forward: Vec<(PathBuf, Pos)>,
    seen_saves: u64,
}

struct App {
    ed: Editor,
    other: Option<Parked>,
    /// With two notes open: the one being written in is the right-hand one.
    on_right: bool,
    /// Where the other note is on screen (if there is room for it), for the mouse.
    other_area: Option<ratatui::layout::Rect>,
    picker: Option<Picker>,
    save_as: Option<SaveAs>,
    /// The note suggestions open under an `@` being typed.
    mention: Option<Mention>,
    /// Ctrl+R: the list of your own commands, and the one that is running.
    palette: Option<commands::Palette>,
    running: Option<commands::Running>,
    /// Spelling: the checker on its thread, what it found in the note on
    /// screen (and which edit that answers), and the F7 fix list.
    spell: Option<spell::Checker>,
    /// The grammar model's checker, when its file is there, and what each checker last found.
    grammar: Option<grammar::Checker>,
    spelling_found: Vec<spell::Problem>,
    grammar_found: Vec<spell::Problem>,
    grammar_asked: u64,
    problems: Vec<spell::Problem>,
    spell_note: u64,
    spell_asked: u64,
    spell_answered: u64,
    ignored: std::collections::HashSet<String>,
    fixer: Option<spell::Fixer>,
    /// Shift+F7 switches the checking on and off; remembered between sessions.
    /// Off until it is asked for: the grammar model is downloaded then.
    checking: bool,
    downloading: Option<grammar::Download>,
    spell_dialect: Option<harper_core::Dialect>,
    /// Ctrl+F: the find bar, and what was searched for last (for F3).
    find: Option<find::Find>,
    last_find: String,
    /// Notes left behind on the way here, and where the cursor was in each:
    /// Alt+← walks back through them, Alt+→ forward again.
    history: Vec<(PathBuf, Pos)>,
    forward: Vec<(PathBuf, Pos)>,
    /// The assistant's terminal beside the note, and whether it has the keyboard.
    pane: Option<pane::Pane>,
    pane_focused: bool,
    chooser: Option<agents::Chooser>,
    /// An agent that quit the moment it started most likely printed why. Its
    /// pane then stays up until a key is pressed, so the reason can be read.
    pane_started: Instant,
    pane_failed: bool,
    /// The file that tells the agent what is on screen now, the agent's name
    /// and the folder it runs in. The file goes when the pane does.
    now: Option<now::Now>,
    pane_agent: String,
    pane_dir: PathBuf,
    pane_inbox: Option<PathBuf>,
    pane_vaults: Vec<(PathBuf, bool)>,
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
                look::apply(&settings.look);
                self.settings = settings;
                self.spell_setup();
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
        self.refresh_other();
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
    /// The note beside this one changed on disk (the agent, a sync): show that.
    /// It was saved when the keyboard left it, so there is nothing to lose.
    fn refresh_other(&mut self) {
        let Some(parked) = &self.other else { return };
        let Some(path) = parked.ed.path.clone() else { return };
        let on_disk = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if on_disk.is_none() || on_disk == parked.ed.disk_mtime || parked.ed.dirty {
            return;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let (cursor, top, top_skip) = (parked.ed.cursor, parked.ed.top, parked.ed.top_skip);
        let mut fresh = self.editor_with(&text, Some(path), parked.ed.images.high_ids());
        let row = cursor.row.min(fresh.lines.len() - 1);
        fresh.move_to(Pos { row, col: cursor.col.min(fresh.lines[row].len()) }, false);
        (fresh.top, fresh.top_skip) = (top.min(fresh.lines.len() - 1), top_skip);
        if let Some(parked) = &mut self.other {
            parked.ed = fresh;
            parked.seen_saves = 0;
        }
    }

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

        let now = match now::Now::new(&vaults::home()) {
            Ok(now) => now,
            Err(e) => return self.say(format!("Could not write the agent's context file: {e}")),
        };
        let mut context = format!(
            "The user is writing markdown notes in omanote, a terminal editor, and you are in a pane beside it. \
             Right now the note is {file} (full path: {}) and their cursor is on line {}. They move between notes \
             while you run, so before acting on \"this note\", \"here\", \"this line\" or \"what I selected\", read {}: \
             omanote keeps that file up to date with the note that is open, the cursor line and the selected text, \
             and it says where their other notes are and how to work with them and their inbox. \
             omanote saves as they type and reloads a note when it changes on disk, so when asked to change the \
             text, edit the file directly. Your pane is narrow: keep replies short.",
            path.display(),
            self.ed.cursor.row + 1,
            now.path().display()
        );
        if let Some(selected) = self.ed.selected_text().filter(|t| !t.trim().is_empty()) {
            let selected: String = selected.chars().take(4000).collect();
            context.push_str(&format!("\n\nThey have this text selected:\n{selected}"));
        }
        let command = agent
            .command
            .replace("{context}", "\"$OMANOTE_CONTEXT\"")
            .replace("{file}", "\"$OMANOTE_FILE\"")
            .replace("{dir}", "\"$OMANOTE_DIR\"")
            .replace("{nowdir}", "\"$OMANOTE_NOW_DIR\"")
            .replace("{now}", "\"$OMANOTE_NOW\"");
        let env = [
            ("OMANOTE_CONTEXT", context),
            ("OMANOTE_FILE", file),
            ("OMANOTE_DIR", dir.to_string_lossy().into_owned()),
            ("OMANOTE_NOW", now.path().to_string_lossy().into_owned()),
            ("OMANOTE_NOW_DIR", now.dir().to_string_lossy().into_owned()),
        ];
        match pane::Pane::spawn(&command, &dir, &env, 24, 60) {
            Ok(pane) => {
                self.pane = Some(pane);
                self.now = Some(now);
                self.pane_agent = agent.name.clone();
                self.pane_dir = dir;
                self.pane_inbox = all.first().map(|v| capture::inbox(&v.path));
                self.pane_vaults = all.iter().map(|v| (v.path.clone(), v.github.is_some())).collect();
                // Before the agent has had time to look.
                self.tell_agent();
                self.pane_started = Instant::now();
                self.pane_failed = false;
                self.pane_focused = true;
                self.repaint = true;
            }
            Err(e) => self.say(e),
        }
    }

    /// Keep the agent's view of things true: the file it was told to read,
    /// and the pane's title, which is how you can see what it has been told.
    fn tell_agent(&mut self) {
        let (Some(now), false) = (&mut self.now, self.pane_failed) else { return };
        let ed = &self.ed;
        let note = ed.path.as_ref().map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()));
        let text: String = ed.lines[ed.cursor.row].iter().collect();
        let selected = ed.selected_text();
        let beside = self.other.as_ref().and_then(|o| o.ed.path.as_ref()).map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()));
        let looking = now::Looking { note: note.as_deref(), dir: &self.pane_dir, line: ed.cursor.row + 1, text: &text, selected: selected.as_deref(), unsaved: ed.dirty, inbox: self.pane_inbox.as_deref(), beside: beside.as_deref().map(|p| (p, !self.on_right)), vaults: &self.pane_vaults };
        if let Err(e) = now.set(now::describe(&looking)) {
            self.now = None;
            return self.say(format!("The agent can no longer be told where you are: {e}"));
        }
        let name = note.as_ref().and_then(|p| p.file_name()).map_or("a new note".into(), |n| n.to_string_lossy().into_owned());
        if let Some(pane) = &mut self.pane {
            pane.label = format!("{} · {name}", self.pane_agent);
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
            self.now = None;
            self.pane_failed = false;
            self.pane_focused = false;
            self.pane_x = u16::MAX;
            self.repaint = true;
            self.say("AI chat closed");
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
        // Alt+← leads back to the note this one was started from.
        if let Some(from) = self.ed.path.clone() {
            self.history.push((from, self.ed.cursor));
            self.forward.clear();
        }
        self.mention = None;
        self.ed = self.editor("", None);
        self.seen_saves = 0;
        self.say("New note — Ctrl+S to choose where it goes");
    }

    /// Switch to another note, saving the current one first.
    fn open(&mut self, path: PathBuf) {
        // Two live copies of one file would overwrite each other: go to the one there is.
        if self.is_other(&path) {
            return self.swap_focus();
        }
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
        // Wherever this was opened from (a link, Ctrl+P), Alt+← leads back.
        if let Some(from) = self.ed.path.clone().filter(|p| *p != path) {
            self.history.push((from, self.ed.cursor));
            self.history.drain(..self.history.len().saturating_sub(HISTORY));
            self.forward.clear();
        }
        // Send what the last note left behind, and freshen the new one's vault.
        self.note_saves();
        self.sync.flush();
        self.sync.opened(&path, &vaults::all(&vaults::home()));
        self.ed = self.editor(&text, Some(path));
        self.seen_saves = 0;
        self.mention = None;
        self.close_find();
    }

    fn editor(&self, text: &str, path: Option<PathBuf>) -> Editor {
        self.editor_with(text, path, self.other.as_ref().is_some_and(|o| !o.ed.images.high_ids()))
    }

    /// `high_ids`: number its images apart from those of the note beside it.
    fn editor_with(&self, text: &str, path: Option<PathBuf>, high_ids: bool) -> Editor {
        let vault = vaults::root_of(path.as_deref(), &vaults::all(&vaults::home()));
        let images = Images::new(self.image_mode, cell_pixels(), path.as_deref(), vault);
        Editor::new(text, path).with_images(if high_ids { images.with_high_ids() } else { images })
    }

    fn is_other(&self, path: &std::path::Path) -> bool {
        let same = |a: &PathBuf| std::path::absolute(a).ok() == std::path::absolute(path).ok();
        self.other.as_ref().is_some_and(|o| o.ed.path.as_ref().is_some_and(same))
    }

    /// F6, or a click in the other note: move the keyboard across.
    fn swap_focus(&mut self) {
        if self.other.is_none() {
            return self.say("There is no other note open · Alt+O on a link opens it beside this one");
        }
        if self.ed.dirty && self.ed.path.is_some() {
            if let Err(e) = self.ed.save() {
                return self.say(format!("Could not save, staying here: {e}"));
            }
        }
        self.note_saves();
        let Some(mut parked) = self.other.take() else { return };
        std::mem::swap(&mut self.ed, &mut parked.ed);
        std::mem::swap(&mut self.history, &mut parked.history);
        std::mem::swap(&mut self.forward, &mut parked.forward);
        std::mem::swap(&mut self.seen_saves, &mut parked.seen_saves);
        self.other = Some(parked);
        self.on_right = !self.on_right;
        self.mention = None;
        self.toast = None;
    }

    /// Open a note on the right and keep this one on the left. With two notes
    /// open already, the right-hand one is where it goes.
    fn open_aside(&mut self, path: PathBuf) {
        if self.is_other(&path) {
            return self.swap_focus();
        }
        if self.other.is_some() {
            if !self.on_right {
                self.swap_focus();
            }
            return self.open(path);
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return self.say(format!("Could not open {}: {e}", path.display())),
        };
        if self.ed.dirty && self.ed.path.is_some() {
            if let Err(e) = self.ed.save() {
                return self.say(format!("Could not save, staying here: {e}"));
            }
        }
        self.note_saves();
        let left = std::mem::replace(&mut self.ed, Editor::new("", None));
        self.other = Some(Parked { ed: left, history: std::mem::take(&mut self.history), forward: std::mem::take(&mut self.forward), seen_saves: self.seen_saves });
        self.on_right = true;
        self.sync.opened(&path, &vaults::all(&vaults::home()));
        self.ed = self.editor(&text, Some(path));
        self.seen_saves = 0;
        self.mention = None;
        self.repaint = true;
    }

    /// Ctrl+Q with two notes open closes the one being written in; the other
    /// gets the window, and the keyboard.
    fn close_side(&mut self) {
        if self.unsaved_draft() {
            return self.ask_where(After::Quit);
        }
        if self.ed.dirty && self.ed.path.is_some() {
            if let Err(e) = self.ed.save() {
                return self.say(format!("Could not save, staying here: {e}"));
            }
        }
        self.note_saves();
        self.sync.flush();
        let Some(parked) = self.other.take() else { return };
        self.ed = parked.ed;
        self.history = parked.history;
        self.forward = parked.forward;
        self.seen_saves = parked.seen_saves;
        self.on_right = false;
        self.other_area = None;
        self.mention = None;
        self.repaint = true;
    }

    fn picker_key(&mut self, key: KeyEvent) {
        let Some(p) = &mut self.picker else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Enter => {
                let found = p.found();
                if let Some(path) = p.chosen() {
                    self.picker = None;
                    self.open(path.clone());
                    // A line found by `>words`: land on it, with the match selected, and
                    // let F3 carry on looking for the same word inside the note.
                    if let Some(found) = found.filter(|_| self.ed.path.as_ref() == Some(&path)) {
                        self.land_on(&found);
                    }
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

    /// Leaving without being able to ask anything: the window was closed.
    fn leave(&mut self) {
        if self.ed.dirty && self.ed.path.is_some() {
            let _ = self.ed.save();
        }
        self.note_saves();
        self.sync.flush();
        self.quit = true;
    }

    /// `--line 12 --match kyoto`: the cursor goes to that line, and if the word
    /// is given, onto the nearest place it occurs, selected, with F3 primed to
    /// find the next. (Nearest, not "on that line": pasted images are kept out
    /// of the editor's lines, so the file's line 12 may be the editor's 11.)
    fn land_at(&mut self, line: usize, word: &str) {
        let want = line.saturating_sub(1).min(self.ed.lines.len() - 1);
        let nearest = find::search(&self.ed.lines, word).into_iter().min_by_key(|hit| hit.0.abs_diff(want));
        match nearest {
            Some((row, from, to)) => {
                self.ed.move_to(Pos { row, col: from }, false);
                self.ed.move_to(Pos { row, col: to }, true);
                self.last_find = word.to_string();
            }
            None => self.ed.move_to(Pos { row: want, col: 0 }, false),
        }
    }

    fn land_on(&mut self, found: &picker::Found) {
        let is_it = |row: usize| self.ed.lines.get(row).is_some_and(|l| l.iter().collect::<String>() == found.text);
        // Pasted images are kept out of the editor's lines, so a line may sit
        // a little higher here than it does in the file: look for its text.
        let Some(row) = (is_it(found.line)).then_some(found.line).or_else(|| (0..self.ed.lines.len()).find(|&r| is_it(r))) else { return };
        self.ed.move_to(Pos { row, col: found.mark.0 }, false);
        self.ed.move_to(Pos { row, col: found.mark.1 }, true);
        self.last_find = found.term.clone();
    }

    fn quit(&mut self) {
        if self.other.is_some() {
            return self.close_side();
        }
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
        if self.palette.is_some() {
            return self.palette_key(key);
        }
        if self.fixer.is_some() {
            return self.fixer_key(key);
        }
        if self.find.is_some() {
            return self.find_key(key);
        }
        // Esc stops a command that is running; a key of yours starts one.
        if key.code == KeyCode::Esc && self.running.is_some() && self.mention.is_none() {
            if let Some(running) = &self.running {
                running.cancel();
            }
            return;
        }
        if let Some(at) = commands::bound(&self.settings.commands, &key) {
            return self.run_command(at);
        }
        if self.mention_key(key) {
            return;
        }
        self.editor_key(key);
        self.mention_after(key);
    }

    // ---- spelling ----------------------------------------------------------

    /// Start, stop or restart the checker to match the settings.
    fn spell_setup(&mut self) {
        let wanted = (self.settings.spelling && self.checking).then(|| spell::dialect(&self.settings.dialect)).flatten();
        match (wanted, &self.spell) {
            (None, None) => {}
            (None, Some(_)) => {
                self.spell = None;
                self.grammar = None;
                self.problems.clear();
                self.fixer = None;
            }
            (Some(dialect), current) => {
                if current.is_none() || self.spell_dialect != Some(dialect) {
                    self.spell = Some(spell::Checker::start(&vaults::home(), dialect));
                    self.spell_dialect = Some(dialect);
                    self.spell_asked = u64::MAX;
                }
                // The grammar model, if its file is there. It is English whatever the dialect.
                let model = grammar::model_path();
                if self.grammar.is_none() && model.exists() {
                    self.grammar = Some(grammar::Checker::start(model));
                    self.grammar_asked = u64::MAX;
                }
            }
        }
    }

    /// Ask for a check once the typing has paused, and take in what came
    /// back. True when the screen should change.
    fn spell_tick(&mut self) -> bool {
        let Some(checker) = &self.spell else { return false };
        if self.spell_note != self.ed.id {
            self.spell_note = self.ed.id;
            self.spell_asked = u64::MAX;
            self.grammar_asked = u64::MAX;
            self.problems.clear();
            self.spelling_found.clear();
            self.grammar_found.clear();
            self.fixer = None;
        }
        if !self.ed.markdown {
            return !std::mem::take(&mut self.problems).is_empty();
        }
        if self.spell_asked != self.ed.edits && self.ed.last_change.elapsed() >= SPELL_SETTLE {
            self.spell_asked = self.ed.edits;
            let text: Vec<String> = self.ed.lines.iter().map(|l| l.iter().collect()).collect();
            checker.check(self.spell_asked, text.join("\n"));
        }
        let mut changed = false;
        while let Some((n, found)) = checker.results() {
            if n == self.spell_asked {
                self.spelling_found = found;
                self.spell_answered = n;
                changed = true;
            }
        }
        if let Some(model) = &self.grammar {
            if self.grammar_asked != self.ed.edits && self.ed.last_change.elapsed() >= SPELL_SETTLE {
                self.grammar_asked = self.ed.edits;
                // Prose only: headings, lists, quotes and paragraphs; not code, tables or long pasted lines.
                let lines: Vec<(usize, String)> = self
                    .ed
                    .lines
                    .iter()
                    .enumerate()
                    .filter(|(row, line)| matches!(self.ed.blocks.get(*row), Some(markdown::Block::Normal)) && !line.is_empty() && line.len() < 600)
                    .map(|(row, line)| (row, line.iter().collect()))
                    .collect();
                model.check(self.grammar_asked, lines);
            }
            let mut broken = None;
            while let Some(news) = model.news() {
                match news {
                    grammar::News::Found(n, found) if n == self.grammar_asked => {
                        self.grammar_found = found;
                        changed = true;
                    }
                    grammar::News::Unavailable(why) => broken = Some(why),
                    _ => {}
                }
            }
            if let Some(why) = broken {
                self.grammar = None;
                self.say(format!("Grammar checking is off: {why}"));
            }
        }
        if changed {
            self.merge_problems();
        }
        changed
    }

    /// One list from both checkers, in reading order. Where both mark the same
    /// words, one mark, with the grammar model's fix first: it has read the sentence.
    fn merge_problems(&mut self) {
        let mut all: Vec<spell::Problem> = self.grammar_found.clone();
        for p in &self.spelling_found {
            match all.iter_mut().find(|g| g.row == p.row && g.from < p.to && p.from < g.to) {
                Some(g) if g.from == p.from && g.to == p.to => {
                    for fix in &p.fixes {
                        if !g.fixes.contains(fix) {
                            g.fixes.push(fix.clone());
                        }
                    }
                    g.spelling |= p.spelling;
                }
                Some(_) => {}
                None => all.push(p.clone()),
            }
        }
        all.retain(|p| !self.ignored.contains(&p.word.to_lowercase()));
        all.sort_by_key(|p| (p.row, p.from));
        self.problems = all;
    }

    fn problem_at_cursor(&self) -> Option<&spell::Problem> {
        self.problems.iter().find(|p| p.has(self.ed.cursor) && p.still_there(&self.ed.lines))
    }

    /// F7: the fix list for the problem under the cursor, or the next one along.
    fn next_problem(&mut self) {
        if self.spell.is_none() {
            return self.say(if self.checking { "Spelling is off · spelling = true in the settings (omanote --config) turns it on" } else { "Checking is off · Shift+F7 turns it on" });
        }
        if !self.ed.markdown {
            return self.say("Only markdown notes are checked");
        }
        let lines = &self.ed.lines;
        self.problems.retain(|p| p.still_there(lines));
        let was_open = self.fixer.take().is_some();
        if self.problems.is_empty() {
            return self.say(if self.spell_answered == self.ed.edits { "No spelling or grammar problems found" } else { "Still checking…" });
        }
        let cursor = self.ed.cursor;
        let here = self.problems.iter().position(|p| p.has(cursor));
        let at = match here {
            Some(i) if !was_open => i,
            Some(i) => (i + 1) % self.problems.len(),
            None => self.problems.iter().position(|p| (p.row, p.from) > (cursor.row, cursor.col)).unwrap_or(0),
        };
        let problem = self.problems[at].clone();
        self.ed.move_to(Pos { row: problem.row, col: problem.from }, false);
        self.mention = None;
        self.fixer = Some(spell::Fixer { problem, selected: 0 });
    }

    /// Shift+F7: spelling and grammar on, or off with their memory freed. The
    /// first time on, the grammar model is fetched first.
    fn toggle_checking(&mut self) {
        if let Some(download) = self.downloading.take() {
            download.cancel();
            return self.say("Download stopped · Shift+F7 starts it again");
        }
        if self.checking {
            self.set_checking(false);
            return self.say("Spelling and grammar off · Shift+F7 turns them back on");
        }
        if !grammar::model_path().exists() {
            self.downloading = Some(grammar::Download::start(grammar::model_path()));
            return;
        }
        self.set_checking(true);
        self.say(if self.settings.spelling { "Spelling and grammar on · Shift+F7 turns them off" } else { "Checking is on, but spelling = false in the settings keeps it off" });
    }

    fn set_checking(&mut self, on: bool) {
        self.checking = on;
        let file = checking_on_file();
        let _ = if on { std::fs::write(&file, "") } else { std::fs::remove_file(&file) };
        if on {
            self.spell_setup();
        } else {
            self.spell = None;
            self.grammar = None;
            self.spell_dialect = None;
            self.problems.clear();
            self.spelling_found.clear();
            self.grammar_found.clear();
            self.fixer = None;
        }
    }

    /// The model arrived (or did not): checking goes on, as was asked.
    fn poll_download(&mut self) -> bool {
        let Some(result) = self.downloading.as_ref().and_then(|d| d.finished()) else { return false };
        self.downloading = None;
        match result {
            Ok(()) => {
                self.set_checking(true);
                self.say(format!("Grammar model downloaded ({} MB) · spelling and grammar on", grammar::MODEL_BYTES / 1_000_000));
            }
            Err(why) if why == "cancelled" => {}
            Err(why) => self.say(format!("Could not download the grammar model: {why} · Shift+F7 tries again")),
        }
        true
    }

    /// The fix list for the problem under the cursor, if there is one.
    fn open_fixer_here(&mut self) {
        if let Some(problem) = self.problem_at_cursor().cloned() {
            self.mention = None;
            self.fixer = Some(spell::Fixer { problem, selected: 0 });
        }
    }

    fn fixer_key(&mut self, key: KeyEvent) {
        let Some(fixer) = &mut self.fixer else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.fixer = None,
            KeyCode::Char('c') if ctrl => self.fixer = None,
            KeyCode::F(7) if key.modifiers.contains(KeyModifiers::SHIFT) => self.toggle_checking(),
            KeyCode::F(19) => self.toggle_checking(),
            KeyCode::F(7) | KeyCode::Right => self.next_problem(),
            KeyCode::Up | KeyCode::BackTab => fixer.step(-1),
            KeyCode::Down | KeyCode::Tab => fixer.step(1),
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < fixer.choices().len() => {
                fixer.selected = c as usize - '1' as usize;
                self.apply_fix();
            }
            KeyCode::Enter => self.apply_fix(),
            // Anything else is typing: the list goes, the key reaches the note.
            _ => {
                self.fixer = None;
                self.editor_key(key);
                self.mention_after(key);
            }
        }
    }

    fn apply_fix(&mut self) {
        let Some(fixer) = self.fixer.take() else { return };
        if !fixer.problem.still_there(&self.ed.lines) {
            return self.say("That text has changed since it was checked");
        }
        let problem = fixer.problem.clone();
        let word = problem.word.clone();
        let choices = fixer.choices();
        match choices.get(fixer.selected) {
            Some(spell::Choice::Fix(fix)) => {
                match fix {
                    spell::Fix::Replace(with) => self.ed.replace_on_line(problem.from, problem.to, with),
                    spell::Fix::InsertAfter(with) => {
                        self.ed.move_to(Pos { row: problem.row, col: problem.to }, false);
                        self.ed.insert_str(with);
                    }
                    spell::Fix::Remove => self.ed.replace_on_line(problem.from, problem.to, ""),
                }
                self.problems.retain(|p| *p != problem);
                self.grammar_found.retain(|p| *p != problem);
                self.spelling_found.retain(|p| !(p.row == problem.row && p.from == problem.from));
                self.say("Fixed · F7 goes to the next one, Ctrl+Z undoes");
            }
            Some(spell::Choice::Learn) => {
                if let Some(checker) = &self.spell {
                    checker.learn(&word);
                    self.say(format!("“{word}” added to your dictionary ({})", vaults::tilde(&checker.dictionary_file())));
                }
                self.ignored.insert(word.to_lowercase());
                self.problems.retain(|p| p.word.to_lowercase() != word.to_lowercase());
            }
            Some(spell::Choice::Ignore) => {
                self.ignored.insert(word.to_lowercase());
                self.problems.retain(|p| p.word.to_lowercase() != word.to_lowercase());
                self.say(format!("“{word}” ignored until omanote is next started"));
            }
            None => {}
        }
    }

    /// Ctrl+R: your own commands.
    fn open_palette(&mut self) {
        if self.settings.commands.is_empty() {
            return self.say("No commands yet. Add yours in the settings (omanote --config): command.Sort lines = \"sort\"");
        }
        self.mention = None;
        self.palette = Some(commands::Palette { selected: 0 });
    }

    fn palette_key(&mut self, key: KeyEvent) {
        let Some(palette) = &mut self.palette else { return };
        let n = self.settings.commands.len().max(1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.palette = None,
            KeyCode::Char('c' | 'r') if ctrl => self.palette = None,
            KeyCode::Up | KeyCode::BackTab => palette.selected = (palette.selected + n - 1) % n,
            KeyCode::Down | KeyCode::Tab => palette.selected = (palette.selected + 1) % n,
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < n => {
                self.palette = None;
                self.run_command(c as usize - '1' as usize);
            }
            KeyCode::Enter => {
                let at = palette.selected;
                self.palette = None;
                self.run_command(at);
            }
            _ => {}
        }
    }

    /// Start one of your commands. It runs beside the editor, which stays
    /// usable; `poll_command` puts the result in when it is done.
    fn run_command(&mut self, at: usize) {
        let Some(command) = self.settings.commands.get(at).cloned() else { return };
        if let Some(running) = &self.running {
            return self.say(format!("{} is still running · Esc stops it", running.name));
        }
        // {file} should be what is on screen.
        if self.ed.dirty && self.ed.path.is_some() {
            if let Err(e) = self.ed.save() {
                return self.say(format!("Could not save: {e}"));
            }
        }
        // In the note's vault, as the assistant is; a loose file's own folder.
        let all = vaults::all(&vaults::home());
        let note = self.ed.path.as_ref().map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()));
        let vault = note.as_ref().and_then(|n| all.iter().map(|v| &v.path).filter(|v| n.starts_with(v)).max_by_key(|v| v.as_os_str().len()).cloned());
        let dir = vault.or_else(|| note.as_ref().and_then(|n| n.parent().map(PathBuf::from))).or_else(|| all.first().map(|v| v.path.clone())).unwrap_or_else(|| PathBuf::from("."));
        match commands::start(&command, &self.ed, &dir) {
            Ok(running) => {
                self.mention = None;
                self.running = Some(running);
            }
            Err(e) => self.say(e),
        }
    }

    /// Whether a running command is done; true if something changed on screen.
    fn poll_command(&mut self) -> bool {
        let Some(running) = &self.running else { return false };
        let mut spare = None;
        let commands::Outcome::Said(said) = running.finish(&mut self.ed, &mut spare) else { return false };
        self.running = None;
        if let Some(text) = spare {
            clipboard::copy(&text);
            self.ed.clipboard = text;
        }
        self.say(said);
        true
    }

    /// Ctrl+F, or F3 to carry on with the last search.
    fn open_find(&mut self, step: isize) {
        let mut find = find::Find::open(&self.ed, &self.last_find);
        self.mention = None;
        self.toast = None;
        if step != 0 && !find.query.is_empty() {
            find.refresh(&mut self.ed);
            // Already on a match (F3 pressed again): move on from it.
            if find.matches.len() > 1 {
                find.step(step, &mut self.ed);
            }
        } else {
            find.refresh(&mut self.ed);
        }
        self.find = Some(find);
    }

    fn close_find(&mut self) {
        if let Some(find) = self.find.take() {
            if !find.query.is_empty() {
                self.last_find = find.query;
            }
        }
    }

    fn find_key(&mut self, key: KeyEvent) {
        let Some(find) = &mut self.find else { return };
        let ed = &mut self.ed;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.close_find(),
            KeyCode::Enter | KeyCode::F(3) if shift => find.step(-1, ed),
            KeyCode::Enter | KeyCode::F(3) | KeyCode::Down | KeyCode::Tab => find.step(1, ed),
            KeyCode::Up | KeyCode::BackTab => find.step(-1, ed),
            KeyCode::Backspace if ctrl || alt => {
                find.delete_word();
                find.refresh(ed);
            }
            KeyCode::Backspace => {
                find.query.pop();
                find.refresh(ed);
            }
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                'f' | 'n' => find.step(1, ed),
                'p' => find.step(-1, ed),
                'u' => {
                    find.query.clear();
                    find.refresh(ed);
                }
                'w' | 'h' => {
                    find.delete_word();
                    find.refresh(ed);
                }
                'c' => self.close_find(),
                'q' => {
                    self.close_find();
                    self.quit();
                }
                _ => {}
            },
            KeyCode::Char(c) if !alt => {
                find.query.push(c);
                find.refresh(ed);
            }
            // Anything else is about the note, not the search: back to the note with it.
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Delete => {
                self.close_find();
                self.editor_key(key);
            }
            _ => {}
        }
    }

    /// Keys the `@` suggestions keep for themselves. Everything else is typing,
    /// and goes to the note as usual.
    fn mention_key(&mut self, key: KeyEvent) -> bool {
        let Some(m) = &mut self.mention else { return false };
        let chord = key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.mention = None,
            _ if m.picker.len() == 0 => return false,
            KeyCode::Up | KeyCode::BackTab => m.picker.step(-1),
            KeyCode::Down | KeyCode::Tab => m.picker.step(1),
            KeyCode::Enter if !chord => self.link_mention(),
            _ => return false,
        }
        true
    }

    /// After a key reached the note: keep the suggestions in step with what
    /// now follows the `@`, or open them if an `@` was just typed.
    fn mention_after(&mut self, key: KeyEvent) {
        if self.picker.is_some() || self.save_as.is_some() || self.chooser.is_some() || self.pane_focused {
            self.mention = None;
        } else if let Some(m) = &mut self.mention {
            if !m.update(&self.ed) {
                self.mention = None;
            }
        } else if key.code == KeyCode::Char('@') && !key.modifiers.contains(KeyModifiers::CONTROL) {
            self.mention = Mention::begin(&self.ed, &vaults::all(&vaults::home()));
        }
    }

    /// Enter on a suggestion: `@query` becomes a link to that note.
    fn link_mention(&mut self) {
        let Some(m) = self.mention.take() else { return };
        let all = vaults::all(&vaults::home());
        let got = match m.accept(self.ed.path.as_deref(), &all) {
            Ok(got) => got,
            Err(e) => return self.say(e),
        };
        if let Some(path) = &got.create {
            if let Err(e) = start_note(path) {
                return self.say(format!("Could not create {}: {e}", path.display()));
            }
        }
        self.ed.replace_on_line(m.at, self.ed.cursor.col, &got.link);
        // Inside `[text](…)` the link is finished: step out of the brackets.
        if m.bare && self.ed.lines[self.ed.cursor.row].get(self.ed.cursor.col) == Some(&')') {
            self.ed.right(false);
        }
        let how = if self.enhanced_keys { "Ctrl+Enter" } else { "Ctrl+O" };
        match got.create {
            Some(path) => self.say(format!("Created {} · {how} opens it", path.file_name().unwrap_or_default().to_string_lossy())),
            None => self.say(format!("Linked · {how} opens it")),
        }
    }

    /// Ctrl+O, Ctrl+Enter, Ctrl+click: go where the link at `pos` points.
    fn follow(&mut self, pos: Pos, aside: bool) {
        let Some(link) = self.ed.link_at(pos) else { return self.say("No link here to open") };
        let all = vaults::all(&vaults::home());
        let outside = |what: String| match mention::open_outside(&what) {
            Ok(()) => format!("Opening {what}"),
            Err(e) => e,
        };
        match mention::destination(&link, self.ed.path.as_deref(), &all) {
            Err(e) => self.say(e),
            Ok(Dest::Web(url)) => self.say(outside(url)),
            Ok(Dest::File(path)) => self.say(outside(path.to_string_lossy().into_owned())),
            Ok(Dest::Note(path)) => {
                let here = self.ed.path.as_ref().and_then(|p| std::path::absolute(p).ok());
                if here.is_some() && here == std::path::absolute(&path).ok() {
                    return self.say("That is this note");
                }
                let fresh = !path.exists();
                if aside { self.open_aside(path) } else { self.open(path) }
                if fresh && self.save_as.is_none() {
                    self.say("A new note: start writing, or Alt+← to go back");
                }
            }
        }
    }

    /// Alt+← and Alt+→: through the notes visited, like a browser. Notes that
    /// have since been moved or deleted are passed over.
    fn travel(&mut self, back: bool) {
        let (mut history, mut forward) = (std::mem::take(&mut self.history), std::mem::take(&mut self.forward));
        let (from, onto) = if back { (&mut history, &mut forward) } else { (&mut forward, &mut history) };
        let mut found = None;
        while let Some((path, pos)) = from.pop() {
            if path.exists() && !self.is_other(&path) {
                found = Some((path, pos));
                break;
            }
        }
        match found {
            None => self.say(if back { "No note to go back to" } else { "No note to go forward to" }),
            Some((path, pos)) => {
                let here = self.ed.path.clone().map(|p| (p, self.ed.cursor));
                self.open(path.clone());
                if self.ed.path.as_ref() == Some(&path) {
                    onto.extend(here);
                    let row = pos.row.min(self.ed.lines.len() - 1);
                    self.ed.move_to(Pos { row, col: pos.col.min(self.ed.lines[row].len()) }, false);
                    self.toast = None;
                } else {
                    from.push((path, pos));
                }
            }
        }
        // `open` kept its own record of this move; ours is the one that counts.
        self.history = history;
        self.forward = forward;
    }

    fn editor_key(&mut self, key: KeyEvent) {
        let ed = &mut self.ed;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                'q' => self.quit(),
                'r' => self.open_palette(),
                'f' => self.open_find(0),
                'o' => self.follow(self.ed.cursor, shift),
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
                'v' => self.paste_clipboard(),
                'k' if !ed.markdown => self.say("That is a markdown shortcut; this file is plain text"),
                'k' => {
                    let url = clipboard::paste().and_then(|t| mention::as_url(&t).map(mention::url_target));
                    let done = url.is_some();
                    match ed.link_selection(url.as_deref()) {
                        Ok(()) if done => self.say("Linked to the address in the clipboard"),
                        Ok(()) => self.say("Type or paste the address · @ picks a note"),
                        Err(msg) => self.say(msg),
                    }
                }
                'b' | 'i' | 't' if !ed.markdown => self.say("That is a markdown shortcut; this file is plain text"),
                'b' => self.wrap("**"),
                'i' => self.wrap("*"),
                't' => ed.toggle_task(ed.cursor.row),
                'h' | 'w' => ed.delete_word_back(),
                _ => {}
            },
            // Ctrl+Shift+O needs a terminal that can tell it from Ctrl+O; Alt+O always works.
            KeyCode::Char('o' | 'O') if alt => self.follow(self.ed.cursor, true),
            KeyCode::Char(c) if !alt => ed.insert_char(c),
            // Super+Shift is the natural chord, but most window managers keep Super
            // for themselves, so Alt+Shift does the same thing.
            KeyCode::Left | KeyCode::Right if shift && (alt || key.modifiers.contains(KeyModifiers::SUPER)) => {
                if !ed.add_table_column(key.code == KeyCode::Right) {
                    self.say("Put the cursor in a table to add a column");
                }
            }
            KeyCode::Enter if ctrl => self.follow(self.ed.cursor, shift || alt),
            KeyCode::Enter if shift && ed.add_table_row() => {}
            KeyCode::Left if alt => self.travel(true),
            KeyCode::Right if alt => self.travel(false),
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
            // The other copy, cut and paste: Ctrl+Insert, Shift+Delete, Shift+Insert.
            // Omarchy's Super+C and Super+V send these to terminals.
            KeyCode::Insert if ctrl => {
                clipboard::copy(&ed.copy());
                self.say("Copied");
            }
            KeyCode::Insert if shift => self.paste_clipboard(),
            KeyCode::Delete if shift => clipboard::copy(&ed.cut()),
            KeyCode::Delete if ctrl => ed.delete_word_forward(),
            KeyCode::Delete => ed.delete(),
            KeyCode::Enter => ed.enter(),
            KeyCode::Tab => ed.tab(),
            KeyCode::BackTab => ed.backtab(),
            KeyCode::F(2) => self.relocate(),
            KeyCode::F(3) => self.open_find(if shift { -1 } else { 1 }),
            KeyCode::F(6) => self.swap_focus(),
            // Shift+F7 arrives as F19 from terminals that do not report Shift with F keys.
            KeyCode::F(7) if shift => self.toggle_checking(),
            KeyCode::F(19) => self.toggle_checking(),
            KeyCode::F(7) => self.next_problem(),
            KeyCode::Esc => ed.clear_selection(),
            _ => {}
        }
    }

    /// Ctrl+V: a picture in the clipboard is embedded, anything else is text.
    fn paste_clipboard(&mut self) {
        match clipboard::image() {
            Some(_) if !self.ed.markdown => self.say("Images can be pasted into markdown notes only"),
            Some((mime, bytes)) => match images::embeddable(&mime, bytes, PASTE_MAX_WIDTH) {
                Ok(uri) => {
                    let size = uri.len();
                    let label = self.ed.paste_image(uri);
                    self.say(format!("Image embedded as [{label}] · {}", ui::human(size)));
                }
                Err(e) => self.say(e),
            },
            None => {
                let text = clipboard::paste().unwrap_or_else(|| self.ed.clipboard.clone());
                self.paste_text(&text);
            }
        }
    }

    /// Pasted text, however it got here: Ctrl+V, or the terminal's own paste
    /// (Super+V on Omarchy, Ctrl+Shift+V, the middle button). A web address
    /// becomes a link: around the selected text if there is any, otherwise
    /// under a short readable name.
    fn paste_text(&mut self, text: &str) {
        let ed = &mut self.ed;
        match mention::as_url(text).filter(|_| ed.takes_links()) {
            Some(url) if ed.selection().is_some() => {
                if let Err(msg) = ed.link_selection(Some(&mention::url_target(url))) {
                    self.say(msg);
                }
            }
            Some(url) => {
                let alone = ed.lines[ed.cursor.row].iter().all(|c| c.is_whitespace());
                ed.paste_link(url, &mention::url_link(url, alone));
                self.say("Pasted as a link · Ctrl+Z for the plain address");
            }
            None => ed.insert_str(text),
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
        // `--keys`: what the terminal says the mouse did, and where in the note
        // that lands. Bare movement is left out: it would drown the rest.
        if let Some(log) = self.key_log.as_mut().filter(|_| !matches!(m.kind, MouseEventKind::Moved)) {
            use std::io::Write;
            let (pos, _) = self.ed.pos_at(m.column, m.row);
            let line = format!("mouse {:?} {:?} at column {} row {} -> line {} col {}", m.kind, m.modifiers, m.column, m.row, pos.row + 1, pos.col + 1);
            let _ = writeln!(log, "{line}");
            self.toast = Some((line, Instant::now()));
        }
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
        // A click on one of the fixes applies it.
        if let (Some(_), MouseEventKind::Down(MouseButton::Left), Some((x, y, w, rows, first))) = (&self.fixer, m.kind, self.ed.view.fixer) {
            if (x..x + w).contains(&m.column) && (y..y + rows).contains(&m.row) {
                let at = first + (m.row - y) as usize;
                if let Some(fixer) = &mut self.fixer {
                    if at < fixer.choices().len() {
                        fixer.selected = at;
                    }
                }
                return self.apply_fix();
            }
        }
        if matches!(m.kind, MouseEventKind::Down(_)) {
            self.close_find();
            self.palette = None;
            self.fixer = None;
        }
        // Over the other note: the wheel scrolls it where it is, a click moves in.
        if self.other_area.is_some_and(|r| (r.x..r.x + r.width).contains(&m.column)) {
            match m.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let by = if m.kind == MouseEventKind::ScrollUp { -3 } else { 3 };
                    if let Some(parked) = &mut self.other {
                        parked.ed.scroll(by);
                    }
                    return;
                }
                MouseEventKind::Down(_) => self.swap_focus(),
                _ => return,
            }
        }
        // The wheel moves through the `@` suggestions; a click is the end of them.
        if let Some(mention) = &mut self.mention {
            match m.kind {
                MouseEventKind::ScrollUp => return mention.picker.step(-1),
                MouseEventKind::ScrollDown => return mention.picker.step(1),
                MouseEventKind::Down(_) => self.mention = None,
                _ => {}
            }
        }
        let ed = &mut self.ed;
        match m.kind {
            MouseEventKind::ScrollUp => ed.scroll(-3),
            MouseEventKind::ScrollDown => ed.scroll(3),
            MouseEventKind::Down(MouseButton::Left) if ed.view.back.is_some_and(|(y, from, to)| m.row == y && (from..to).contains(&m.column)) => self.travel(true),
            MouseEventKind::Down(MouseButton::Left) => {
                let (pos, checkbox) = ed.pos_at(m.column, m.row);
                if checkbox {
                    return ed.toggle_task(pos.row);
                }
                // Ctrl+click follows a link here; with Alt, beside this note.
                let aside = m.modifiers.contains(KeyModifiers::ALT);
                if aside || m.modifiers.contains(KeyModifiers::CONTROL) {
                    return self.follow(pos, aside);
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
            // A plain click (not a drag, not a double click) on an underlined word: its fixes.
            MouseEventKind::Up(MouseButton::Left) if ed.selection().is_none() && self.last_click.is_some_and(|(_, at)| at == ed.cursor) => self.open_fixer_here(),
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
                // A terminal asked to paste a picture has no text to send: look for ourselves.
                (None, None) if self.find.is_some() => {
                    if let Some(find) = &mut self.find {
                        find.query.extend(text.chars().take_while(|c| *c != '\n').filter(|c| !c.is_control()));
                        find.refresh(&mut self.ed);
                    }
                }
                (None, None) if text.is_empty() => self.paste_clipboard(),
                (None, None) => {
                    self.paste_text(&text);
                    if let Some(m) = &mut self.mention {
                        if !m.update(&self.ed) {
                            self.mention = None;
                        }
                    }
                }
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
    /// macOS runs /bin/sh as bash 3.2, which reads the bytes of a character
    /// such as an ellipsis as part of a variable's name: `"from $REPO…"` asked
    /// for a variable that does not exist and the install died under `set -u`.
    /// Nothing on Linux notices, so this does.
    #[test]
    fn the_install_script_is_plain_ascii() {
        let script = include_str!("../install.sh");
        let odd: Vec<&str> = script.lines().filter(|l| !l.is_ascii()).collect();
        assert!(odd.is_empty(), "not ASCII: {odd:?}");
    }
}

const HELP: &str = "\
omanote — a small markdown note editor

  omanote                     start a new note; Ctrl+S asks where to keep it
  omanote <name>              find a note by name in this folder and every vault:
                              one match opens, several are listed (Tab, Enter),
                              none starts a new note. Case and .md do not matter.
  omanote <path/to/file.md>   open (or start) that file
  omanote <file> --line 12    …with the cursor on that line (--match word: on that word)
  omanote <note> --agent      …with the AI agent menu open (--agent=codex: that agent)
  omanote --demo              a note that shows off what the editor renders
  Ctrl+N inside the editor    start a new note (asks to save an unnamed one first)
  Ctrl+P inside the editor    fuzzy-find a note in any vault; start with > to search inside
                              the notes instead (>kyoto rail), Enter opens it on that line
  F2 inside the editor        move, rename or copy the note (another vault, another folder)
  Ctrl+F inside the editor    find in the note: matches light up as you type, Enter or
                              the arrows move between them, Esc leaves you on the match.
                              F3 / Shift+F3 search again for the same thing
  F7 inside the editor        spelling and grammar: mistakes are underlined as you write;
                              F7 (or a click on the word) lists the fixes, Enter applies,
                              or adds the word to ~/.omanote/dictionary.txt. Checking is
                              off until Shift+F7 turns it on (the first time, it downloads
                              the 78 MB grammar model); Shift+F7 again turns it off
  Ctrl+R inside the editor    your own commands: command.<name> = \"<anything a shell runs>\"
                              in the settings. The selection (or the paragraph) goes in,
                              what comes out takes its place: grammar, translation, dates
  @ inside the editor         link a note: type @ and a few letters, pick from the list
                              (the last entry creates a note by that name), Enter
  Ctrl+K                      make the selected text a link: [text](), with the cursor
                              where the address goes (type it, paste it, or @ to pick a
                              note). An address already in the clipboard is filled in
  Pasting a web address       (Ctrl+V, Super+V, the terminal's paste) makes a link: around the selected text, or under a short
                              name (github.com/you/repo); Ctrl+Z gives the plain address
  Ctrl+O on a link            open it: a note opens here (Alt+Left goes back), a web
                              address or other file opens on the desktop. Ctrl+Enter
                              and Ctrl+click do the same
  Alt+O on a link             open it beside this note, on the right, and keep this one
                              where it is (also Ctrl+Shift+O, Alt+click). Links in the
                              right-hand note open there too. F6 or a click changes
                              sides; Ctrl+Q closes the side you are in
  Alt+Left / Alt+Right        back to the note you came from, and forward again; the
                              status line names it, and a click on it goes there too
  Ctrl+G inside the editor    an AI agent in a pane beside the note: pick from the agent
                              CLIs you have installed (Claude Code, Codex, Gemini, …) or
                              your own from the settings. Ctrl+G again moves between the
                              two; quit the agent to close the pane

Quick notes and reminders:
  omanote --capture <text>    add a line to inbox.md in the default vault, without the editor
  omanote --capture \"call the dentist !tomorrow 9:00\"
                              …and be reminded: end with ! and a time. !30m  !2h  !15:30
                              !fri 10:00  !2026-09-25 14:00, or what repeats:
                              !every mon 3pm, tue 1pm   !every weekday 9:30   !every day 8am
  omanote --reminders         what is coming up (on / off: start or stop the systemd user
                              timer that fires them; the first reminder turns it on)

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

  --keys                      show and log every key and mouse event (for diagnosing a terminal)
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
/// Where to put the cursor in the note that opens: a line (from 1), and a word to select on it.
type Land = Option<(usize, String)>;

fn cli(args: &[String]) -> Result<(Target, bool, bool, Option<String>, Land), String> {
    let home = vaults::home();
    let (mut words, mut keys, mut demo) = (Vec::new(), false, false);
    // `--agent`: open the assistant straight away ("" = ask which); `--agent=codex`: that one.
    let mut agent: Option<String> = None;
    // `--line 12 --match kyoto`: how the desktop popup opens a line it found.
    let (mut line, mut word) = (None, String::new());
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
                return Ok((Target::New(name.join(" ")), keys, demo, agent, None));
            }
            "--capture" => {
                // Everything after the flag is the note, quoted or not.
                let text: Vec<String> = it.by_ref().cloned().collect();
                let vault = vaults::all(&home).first().map(|v| v.path.clone()).unwrap_or_default();
                capture::capture(&vault, &text.join(" "), true)?
            }
            "--remind" => {
                // What the timer runs, every minute. Says nothing unless asked to by a failure.
                let vault = vaults::all(&home).first().map(|v| v.path.clone()).unwrap_or_default();
                remind::run(&home, &capture::inbox(&vault))?;
                std::process::exit(0);
            }
            "--reminders" => match it.next().map(String::as_str) {
                Some("on") => remind::turn_on()?,
                Some("off") => remind::turn_off()?,
                None => {
                    let vault = vaults::all(&home).first().map(|v| v.path.clone()).unwrap_or_default();
                    remind::list(&capture::inbox(&vault)).trim_end().to_string()
                }
                Some(other) => return Err(format!("--reminders takes on, off or nothing, not {other}")),
            },
            "--config" => {
                // Open the settings in the editor itself; saving applies them.
                let file = config::ensure(&home).map_err(|e| format!("cannot write the config: {e}"))?;
                return Ok((Target::File(file), keys, demo, agent, None));
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
            "--line" => {
                let given = value("a line number")?;
                line = Some(given.trim_start_matches('+').parse::<usize>().map_err(|_| format!("--line needs a number, not {given}"))?);
                continue;
            }
            "--match" => {
                word = value("the text to select")?;
                continue;
            }
            flag if flag == "--agent" || flag.starts_with("--agent=") => {
                agent = Some(flag.strip_prefix("--agent=").unwrap_or("").to_string());
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
    Ok((target(&words), keys, demo, agent, line.map(|l| (l, word))))
}

/// Present while checking is switched on with Shift+F7.
fn checking_on_file() -> PathBuf {
    vaults::home().join("checking-on")
}

/// A note made from an `@` mention: just its title, ready to be written in.
/// An existing file is left alone.
fn start_note(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let title = path.file_stem().unwrap_or_default().to_string_lossy();
    match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(format!("# {title}\n\n").as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// How many notes back Alt+← remembers.
const HISTORY: usize = 50;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let (target, keys, demo, start_agent, land) = cli(&args).unwrap_or_else(|msg| {
        eprintln!("omanote: {msg}");
        std::process::exit(2);
    });
    let key_log = match keys {
        true => {
            // Which terminal this is goes first: the log is only useful knowing that.
            let mut log = std::fs::File::create(std::env::temp_dir().join("omanote-keys.log"))?;
            let var = |name: &str| std::env::var(name).unwrap_or_default();
            let size = ratatui::crossterm::terminal::size().unwrap_or_default();
            writeln!(log, "omanote {} on {} · TERM={} TERM_PROGRAM={} {} · LC_TERMINAL={} · {}x{} cells", env!("CARGO_PKG_VERSION"), std::env::consts::OS, var("TERM"), var("TERM_PROGRAM"), var("TERM_PROGRAM_VERSION"), var("LC_TERMINAL"), size.0, size.1)?;
            Some(log)
        }
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

    // A closed window or a `kill` is a request to leave, not an execution:
    // the note gets saved and nothing is left lying around.
    stop_on_signals();
    now::sweep(&vaults::home());

    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    let mut app = App {
        ed: Editor::new("", None),
        other: None,
        on_right: false,
        other_area: None,
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
        mention: None,
        palette: None,
        running: None,
        spell: None,
        grammar: None,
        spelling_found: Vec::new(),
        grammar_found: Vec::new(),
        grammar_asked: u64::MAX,
        problems: Vec::new(),
        spell_note: 0,
        spell_asked: u64::MAX,
        spell_answered: u64::MAX,
        ignored: std::collections::HashSet::new(),
        fixer: None,
        checking: checking_on_file().exists(),
        downloading: None,
        spell_dialect: None,
        find: None,
        last_find: String::new(),
        now: None,
        pane_agent: String::new(),
        pane_dir: PathBuf::new(),
        pane_inbox: None,
        pane_vaults: Vec::new(),
        history: Vec::new(),
        forward: Vec::new(),
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
    if let Some((line, word)) = &land {
        app.land_at(*line, word);
    }
    if let Some(msg) = greeting {
        app.say(msg);
    }

    let all_vaults = vaults::all(&vaults::home());
    let (settings, problems) = config::load(&vaults::home());
    look::apply(&settings.look);
    app.settings = settings;
    app.spell_setup();
    if let Some(first) = problems.first() {
        app.say(format!("config.toml, {first}"));
    }
    match start_agent.as_deref() {
        Some("") => app.assistant(),
        Some(name) if app.ed.path.is_some() => {
            let found = agents::available(&app.settings.agents);
            app.open_assistant(agents::named(name, &found));
        }
        Some(_) => app.assistant(),
        None => {}
    }
    let result = (|| -> std::io::Result<()> {
        let mut redraw = true;
        let mut drawn = Instant::now();
        while !app.quit && !STOP.load(Ordering::Relaxed) {
            app.reap_pane();
            if app.poll_command() {
                redraw = true;
            }
            if app.spell_tick() || app.poll_download() {
                redraw = true;
            }
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
                app.tell_agent();
                let toast = match (&app.running, &app.downloading) {
                    (Some(running), _) => Some(format!("Running {}… {}s · Esc stops it", running.name, running.seconds())),
                    (None, Some(d)) => {
                        let mb = |a: &std::sync::atomic::AtomicU64| a.load(std::sync::atomic::Ordering::Relaxed) / 1_000_000;
                        Some(format!("Downloading the grammar model… {} of {} MB · Shift+F7 stops it", mb(&d.got), mb(&d.total)))
                    }
                    (None, None) => app.toast.as_ref().map(|(msg, _)| msg.clone()),
                };
                // On a misspelling, with nothing else to say: what is wrong with it.
                let toast = toast.or_else(|| app.fixer.is_none().then(|| app.problem_at_cursor().map(|p| format!("{} · F7", p.message))).flatten());
                let syncing = app.ed.path.as_ref().and_then(|p| app.sync.state(p, &all_vaults));
                let size = terminal.size()?;
                if std::mem::take(&mut app.repaint) {
                    terminal.clear()?;
                }
                let full = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                let panes = ui::arrange(full, app.other.is_some(), app.pane.is_some(), app.pane_focused);
                app.pane_x = panes.agent.map_or(u16::MAX, |r| r.x);
                if let (Some(pane), Some(rect)) = (&mut app.pane, panes.agent) {
                    let screen = ui::pane_screen(rect);
                    pane.resize(screen.height, screen.width);
                    app.pane_screen = screen;
                }
                let (mine, theirs) = match (panes.right, app.on_right) {
                    (Some(right), true) => (right, Some(panes.left)),
                    (Some(right), false) => (panes.left, Some(right)),
                    (None, _) => (panes.left, None),
                };
                app.other_area = theirs;
                // Images have to be in the terminal before the frame that refers to them.
                let mut pending = Vec::new();
                let shown = [(Some(&mut app.ed), Some(mine)), (app.other.as_mut().map(|o| &mut o.ed), theirs)];
                for (ed, area) in shown {
                    let (Some(ed), Some(area)) = (ed, area.filter(|a| a.width > 0)) else { continue };
                    if let Some((x, y, w, h, table_w)) = ui::text_area(area, &app.settings) {
                        ed.table_w = table_w;
                        ed.set_view(x, y, w, h);
                        pending.extend(ed.images.take_outbox());
                    }
                }
                if !pending.is_empty() {
                    let mut out = stdout();
                    out.write_all(&pending)?;
                    out.flush()?;
                }
                let back = app.history.iter().rev().find(|(p, _)| p.exists() && !app.is_other(p)).map(|(p, _)| p.file_name().unwrap_or_default().to_string_lossy().into_owned());
                let place = |ed: &Editor| ed.path.as_ref().map(|p| vaults::place(p, &all_vaults)).unwrap_or_default();
                let places = (place(&app.ed), app.other.as_ref().map(|o| place(&o.ed)).unwrap_or_default());
                let scene = ui::Scene {
                    panes: &panes,
                    other: app.other.as_mut().map(|o| &mut o.ed),
                    on_right: app.on_right,
                    picker: app.picker.as_ref(),
                    save_as: app.save_as.as_ref(),
                    toast: toast.as_deref(),
                    sync: syncing,
                    config: &app.settings,
                    assistant: app.pane.as_ref().map(|p| (p, app.pane_focused)),
                    chooser: app.chooser.as_ref(),
                    mention: app.mention.as_ref(),
                    find: app.find.as_ref(),
                    palette: app.palette.as_ref().map(|p| (p, app.settings.commands.as_slice())),
                    problems: &app.problems,
                    fixer: app.fixer.as_ref(),
                    back: back.as_deref(),
                    places: (&places.0, &places.1),
                };
                terminal.draw(|f| ui::draw(f, &mut app.ed, scene))?;
            }

            let wait = Duration::from_millis(if app.pane.is_some() { 25 } else if app.running.is_some() { 80 } else { 250 });
            let input = match watch_terminal(wait) {
                Tty::Gone => {
                    STOP.store(true, Ordering::Relaxed);
                    break;
                }
                Tty::Look => event::poll(Duration::ZERO)?,
                Tty::NotStdin => event::poll(wait)?,
            };
            if input {
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

    // Told to stop (the terminal may already be gone, which is why the loop
    // can also end in an error): nobody can be asked anything, so save what
    // has a file and go.
    if STOP.load(Ordering::Relaxed) {
        app.leave();
    }
    restore_terminal(enhanced_keys);
    if keys {
        println!("What the terminal sent is in {}", std::env::temp_dir().join("omanote-keys.log").display());
    }
    result
}

enum Tty {
    /// The window was closed under us.
    Gone,
    /// Input, a signal or just time passing: see what there is.
    Look,
    NotStdin,
}

/// Wait for input, but look at the terminal ourselves first. crossterm never
/// returns from `poll` once the terminal has hung up (it reads "nothing" from
/// it in a loop, forever), which would turn a closed window into a process
/// spinning in the background with the note unsaved.
fn watch_terminal(wait: Duration) -> Tty {
    if unsafe { libc::isatty(0) } != 1 {
        return Tty::NotStdin;
    }
    let mut stdin = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
    let ready = unsafe { libc::poll(&mut stdin, 1, wait.as_millis() as libc::c_int) };
    let gone = ready > 0 && stdin.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0;
    if gone { Tty::Gone } else { Tty::Look }
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn stop_on_signals() {
    for signal in [libc::SIGHUP, libc::SIGTERM] {
        unsafe { libc::signal(signal, stop as extern "C" fn(libc::c_int) as libc::sighandler_t) };
    }
    // The way out above depends on the main loop coming round again. Should it
    // ever be stuck when told to stop, do not linger: tidy up and go.
    std::thread::spawn(|| {
        while !STOP.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
        std::thread::sleep(Duration::from_secs(3));
        now::remove_own(&vaults::home());
        std::process::exit(1);
    });
}
