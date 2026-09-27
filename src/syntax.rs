//! Syntax colouring for source and config files, and for fenced code in a note.
//!
//! One small tokenizer, driven by a table per language: comments, strings,
//! numbers, keywords, and the keys and `[sections]` of config files. It
//! colours what a reader's eye looks for and leaves the rest alone. It is not
//! a parser, so an odd construct is coloured wrong now and then, as in nano.
//!
//! A line is coloured on its own, given what the lines above left `Open` (a
//! block comment, a string that runs over several lines). `blocks` works that
//! out for a whole file and `markdown::classify` for fenced code, so any line
//! can be drawn without looking at its neighbours.

use std::path::Path;

use ratatui::style::{Modifier, Style};

use crate::look::{self, El};
use crate::markdown::{Block, CharCell, indent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Toml,
    Yaml,
    Json,
    Ini,
    Lua,
    Shell,
    Python,
    Rust,
    Js,
    Go,
    C,
}

/// What a line starts inside of, left open by the lines above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    No,
    /// A block comment, until this closes it.
    Comment(&'static str),
    /// A string that runs over several lines, until this closes it.
    Str(&'static str),
    /// A YAML block scalar (`key: |`): every line indented deeper than this.
    Scalar(usize),
}

impl Lang {
    /// By a file's extension or the word after a fence: `toml`, `yml`, `rs`, `bash`, …
    pub fn named(name: &str) -> Option<Lang> {
        Some(match name.to_lowercase().as_str() {
            "toml" => Lang::Toml,
            "yaml" | "yml" => Lang::Yaml,
            "json" | "jsonc" | "json5" => Lang::Json,
            "ini" | "conf" | "cfg" | "desktop" | "service" | "env" | "properties" | "gitconfig" | "editorconfig" => Lang::Ini,
            "lua" => Lang::Lua,
            "sh" | "bash" | "zsh" | "shell" | "fish" => Lang::Shell,
            "py" | "python" => Lang::Python,
            "rs" | "rust" => Lang::Rust,
            "js" | "javascript" | "mjs" | "cjs" | "jsx" | "ts" | "typescript" | "tsx" => Lang::Js,
            "go" | "golang" => Lang::Go,
            "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "c++" => Lang::C,
            _ => return None,
        })
    }

    /// The language of a file, by its extension or, failing that, its name.
    pub fn of_path(path: Option<&Path>) -> Option<Lang> {
        let path = path?;
        if let Some(ext) = path.extension() {
            return Lang::named(&ext.to_string_lossy());
        }
        match path.file_name()?.to_str()? {
            ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" | ".zprofile" | ".bash_aliases" | "Makefile" | "makefile" | "Dockerfile" | "PKGBUILD" => Some(Lang::Shell),
            ".env" | ".gitconfig" | ".editorconfig" | ".npmrc" => Some(Lang::Ini),
            _ => None,
        }
    }

    fn spec(self) -> &'static Spec {
        match self {
            Lang::Toml => &TOML,
            Lang::Yaml => &YAML,
            Lang::Json => &JSON,
            Lang::Ini => &INI,
            Lang::Lua => &LUA,
            Lang::Shell => &SHELL,
            Lang::Python => &PYTHON,
            Lang::Rust => &RUST,
            Lang::Js => &JS,
            Lang::Go => &GO,
            Lang::C => &C,
        }
    }
}

/// What there is to know about a language to colour it.
struct Spec {
    /// What starts a comment that runs to the end of the line.
    line_comments: &'static [&'static str],
    /// A comment that may run over several lines, and what closes it.
    block_comment: Option<(&'static str, &'static str)>,
    /// Strings that may run over several lines, and what closes each.
    block_strings: &'static [(&'static str, &'static str)],
    quotes: &'static [char],
    keywords: &'static [&'static str],
    /// The character that makes the word before it a key, `=` or `:`. Config
    /// files only; in code it would colour every assignment.
    key_sep: Option<char>,
    /// `[section]` lines.
    sections: bool,
    /// Dates and times, `1979-05-27T07:32:00`, read as one number.
    dates: bool,
}

