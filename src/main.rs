mod clipboard;
mod config;
mod diacritics;
mod editor;
mod images;
mod layout;
mod markdown;
mod picker;
mod saveas;
mod sync;
mod table;
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
    sync: sync::Sync,
    /// What was asked for on the command line when nothing matched: a name for the new note.
    wanted: Option<String>,
    /// `ed.save_count` as of the last time the sync was told about saves.
    seen_saves: u64,
    toast: Option<(String, Instant)>,
    last_click: Option<(Instant, Pos)>,
    enhanced_keys: bool,
    image_mode: images::Mode,
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

    fn save(&mut self) {
        match self.ed.save() {
            Ok(true) => self.say("Saved"),
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
        let here = std::env::current_dir().ok();
        self.save_as = Some(SaveAs::new(&self.ed.lines, vaults::all(&vaults::home()), here, self.wanted.as_deref(), after));
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
                'l' => self.repaint = true,
                'c' => {
                    clipboard::copy(&ed.copy());
                    self.say("Copied");
                }
                'x' => clipboard::copy(&ed.cut()),
                'v' => match clipboard::image() {
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
  Ctrl+P inside the editor    fuzzy-find a note in any vault
  F2 inside the editor        move, rename or copy the note (another vault, another folder)

Vaults (where Ctrl+P looks; new notes go in ~/.omanote/docs):
  omanote --vl <folder>       add a folder of notes
  omanote --vlgh <owner/repo> clone a GitHub repo into ~/.omanote/vaults and add it
  omanote --vlrm <name>       forget a vault (folder, owner/repo or name); files are kept
  omanote --vls               list vaults
  omanote --sync              sync every GitHub vault now: pull, then push

  omanote --config            where the settings file is (creates it), and what it says:
                              text width, left/center/right, margins
  Ctrl+L inside the editor    repaint the screen

  --keys                      show every key event (for diagnosing a terminal)
  -h, --help                  this text
";

/// What the non-flag words on the command line ask for.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    Untitled,
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
            "--config" => {
                let file = config::ensure(&home).map_err(|e| format!("cannot write the config: {e}"))?;
                let (now, problems) = config::load(&home);
                let align = format!("{:?}", now.align).to_lowercase();
                let mut out = format!("{}\n\n  width = {}\n  align = \"{align}\"\n  margin = {}\n", vaults::tilde(&file), now.width, now.margin);
                for problem in problems {
                    out.push_str(&format!("\n  ! {problem}"));
                }
                out
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
        sync: early_sync,
        seen_saves: 0,
        toast: None,
        last_click: None,
        enhanced_keys,
        image_mode: images::detect(),
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
    if let Some(first) = problems.first() {
        app.say(format!("config.toml, {first}"));
    }
    let result = (|| -> std::io::Result<()> {
        while !app.quit {
            if app.toast.as_ref().is_some_and(|(_, at)| at.elapsed() > TOAST) {
                app.toast = None;
            }
            app.tick();
            let toast = app.toast.as_ref().map(|(msg, _)| msg.clone());
            let syncing = app.ed.path.as_ref().and_then(|p| app.sync.state(p, &all_vaults));
            // Images have to be in the terminal before the frame that refers to them.
            let size = terminal.size()?;
            if std::mem::take(&mut app.repaint) {
                terminal.clear()?;
            }
            if let Some((x, y, w, h, table_w)) = ui::text_area(ratatui::layout::Rect::new(0, 0, size.width, size.height), &settings) {
                app.ed.table_w = table_w;
                app.ed.set_view(x, y, w, h);
                let pending = app.ed.images.take_outbox();
                if !pending.is_empty() {
                    let mut out = stdout();
                    out.write_all(&pending)?;
                    out.flush()?;
                }
            }
            terminal.draw(|f| ui::draw(f, &mut app.ed, app.picker.as_ref(), app.save_as.as_ref(), toast.as_deref(), syncing, &settings, app.enhanced_keys))?;

            if event::poll(Duration::from_millis(250))? {
                // Drain everything pending so a burst of input costs one redraw.
                loop {
                    app.handle(event::read()?);
                    if app.quit || !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            } else if app.ed.dirty && app.ed.path.is_some() && app.ed.last_change.elapsed() > AUTOSAVE_IDLE {
                if let Err(e) = app.ed.save() {
                    app.say(format!("Could not save: {e}"));
                }
            }
        }
        Ok(())
    })();

    restore_terminal(enhanced_keys);
    result
}
