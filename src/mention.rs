//! `@` in a note: suggests notes to link to, writes the link, and knows where
//! a link leads when it is followed.
//!
//! Links are ordinary markdown, `[name](relative/path.md)`, so they also work
//! on GitHub and in every other markdown tool. A link whose file has moved is
//! still followed: the note is looked up by name in the vaults.

use std::path::{Component, Path, PathBuf};

use crate::editor::Editor;
use crate::markdown::{Block, LinkTo};
use crate::picker::{Picker, Row};
use crate::vaults::Vault;

const MAX_QUERY: usize = 60;
const NOTE_TYPES: [&str; 3] = ["md", "markdown", "txt"];

/// The suggestions open under an `@`. What is searched for is not kept here:
/// it is whatever stands between the `@` and the cursor, so typing, deleting
/// and undo all just work.
pub struct Mention {
    pub row: usize,
    /// Column of the `@`.
    pub at: usize,
    /// Typed inside the brackets of `[text](…)`: the text is there already,
    /// so only the path is wanted.
    pub bare: bool,
    pub picker: Picker,
}

/// What Enter on a suggestion should do.
pub struct Accepted {
    pub link: String,
    /// A note to create first (the "create" row).
    pub create: Option<PathBuf>,
}

impl Mention {
    /// Call right after an `@` was typed. It only counts at the start of a
    /// word, so an e-mail address does not open anything.
    pub fn begin(ed: &Editor, vaults: &[Vault]) -> Option<Self> {
        let (row, col) = (ed.cursor.row, ed.cursor.col);
        let line = &ed.lines[row];
        let at = col.checked_sub(1).filter(|&a| line.get(a) == Some(&'@'))?;
        let writable = ed.markdown && matches!(ed.blocks.get(row)?, Block::Normal | Block::Table { .. });
        let word_start = at == 0 || line[at - 1].is_whitespace() || "([{|>\"'".contains(line[at - 1]);
        if !writable || !word_start {
            return None;
        }
        let folder = folder(ed.path.as_deref(), vaults);
        let mut picker = Picker::open_with(vaults, Some(&folder));
        if let Some(own) = &ed.path {
            picker.exclude(&absolute(own));
        }
        let bare = at >= 2 && line[at - 1] == '(' && line[at - 2] == ']';
        Some(Mention { row, at, bare, picker })
    }

    /// Re-read what was typed after the `@`. False when the mention is over:
    /// the cursor left, the `@` is gone, or this is plainly not a note's name.
    pub fn update(&mut self, ed: &Editor) -> bool {
        let line = &ed.lines[ed.cursor.row.min(ed.lines.len() - 1)];
        let alive = ed.cursor.row == self.row && ed.cursor.col > self.at && line.get(self.at) == Some(&'@') && ed.selection().is_none();
        if !alive {
            return false;
        }
        let query: String = line[self.at + 1..ed.cursor.col].iter().collect();
        if query.starts_with(char::is_whitespace) || query.chars().count() > MAX_QUERY {
            return false;
        }
        self.picker.set_query(&query);
        // "see you @home tomorrow": once words follow and nothing matches, let go.
        !(query.contains(char::is_whitespace) && self.picker.matches() == 0)
    }