static TOML: Spec = Spec {
    line_comments: &["#"],
    block_comment: None,
    block_strings: &[("\"\"\"", "\"\"\""), ("'''", "'''")],
    quotes: &['"', '\''],
    keywords: &["true", "false", "inf", "nan"],
    key_sep: Some('='),
    sections: true,
    dates: true,
};

static YAML: Spec = Spec {
    line_comments: &["#"],
    block_comment: None,
    block_strings: &[],
    quotes: &['"', '\''],
    keywords: &[
        "true", "false", "null", "yes", "no", "on", "off", "True", "False", "Null", "Yes", "No", "On", "Off", "TRUE", "FALSE", "NULL", "YES", "NO", "ON", "OFF",
    ],
    key_sep: Some(':'),
    sections: false,
    dates: true,
};

static JSON: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    block_strings: &[],
    quotes: &['"'],
    keywords: &["true", "false", "null"],
    key_sep: Some(':'),
    sections: false,
    dates: false,
};

static INI: Spec = Spec {
    line_comments: &["#", ";"],
    block_comment: None,
    block_strings: &[],
    quotes: &['"'],
    keywords: &["true", "false", "yes", "no", "on", "off"],
    key_sep: Some('='),
    sections: true,
    dates: false,
};

static LUA: Spec = Spec {
    line_comments: &["--"],
    block_comment: Some(("--[[", "]]")),
    block_strings: &[("[[", "]]")],
    quotes: &['"', '\''],
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until",
        "while",
    ],
    key_sep: None,
    sections: false,
    dates: false,
};

static SHELL: Spec = Spec {
    line_comments: &["#"],
    block_comment: None,
    block_strings: &[],
    quotes: &['"', '\''],
    keywords: &[
        "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "in", "function", "select", "return", "local", "export", "readonly", "declare",
        "break", "continue", "source", "set", "unset", "shift", "exit",
    ],
    key_sep: None,
    sections: false,
    dates: false,
};

static PYTHON: Spec = Spec {
    line_comments: &["#"],
    block_comment: None,
    block_strings: &[("\"\"\"", "\"\"\""), ("'''", "'''")],
    quotes: &['"', '\''],
    keywords: &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
    ],
    key_sep: None,
    sections: false,
    dates: false,
};

static RUST: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    block_strings: &[],
    // No `'`: it is a lifetime far more often than a character.
    quotes: &['"'],
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
        "move", "mut", "pub", "ref", "return", "Self", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while",
    ],
    key_sep: None,
    sections: false,
    dates: false,
};

static JS: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    block_strings: &[("`", "`")],
    quotes: &['"', '\''],
    keywords: &[
        "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do", "else", "enum", "export", "extends", "false", "finally",
        "for", "function", "if", "import", "in", "instanceof", "let", "new", "null", "of", "return", "static", "super", "switch", "this", "throw", "true", "try", "typeof",
        "undefined", "var", "void", "while", "with", "yield",
        // TypeScript
        "abstract", "any", "declare", "implements", "interface", "keyof", "namespace", "never", "private", "protected", "public", "readonly", "type", "unknown",
    ],
    key_sep: None,
    sections: false,
    dates: false,
};

static GO: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    block_strings: &[("`", "`")],
    quotes: &['"', '\''],
    keywords: &[
        "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func", "go", "goto", "if", "import", "interface", "map", "package", "range",
        "return", "select", "struct", "switch", "type", "var", "true", "false", "nil", "iota", "any", "bool", "byte", "error", "float32", "float64", "int", "int8", "int16", "int32",
        "int64", "rune", "string", "uint", "uint8", "uint16", "uint32", "uint64",
    ],
    key_sep: None,
    sections: false,
    dates: false,
};

