//! Your own commands: anything a shell can run, working on the note.
//!
//!     command.Rewrite = "llm 'Rewrite this more clearly. Reply with the text only.'"
//!     command.Rewrite.key = "F5"
//!     command.Insert date = "date +%F"
//!     command.Insert date.output = "insert"
//!     command.Publish = "pandoc {file} -o ~/site/{name}.html"
//!     command.Publish.output = "message"
//!
//! This is how omanote is extended. There is no plugin language to learn and
//! nothing inside omanote for a plugin to break: a command gets text on its
//! standard input, says something on its standard output, and is written in
//! whatever you like. Ctrl+R lists them.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command as Process, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::editor::{Editor, Pos};

/// A command that says nothing for this long is stopped.
const TIMEOUT: Duration = Duration::from_secs(180);
/// More than any text worth putting in a note; a runaway command is cut off here.
const MAX_OUTPUT: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// The selected text goes in, and what comes out takes its place. With
    /// nothing selected it is the paragraph the cursor is in.
    Replace,
    /// What comes out is written at the cursor. (The selection still goes in.)
    Insert,
    /// Nothing in the note changes: the first line that comes out is shown.
    Message,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub run: String,
    pub output: Output,
    pub key: Option<(KeyCode, KeyModifiers)>,
}

/// `F5`, `alt+g`, `ctrl+alt+p`. Plain letters and Ctrl+letter are the
/// editor's own, so a command's key is a function key or has Alt in it.
pub fn parse_key(text: &str) -> Result<(KeyCode, KeyModifiers), String> {
    let lower = text.trim().to_lowercase();
    let mut parts: Vec<&str> = lower.split(['+', '-']).map(str::trim).filter(|p| !p.is_empty()).collect();
    let last = parts.pop().ok_or("which key?")?;
    let mut mods = KeyModifiers::NONE;
    for part in parts {
        mods |= match part {
            "alt" | "option" | "meta" => KeyModifiers::ALT,
            "ctrl" | "control" => KeyModifiers::CONTROL,
            "shift" => KeyModifiers::SHIFT,
            other => return Err(format!("`{other}` is not a modifier: use alt, ctrl or shift")),
        };
    }
    let code = match last.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        Some(n @ 1..=12) => KeyCode::F(n),
        Some(_) => return Err("function keys go from F1 to F12".into()),
        None if last.chars().count() == 1 => KeyCode::Char(last.chars().next().unwrap_or(' ')),
        None => return Err(format!("`{last}` is not a key: use F1 to F12, or a letter with alt (alt+g)")),
    };
    let taken = |what: &str| Err(format!("{text} is already {what}"));
    match (code, mods) {
        (KeyCode::F(2), m) if m.is_empty() => taken("move / rename"),
        (KeyCode::F(3), m) if m.is_empty() || m == KeyModifiers::SHIFT => taken("find next"),
        (KeyCode::F(6), m) if m.is_empty() => taken("the other note"),
        (KeyCode::F(7), m) if m.is_empty() || m == KeyModifiers::SHIFT => taken("spelling"),
        (KeyCode::Char('o'), KeyModifiers::ALT) => taken("open the link beside this note"),
        (KeyCode::Char(_), m) if !m.contains(KeyModifiers::ALT) => Err(format!("{text} is the editor's own, or just a letter: a command's key is F1 to F12, or has alt in it (alt+g)")),
        found => Ok(found),
    }
}

