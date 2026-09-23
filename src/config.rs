//! `~/.omanote/config.toml`: how the page is laid out. Everything is optional;
//! `omanote --config` writes a commented file to start from.

use std::path::{Path, PathBuf};

use crate::commands::Command;
use crate::look::Look;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// What checks the grammar: the model omanote downloads for it, or the
/// language model named for type-ahead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grammar {
    /// The language model when there is one, else the built-in.
    Auto,
    Builtin,
    Llm,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Widest the text column gets, in characters. 0 = as wide as the window.
    pub width: u16,
    pub align: Align,
    /// Blank columns kept free on both sides of the window.
    pub margin: u16,
    /// Skip the Ctrl+G menu and always open this: an agent's name, or a command.
    pub assistant: Option<String>,
    /// Your own agents for the Ctrl+G menu: (name, command).
    pub agents: Vec<(String, String)>,
    /// `editor.style.<element>.<property>`: how the text looks.
    pub look: Look,
    /// `command.<name> = "..."`: your own commands, for Ctrl+R.
    pub commands: Vec<Command>,
    /// Spelling and grammar, checked as you write, and in which English.
    pub spelling: bool,
    pub dialect: String,
    pub grammar: Grammar,
    /// `complete.*`: type-ahead from a language model of your own.
    pub complete: crate::complete::Settings,
}

impl Default for Config {
    fn default() -> Self {
        Config { width: 84, align: Align::Center, margin: 2, assistant: None, agents: Vec::new(), look: Look::default(), commands: Vec::new(), spelling: true, dialect: "american".into(), grammar: Grammar::Auto, complete: Default::default() }
    }
}

const TEMPLATE: &str = r##"# omanote settings. Delete a line to get its default back.

# Widest the text column gets, in characters. Long lines are easier to read
# when they are not too long. 0 = use the whole window.
width = 84

# Where the text column sits when the window is wider than it:
# "left", "center" or "right".
align = "center"

# Blank columns always kept free at the window's edges.
margin = 2

# Ctrl+G opens an AI agent in a pane beside the note. omanote finds the agent
# CLIs you have installed (Claude Code, Codex, Gemini, opencode, ...) and asks
# which one; with only one installed it opens that.
#
# Add your own to the menu, or redefine a known one:
#   {context}  what you are doing: the note, the cursor line, any selected text
#   {file}     the note          {dir}  the folder the agent starts in
#   {now}      a file omanote keeps up to date with the note you are in, as you move
#              between notes ({context} tells the agent to read it); {nowdir} is its
#              folder, for agents that must be allowed to read outside their own
# agent.Work bot = "workbot --chat {context}"
# agent.claude = "claude --model opus --add-dir {nowdir} --append-system-prompt {context}"
#
# To skip the menu, name the one you always want (or give a full command):
# assistant = "codex"

# How the text looks. Every element has a colour and can be bold, italic,
# underlined, dim or struck out; anything you do not mention keeps its default.
# (The font itself, its size and family, is your terminal's to set.)
#
#   editor.style.<element>.color       a name (red, bright-blue, gray, default),
#   editor.style.<element>.background  "#rrggbb", or a number from 0 to 255
#   editor.style.<element>.bold        true / false; also italic, underline,
#                                      dim, strike
#
# Elements: text  h1 h2 h3 h4 h5 h6  bold  italic  strike  highlight  code
#   codeblock  link  tag  quote  quote.bar  list  task  task.done
#   task.done.text  syntax  table.border  table.header  rule
#
# editor.style.h1.color = "#ff9e64"
# editor.style.h1.underline = false
# editor.style.text.color = "#c0caf5"
# editor.style.code.background = "#1f2335"
# editor.style.link.color = "cyan"

# Your own commands, listed by Ctrl+R: anything a shell can run. The selected
# text (or, with nothing selected, the paragraph the cursor is in) goes to the
# command's standard input, and what it prints takes its place. Ctrl+Z undoes it.
#
#   command.<name> = "<what to run>"
#   command.<name>.output = "replace"   (the default), "insert": print at the
#                                       cursor, or "message": just show it
#   command.<name>.key = "F5"           F1-F12, or a letter with alt: "alt+g"
#
# In the command, {file} is the note, {dir} its folder, {name} its name without
# .md, {line} the cursor's line. The note is saved before the command runs.
#
# command.Rewrite = "llm 'Rewrite this more clearly. Reply with the text only.'"   # any AI CLI that reads stdin
# command.Rewrite.key = "F5"
# command.Sort lines = "sort"
# command.Insert date = "date +%F"
# command.Insert date.output = "insert"
# command.Word count = "wc -w < {file}"
# command.Word count.output = "message"