static C: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    block_strings: &[],
    quotes: &['"', '\''],
    keywords: &[
        "auto", "bool", "break", "case", "catch", "char", "class", "const", "constexpr", "continue", "default", "delete", "do", "double", "else", "enum", "explicit", "extern",
        "false", "float", "for", "goto", "if", "inline", "int", "long", "namespace", "new", "nullptr", "operator", "override", "private", "protected", "public", "return", "short",
        "signed", "sizeof", "static", "struct", "switch", "template", "this", "throw", "true", "try", "typedef", "typename", "union", "unsigned", "using", "virtual", "void",
        "volatile", "while",
    ],
    key_sep: None,
    sections: false,
    dates: false,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok {
    Comment,
    Keyword,
    Str,
    Number,
    Key,
    Section,
}

impl Tok {
    fn style(self) -> Style {
        match self {
            Tok::Comment => look::of(El::Comment),
            Tok::Keyword => look::of(El::Keyword),
            Tok::Str => look::of(El::String),
            Tok::Number => look::of(El::Number),
            Tok::Key => look::of(El::Key),
            Tok::Section => look::of(El::Key).add_modifier(Modifier::BOLD),
        }
    }
}

/// Blocks for a file that is not markdown: every line `Source`, with what the
/// lines above it left open.
pub fn blocks(lines: &[Vec<char>], lang: Option<Lang>) -> Vec<Block> {
    let mut open = Open::No;
    lines
        .iter()
        .map(|line| {
            let block = Block::Source { lang, open };
            if let Some(lang) = lang {
                open = carry(line, lang, open);
            }
            block
        })
        .collect()
}

/// What `line` leaves open for the line after it.
pub fn carry(line: &[char], lang: Lang, open: Open) -> Open {
    scan(line, lang, open, |_, _, _| {})
}

/// Colour one line's cells: the language's tokens laid over `base`.
pub fn paint(line: &[char], lang: Lang, open: Open, base: Style, cells: &mut [CharCell]) {
    scan(line, lang, open, |start, end, tok| {
        for cell in &mut cells[start..end] {
            cell.style = base.patch(tok.style());
        }
    });
}

fn starts_with(ch: &[char], i: usize, s: &str) -> bool {
    s.chars().enumerate().all(|(k, c)| ch.get(i + k) == Some(&c))
}

/// Index just past the first `closer` at or after `from`.
fn find_close(ch: &[char], from: usize, closer: &str) -> Option<usize> {
    (from..ch.len()).find(|&k| starts_with(ch, k, closer)).map(|k| k + closer.len())
}

/// Index just past the quote that closes the string opening at `i`, or the
/// end of the line when nothing does.
fn quoted_end(ch: &[char], i: usize, quote: char) -> usize {
    let mut k = i + 1;
    while k < ch.len() {
        match ch[k] {
            '\\' => k += 2,
            c if c == quote => return k + 1,
            _ => k += 1,
        }
    }
    ch.len()
}

/// Whether the token ending at `j` is a key: the separator comes next. A `:`
/// must then be followed by space, so that `http://` is not a key; a quoted
/// JSON key, `"a":1`, is one regardless.
fn key_follows(ch: &[char], j: usize, sep: char, quoted: bool) -> bool {
    let k = (j..ch.len()).find(|&k| !matches!(ch[k], ' ' | '\t')).unwrap_or(ch.len());
    if ch.get(k) != Some(&sep) {
        return false;
    }
    match sep {
        ':' => quoted || ch.get(k + 1).is_none_or(|c| c.is_whitespace()),
        _ => ch.get(k + 1) != Some(&'='),
    }
}