/// The command this key press is for.
pub fn bound(commands: &[Command], key: &KeyEvent) -> Option<usize> {
    let mods = key.modifiers & (KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    let code = match key.code {
        KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
        code => code,
    };
    commands.iter().position(|c| c.key == Some((code, mods)))
}

/// How a key is written in the list.
pub fn key_name(key: (KeyCode, KeyModifiers)) -> String {
    let mut out = String::new();
    for (m, name) in [(KeyModifiers::CONTROL, "Ctrl+"), (KeyModifiers::ALT, "Alt+"), (KeyModifiers::SHIFT, "Shift+")] {
        if key.1.contains(m) {
            out.push_str(name);
        }
    }
    match key.0 {
        KeyCode::F(n) => out.push_str(&format!("F{n}")),
        KeyCode::Char(c) => out.extend(c.to_uppercase()),
        _ => {}
    }
    out
}

/// One line of the settings: `key` is what follows `command.`.
pub fn set(commands: &mut Vec<Command>, key: &str, value: &str) -> Result<(), String> {
    let (name, property) = match key.rsplit_once('.') {
        Some((name, p)) if ["output", "key"].contains(&p.trim().to_lowercase().as_str()) => (name.trim(), p.trim().to_lowercase()),
        _ => (key.trim(), String::new()),
    };
    if name.is_empty() {
        return Err("the command needs a name: command.<name> = \"<what to run>\"".into());
    }
    let at = commands.iter().position(|c| c.name.eq_ignore_ascii_case(name));
    match (property.as_str(), at) {
        ("", _) if value.is_empty() => Err(format!("{name} needs something to run")),
        ("", Some(at)) => {
            commands[at].run = value.to_string();
            Ok(())
        }
        ("", None) => {
            commands.push(Command { name: name.to_string(), run: value.to_string(), output: Output::Replace, key: None });
            Ok(())
        }
        (_, None) => Err(format!("say what {name} runs first: command.{name} = \"...\"")),
        ("output", Some(at)) => {
            commands[at].output = match value.to_lowercase().as_str() {
                "replace" => Output::Replace,
                "insert" => Output::Insert,
                "message" | "show" | "none" => Output::Message,
                other => return Err(format!("output is \"replace\", \"insert\" or \"message\", not `{other}`")),
            };
            Ok(())
        }
        (_, Some(at)) => {
            let key = parse_key(value)?;
            if let Some(other) = commands.iter().find(|c| c.key == Some(key) && !c.name.eq_ignore_ascii_case(name)) {
                return Err(format!("{value} already runs {}", other.name));
            }
            commands[at].key = Some(key);
            Ok(())
        }
    }
}

/// Ctrl+R: which command?
pub struct Palette {
    pub selected: usize,
}

/// The paragraph around `row`: the lines up and down to the nearest blank ones.
pub fn paragraph(lines: &[Vec<char>], row: usize) -> Option<(Pos, Pos)> {
    let blank = |r: usize| lines[r].iter().all(|c| c.is_whitespace());
    if blank(row) {
        return None;
    }
    let first = (0..row).rev().find(|&r| blank(r)).map_or(0, |r| r + 1);
    let last = (row..lines.len()).find(|&r| blank(r)).map_or(lines.len(), |r| r) - 1;
    Some((Pos { row: first, col: 0 }, Pos { row: last, col: lines[last].len() }))
}

fn text_of(lines: &[Vec<char>], from: Pos, to: Pos) -> String {
    let mut out = String::new();
    for row in from.row..=to.row.min(lines.len() - 1) {
        let line = &lines[row];
        let a = if row == from.row { from.col.min(line.len()) } else { 0 };
        let b = if row == to.row { to.col.min(line.len()) } else { line.len() };
        out.extend(&line[a.min(b)..b]);
        if row != to.row {
            out.push('\n');
        }
    }
    out
}

pub struct Finished {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// A command on its way: what it was given, where that came from, and the
/// thread that will say how it went.
pub struct Running {
    pub name: String,
    pub output: Output,
    /// What went in, and the range it came from: `None` for `insert` with nothing selected.
    range: Option<(Pos, Pos)>,
    given: String,
    pid: u32,
    started: Instant,
    stopped: std::cell::Cell<bool>,
    done: Receiver<Finished>,
}

/// What to tell the user when a command is over.
pub enum Outcome {
    /// Still going.
    Waiting,
    Said(String),
}

fn shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())
}