# Spelling and grammar are checked as you write, in English, once Shift+F7 has
# turned it on (the first time, it downloads the 78 MB grammar model). Mistakes
# are underlined; F7 or a click on the word offers the fixes.
# Words of your own go in ~/.omanote/dictionary.txt (F7 puts them there).
# spelling = false
# spelling.dialect = "british"     # american (the default), british, canadian, australian, indian
#
# Grammar is read by a small model omanote downloads, or, once complete.model
# names a language model (below), by that one: it knows more, and there is
# nothing to download. It is sent each sentence you change, to the same address.
# "auto", the default, uses the language model when there is one; "builtin"
# always the small model; "llm" always the language model.
# spelling.grammar = "builtin"

# Type-ahead: as you write, a word or two of what might come next shows dim
# after the cursor, and Tab takes it. The guess comes from a language model
# you run yourself; naming one turns it on. What you are writing is sent to
# the address below as you type, so point it at a server you trust.
#
# The default address is Ollama's (https://ollama.com). An address ending in
# /v1 is asked as an OpenAI-style server instead: llama.cpp, LM Studio, vLLM.
# complete.model = "qwen3.5:9b"
# complete.url = "http://localhost:11434"
# complete.key = "..."          # for a server that wants a key
# complete.words = 3            # the most words a guess shows
#
# The model's context window, in tokens, and about how many characters of the
# note are sent. Ollama sets aside memory for all of it, and a model's own
# window (often 131072) can take gigabytes. Other servers set it themselves.
# complete.context = 2048
"##;

pub fn path(home: &Path) -> PathBuf {
    home.join("config.toml")
}

/// Write the commented template unless a config is already there.
pub fn ensure(home: &Path) -> std::io::Result<PathBuf> {
    let file = path(home);
    std::fs::create_dir_all(home)?;
    let current = std::fs::read_to_string(&file).unwrap_or_default();
    let updated = with_new_sections(&current);
    if updated != current {
        std::fs::write(&file, updated)?;
    }
    Ok(file)
}

/// The setting a block of the template is about: `width`, `agent`, `editor`…
fn topic(block: &str) -> Option<&str> {
    block.lines().find_map(|l| {
        let l = l.trim_start_matches('#').trim();
        let (key, _) = l.split_once('=')?;
        // `width = 84`, `agent.Work bot = "..."`, `editor.style.<element>.color`: a
        // setting's first part is one plain word. Prose with an `=` in it is not.
        let first = key.trim().split('.').next().unwrap_or("").trim();
        (!first.is_empty() && first.chars().all(|c| c.is_ascii_lowercase() || c == '_')).then_some(first)
    })
}

/// A settings file written by an older omanote knows nothing of what has
/// been added since. Every block of the template about a setting the file
/// never mentions, not even in a comment, is added at the end, so the file
/// always shows what there is to set. Nothing already there is touched.
fn with_new_sections(current: &str) -> String {
    if current.trim().is_empty() {
        return TEMPLATE.to_string();
    }
    let mentions = |topic: &str| current.lines().any(|l| l.trim_start_matches('#').trim_start().starts_with(topic));
    let mut out = current.trim_end().to_string();
    let mut block = String::new();
    let mut blocks = Vec::new();
    for line in TEMPLATE.lines().skip(1) {
        if line.trim().is_empty() && !block.is_empty() {
            blocks.push(std::mem::take(&mut block));
        } else if !line.trim().is_empty() {
            block.push_str(line);
            block.push('\n');
        }
    }
    if !block.is_empty() {
        blocks.push(block);
    }
    // A block about nothing (more comment) belongs with the block before it.
    let mut sections: Vec<(String, String)> = Vec::new();
    for block in blocks {
        match (topic(&block), sections.last_mut()) {
            (Some(topic), _) => sections.push((topic.to_string(), block)),
            (None, Some((_, text))) => {
                text.push('\n');
                text.push_str(&block);
            }
            (None, None) => {}
        }
    }
    let mut missing_lines = Vec::new();
    for (topic, text) in sections {
        if !mentions(&topic) {
            out.push_str("\n\n");
            out.push_str(text.trim_end());
        } else {
            missing_lines.push((topic, text));
        }
    }
    // A section the file has, from before a setting was added to it: the new
    // setting, with the comment above it, goes after the section's last line.
    for (topic, text) in missing_lines {
        for chunk in new_settings(&text, current) {
            let lines: Vec<&str> = out.lines().collect();
            let last = lines.iter().rposition(|l| setting_key(l).is_some_and(|k| k == topic || k.starts_with(&format!("{topic}."))));
            let at = last.map_or(lines.len(), |i| i + 1);
            let mut grown: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            grown.splice(at..at, chunk);
            out = grown.join("\n");
        }
    }
    out.push('\n');
    out
}

