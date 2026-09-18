mod clipboard;
mod diacritics;
mod editor;
mod images;
mod layout;
mod markdown;
mod picker;
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
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode,
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
use picker::Picker;

const DEMO: &str = include_str!("../demo.md");
const AUTOSAVE_IDLE: Duration = Duration::from_millis(1500);
const TOAST: Duration = Duration::from_secs(2);
const DOUBLE_CLICK: Duration = Duration::from_millis(350);
/// Pasted images wider than this are scaled down before they are embedded.
const PASTE_MAX_WIDTH: u32 = 2000;

struct App {
    ed: Editor,
    picker: Option<Picker>,
    toast: Option<(String, Instant)>,
    last_click: Option<(Instant, Pos)>,
    enhanced_keys: bool,
    image_mode: images::Mode,
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
            Ok(false) => self.say("Demo buffer — run `omanote notes.md` to edit a real file"),
            Err(e) => self.say(format!("Could not save: {e}")),
        }
    }

    /// Switch to another note, saving the current one first.
    fn open(&mut self, path: PathBuf) {
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
        self.ed = self.editor(&text, Some(path));
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
        if self.ed.dirty && self.ed.path.is_some() {
            self.save();
        }
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
                'p' => self.picker = Some(Picker::open(&vaults::all(&vaults::home()))),
                's' => self.save(),
                'z' if shift => self.redo(),
                'z' => {
                    if !ed.undo() {
                        self.say("Nothing to undo");
                    }
                }
                'y' => self.redo(),
                'a' => ed.select_all(),
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
            Event::Resize(..) => self.ed.images.set_cell(cell_pixels()),
            Event::Paste(text) => match &mut self.picker {
                Some(p) => p.push(&text),
                None => self.ed.insert_str(&text),
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
    let _ = execute!(out, DisableBracketedPaste, DisableMouseCapture, SetCursorStyle::DefaultUserShape, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

const HELP: &str = "\
omanote — a small markdown note editor

  omanote                     open the demo note
  omanote <file.md>           open (or start) a note
  Ctrl+P inside the editor    fuzzy-find a note in any vault

Vaults (where Ctrl+P looks; new notes go in ~/.omanote/docs):
  omanote --vl <folder>       add a folder of notes
  omanote --vlgh <owner/repo> clone a GitHub repo into ~/.omanote/vaults and add it
  omanote --vlrm <name>       forget a vault (folder, owner/repo or name); files are kept
  omanote --vls               list vaults

  --keys                      show every key event (for diagnosing a terminal)
  -h, --help                  this text
";

/// Handle the flags that do their job and exit. Returns what is left: the note to open.
fn cli(args: &[String]) -> Result<(Option<PathBuf>, bool), String> {
    let home = vaults::home();
    let (mut path, mut keys) = (None, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |what: &str| it.next().cloned().ok_or(format!("{arg} needs {what}"));
        let done = match arg.as_str() {
            "--vl" => vaults::add_local(&home, &value("a folder")?)?,
            "--vlgh" => vaults::add_github(&home, &value("a GitHub repo, like owner/repo")?)?,
            "--vlrm" => vaults::remove(&home, &value("the vault to remove")?)?,
            "--vls" => vaults::list(&home),
            "-h" | "--help" => HELP.to_string(),
            "--keys" => {
                keys = true;
                continue;
            }
            flag if flag.starts_with('-') && flag.len() > 1 => return Err(format!("unknown option {flag} — see omanote --help")),
            file => {
                path = Some(PathBuf::from(file));
                continue;
            }
        };
        println!("{}", done.trim_end());
        std::process::exit(0);
    }
    Ok((path, keys))
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let (path, keys) = cli(&args).unwrap_or_else(|msg| {
        eprintln!("omanote: {msg}");
        std::process::exit(2);
    });
    let key_log = match keys {
        true => Some(std::fs::File::create(std::env::temp_dir().join("omanote-keys.log"))?),
        false => None,
    };
    let text = match &path {
        Some(p) if p.exists() => std::fs::read_to_string(p)?,
        Some(_) => String::new(),
        None => DEMO.to_string(),
    };

    enable_raw_mode()?;
    let enhanced_keys = supports_keyboard_enhancement().unwrap_or(false);
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, SetCursorStyle::SteadyBar)?;
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
        picker: None,
        toast: None,
        last_click: None,
        enhanced_keys,
        image_mode: images::detect(),
        key_log,
        quit: false,
    };
    app.ed = app.editor(&text, path);

    let result = (|| -> std::io::Result<()> {
        while !app.quit {
            if app.toast.as_ref().is_some_and(|(_, at)| at.elapsed() > TOAST) {
                app.toast = None;
            }
            let toast = app.toast.as_ref().map(|(msg, _)| msg.clone());
            // Images have to be in the terminal before the frame that refers to them.
            let size = terminal.size()?;
            if let Some((x, y, w, h)) = ui::text_area(ratatui::layout::Rect::new(0, 0, size.width, size.height)) {
                app.ed.set_view(x, y, w, h);
                let pending = app.ed.images.take_outbox();
                if !pending.is_empty() {
                    let mut out = stdout();
                    out.write_all(&pending)?;
                    out.flush()?;
                }
            }
            terminal.draw(|f| ui::draw(f, &mut app.ed, app.picker.as_ref(), toast.as_deref(), app.enhanced_keys))?;

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