    /// The link for the selected row, to put in place of `@query`.
    pub fn accept(&self, note: Option<&Path>, vaults: &[Vault]) -> Result<Accepted, String> {
        let folder = folder(note, vaults);
        let link = |to: &Path| if self.bare { target(&folder, to) } else { link(&folder, to) };
        match self.picker.row(self.picker.selected).ok_or("Nothing to link to")? {
            // `@>words` finds the note by what it says; the link is to the note all the same.
            Row::Note(found, _) | Row::Line(found, _) => Ok(Accepted { link: link(&found.path), create: None }),
            Row::Create(name) => {
                let inside = Path::new(&name).components().all(|c| matches!(c, Component::Normal(_)));
                if !inside {
                    return Err("The name cannot contain ..".into());
                }
                let path = folder.join(format!("{name}.md"));
                Ok(Accepted { link: link(&path), create: Some(path) })
            }
        }
    }
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The folder a note's links are relative to. A note that has no file yet is
/// most likely headed for the default vault.
pub fn folder(note: Option<&Path>, vaults: &[Vault]) -> PathBuf {
    let parent = note.and_then(|p| absolute(p).parent().map(Path::to_path_buf));
    parent.or_else(|| vaults.first().map(|v| v.path.clone())).or_else(|| std::env::current_dir().ok()).unwrap_or_else(|| PathBuf::from("."))
}

fn relative(from: &Path, to: &Path) -> PathBuf {
    let (from, to) = (normal(&absolute(from)), normal(&absolute(to)));
    let a: Vec<Component> = from.components().collect();
    let b: Vec<Component> = to.components().collect();
    let shared = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in shared..a.len() {
        out.push("..");
    }
    for part in &b[shared..] {
        out.push(part);
    }
    out
}

/// `a/b/../c` → `a/c`, without touching the disk.
fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            part => out.push(part),
        }
    }
    out
}

/// Spaces, brackets and the like would end a markdown link early.
fn encode(path: &str) -> String {
    let mut out = String::new();
    for c in path.chars() {
        if c.is_alphanumeric() || "-_.~/".contains(c) || !c.is_ascii() {
            out.push(c);
        } else {
            let mut bytes = [0; 4];
            for b in c.encode_utf8(&mut bytes).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%').then(|| text.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `../travel/trip%20plan.md`: what goes between the brackets, as seen from `folder`.
pub fn target(folder: &Path, note: &Path) -> String {
    encode(&relative(folder, note).to_string_lossy())
}

/// `[trip plan](../travel/trip%20plan.md)`.
pub fn link(folder: &Path, note: &Path) -> String {
    let text = note.file_stem().unwrap_or_default().to_string_lossy().replace('[', "\\[").replace(']', "\\]");
    format!("[{text}]({})", target(folder, note))
}

const MAX_LABEL: usize = 48;

/// The clipboard holds a web address and nothing else.
pub fn as_url(text: &str) -> Option<&str> {
    let text = text.trim();
    let rest = text.strip_prefix("https://").or_else(|| text.strip_prefix("http://"))?;
    (rest.len() > 3 && rest.contains('.') && !text.contains(char::is_whitespace) && !text.contains(['<', '>', '"'])).then_some(text)
}

/// A bracket in the address would end the link early.
pub fn url_target(url: &str) -> String {
    url.replace('(', "%28").replace(')', "%29")
}

/// What a pasted address is shown as: the site and the page, without the
/// noise around them. `https://www.github.com/iluxav/omanote/?tab=readme#use`
/// reads `github.com/iluxav/omanote`.
pub fn url_label(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest).trim_end_matches('/');
    let label = match rest.char_indices().nth(MAX_LABEL) {
        Some((cut, _)) => format!("{}…", &rest[..cut]),
        None => rest.to_string(),
    };
    decode(&label).replace('[', "(").replace(']', ")")
}

/// A pasted address as markdown. One that is a picture, alone on its line, is
/// shown as the picture.
pub fn url_link(url: &str, alone_on_line: bool) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_lowercase();
    let picture = ["png", "jpg", "jpeg", "gif", "webp"].iter().any(|e| path.ends_with(&format!(".{e}")));
    if picture && alone_on_line {
        return format!("![]({})", url_target(url));
    }
    format!("[{}]({})", url_label(url), url_target(url))
}

/// Where a link leads.
#[derive(Debug, PartialEq, Eq)]
pub enum Dest {
    /// Hand it to the desktop: a web page, a mail address.
    Web(String),
    /// A note to open here. It may not exist yet.
    Note(PathBuf),
    /// Some other file (a picture, a PDF), for the desktop to open.
    File(PathBuf),
}

fn is_web(target: &str) -> bool {
    let scheme = target.split_once(':').map_or("", |(s, _)| s);
    let named = scheme.len() > 1 && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
    named && (target[scheme.len()..].starts_with("://") || ["mailto", "tel"].contains(&scheme))
}

fn is_note(path: &Path) -> bool {
    path.extension().is_none_or(|e| NOTE_TYPES.iter().any(|t| e.eq_ignore_ascii_case(t)))
}

/// Of several notes with the same name, the one nearest to `folder`.
fn nearest<'a>(found: &[&'a Path], folder: &Path) -> Option<&'a Path> {
    let shared = |p: &&Path| p.components().zip(folder.components()).take_while(|(a, b)| a == b).count();
    found.iter().copied().max_by_key(|p| shared(p))
}