/// The name a line sets, commented out or not: `spelling.grammar` for
/// `# spelling.grammar = "llm"`. Prose with an `=` in it names nothing.
fn setting_key(line: &str) -> Option<&str> {
    let (key, _) = line.trim_start_matches('#').trim().split_once('=')?;
    let key = key.trim();
    (!key.is_empty() && !key.contains(['<', ' ']) && key.chars().all(|c| c.is_ascii_lowercase() || c == '.' || c == '_')).then_some(key)
}

/// The settings in a template section that `current` never mentions, each
/// with the comment lines just above it.
fn new_settings(section: &str, current: &str) -> Vec<Vec<String>> {
    let known: std::collections::HashSet<&str> = current.lines().filter_map(setting_key).collect();
    let mut out = Vec::new();
    let mut comment: Vec<String> = Vec::new();
    for line in section.lines() {
        match setting_key(line) {
            Some(key) => {
                if !known.contains(key) {
                    comment.push(line.to_string());
                    out.push(std::mem::take(&mut comment));
                }
                comment.clear();
            }
            None => comment.push(line.to_string()),
        }
    }
    out
}

/// The settings, and a complaint for every line that could not be used.
pub fn load(home: &Path) -> (Config, Vec<String>) {
    parse(&std::fs::read_to_string(path(home)).unwrap_or_default())
}

