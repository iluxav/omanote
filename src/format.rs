//! Formatters: a program that reads text on its standard input and prints it
//! back tidied. What each one is given is omanote's to decide. A file goes
//! through the formatter for its kind whole. A note goes through `format.md`,
//! if there is one, and then each fenced block through the formatter for the
//! language its fence names, so a ```` ```yaml ```` block is tidied by
//! `format.yaml` and the prose around it is left alone.
//!
//! Nothing here runs a program: `tidy` works on a string and is handed the
//! way to put text through a formatter, so it can be tried without one.

use std::ops::Range;
use std::path::Path;

use crate::markdown::{self, Block, indent};
use crate::syntax::Lang;

/// `format.<kind> = "..."`: a program that reads text on its standard input
/// and prints it back tidied. `kind` is a file extension (`lua`, `md`) or the
/// name of a language omanote knows, so `sh` also covers `.bashrc`, and
/// `yaml` a ```` ```yml ```` block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Formatter {
    pub kind: String,
    pub run: String,
    /// After Ctrl+S too, not only on Ctrl+W.
    pub on_save: bool,
}

/// One line of the settings: `key` is what follows `format.`: `lua`, or `lua.save`.
pub fn set_formatter(formatters: &mut Vec<Formatter>, key: &str, value: &str) -> Result<(), String> {
    let (kind, property) = match key.rsplit_once('.') {
        Some((kind, p)) if p.trim().eq_ignore_ascii_case("save") => (kind, "save"),
        _ => (key, ""),
    };
    let kind = kind.trim().trim_start_matches('.').to_lowercase();
    if kind.is_empty() || kind.contains(char::is_whitespace) {
        return Err("the formatter needs a file kind: format.<extension> = \"<what to run>\", as in format.lua".into());
    }
    let at = formatters.iter().position(|f| f.kind == kind);
    match (property, at) {
        ("", _) if value.is_empty() => Err(format!("format.{kind} needs something to run")),
        ("", Some(at)) => {
            formatters[at].run = value.to_string();
            Ok(())
        }
        ("", None) => {
            formatters.push(Formatter { kind, run: value.to_string(), on_save: true });
            Ok(())
        }
        (_, None) => Err(format!("say what format.{kind} runs first: format.{kind} = \"...\"")),
        (_, Some(at)) => {
            formatters[at].on_save = match value.to_lowercase().as_str() {
                "true" | "on" | "yes" => true,
                "false" | "off" | "no" => false,
                other => return Err(format!("format.{kind}.save is true or false, not `{other}`")),
            };
            Ok(())
        }
    }
}

/// The formatter for a file: the one named by its extension, else the one
/// for its language. A note is markdown, `md`, whatever it is called.
pub fn formatter<'a>(formatters: &'a [Formatter], path: Option<&Path>, markdown: bool) -> Option<&'a Formatter> {
    let ext = path.and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_lowercase());
    let lang = Lang::of_path(path);
    let by_extension = formatters.iter().find(|f| Some(&f.kind) == ext.as_ref());
    by_extension.or_else(|| {
        formatters.iter().find(|f| (markdown && matches!(f.kind.as_str(), "md" | "markdown")) || (lang.is_some() && Lang::named(&f.kind) == lang))
    })
}

/// The formatter for a fenced block, by the word after its fence: the one of
/// that name, else one for the same language, so `format.yaml` takes a
/// ```` ```yml ```` block too.
pub fn for_block<'a>(formatters: &'a [Formatter], word: &str) -> Option<&'a Formatter> {
    let word = word.to_lowercase();
    let lang = Lang::named(&word);
    formatters.iter().find(|f| f.kind == word).or_else(|| formatters.iter().find(|f| lang.is_some() && Lang::named(&f.kind) == lang))
}

/// Whether Ctrl+W has anything to do: a formatter for the file, or in a note
/// one for a language one of its fenced blocks is in.
pub fn any(formatters: &[Formatter], path: Option<&Path>, markdown: bool, lines: &[Vec<char>], blocks: &[Block]) -> bool {
    formatter(formatters, path, markdown).is_some() || (markdown && fenced(lines, blocks).iter().any(|f| for_block(formatters, &f.word).is_some()))
}