/// Work out what following `link` from `note` means.
pub fn destination(link: &LinkTo, note: Option<&Path>, vaults: &[Vault]) -> Result<Dest, String> {
    let folder = folder(note, vaults);
    let lookup = |name: &str| {
        let picker = Picker::open_with(vaults, Some(&folder));
        nearest(&picker.named(name), &folder).map(Path::to_path_buf)
    };
    match link {
        LinkTo::Label(label) => Err(format!("[{label}] is not defined in this note")),
        LinkTo::Wiki(inner) => {
            // [[note#heading|shown text]]
            let name = inner.split(['|', '#']).next().unwrap_or("").trim();
            if name.is_empty() {
                return Err("That link stays inside this note".into());
            }
            if !Path::new(name).components().all(|c| matches!(c, Component::Normal(_))) {
                return Err("The name cannot contain ..".into());
            }
            Ok(Dest::Note(lookup(name).unwrap_or_else(|| folder.join(format!("{}.md", name.trim_end_matches(".md"))))))
        }
        LinkTo::Target(target) => {
            // `<path with spaces>` and `path "a title"` are both allowed.
            let target = target.trim();
            let target = match target.strip_prefix('<').and_then(|t| t.split_once('>')) {
                Some((inside, _)) => inside,
                None => target.split_whitespace().next().unwrap_or(""),
            };
            if is_web(target) {
                return Ok(Dest::Web(target.to_string()));
            }
            let file = decode(target.split('#').next().unwrap_or(""));
            if file.is_empty() {
                return Err("That link stays inside this note".into());
            }
            let file = match file.strip_prefix("~/") {
                Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
                None => PathBuf::from(file),
            };
            let path = normal(&folder.join(file));
            if path.is_dir() {
                return Ok(Dest::File(path));
            }
            if path.exists() {
                return Ok(if is_note(&path) { Dest::Note(path) } else { Dest::File(path) });
            }
            if !is_note(&path) {
                return Err(format!("{} is not there", path.display()));
            }
            // Moved or renamed since the link was written? Find it by name.
            let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let path = if path.extension().is_none() { path.with_extension("md") } else { path };
            Ok(Dest::Note(lookup(&stem).unwrap_or(path)))
        }
    }
}