fn parse(text: &str) -> (Config, Vec<String>) {
    let mut config = Config::default();
    let mut problems = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        // A `#` starts a comment, unless it is inside a quoted value.
        let cut = raw.char_indices().scan(false, |quoted, (i, c)| {
            if c == '"' {
                *quoted = !*quoted;
            }
            Some((i, c, *quoted))
        });
        let end = cut.into_iter().find(|(_, c, quoted)| *c == '#' && !quoted).map_or(raw.len(), |(i, _, _)| i);
        let line = raw[..end].trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            problems.push(format!("line {}: expected `name = value`", n + 1));
            continue;
        };
        let value = value.trim().trim_matches('"');
        let number = |problems: &mut Vec<String>| match value.parse::<u16>() {
            Ok(v) => Some(v),
            Err(_) => {
                problems.push(format!("line {}: `{value}` is not a number", n + 1));
                None
            }
        };
        match key.trim() {
            "width" => config.width = number(&mut problems).unwrap_or(config.width),
            "margin" => config.margin = number(&mut problems).unwrap_or(config.margin),
            "assistant" if !value.is_empty() => config.assistant = Some(value.to_string()),
            "assistant" => problems.push(format!("line {}: assistant needs an agent's name or a command", n + 1)),
            agent if agent.starts_with("agent.") => match (agent["agent.".len()..].trim(), value.is_empty()) {
                ("", _) => problems.push(format!("line {}: the agent needs a name: agent.<name> = \"<command>\"", n + 1)),
                (_, true) => problems.push(format!("line {}: the agent needs a command", n + 1)),
                (name, false) => config.agents.push((name.to_string(), value.to_string())),
            },
            "spelling" => match value.to_lowercase().as_str() {
                "true" | "on" | "yes" => config.spelling = true,
                "false" | "off" | "no" => config.spelling = false,
                _ => problems.push(format!("line {}: spelling is true or false", n + 1)),
            },
            "spelling.dialect" | "spelling.english" | "dialect" => match crate::spell::dialect(value) {
                Some(_) => config.dialect = value.trim().to_lowercase(),
                None => problems.push(format!("line {}: the dialect is \"american\", \"british\", \"canadian\", \"australian\" or \"indian\", not `{value}`", n + 1)),
            },
            "complete.model" => config.complete.model = value.to_string(),
            "complete.url" if value.starts_with("http://") || value.starts_with("https://") => config.complete.url = value.trim_end_matches('/').to_string(),
            "complete.url" => problems.push(format!("line {}: complete.url is an address starting with http:// or https://", n + 1)),
            "complete.key" => config.complete.key = value.to_string(),
            "complete.words" => match value.parse::<usize>() {
                Ok(words @ 1..=20) => config.complete.words = words,
                _ => problems.push(format!("line {}: complete.words is a number from 1 to 20", n + 1)),
            },
            "complete.context" => match value.parse::<usize>() {
                Ok(tokens @ 256..=262_144) => config.complete.context = tokens,
                _ => problems.push(format!("line {}: complete.context is a number of tokens from 256 to 262144", n + 1)),
            },
            "spelling.grammar" => match value.to_lowercase().as_str() {
                "auto" => config.grammar = Grammar::Auto,
                "builtin" | "built-in" | "gector" => config.grammar = Grammar::Builtin,
                "llm" | "complete" => config.grammar = Grammar::Llm,
                _ => problems.push(format!("line {}: spelling.grammar is \"auto\", \"builtin\" or \"llm\", not `{value}`", n + 1)),
            },
            command if command.starts_with("command.") => {
                if let Err(problem) = crate::commands::set(&mut config.commands, &command["command.".len()..], value) {
                    problems.push(format!("line {}: {problem}", n + 1));
                }
            }
            style if style.starts_with("editor.style.") || style.starts_with("style.") => {
                let element = style.trim_start_matches("editor.").trim_start_matches("style.");
                if let Err(problem) = config.look.set(element, value) {
                    problems.push(format!("line {}: {problem}", n + 1));
                }
            }
            "align" => match value.to_lowercase().as_str() {
                "left" => config.align = Align::Left,
                "center" | "centre" => config.align = Align::Center,
                "right" => config.align = Align::Right,
                _ => problems.push(format!("line {}: align is \"left\", \"center\" or \"right\"", n + 1)),
            },
            other => problems.push(format!("line {}: unknown setting `{other}`", n + 1)),
        }
    }
    if config.grammar == Grammar::Llm && !config.complete.on() {
        problems.push("spelling.grammar = \"llm\" needs a model: complete.model = \"<name>\"".into());
    }
    (config, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_is_the_defaults() {
        assert_eq!(parse(TEMPLATE), (Config::default(), vec![]));
        assert_eq!(parse(""), (Config::default(), vec![]));
    }

    #[test]
    fn reads_settings_and_reports_what_it_cannot_use() {
        let (config, problems) = parse("width = 0   # full window\nalign = \"Left\"\nmargin=4\n");
        assert_eq!((config, problems.len()), (Config { width: 0, align: Align::Left, margin: 4, ..Config::default() }, 0));
        assert_eq!(parse("assistant = \"codex {context}\"").0.assistant.as_deref(), Some("codex {context}"));
        let (config, problems) = parse("agent.Work bot = \"workbot --chat {context}  # not a comment\"\nagent. = \"x\"\nagent.empty = \"\"");
        assert_eq!(config.agents, [("Work bot".to_string(), "workbot --chat {context}  # not a comment".to_string())]);
        assert_eq!(problems.len(), 2);

        let (config, problems) = parse("width = wide\nalign = middle\ncolour = red\njunk\nmargin = 1");
        assert_eq!(config, Config { margin: 1, ..Config::default() }, "bad lines fall back to defaults");
        assert_eq!(problems.len(), 4);
        assert!(problems[0].contains("line 1") && problems[2].contains("colour"));
    }
    #[test]
    fn styles_are_read_and_mistakes_in_them_are_named() {
        let (config, problems) = parse("editor.style.h1.font.color = \"#ff9e64\"   # orange\nstyle.link.underline = false\neditor.style.text.font.size = 14\neditor.style.h9.color = red");
        let mut wanted = Look::default();
        wanted.set("h1.color", "#ff9e64").unwrap();
        wanted.set("link.underline", "false").unwrap();
        assert_eq!(config.look, wanted, "the hash in a quoted colour is not a comment");
        assert_eq!(problems.len(), 2);
        assert!(problems[0].starts_with("line 3:") && problems[0].contains("the terminal's to set"), "{}", problems[0]);
        assert!(problems[1].starts_with("line 4:") && problems[1].contains("no element `h9`"), "{}", problems[1]);
        let (written, none) = parse(TEMPLATE);
        assert!(none.is_empty() && written.look == Look::default(), "the examples in the template are comments: {none:?}");
    }

    #[test]
    fn commands_are_read_with_their_output_and_key() {
        let (config, problems) = parse("command.Fix grammar = \"llm 'fix # this'\"\ncommand.Fix grammar.key = \"F5\"\ncommand.Insert date = \"date +%F\"\ncommand.Insert date.output = insert\ncommand.Insert date.key = \"ctrl+s\"");
        assert_eq!(config.commands.len(), 2);
        assert_eq!(config.commands[0].run, "llm 'fix # this'", "a hash inside the quotes is part of the command");
        assert_eq!(config.commands[1].output, crate::commands::Output::Insert);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("line 5:"));
        assert!(parse(TEMPLATE).0.commands.is_empty(), "the template's commands are examples, commented out");
    }

    #[test]
    fn spelling_can_be_turned_off_and_told_which_english() {
        let (config, problems) = parse("spelling = off\nspelling.dialect = \"British\"\nspelling.dialect = klingon");
        assert!(!config.spelling && config.dialect == "british");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("klingon"));
        let (config, _) = parse("");
        assert!(config.spelling && config.dialect == "american");
    }

    #[test]
    fn grammar_can_be_read_by_the_language_model() {
        assert_eq!(parse("").0.grammar, Grammar::Auto);
        assert_eq!(parse("spelling.grammar = builtin").0.grammar, Grammar::Builtin);
        let (config, problems) = parse("spelling.grammar = \"llm\"\ncomplete.model = \"llama3.2:3b\"");
        assert!(config.grammar == Grammar::Llm && problems.is_empty(), "{problems:?}");
        let (_, problems) = parse("spelling.grammar = llm");
        assert!(problems.len() == 1 && problems[0].contains("complete.model"), "{problems:?}");
        assert_eq!(parse("spelling.grammar = maybe").1.len(), 1);
    }

    #[test]
    fn type_ahead_is_off_until_a_model_is_named() {
        assert!(!parse(TEMPLATE).0.complete.on());
        let (config, problems) = parse("complete.model = \"qwen3.5:9b\"\ncomplete.url = \"http://box:8080/v1/\"\ncomplete.words = 2\ncomplete.words = 0\ncomplete.url = localhost\ncomplete.context = 4096\ncomplete.context = 10");
        assert!(config.complete.on());
        assert_eq!((config.complete.url.as_str(), config.complete.words, config.complete.context), ("http://box:8080/v1", 2, 4096));
        assert_eq!(problems.len(), 3, "{problems:?}");
    }

    #[test]
    fn an_old_settings_file_learns_about_new_settings() {
        let old = "# omanote settings.\n\nwidth = 100\n\nalign = \"center\"\n\nmargin = 2\n";
        let grown = with_new_sections(old);
        assert!(grown.starts_with(old.trim_end()), "what was there is untouched");
        for topic in ["agent.", "editor.style.", "command.", "spelling", "complete."] {
            assert!(grown.contains(topic), "{topic} was added");
        }
        assert_eq!(grown.matches("width = ").count(), 1, "nothing is added twice");
        assert_eq!(with_new_sections(&grown), grown, "and a file that has everything stays as it is");
        assert_eq!(with_new_sections(""), TEMPLATE);
        let (_, problems) = parse(&grown);
        assert!(problems.is_empty(), "{problems:?}");
        // A commented-out mention counts as knowing about it.
        let knows = format!("{old}\n# spelling = false\n");
        assert!(!with_new_sections(&knows).contains("Spelling and grammar are checked"));
    }

    #[test]
    fn a_setting_added_to_a_section_the_file_has_is_added_too() {
        let old = "width = 100\n\n# spelling = false\n# spelling.dialect = \"british\"\n\ncomplete.model = \"llama3.2:3b\"\n# complete.words = 3\n\n# the end\n";
        let grown = with_new_sections(old);
        let lines: Vec<&str> = grown.lines().collect();
        let at = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle} missing:\n{grown}"));
        assert!(at("spelling.dialect") < at("spelling.grammar") && at("spelling.grammar") < at("complete.model = \"llama"), "{grown}");
        assert!(at("complete.words") < at("complete.context") && at("complete.context") < at("# the end"), "{grown}");
        assert!(at("Grammar is read by") + 5 == at("spelling.grammar = \"builtin\""), "its comment comes with it:\n{grown}");
        assert_eq!(grown.matches("spelling.dialect").count(), 1);
        assert_eq!(with_new_sections(&grown), grown, "added once");
        assert!(parse(&grown).1.is_empty());
    }

}