/// A fenced block with lines in it, and a closing fence: one left open
/// runs to the end of the note, and is no block to tidy.
struct Fenced {
    word: String,
    /// The line of its opening fence.
    open: usize,
    /// The lines between its fences.
    inside: Range<usize>,
}

fn fenced(lines: &[Vec<char>], blocks: &[Block]) -> Vec<Fenced> {
    let mut out = Vec::new();
    let mut open = None;
    for (row, block) in blocks.iter().enumerate() {
        match block {
            Block::FenceOpen => open = Some(row),
            Block::FenceClose => {
                if let Some(at) = open.take()
                    && let Some(word) = markdown::fence_name(&lines[at])
                    && row > at + 1
                {
                    out.push(Fenced { word, open: at, inside: at + 1..row });
                }
            }
            _ => {}
        }
    }
    out
}

/// What came of tidying a file.
#[derive(Debug, PartialEq, Eq)]
pub struct Tidied {
    pub text: String,
    /// How many parts came back from their formatter.
    pub done: usize,
    /// The parts left as they were, and why: `the yaml block at line 12: yq is not installed`.
    pub left: Vec<String>,
}

/// Tidy `text`, a whole file, without its final line break: first through
/// `whole`, if there is one, then, given `blocks`, each fenced block through
/// the one of them for its language. The blocks go last so that a formatter
/// named for a language has the last word on it, not one for the whole note
/// that does code as well (prettier). A part whose formatter fails or prints
/// nothing is left as it was; the rest are still tidied.
///
/// `run` puts text through a formatter and says what came back, one trailing
/// line break taken off, or why nothing did. It is also told the language of
/// a block, for a formatter that picks its rules from the file name.
pub fn tidy(
    text: &str,
    whole: Option<&Formatter>,
    blocks: Option<&[Formatter]>,
    mut run: impl FnMut(&Formatter, &str, Option<&str>) -> Result<String, String>,
) -> Tidied {
    let mut out = Tidied { text: text.to_string(), done: 0, left: Vec::new() };
    if let Some(formatter) = whole {
        let part = if blocks.is_some() { "the note" } else { "" };
        match through(&mut run, formatter, &format!("{text}\n"), None) {
            Ok(said) => {
                out.text = said;
                out.done += 1;
            }
            Err(why) => out.left.push(problem(part, &why)),
        }
    }
    let Some(formatters) = blocks else { return out };
    let mut lines: Vec<Vec<char>> = out.text.split('\n').map(|l| l.chars().collect()).collect();
    // The blocks are found before any is tidied; one that grows or shrinks
    // moves those below it by as many lines.
    let mut grown = 0isize;
    for block in fenced(&lines, &markdown::classify(&lines)) {
        let Some(formatter) = for_block(formatters, &block.word) else { continue };
        let open = block.open.saturating_add_signed(grown);
        let inside = block.inside.start.saturating_add_signed(grown)..block.inside.end.saturating_add_signed(grown);
        // Inside a list item the block is indented with its fence; the
        // formatter gets the code as it would be in a file of its own.
        let pad: Vec<char> = lines[open][..indent(&lines[open])].to_vec();
        let code: String = lines[inside.clone()].iter().map(|l| l[indent(l).min(pad.len())..].iter().collect::<String>() + "\n").collect();
        if code.trim().is_empty() {
            continue;
        }
        match through(&mut run, formatter, &code, Some(&block.word)) {
            Ok(said) => {
                let tidied: Vec<Vec<char>> = said.trim_end_matches('\n').split('\n').map(|l| if l.is_empty() { Vec::new() } else { pad.iter().copied().chain(l.chars()).collect() }).collect();
                grown += tidied.len() as isize - inside.len() as isize;
                lines.splice(inside, tidied);
                out.done += 1;
            }
            Err(why) => out.left.push(problem(&format!("the {} block at line {}", block.word, open + 1), &why)),
        }
    }
    out.text = lines.iter().map(|l| l.iter().collect::<String>()).collect::<Vec<_>>().join("\n");
    out
}

