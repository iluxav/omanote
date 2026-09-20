//! A terminal inside the editor: the assistant pane (Ctrl+G).
//!
//! A command runs on a pseudo-terminal. A reader thread feeds what it prints to
//! a VT parser, which keeps the screen the command believes it is drawing on;
//! we paint that screen into a rectangle beside the note and turn key presses
//! back into the bytes a terminal would send. It is a small emulator, not a
//! full one: colours, cursor movement, the alternate screen and bracketed paste
//! work; mouse reporting and terminal-specific protocols do not. The mouse is
//! ours instead: drag selects text (copied on release), the wheel scrolls back.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

pub struct Pane {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// Set by the reader thread when there is something new to draw.
    dirty: Arc<AtomicBool>,
    size: (u16, u16),
    /// Text being selected with the mouse: where the drag began and where it is,
    /// as (row, column) on the pane's screen.
    selection: Option<((u16, u16), (u16, u16))>,
    /// What is running, for the pane's title.
    pub label: String,
}

impl Pane {
    /// Run `command` through the user's login shell (so their PATH applies) in
    /// `cwd`, with `env` added, on a terminal of `rows` x `cols`.
    pub fn spawn(command: &str, cwd: &Path, env: &[(&str, String)], rows: u16, cols: u16) -> Result<Self, String> {
        let (rows, cols) = (rows.max(2), cols.max(10));
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| format!("could not open a terminal: {e}"))?;

        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
        let mut cmd = CommandBuilder::new(shell);
        cmd.args(["-lc", command]);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (key, value) in env {
            cmd.env(key, value);
        }
        let child = pair.slave.spawn_command(cmd).map_err(|e| format!("could not start `{command}`: {e}"))?;
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 2000)));
        let dirty = Arc::new(AtomicBool::new(true));
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        {
            let (parser, dirty) = (parser.clone(), dirty.clone());
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                // Ends when the command exits and the terminal closes.
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut p) = parser.lock() {
                        p.process(&buf[..n]);
                    }
                    dirty.store(true, Ordering::Relaxed);
                }
                dirty.store(true, Ordering::Relaxed);
            });
        }
        let label = command.split_whitespace().next().unwrap_or("assistant").rsplit('/').next().unwrap_or("assistant").to_string();
        Ok(Pane { parser, writer, master: pair.master, child, dirty, size: (rows, cols), selection: None, label })
    }

    /// True once since the last call if the screen changed.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }

    pub fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        if (rows, cols) == self.size {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Scroll back through what has gone off the top (positive = further back).
    pub fn scroll(&mut self, rows: isize) {
        if let Ok(mut p) = self.parser.lock() {
            let now = p.screen().scrollback();
            p.set_scrollback(now.saturating_add_signed(rows));
        }
        self.selection = None;
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// How far back the view is scrolled; 0 is the live screen.
    pub fn scrolled(&self) -> usize {
        self.parser.lock().map(|p| p.screen().scrollback()).unwrap_or(0)
    }

    pub fn select_from(&mut self, row: u16, col: u16) {
        self.selection = Some(((row, col), (row, col)));
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn select_to(&mut self, row: u16, col: u16) {
        if let Some((_, head)) = &mut self.selection {
            *head = (row.min(self.size.0.saturating_sub(1)), col.min(self.size.1.saturating_sub(1)));
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// The selection in reading order, if it covers anything.
    fn selected_range(&self) -> Option<((u16, u16), (u16, u16))> {
        let (a, b) = self.selection?;
        (a != b).then(|| (a.min(b), a.max(b)))
    }

    /// The selected text, as it would be copied. A plain click selects nothing
    /// and clears what was selected.
    pub fn selected_text(&mut self) -> Option<String> {
        let Some((start, end)) = self.selected_range() else {
            self.selection = None;
            self.dirty.store(true, Ordering::Relaxed);
            return None;
        };
        let parser = self.parser.lock().ok()?;
        let text = parser.screen().contents_between(start.0, start.1, end.0, end.1 + 1);
        // Rows are padded with nothing but may end in spaces the program drew.
        let text: Vec<&str> = text.lines().map(str::trim_end).collect();
        Some(text.join("\n")).filter(|t| !t.trim().is_empty())
    }

    pub fn send_key(&mut self, key: KeyEvent) {
        // Typing means "back to now": the live screen, nothing selected.
        self.selection = None;
        if let Ok(mut p) = self.parser.lock() {
            p.set_scrollback(0);
        }
        let app_cursor = self.parser.lock().map(|p| p.screen().application_cursor()).unwrap_or(false);
        let bytes = key_bytes(key, app_cursor);
        if !bytes.is_empty() {
            let _ = self.writer.write_all(&bytes).and_then(|_| self.writer.flush());
        }
    }

    pub fn paste(&mut self, text: &str) {
        let bracketed = self.parser.lock().map(|p| p.screen().bracketed_paste()).unwrap_or(false);
        let data = if bracketed { format!("\x1b[200~{text}\x1b[201~") } else { text.to_string() };
        let _ = self.writer.write_all(data.as_bytes()).and_then(|_| self.writer.flush());
    }

    /// Paint the screen into `area`. Returns where the cursor is, if it is shown.
    pub fn draw(&self, buf: &mut Buffer, area: Rect) -> Option<(u16, u16)> {
        let Ok(parser) = self.parser.lock() else { return None };
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let selected = self.selected_range();
        for row in 0..rows.min(area.height) {
            for col in 0..cols.min(area.width) {
                let Some(cell) = screen.cell(row, col) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut style = Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
                for (on, modifier) in [
                    (cell.bold(), Modifier::BOLD),
                    (cell.italic(), Modifier::ITALIC),
                    (cell.underline(), Modifier::UNDERLINED),
                    (cell.inverse(), Modifier::REVERSED),
                ] {
                    if on {
                        style = style.add_modifier(modifier);
                    }
                }
                if selected.is_some_and(|(start, end)| (row, col) >= start && (row, col) <= end) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                let text = cell.contents();
                if let Some(target) = buf.cell_mut((area.x + col, area.y + row)) {
                    target.set_symbol(if text.is_empty() { " " } else { &text }).set_style(style);
                }
            }
        }
        let (row, col) = screen.cursor_position();
        (!screen.hide_cursor() && row < area.height && col < area.width).then_some((area.x + col, area.y + row))
    }

    /// The text on screen, for tests.
    #[cfg(test)]
    fn contents(&self) -> String {
        self.parser.lock().map(|p| p.screen().contents()).unwrap_or_default()
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// The bytes a terminal sends for a key press (xterm conventions).
pub fn key_bytes(key: KeyEvent, application_cursor: bool) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let arrow = |letter: char| if application_cursor { format!("\x1bO{letter}") } else { format!("\x1b[{letter}") };
    let mut out: Vec<u8> = match key.code {
        KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
            c @ 'a'..='z' => vec![c as u8 - b'a' + 1],
            ' ' | '@' => vec![0],
            '[' => vec![0x1b],
            '\\' => vec![0x1c],
            ']' => vec![0x1d],
            '_' | '/' => vec![0x1f],
            _ => Vec::new(),
        },
        KeyCode::Char(c) => c.to_string().into_bytes(),
        // Alt+Enter is the usual "new line without sending" in chat-style CLIs,
        // and what Shift+Enter means in a terminal that cannot tell them apart.
        KeyCode::Enter if shift || alt => return b"\x1b\r".to_vec(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![if ctrl { 0x17 } else { 0x7f }],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => arrow('A').into_bytes(),
        KeyCode::Down => arrow('B').into_bytes(),
        KeyCode::Right => arrow('C').into_bytes(),
        KeyCode::Left => arrow('D').into_bytes(),
        KeyCode::Home => arrow('H').into_bytes(),
        KeyCode::End => arrow('F').into_bytes(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n @ 1..=4) => format!("\x1bO{}", (b'P' + n - 1) as char).into_bytes(),
        KeyCode::F(n @ 5..=12) => format!("\x1b[{}~", [15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5]).into_bytes(),
        _ => Vec::new(),
    };
    if alt && !out.is_empty() && !matches!(key.code, KeyCode::Enter) {
        out.insert(0, 0x1b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn keys_become_what_a_terminal_would_send() {
        let none = KeyModifiers::NONE;
        assert_eq!(key_bytes(key(KeyCode::Char('a'), none), false), b"a");
        assert_eq!(key_bytes(key(KeyCode::Char('é'), none), false), "é".as_bytes());
        assert_eq!(key_bytes(key(KeyCode::Char('c'), KeyModifiers::CONTROL), false), [3]);
        assert_eq!(key_bytes(key(KeyCode::Char('C'), KeyModifiers::CONTROL | KeyModifiers::SHIFT), false), [3]);
        assert_eq!(key_bytes(key(KeyCode::Char('b'), KeyModifiers::ALT), false), b"\x1bb");
        assert_eq!(key_bytes(key(KeyCode::Enter, none), false), b"\r");
        assert_eq!(key_bytes(key(KeyCode::Enter, KeyModifiers::SHIFT), false), b"\x1b\r");
        assert_eq!(key_bytes(key(KeyCode::Backspace, none), false), [0x7f]);
        assert_eq!(key_bytes(key(KeyCode::Up, none), false), b"\x1b[A");
        assert_eq!(key_bytes(key(KeyCode::Up, none), true), b"\x1bOA", "application cursor mode");
        assert_eq!(key_bytes(key(KeyCode::BackTab, KeyModifiers::SHIFT), false), b"\x1b[Z");
        assert_eq!(key_bytes(key(KeyCode::F(5), none), false), b"\x1b[15~");
        assert!(key_bytes(key(KeyCode::CapsLock, none), false).is_empty());
    }

    fn wait_for(pane: &Pane, what: &str) -> String {
        let start = Instant::now();
        loop {
            let text = pane.contents();
            if text.contains(what) {
                return text;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "never saw {what:?}; screen: {text:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn runs_a_program_talks_to_it_and_notices_it_leaving() {
        let script = r#"printf 'dir:%s ctx:%s\n' "$(basename "$PWD")" "$OMANOTE_CONTEXT"; read line; printf 'got:%s\n' "$line""#;
        let cwd = std::env::temp_dir();
        let mut pane = Pane::spawn(script, &cwd, &[("OMANOTE_CONTEXT", "hello there".into())], 10, 60).unwrap();
        let screen = wait_for(&pane, "ctx:hello there");
        assert!(screen.contains(&format!("dir:{}", cwd.file_name().unwrap().to_string_lossy())), "runs in the folder it was given: {screen}");
        assert!(!pane.exited());

        for c in "hi".chars() {
            pane.send_key(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        pane.send_key(key(KeyCode::Enter, KeyModifiers::NONE));
        wait_for(&pane, "got:hi");
        let start = Instant::now();
        while !pane.exited() {
            assert!(start.elapsed() < Duration::from_secs(10), "never exited");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn draws_colours_and_reports_the_cursor() {
        let pane = Pane::spawn(r"printf '\033[31mred\033[0m plain'; sleep 5", &std::env::temp_dir(), &[], 5, 30).unwrap();
        wait_for(&pane, "plain");
        let area = Rect::new(4, 2, 30, 5);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        let cursor = pane.draw(&mut buf, area);
        assert_eq!(buf[(4, 2)].symbol(), "r");
        assert_eq!(buf[(4, 2)].fg, Color::Indexed(1));
        assert_eq!(buf[(8, 2)].symbol(), "p");
        assert_eq!(buf[(8, 2)].fg, Color::Reset);
        assert_eq!(cursor, Some((4 + 9, 2)), "after the nine characters printed");
        assert_eq!(buf[(0, 0)].symbol(), " ", "nothing outside the pane is touched");
    }

    #[test]
    fn dragging_selects_text_and_the_wheel_scrolls_back() {
        let script = r"i=1; while [ $i -le 30 ]; do echo line-$i; i=$((i+1)); done; printf 'alpha beta gamma'; sleep 5";
        let mut pane = Pane::spawn(script, &std::env::temp_dir(), &[], 6, 40).unwrap();
        wait_for(&pane, "gamma");

        // The last row holds "alpha beta gamma": drag across "beta".
        pane.select_from(5, 6);
        pane.select_to(5, 9);
        assert_eq!(pane.selected_text().as_deref(), Some("beta"));
        // Backwards, and across rows.
        pane.select_from(5, 4);
        pane.select_to(4, 0);
        assert_eq!(pane.selected_text().as_deref(), Some("line-30\nalpha"));
        // A click with no drag selects nothing and clears the highlight.
        pane.select_from(2, 2);
        assert_eq!(pane.selected_text(), None);
        assert!(pane.selection.is_none());

        // Lines that scrolled off the top are still there.
        assert!(!pane.contents().contains("line-5\n"));
        pane.scroll(22);
        assert_eq!(pane.scrolled(), 22);
        assert!(pane.contents().contains("line-5"), "{}", pane.contents());
        pane.scroll(-1000);
        assert_eq!(pane.scrolled(), 0);
        // Typing returns to the live screen.
        pane.scroll(10);
        pane.send_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(pane.scrolled(), 0);
    }

    #[test]
    fn resizing_tells_the_program() {
        let mut pane = Pane::spawn("sleep 0.3; stty size", &std::env::temp_dir(), &[], 10, 40).unwrap();
        pane.resize(12, 50);
        wait_for(&pane, "12 50");
    }
}