/// `[server]` / `[[bin]]` at `i`: index past the closing bracket, when nothing
/// but space or a comment follows.
fn section_end(ch: &[char], i: usize, spec: &Spec) -> Option<usize> {
    if ch.get(i) != Some(&'[') {
        return None;
    }
    let mut depth = 0usize;
    let mut end = None;
    for (k, &c) in ch.iter().enumerate().skip(i) {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(k + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.filter(|&end| end > i + 2)?;
    let rest = (end..ch.len()).find(|&k| !ch[k].is_whitespace());
    rest.is_none_or(|k| spec.line_comments.iter().any(|lc| starts_with(ch, k, lc))).then_some(end)
}

/// A digit, or a sign directly before one that does not follow a value (`a-1`).
fn number_start(ch: &[char], i: usize) -> bool {
    let c = ch[i];
    c.is_ascii_digit()
        || (matches!(c, '-' | '+')
            && ch.get(i + 1).is_some_and(|d| d.is_ascii_digit())
            && (i == 0 || !(ch[i - 1].is_alphanumeric() || matches!(ch[i - 1], '_' | ')' | ']' | '"' | '\''))))
}

fn number_end(ch: &[char], i: usize, dates: bool) -> usize {
    let mut k = i + 1;
    while k < ch.len() {
        let c = ch[k];
        let joiner = c == '.' || (dates && matches!(c, '-' | ':' | '+'));
        let digit_next = ch.get(k + 1).is_some_and(|d| d.is_ascii_digit());
        if c.is_ascii_alphanumeric() || c == '_' || (joiner && digit_next) {
            k += 1;
        } else {
            break;
        }
    }
    k
}

/// A YAML line whose code (up to `upto`, before any comment) ends in `key: |`
/// or `- >-`: what follows, indented deeper, is one long string.
fn block_scalar(ch: &[char], upto: usize) -> bool {
    let end = ch[..upto].iter().rposition(|c| !c.is_whitespace()).map_or(0, |k| k + 1);
    let start = ch[..end].iter().rposition(|c| c.is_whitespace()).map_or(0, |k| k + 1);
    let tok = &ch[start..end];
    let header = matches!(tok.first(), Some('|' | '>')) && tok.len() <= 3 && tok[1..].iter().all(|c| matches!(c, '-' | '+' | '1'..='9'));
    let before = ch[..start].iter().rposition(|c| !c.is_whitespace());
    header && before.is_some_and(|k| matches!(ch[k], ':' | '-'))
}

/// What a line read up to `upto` leaves open for the next one.
fn leaves(ch: &[char], upto: usize, lang: Lang) -> Open {
    if lang == Lang::Yaml && block_scalar(ch, upto) { Open::Scalar(indent(ch)) } else { Open::No }
}

/// Read one line: `emit(start, end, token)` for each token, given what the
/// lines above left open. Returns what this line leaves open.
fn scan(ch: &[char], lang: Lang, open: Open, mut emit: impl FnMut(usize, usize, Tok)) -> Open {
    let spec = lang.spec();
    let n = ch.len();
    let mut i = 0;
    match open {
        Open::No => {}
        Open::Comment(closer) | Open::Str(closer) => {
            let tok = if matches!(open, Open::Comment(_)) { Tok::Comment } else { Tok::Str };
            match find_close(ch, 0, closer) {
                Some(end) => {
                    emit(0, end, tok);
                    i = end;
                }
                None => {
                    emit(0, n, tok);
                    return open;
                }
            }
        }
        Open::Scalar(depth) => {
            if ch.iter().all(|c| c.is_whitespace()) || indent(ch) > depth {
                emit(0, n, Tok::Str);
                return open;
            }
        }
    }

    let first = indent(ch);
    if spec.sections && i == 0 && let Some(end) = section_end(ch, first, spec) {
        emit(first, end, Tok::Section);
        i = end;
    }
    let config = spec.key_sep.is_some();
    while i < n {
        let c = ch[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if let Some((opener, closer)) = spec.block_comment.filter(|(o, _)| starts_with(ch, i, o)) {
            match find_close(ch, i + opener.len(), closer) {
                Some(end) => {
                    emit(i, end, Tok::Comment);
                    i = end;
                    continue;
                }
                None => {
                    emit(i, n, Tok::Comment);
                    return Open::Comment(closer);
                }
            }
        }
        // `#` starts a comment only after space: not in `$#`, `${#x}` or `a#b`.
        let comment = spec.line_comments.iter().any(|lc| starts_with(ch, i, lc) && (*lc != "#" || i == 0 || ch[i - 1].is_whitespace()));
        if comment {
            emit(i, n, Tok::Comment);
            return leaves(ch, i, lang);
        }
        if let Some((opener, closer)) = spec.block_strings.iter().find(|(o, _)| starts_with(ch, i, o)) {
            match find_close(ch, i + opener.len(), closer) {
                Some(end) => {
                    emit(i, end, Tok::Str);
                    i = end;
                    continue;
                }
                None => {
                    emit(i, n, Tok::Str);
                    return Open::Str(closer);
                }
            }
        }
        if spec.quotes.contains(&c) {
            let end = quoted_end(ch, i, c);
            let tok = if spec.key_sep.is_some_and(|sep| key_follows(ch, end, sep, true)) { Tok::Key } else { Tok::Str };
            emit(i, end, tok);
            i = end;
            continue;
        }
        if lang == Lang::C && c == '#' && i == first {
            // `#include`, `#define`: the preprocessor's words.
            let end = (i + 1..n).find(|&k| !ch[k].is_ascii_alphabetic()).unwrap_or(n);
            emit(i, end, Tok::Keyword);
            i = end;
            continue;
        }
        if number_start(ch, i) {
            let end = number_end(ch, i, spec.dates);
            emit(i, end, Tok::Number);
            i = end;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let mut end = i + 1;
            while end < n && (ch[end].is_alphanumeric() || ch[end] == '_' || (config && matches!(ch[end], '-' | '.'))) {
                end += 1;
            }
            if spec.key_sep.is_some_and(|sep| key_follows(ch, end, sep, false)) {
                emit(i, end, Tok::Key);
            } else if spec.keywords.iter().any(|k| k.len() == end - i && starts_with(ch, i, k)) {
                emit(i, end, Tok::Keyword);
            }
            i = end;
            continue;
        }
        i += 1;
    }
    leaves(ch, n, lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One letter per character: `c`omment, `k`eyword, `s`tring, `n`umber,
    /// `K`ey, `S`ection, `.` for anything else; spaces stay spaces.
    fn kinds_from(src: &str, lang: Lang, open: Open) -> (String, Open) {
        let ch: Vec<char> = src.chars().collect();
        let mut out: Vec<char> = ch.iter().map(|c| if c.is_whitespace() { ' ' } else { '.' }).collect();
        let left = scan(&ch, lang, open, |s, e, tok| {
            let mark = match tok {
                Tok::Comment => 'c',
                Tok::Keyword => 'k',
                Tok::Str => 's',
                Tok::Number => 'n',
                Tok::Key => 'K',
                Tok::Section => 'S',
            };
            out[s..e].fill(mark);
        });
        (out.into_iter().collect(), left)
    }

    fn kinds(src: &str, lang: Lang) -> String {
        kinds_from(src, lang, Open::No).0
    }

    fn lines(src: &[&str]) -> Vec<Vec<char>> {
        src.iter().map(|s| s.chars().collect()).collect()
    }

    #[test]
    fn toml() {
        assert_eq!(kinds("name = \"omanote\"  # the app", Lang::Toml), "KKKK . sssssssss  ccccccccc");
        assert_eq!(kinds("[server.http]", Lang::Toml), "SSSSSSSSSSSSS");
        assert_eq!(kinds("[[bin]]  # two", Lang::Toml), "SSSSSSS  ccccc");
        assert_eq!(kinds("port = 8080", Lang::Toml), "KKKK . nnnn");
        assert_eq!(kinds("on = true", Lang::Toml), "KK . kkkk");
        assert_eq!(kinds("born = 1979-05-27T07:32:00Z", Lang::Toml), "KKKK . nnnnnnnnnnnnnnnnnnnn");
        assert_eq!(kinds("x = { a = 1, b = \"s\" }", Lang::Toml), "K . . K . n. K . sss .");
        assert_eq!(kinds("ports = [80, -1]", Lang::Toml), "KKKKK . .nn. nn.");

        let blocks = blocks(&lines(&["a = \"\"\"", "still", "\"\"\" # done", "b = 1"]), Some(Lang::Toml));
        let opens: Vec<Open> = blocks.iter().map(|b| match b {
            Block::Source { lang: Some(Lang::Toml), open } => *open,
            other => panic!("{other:?}"),
        }).collect();
        assert_eq!(opens, [Open::No, Open::Str("\"\"\""), Open::Str("\"\"\""), Open::No]);
        assert_eq!(kinds_from("\"\"\" # done", Lang::Toml, Open::Str("\"\"\"")), ("sss cccccc".into(), Open::No));
    }

    #[test]
    fn yaml() {
        assert_eq!(kinds("- name: Build  # step", Lang::Yaml), ". KKKK. .....  cccccc");
        assert_eq!(kinds("url: http://x.y", Lang::Yaml), "KKK. ..........", "a scheme is not a key");
        assert_eq!(kinds("enabled: yes", Lang::Yaml), "KKKKKKK. kkk");
        assert_eq!(kinds("\"quoted key\": 'v'", Lang::Yaml), "KKKKKKKKKKKK. sss");
        assert_eq!(kinds("when: 2024-01-05 12:30", Lang::Yaml), "KKKK. nnnnnnnnnn nnnnn");

        // `key: |` and everything indented under it is one string.
        let src = ["script: |", "  echo hi", "", "  echo bye", "next: 1"];
        let opens: Vec<Open> = blocks(&lines(&src), Some(Lang::Yaml)).iter().map(|b| match b {
            Block::Source { open, .. } => *open,
            other => panic!("{other:?}"),
        }).collect();
        assert_eq!(opens, [Open::No, Open::Scalar(0), Open::Scalar(0), Open::Scalar(0), Open::Scalar(0)]);
        assert_eq!(kinds_from("  echo hi", Lang::Yaml, Open::Scalar(0)), ("sssssssss".into(), Open::Scalar(0)));
        assert_eq!(kinds_from("next: 1", Lang::Yaml, Open::Scalar(0)), ("KKKK. n".into(), Open::No), "dedenting ends it");
        assert_eq!(kinds_from("      - run: >-  # folded", Lang::Yaml, Open::No).1, Open::Scalar(6));
        assert_eq!(kinds_from("a: b | c", Lang::Yaml, Open::No).1, Open::No, "a bar mid-value is not a block scalar");
    }

    #[test]
    fn json() {
        assert_eq!(kinds("{\"a\": 1, \"b\": [true, null]}", Lang::Json), ".KKK. n. KKK. .kkkk. kkkk..");
        assert_eq!(kinds("{\"a\":-1.5e3}", Lang::Json), ".KKK.nnnnnn.");
        assert_eq!(kinds("// note", Lang::Json), "ccccccc");
    }

    #[test]
    fn ini() {
        assert_eq!(kinds("[Desktop Entry]", Lang::Ini), "SSSSSSSSSSSSSSS");
        assert_eq!(kinds("Exec=omanote %F ; run", Lang::Ini), "KKKK........ .. ccccc");
        assert_eq!(kinds("$mainMod = SUPER", Lang::Ini), ".KKKKKKK . .....", "Hyprland variables");
        assert_eq!(kinds("Terminal=false", Lang::Ini), "KKKKKKKK.kkkkk");
    }

    #[test]
    fn lua() {
        assert_eq!(kinds("local x = \"s\" -- note", Lang::Lua), "kkkkk . . sss ccccccc");
        assert_eq!(kinds("s = [[long]]", Lang::Lua), ". . ssssssss");
        assert_eq!(kinds("if a ~= nil then return end", Lang::Lua), "kk . .. kkk kkkk kkkkkk kkk");
        assert_eq!(kinds_from("--[[ first", Lang::Lua, Open::No), ("cccccccccc".into(), Open::Comment("]]")));
        assert_eq!(kinds_from("second ]] local y = 1", Lang::Lua, Open::Comment("]]")), ("ccccccccc kkkkk . . n".into(), Open::No));
    }

    #[test]
    fn shell() {
        assert_eq!(kinds("echo \"$HOME\" # hi", Lang::Shell), ".... sssssss cccc");
        assert_eq!(kinds("if [ $# -gt 0 ]; then", Lang::Shell), "kk . .. ... n .. kkkk", "$# is not a comment");
        assert_eq!(kinds("#!/bin/bash", Lang::Shell), "ccccccccccc");
        assert_eq!(kinds("n=${#arr[@]}", Lang::Shell), "............");
    }

    #[test]
    fn code() {
        assert_eq!(kinds("let s: &'a str = \"x\"; // c", Lang::Rust), "kkk .. ... ... . sss. cccc", "a lifetime is not a string");
        assert_eq!(kinds_from("/* a", Lang::Rust, Open::No), ("cccc".into(), Open::Comment("*/")));
        assert_eq!(kinds_from("b */ fn f() {}", Lang::Rust, Open::Comment("*/")), ("cccc kk ... ..".into(), Open::No));
        assert_eq!(kinds("def f():  # doc", Lang::Python), "kkk ....  ccccc");
        assert_eq!(kinds_from("x = \"\"\"a", Lang::Python, Open::No), (". . ssss".into(), Open::Str("\"\"\"")));
        assert_eq!(kinds_from("s := `raw", Lang::Go, Open::No), (". .. ssss".into(), Open::Str("`")));
        assert_eq!(kinds("const n = 0xFF; // hex", Lang::Js), "kkkkk . . nnnn. cccccc");
        assert_eq!(kinds("#include <stdio.h>", Lang::C), "kkkkkkkk .........");
        assert_eq!(kinds("int x = 'c';", Lang::C), "kkk . . sss.");
    }

    #[test]
    fn knows_files_by_extension_or_name() {
        let of = |name: &str| Lang::of_path(Some(Path::new(name)));
        assert_eq!(of("a/config.TOML"), Some(Lang::Toml));
        assert_eq!(of("ci.yml"), Some(Lang::Yaml));
        assert_eq!(of("hypr/hyprland.conf"), Some(Lang::Ini));
        assert_eq!(of("omanote.desktop"), Some(Lang::Ini));
        assert_eq!(of("init.lua"), Some(Lang::Lua));
        assert_eq!(of(".bashrc"), Some(Lang::Shell));
        assert_eq!(of("Makefile"), Some(Lang::Shell));
        assert_eq!(of(".env"), Some(Lang::Ini));
        assert_eq!(of("x.env"), Some(Lang::Ini));
        assert_eq!(of("notes.txt"), None);
        assert_eq!(of("LICENSE"), None);
        assert_eq!(Lang::of_path(None), None);
        assert_eq!(Lang::named("TypeScript"), Some(Lang::Js));
        assert_eq!(Lang::named("text"), None);
        assert_eq!(blocks(&lines(&["a", "b"]), None), [Block::Source { lang: None, open: Open::No }; 2]);
    }

    #[test]
    fn paints_over_the_base_style() {
        let ch: Vec<char> = "local x -- c".chars().collect();
        let base = Style::new().fg(ratatui::style::Color::Green);
        let mut cells = vec![CharCell::default(); ch.len()];
        for cell in &mut cells {
            cell.style = base;
        }
        paint(&ch, Lang::Lua, Open::No, base, &mut cells);
        assert_eq!(cells[0].style, base.patch(look::of(El::Keyword)));
        assert_eq!(cells[6].style, base, "an identifier keeps the block's own colour");
        assert_eq!(cells[8].style, base.patch(look::of(El::Comment)));
        assert!(cells[8].style.add_modifier.contains(Modifier::ITALIC));
    }
}