/// One part through its formatter. Nothing back for something given is a
/// failure: a formatter that says nothing has not tidied the text away.
fn through(run: &mut impl FnMut(&Formatter, &str, Option<&str>) -> Result<String, String>, formatter: &Formatter, text: &str, kind: Option<&str>) -> Result<String, String> {
    let said = run(formatter, text, kind)?;
    if said.trim().is_empty() && !text.trim().is_empty() {
        return Err("it printed nothing".into());
    }
    Ok(said)
}

fn problem(part: &str, why: &str) -> String {
    match (part.is_empty(), why.is_empty()) {
        (true, _) => why.to_string(),
        (false, true) => format!("{part} failed"),
        (false, false) => format!("{part}: {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(kind: &str, run: &str) -> Formatter {
        Formatter { kind: kind.into(), run: run.into(), on_save: true }
    }

    /// A formatter that is not a program: each kind does something to its text you can see.
    fn run(formatter: &Formatter, text: &str, kind: Option<&str>) -> Result<String, String> {
        let text = text.strip_suffix('\n').unwrap_or(text);
        match formatter.run.as_str() {
            "upper" => Ok(text.to_uppercase()),
            "indent" => Ok(text.lines().map(|l| if l.trim().is_empty() { String::new() } else { format!("  {}", l.trim()) }).collect::<Vec<_>>().join("\n")),
            "kind" => Ok(format!("{}\n", kind.unwrap_or("-"))),
            "silent" => Ok(String::new()),
            "fail" => Err("yq is not installed".into()),
            other => panic!("no such formatter: {other}"),
        }
    }

    #[test]
    fn formatters_are_read_and_found_by_file_kind() {
        let mut all = Vec::new();
        set_formatter(&mut all, "lua", "stylua -").unwrap();
        set_formatter(&mut all, "Lua.save", "false").unwrap();
        set_formatter(&mut all, "sh", "shfmt").unwrap();
        set_formatter(&mut all, "md", "prettier --parser markdown").unwrap();
        assert_eq!(all[0], Formatter { kind: "lua".into(), run: "stylua -".into(), on_save: false });
        assert!(all[1].on_save, "on Ctrl+S unless told otherwise");
        set_formatter(&mut all, "lua", "stylua --indent-type Spaces -").unwrap();
        assert_eq!((all.len(), all[0].run.as_str(), all[0].on_save), (3, "stylua --indent-type Spaces -", false), "said again: it replaces, the rest kept");
        let said = |all: &mut Vec<Formatter>, key: &str, value: &str| set_formatter(all, key, value).unwrap_err();
        assert!(said(&mut all, "py.save", "false").contains("say what format.py runs first"));
        assert!(said(&mut all, "lua.save", "maybe").contains("true or false"));
        assert!(said(&mut all, "", "x").contains("needs a file kind") && said(&mut all, "lua", "").contains("something to run"));

        let by = |path: &str| formatter(&all, Some(Path::new(path)), crate::editor::is_markdown(Some(Path::new(path)))).map(|f| f.kind.as_str());
        assert_eq!(by("/v/init.lua"), Some("lua"));
        assert_eq!(by("/home/me/.bashrc"), Some("sh"), "by language when the name says it");
        assert_eq!(by("/v/run.zsh"), Some("sh"));
        assert_eq!(by("/v/notes/trip.md"), Some("md"));
        assert_eq!(by("/v/notes/trip.markdown"), Some("md"));
        assert_eq!(by("/v/x.toml"), None);
        assert_eq!(formatter(&all, None, true).map(|f| f.kind.as_str()), Some("md"), "a new note is markdown");

        set_formatter(&mut all, "sql", "sqlfluff fix -").unwrap();
        let block = |word: &str| for_block(&all, word).map(|f| f.kind.as_str());
        assert_eq!(block("bash"), Some("sh"), "by language");
        assert_eq!(block("LUA"), Some("lua"));
        assert_eq!(block("toml"), None);
        assert_eq!(block("sql"), Some("sql"), "a language omanote does not colour, by its name");
    }

    #[test]
    fn a_file_goes_through_whole() {
        let tidied = tidy("a\n```lua\nb\n```", Some(&fmt("lua", "upper")), None, run);
        assert_eq!(tidied, Tidied { text: "A\n```LUA\nB\n```".into(), done: 1, left: vec![] }, "not a note: fences are just text");
        let tidied = tidy("a", Some(&fmt("lua", "fail")), None, run);
        assert_eq!((tidied.text.as_str(), tidied.done, tidied.left), ("a", 0, vec!["yq is not installed".to_string()]));
    }

    #[test]
    fn a_note_has_only_its_fenced_blocks_tidied() {
        let all = [fmt("yaml", "indent"), fmt("sh", "upper")];
        let note = "# Setup\n\n```yml\na:\n      b\n```\n\nsome **prose**\n\n```bash\necho hi\n```\n\n```toml\nx=1\n```";
        let tidied = tidy(note, None, Some(&all), run);
        assert_eq!(tidied.text, "# Setup\n\n```yml\n  a:\n  b\n```\n\nsome **prose**\n\n```bash\nECHO HI\n```\n\n```toml\nx=1\n```");
        assert_eq!((tidied.done, tidied.left.len()), (2, 0), "no formatter for toml: that block is left alone, and that is no failure");
    }

    #[test]
    fn the_note_formatter_goes_first_and_the_blocks_have_the_last_word() {
        let all = [fmt("md", "upper"), fmt("yaml", "kind")];
        let tidied = tidy("text\n```yaml\nk: v\n```", Some(&all[0]), Some(&all), run);
        // The note's formatter made it YAML; the block is found by that name and tidied after.
        assert_eq!(tidied.text, "TEXT\n```YAML\nYAML\n```");
        assert_eq!(tidied.done, 2);
    }

    #[test]
    fn a_block_in_a_list_keeps_its_indent() {
        let all = [fmt("sh", "indent")];
        let tidied = tidy("- step\n\n  ```sh\n  a\n\n    b\n  ```", None, Some(&all), run);
        assert_eq!(tidied.text, "- step\n\n  ```sh\n    a\n\n    b\n  ```", "the fence's indent comes off before and goes back after, blank lines stay blank");
    }

    #[test]
    fn a_block_that_fails_is_left_as_it_was_and_the_rest_are_tidied() {
        let all = [fmt("yaml", "fail"), fmt("sh", "upper"), fmt("lua", "silent")];
        let note = "```yaml\nk: v\n```\n```sh\nls\n```\n```lua\nx()\n```\n```sh\n\n```\n```sh\nopen";
        let tidied = tidy(note, Some(&fmt("md", "fail")), Some(&all), run);
        assert_eq!(tidied.text, "```yaml\nk: v\n```\n```sh\nLS\n```\n```lua\nx()\n```\n```sh\n\n```\n```sh\nopen", "a blank block and one never closed are not touched");
        assert_eq!(tidied.done, 1);
        assert_eq!(tidied.left, ["the note: yq is not installed", "the yaml block at line 1: yq is not installed", "the lua block at line 7: it printed nothing"], "top to bottom");
    }

    #[test]
    fn a_failure_names_the_line_the_block_is_on_after() {
        let all = [fmt("sh", "kind"), fmt("yaml", "fail")];
        let tidied = tidy("```sh\na\nb\nc\n```\n```yaml\nk: v\n```", None, Some(&all), run);
        assert_eq!(tidied.text, "```sh\nsh\n```\n```yaml\nk: v\n```");
        assert_eq!(tidied.left, ["the yaml block at line 4: yq is not installed"], "the sh block shrank by two lines above it");
    }

    #[test]
    fn a_block_tells_its_formatter_its_language() {
        let all = [fmt("yaml", "kind")];
        assert_eq!(tidy("```yml\nk: v\n```", None, Some(&all), run).text, "```yml\nyml\n```");
    }
}