/// Start `command` on the note in `ed`. The text it works on is taken now;
/// what it says is applied when it is done, by `Running::finish`.
pub fn start(command: &Command, ed: &Editor, dir: &Path) -> Result<Running, String> {
    let selection = ed.selection();
    let range = match command.output {
        Output::Replace => Some(selection.or_else(|| paragraph(&ed.lines, ed.cursor.row)).ok_or("Select some text, or put the cursor in a paragraph")?),
        _ => selection,
    };
    let given = range.map(|(from, to)| text_of(&ed.lines, from, to)).unwrap_or_default();

    let file = ed.path.as_ref().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()));
    let text = |p: Option<&Path>| p.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let name = file.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // Placeholders become environment variables, quoted: a file name with a
    // space or a quote in it cannot break the command, let alone run one.
    let run = command
        .run
        .replace("{file}", "\"$OMANOTE_FILE\"")
        .replace("{dir}", "\"$OMANOTE_DIR\"")
        .replace("{name}", "\"$OMANOTE_NAME\"")
        .replace("{line}", "\"$OMANOTE_LINE\"")
        .replace("{selection}", "\"$OMANOTE_SELECTION\"");
    let mut child = Process::new(shell())
        .args(["-lc", &run])
        .current_dir(dir)
        .env("OMANOTE_FILE", text(file.as_deref()))
        .env("OMANOTE_DIR", text(Some(dir)))
        .env("OMANOTE_NAME", name)
        .env("OMANOTE_LINE", (ed.cursor.row + 1).to_string())
        .env("OMANOTE_SELECTION", &given)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own group, so Esc can stop the shell and whatever the shell started.
        .process_group(0)
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", command.name))?;

    let pid = child.id();
    let (tx, done) = channel();
    let (mut stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let input = given.clone();
    // Fed from a thread of its own: a command that talks before it has
    // finished listening would otherwise leave both sides waiting.
    std::thread::spawn(move || {
        if let Some(stdin) = stdin.as_mut() {
            let _ = stdin.write_all(input.as_bytes());
        }
        drop(stdin);
    });
    std::thread::spawn(move || {
        let read = |pipe: Option<&mut dyn Read>| {
            let mut bytes = Vec::new();
            if let Some(pipe) = pipe {
                let _ = pipe.take(MAX_OUTPUT).read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let mut stderr = stderr;
        let errors = std::thread::spawn(move || read(stderr.as_mut().map(|p| p as &mut dyn Read)));
        let mut stdout = stdout;
        let said = read(stdout.as_mut().map(|p| p as &mut dyn Read));
        let ok = child.wait().is_ok_and(|s| s.success());
        let _ = tx.send(Finished { ok, stdout: said, stderr: errors.join().unwrap_or_default() });
    });
    Ok(Running { name: command.name.clone(), output: command.output, range, given, pid, started: Instant::now(), stopped: std::cell::Cell::new(false), done })
}

impl Running {
    /// Esc: stop it, and everything it started.
    pub fn cancel(&self) {
        self.stopped.set(true);
        unsafe { libc::kill(-(self.pid as i32), libc::SIGTERM) };
    }

    pub fn seconds(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Look whether it is done, and if so put what it said into the note.
    /// The note may have been edited meanwhile. If the text the command was
    /// given is no longer where it was, nothing is overwritten: the result
    /// goes to `spare` (the clipboard) instead.
    pub fn finish(&self, ed: &mut Editor, spare: &mut Option<String>) -> Outcome {
        let done = match self.done.try_recv() {
            Ok(done) => done,
            Err(std::sync::mpsc::TryRecvError::Empty) if self.started.elapsed() > TIMEOUT => {
                self.cancel();
                return Outcome::Said(format!("{} took more than {} minutes and was stopped", self.name, TIMEOUT.as_secs() / 60));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => return Outcome::Waiting,
            Err(_) => return Outcome::Said(format!("{} was stopped", self.name)),
        };
        let first = |text: &str| text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
        if self.stopped.get() {
            return Outcome::Said(format!("{}: stopped, nothing changed", self.name));
        }
        if !done.ok {
            let why = first(&done.stderr);
            let why = if why.is_empty() { first(&done.stdout) } else { why };
            return Outcome::Said(format!("{} failed{}", self.name, if why.is_empty() { String::new() } else { format!(": {why}") }));
        }
        // One trailing line break is the command ending its output, not part of the text.
        let said = done.stdout.strip_suffix('\n').unwrap_or(&done.stdout).replace("\r\n", "\n");
        if self.output == Output::Message {
            let line = first(&said);
            return Outcome::Said(if line.is_empty() { format!("{}: done", self.name) } else { format!("{}: {line}", self.name) });
        }
        if said.trim().is_empty() {
            return Outcome::Said(format!("{} said nothing, so nothing was changed", self.name));
        }
        let still_there = self.range.is_none_or(|(from, to)| from.row < ed.lines.len() && text_of(&ed.lines, from, to) == self.given);
        if !still_there {
            *spare = Some(said);
            return Outcome::Said(format!("The text changed while {} ran: its result is in the clipboard (Ctrl+V)", self.name));
        }
        match (self.output, self.range) {
            (Output::Replace, Some((from, to))) => {
                if said == self.given {
                    return Outcome::Said(format!("{}: nothing to change", self.name));
                }
                ed.move_to(from, false);
                ed.move_to(to, true);
                ed.insert_str(&said);
            }
            (_, range) => {
                // After the selection, not over it.
                if let Some((_, to)) = range {
                    ed.move_to(to, false);
                }
                ed.clear_selection();
                ed.insert_str(&said);
            }
        }
        Outcome::Said(format!("{}: done · Ctrl+Z undoes it", self.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(run: &str, output: Output) -> Command {
        Command { name: "Test".into(), run: run.into(), output, key: None }
    }

    fn wait(running: &Running, ed: &mut Editor) -> (String, Option<String>) {
        let mut spare = None;
        for _ in 0..600 {
            if let Outcome::Said(said) = running.finish(ed, &mut spare) {
                return (said, spare);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the command never finished");
    }

    #[test]
    fn reads_commands_from_the_settings() {
        let mut all = Vec::new();
        set(&mut all, "Fix grammar", "llm 'fix it'").unwrap();
        set(&mut all, "Fix grammar.key", "F5").unwrap();
        set(&mut all, "Insert date", "date +%F").unwrap();
        set(&mut all, "insert date.output", "insert").unwrap();
        set(&mut all, "Insert date.key", "Alt+D").unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], Command { name: "Fix grammar".into(), run: "llm 'fix it'".into(), output: Output::Replace, key: Some((KeyCode::F(5), KeyModifiers::NONE)) });
        assert_eq!((all[1].output, all[1].key.map(key_name).as_deref()), (Output::Insert, Some("Alt+D")));
        set(&mut all, "Fix grammar", "llm 'better'").unwrap();
        assert_eq!((all.len(), all[0].run.as_str()), (2, "llm 'better'"), "said again: it replaces, keys and all kept");

        let said = |all: &mut Vec<Command>, key: &str, value: &str| set(all, key, value).unwrap_err();
        assert!(said(&mut all, "Nope.output", "insert").contains("say what Nope runs first"));
        assert!(said(&mut all, "Fix grammar.output", "sideways").contains("\"replace\", \"insert\" or \"message\""));
        assert!(said(&mut all, "Insert date.key", "f5").contains("already runs Fix grammar"));
        assert!(said(&mut all, "Insert date.key", "F2").contains("already move"));
        assert!(said(&mut all, "Insert date.key", "ctrl+s").contains("has alt in it"));
        assert!(said(&mut all, "Insert date.key", "g").contains("has alt in it"));
        assert!(said(&mut all, "Insert date.key", "F13").contains("F1 to F12"));
        assert!(said(&mut all, "Insert date.key", "hyper+x").contains("not a modifier"));
        assert!(said(&mut all, "", "x").contains("needs a name") && said(&mut all, "Empty", "").contains("something to run"));

        let press = |code, modifiers| KeyEvent::new(code, modifiers);
        assert_eq!(bound(&all, &press(KeyCode::F(5), KeyModifiers::NONE)), Some(0));
        assert_eq!(bound(&all, &press(KeyCode::Char('D'), KeyModifiers::ALT)), Some(1), "capitals or not");
        assert_eq!(bound(&all, &press(KeyCode::Char('d'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn replaces_the_selection_or_the_paragraph() {
        let mut ed = Editor::new("# Notes\n\nteh quick\nbrown fox\n\nlast line", None);
        ed.move_to(Pos { row: 3, col: 2 }, false);
        assert_eq!(paragraph(&ed.lines, 3), Some((Pos { row: 2, col: 0 }, Pos { row: 3, col: 9 })));
        assert_eq!(paragraph(&ed.lines, 1), None);
        let running = start(&command("tr a-z A-Z", Output::Replace), &ed, Path::new("/")).unwrap();
        assert!(wait(&running, &mut ed).0.contains("done"));
        assert_eq!(ed.text(), "# Notes\n\nTEH QUICK\nBROWN FOX\n\nlast line\n", "nothing selected: the paragraph the cursor is in");
        assert!(ed.undo());
        assert_eq!(ed.text(), "# Notes\n\nteh quick\nbrown fox\n\nlast line\n", "one undo");

        ed.move_to(Pos { row: 2, col: 0 }, false);
        ed.move_to(Pos { row: 2, col: 3 }, true);
        let running = start(&command("sed s/teh/the/", Output::Replace), &ed, Path::new("/")).unwrap();
        wait(&running, &mut ed);
        assert_eq!(ed.lines[2].iter().collect::<String>(), "the quick", "just the selection");

        ed.move_to(Pos { row: 1, col: 0 }, false);
        assert!(start(&command("cat", Output::Replace), &ed, Path::new("/")).is_err(), "a blank line is not a paragraph");
    }

    #[test]
    fn inserts_shows_and_reports_failure() {
        let mut ed = Editor::new("today: ", Some("/v/docs/trip plan.md".into()));
        ed.move_to(Pos { row: 0, col: 7 }, false);
        let running = start(&command("printf '%s|%s|%s' {name} {line} \"$OMANOTE_DIR\"", Output::Insert), &ed, Path::new("/tmp")).unwrap();
        wait(&running, &mut ed);
        assert_eq!(ed.text(), "today: trip plan|1|/tmp\n", "placeholders arrive whole, spaces and all");

        let before = ed.text();
        let running = start(&command("echo 42 words; echo ignored", Output::Message), &ed, Path::new("/")).unwrap();
        assert_eq!(wait(&running, &mut ed).0, "Test: 42 words");
        let running = start(&command("echo oops >&2; exit 3", Output::Insert), &ed, Path::new("/")).unwrap();
        assert_eq!(wait(&running, &mut ed).0, "Test failed: oops");
        let running = start(&command("true", Output::Insert), &ed, Path::new("/")).unwrap();
        assert!(wait(&running, &mut ed).0.contains("said nothing"));
        assert_eq!(ed.text(), before, "none of those touched the note");
    }

    #[test]
    fn never_overwrites_text_that_changed_while_it_ran() {
        let mut ed = Editor::new("fix this sentence", None);
        let running = start(&command("sleep 0.3; echo Fixed.", Output::Replace), &ed, Path::new("/")).unwrap();
        // The user carries on editing that very text.
        ed.move_to(Pos { row: 0, col: 4 }, false);
        ed.insert_str("all of ");
        let (said, spare) = wait(&running, &mut ed);
        assert!(said.contains("changed while"), "{said}");
        assert_eq!((ed.text().as_str(), spare.as_deref()), ("fix all of this sentence\n", Some("Fixed.")));

        // Typing after it is no conflict: what was given is still there, and is what gets replaced.
        let mut ed = Editor::new("fix this sentence", None);
        let running = start(&command("sleep 0.3; echo Fixed.", Output::Replace), &ed, Path::new("/")).unwrap();
        ed.move_to(Pos { row: 0, col: 17 }, false);
        ed.insert_str(" and more");
        assert!(wait(&running, &mut ed).0.contains("done"));
        assert_eq!(ed.text(), "Fixed. and more\n");

        // Esc stops a command, and whatever it started.
        let running = start(&command("sleep 30; echo late", Output::Insert), &ed, Path::new("/")).unwrap();
        running.cancel();
        assert!(wait(&running, &mut ed).0.contains("stopped"));
        let gone = std::process::Command::new("sh").args(["-c", &format!("kill -0 -- -{} 2>/dev/null", running.pid)]).status().unwrap();
        assert!(!gone.success(), "the shell and the sleep it started are both gone");
        assert_eq!(ed.text(), "Fixed. and more\n");
    }
}