/// Open something with the desktop's default program, without waiting for it.
pub fn open_outside(what: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let run = Command::new(opener).arg(what).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    run.map(drop).map_err(|e| format!("Could not run {opener}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Pos;

    fn vault(tag: &str) -> (PathBuf, Vec<Vault>) {
        let root = std::env::temp_dir().join(format!("omanote-mention-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (file, text) in [("docs/test.md", "# test"), ("docs/testing plan.md", ""), ("docs/trips/japan.md", ""), ("docs/pic.png", ""), ("work/roadmap.md", "")] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let vaults = vec![Vault { path: root.join("docs"), github: None }, Vault { path: root.join("work"), github: None }];
        (root, vaults)
    }

    fn typed(text: &str, path: Option<PathBuf>) -> Editor {
        let mut ed = Editor::new(text, path);
        let row = ed.lines.len() - 1;
        ed.cursor = Pos { row, col: ed.lines[row].len() };
        ed
    }

    fn rows(m: &Mention) -> Vec<String> {
        (0..m.picker.len())
            .map(|i| match m.picker.row(i).unwrap() {
                Row::Note(n, _) | Row::Line(n, _) => n.name.iter().collect(),
                Row::Create(name) => format!("create {name}.md"),
            })
            .collect()
    }

    #[test]
    fn suggests_notes_and_offers_to_create_last() {
        let (root, vaults) = vault("suggest");
        let mut ed = typed("see @", Some(root.join("docs/today.md")));
        let mut m = Mention::begin(&ed, &vaults).expect("an @ at the start of a word");
        assert_eq!(rows(&m).len(), 4, "every note, before anything is typed");
        for c in "tes".chars() {
            ed.insert_char(c);
            assert!(m.update(&ed));
        }
        assert_eq!(rows(&m), ["test", "testing plan", "create tes.md"]);
        ed.insert_char('t');
        assert!(m.update(&ed));
        assert_eq!(rows(&m), ["test", "testing plan"], "a note is called exactly that: nothing to create");

        let got = m.accept(ed.path.as_deref(), &vaults).unwrap();
        assert_eq!((got.link.as_str(), got.create), ("[test](test.md)", None));
        ed.replace_on_line(m.at, ed.cursor.col, &got.link);
        assert_eq!(ed.text(), "see [test](test.md)\n");
        assert!(ed.undo());
        assert_eq!(ed.text(), "see @test\n", "one undo brings the mention back");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_create_row_makes_a_note_beside_this_one() {
        let (root, vaults) = vault("create");
        let mut ed = typed("@brand new", Some(root.join("docs/trips/japan.md")));
        ed.cursor = Pos { row: 0, col: 1 };
        let mut m = Mention::begin(&ed, &vaults).unwrap();
        ed.cursor = Pos { row: 0, col: 6 };
        assert!(m.update(&ed));
        assert_eq!(rows(&m), ["create brand.md"]);
        let got = m.accept(ed.path.as_deref(), &vaults).unwrap();
        assert_eq!(got.link, "[brand](brand.md)");
        assert_eq!(got.create, Some(root.join("docs/trips/brand.md")));
        // Words after a name that matches nothing: this was never a mention.
        ed.cursor = Pos { row: 0, col: 10 };
        assert!(!m.update(&ed));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn only_a_word_start_in_markdown_text_counts() {
        let (root, vaults) = vault("start");
        assert!(Mention::begin(&typed("mail me@", None), &vaults).is_none(), "an address");
        assert!(Mention::begin(&typed("(@", None), &vaults).is_some());
        assert!(Mention::begin(&typed("```\n@", None), &vaults).is_none(), "code");
        assert!(Mention::begin(&typed("@", Some(root.join("docs/settings.toml"))), &vaults).is_none(), "plain text");

        let mut ed = typed("@", None);
        let mut m = Mention::begin(&ed, &vaults).unwrap();
        ed.insert_char(' ');
        assert!(!m.update(&ed), "a lone @");
        let mut ed = typed("@te", None);
        ed.cursor.col = 1;
        let mut m = Mention::begin(&ed, &vaults).unwrap();
        ed.backspace();
        assert!(!m.update(&ed), "the @ was deleted");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_mention_inside_a_link_fills_in_the_path_only() {
        let (root, vaults) = vault("bare");
        let mut ed = typed("read [the plan](@", Some(root.join("docs/today.md")));
        let mut m = Mention::begin(&ed, &vaults).unwrap();
        assert!(m.bare);
        for c in "testing".chars() {
            ed.insert_char(c);
            assert!(m.update(&ed));
        }
        assert_eq!(m.accept(ed.path.as_deref(), &vaults).unwrap().link, "testing%20plan.md");
        assert!(!Mention::begin(&typed("(@", None), &vaults).unwrap().bare, "just a bracket");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_pasted_address_becomes_a_readable_link() {
        assert_eq!(as_url("  https://omarchy.org/docs \n"), Some("https://omarchy.org/docs"));
        for not in ["omarchy.org", "https://", "see https://omarchy.org", "https://omarchy.org and more", "ftp://x.org/a", "https://localhost"] {
            assert_eq!(as_url(not), None, "{not}");
        }
        assert_eq!(url_label("https://www.github.com/iluxav/omanote/?tab=readme#use"), "github.com/iluxav/omanote");
        assert_eq!(url_label("http://example.com/"), "example.com");
        assert_eq!(url_label("https://en.wikipedia.org/wiki/Ry%C5%8Dkan_(inn)"), "en.wikipedia.org/wiki/Ryōkan_(inn)");
        assert_eq!(url_label(&format!("https://x.org/{}", "a".repeat(80))).chars().count(), MAX_LABEL + 1);

        assert_eq!(url_link("https://omarchy.org/docs", false), "[omarchy.org/docs](https://omarchy.org/docs)");
        assert_eq!(url_link("https://en.wikipedia.org/wiki/Ryokan_(inn)", true), "[en.wikipedia.org/wiki/Ryokan_(inn)](https://en.wikipedia.org/wiki/Ryokan_%28inn%29)");
        assert_eq!(url_link("https://x.org/pics/Cat.JPG?w=200", true), "![](https://x.org/pics/Cat.JPG?w=200)");
        assert_eq!(url_link("https://x.org/pics/cat.jpg", false), "[x.org/pics/cat.jpg](https://x.org/pics/cat.jpg)", "mid-sentence it stays a link");
    }

    #[test]
    fn links_are_relative_and_survive_odd_names() {
        let docs = Path::new("/v/docs");
        assert_eq!(link(docs, Path::new("/v/docs/trips/japan.md")), "[japan](trips/japan.md)");
        assert_eq!(link(&docs.join("trips"), Path::new("/v/docs/test.md")), "[test](../test.md)");
        assert_eq!(link(&docs.join("trips"), Path::new("/v/work/road map (v2).md")), "[road map (v2)](../../work/road%20map%20%28v2%29.md)");
        assert_eq!(link(docs, Path::new("/v/docs/ünï.md")), "[ünï](ünï.md)");
        assert_eq!(decode("road%20map%20%28v2%29.md"), "road map (v2).md");
        assert_eq!(decode("100%.md"), "100%.md");
    }

    #[test]
    fn follows_links_to_notes_files_and_the_web() {
        let (root, vaults) = vault("follow");
        let note = root.join("docs/trips/japan.md");
        let go = |link: LinkTo| destination(&link, Some(&note), &vaults);
        let target = |t: &str| LinkTo::Target(t.to_string());

        assert_eq!(go(target("https://omarchy.org/x?y#z")), Ok(Dest::Web("https://omarchy.org/x?y#z".into())));
        assert_eq!(go(target("mailto:me@example.com")), Ok(Dest::Web("mailto:me@example.com".into())));
        assert_eq!(go(target("../test.md#intro")), Ok(Dest::Note(root.join("docs/test.md"))));
        assert_eq!(go(target("../testing%20plan.md \"a title\"")), Ok(Dest::Note(root.join("docs/testing plan.md"))));
        assert_eq!(go(target("<../testing plan.md>")), Ok(Dest::Note(root.join("docs/testing plan.md"))));
        assert_eq!(go(target("../pic.png")), Ok(Dest::File(root.join("docs/pic.png"))));
        assert!(go(target("gone.png")).is_err());
        assert!(go(target("#just-a-heading")).is_err());

        // The file moved: found again by its name, in whichever vault.
        assert_eq!(go(target("old/place/roadmap.md")), Ok(Dest::Note(root.join("work/roadmap.md"))));
        // Nothing by that name anywhere: a new note, where the link says.
        assert_eq!(go(target("ideas.md")), Ok(Dest::Note(root.join("docs/trips/ideas.md"))));
        assert_eq!(go(target("ideas")), Ok(Dest::Note(root.join("docs/trips/ideas.md"))));

        assert_eq!(go(LinkTo::Wiki("Testing Plan|the plan".into())), Ok(Dest::Note(root.join("docs/testing plan.md"))));
        assert_eq!(go(LinkTo::Wiki("trips/japan#food".into())), Ok(Dest::Note(note.clone())));
        assert_eq!(go(LinkTo::Wiki("not yet".into())), Ok(Dest::Note(root.join("docs/trips/not yet.md"))));
        assert!(go(LinkTo::Wiki("../../escape".into())).is_err());
        assert!(go(LinkTo::Label("nowhere".into())).is_err());

        // A note with no file yet links from the default vault.
        assert_eq!(destination(&target("test.md"), None, &vaults), Ok(Dest::Note(root.join("docs/test.md"))));
        let _ = std::fs::remove_dir_all(root);
    }
}
