//! Pins: a project's docs, readable from a vault.
//!
//! `omanote --pin README.md --as my-project`, run inside a project, puts
//! `<vault>/my-project.md` in a vault as a symlink to the project's file and
//! lists it in the vault's `index.md`. The project file stays the only copy:
//! saving from the vault writes through the link, so agents working in the
//! project and you reading in the vault see the same text.
//!
//! The link only means something on this machine, so it goes in the vault's
//! `.gitignore` before it is made: a GitHub vault's sync stages everything
//! with `git add -A`, and would otherwise push a path that exists nowhere else.
//!
//! Pins only go one way. Linking vault notes into projects breaks in clones,
//! CI and containers, and would have agents writing project details into a
//! shared note.

use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};

use crate::vaults::{Vault, tilde};

const INDEX: &str = "index.md";
const IGNORE_HEADER: &str = "# Pinned from other projects by `omanote --pin`: these links only work on this machine.";
/// How deep `--pins` looks inside a vault, as deep as Ctrl+P does.
const MAX_DEPTH: usize = 8;

/// A pin found in a vault: where the link is, and what it points to.
#[derive(Debug, PartialEq, Eq)]
pub struct Pin {
    pub link: PathBuf,
    pub target: PathBuf,
    pub broken: bool,
}

// ---- commands ------------------------------------------------------------

/// `--pin <file> [--as <name>]`
pub fn pin(vaults: &[Vault], file: &str, name: Option<&str>) -> Result<String, String> {
    let source = std::path::absolute(expand(file)).map_err(|e| format!("cannot find {file}: {e}"))?;
    if !source.is_file() {
        return Err(match source.exists() {
            true => format!("{file} is a folder — pin a file in it, like {file}/README.md"),
            false => format!("{file} does not exist"),
        });
    }
    let source = source.canonicalize().map_err(|e| format!("cannot read {file}: {e}"))?;
    let name = match name {
        Some(given) => clean_name(given)?,
        None => default_name(&source),
    };
    let question = format!("Pin {} as {name}.md in which vault?", tilde(&source));
    let vault = choose(vaults, &question)?;
    pin_into(&vault, &source, &name)
}

/// `--unpin <name>`
pub fn unpin(vaults: &[Vault], name: &str) -> Result<String, String> {
    let name = clean_name(name)?;
    let rel = PathBuf::from(format!("{name}.md"));
    let is_link = |v: &&Vault| v.path.join(&rel).symlink_metadata().is_ok_and(|m| m.file_type().is_symlink());
    let holding: Vec<Vault> = vaults.iter().filter(is_link).cloned().collect();
    if holding.is_empty() {
        if let Some(v) = vaults.iter().find(|v| v.path.join(&rel).exists()) {
            return Err(format!("{}/{name}.md is a note, not a pin — omanote does not delete notes", v.name()));
        }
        return Err(format!("nothing is pinned as {name} — see omanote --pins"));
    }
    let vault = choose(&holding, &format!("{name} is in more than one vault. Unpin it from which?"))?;
    unpin_from(&vault, &name)
}

/// `--pins`
pub fn list(vaults: &[Vault]) -> String {
    let found: Vec<(String, Pin)> = vaults.iter().flat_map(|v| pins_in(v).into_iter().map(move |p| (shown(v, &p.link), p))).collect();
    if found.is_empty() {
        return "No pins. Inside a project: omanote --pin README.md --as my-project".to_string();
    }
    let width = found.iter().map(|(name, _)| name.chars().count()).max().unwrap_or(0);
    let mut out = String::from("Pins — project files linked into vaults (omanote --unpin <name> removes one):\n");
    for (name, pin) in &found {
        let broken = if pin.broken { "   broken: the file is gone (moved or deleted?)" } else { "" };
        out.push_str(&format!("  {name:<width$}  → {}{broken}\n", tilde(&pin.target)));
    }
    let broken = found.iter().filter(|(_, p)| p.broken).count();
    if broken > 0 {
        out.push_str(&format!("\n{broken} broken. Pin the file again from where it is now, or unpin it.\n"));
    }
    out
}

// ---- the work ------------------------------------------------------------

/// Link `source` into `vault` as `<name>.md`, keep it out of git, and list it
/// in the index. Pinning the same file again repairs whatever is missing; a
/// link whose file is gone is replaced, which is how a moved project is re-pinned.
fn pin_into(vault: &Vault, source: &Path, name: &str) -> Result<String, String> {
    let root = vault.path.canonicalize().map_err(|_| format!("the vault {} does not exist", tilde(&vault.path)))?;
    if source.starts_with(&root) {
        return Err(format!("{} is already in the vault {} — no need to pin it", tilde(source), vault.name()));
    }
    let rel = PathBuf::from(format!("{name}.md"));
    let link = root.join(&rel);
    let mut replaced = None;
    if let Ok(meta) = link.symlink_metadata() {
        if !meta.file_type().is_symlink() {
            return Err(format!("{} already has a note called {name}.md — pick another name with --as", vault.name()));
        }
        let old = std::fs::read_link(&link).map(|t| resolve(&link, &t)).unwrap_or_default();
        let same = old.canonicalize().is_ok_and(|o| o == source);
        if !same && old.exists() {
            return Err(format!("{name} is already pinned to {} — pick another name with --as, or omanote --unpin {name} first", tilde(&old)));
        }
        if !same {
            replaced = Some(old);
        }
    }

    // Ignored before it exists: a sync running meanwhile never sees it untracked.
    let ignore = root.join(".gitignore");
    let ignored_before = edit_lines(&ignore, |lines| add_ignore(lines, &ignore_pattern(&rel)))?;
    let made = (|| {
        if let Some(dir) = link.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if link.symlink_metadata().is_ok() {
            std::fs::remove_file(&link)?;
        }
        std::os::unix::fs::symlink(source, &link)
    })();
    if let Err(e) = made {
        if !ignored_before {
            let _ = edit_lines(&ignore, |lines| remove_ignore(lines, &ignore_pattern(&rel)));
        }
        return Err(format!("cannot make the link {}: {e}", tilde(&link)));
    }
    let index = root.join(INDEX);
    let entry = format!("- {}", crate::mention::link(&root, &link));
    let target = crate::mention::target(&root, &link);
    edit_lines(&index, |lines| add_to_index(lines, &entry, &target))?;

    let place = format!("{}/{name}.md", vault.name());
    Ok(match replaced {
        Some(old) => format!("Re-pinned {place} → {} (it pointed to {}, which is gone)", tilde(source), tilde(&old)),
        None => format!("Pinned {place} → {}\nListed in {}/{INDEX}; edits from the vault change the project's file.", tilde(source), vault.name()),
    })
}

/// Take the pin out of the vault: the link, its line in `.gitignore` and its
/// entry in the index. The project's file is never touched, and a real note
/// by that name is refused.
fn unpin_from(vault: &Vault, name: &str) -> Result<String, String> {
    let rel = PathBuf::from(format!("{name}.md"));
    let link = vault.path.join(&rel);
    let meta = link.symlink_metadata().map_err(|_| format!("nothing is pinned as {name} in {}", vault.name()))?;
    if !meta.file_type().is_symlink() {
        return Err(format!("{}/{name}.md is a note, not a pin — omanote does not delete notes", vault.name()));
    }
    let target = std::fs::read_link(&link).map(|t| resolve(&link, &t)).unwrap_or_default();
    std::fs::remove_file(&link).map_err(|e| format!("cannot remove {}: {e}", tilde(&link)))?;
    // Folders made for a name like `projects/omanote` go when they are empty.
    let mut dir = link.parent();
    while let Some(d) = dir.filter(|d| *d != vault.path && d.starts_with(&vault.path)) {
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
    let ignore = vault.path.join(".gitignore");
    if ignore.exists() {
        edit_lines(&ignore, |lines| remove_ignore(lines, &ignore_pattern(&rel)))?;
    }
    let index = vault.path.join(INDEX);
    if index.exists() {
        let entry = crate::mention::target(&vault.path, &link);
        edit_lines(&index, |lines| remove_from_index(lines, &entry))?;
    }
    Ok(format!("Unpinned {}/{name}.md ({} is untouched)", vault.name(), tilde(&target)))
}

/// Every link to a note in the vault, however it was made.
fn pins_in(vault: &Vault) -> Vec<Pin> {
    let mut out = Vec::new();
    walk(&vault.path, 0, &mut out);
    out.sort_by(|a, b| a.link.cmp(&b.link));
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<Pin>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if kind.is_dir() && depth < MAX_DEPTH {
            walk(&path, depth + 1, out);
        } else if kind.is_symlink() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown")) {
            let Ok(target) = std::fs::read_link(&path) else { continue };
            let target = resolve(&path, &target);
            let broken = !target.is_file();
            out.push(Pin { link: path, target, broken });
        }
    }
}

// ---- names -----------------------------------------------------------------

/// What `--as` may be: a name, or a path inside the vault (`projects/omanote`),
/// without `.md`. Nothing that climbs out of the vault or hides from Ctrl+P.
fn clean_name(given: &str) -> Result<String, String> {
    let trimmed = given.trim().trim_end_matches('/');
    let name = trimmed.strip_suffix(".md").unwrap_or(trimmed);
    let path = Path::new(name);
    let fine = !name.is_empty()
        && path.components().all(|c| matches!(c, Component::Normal(part) if !part.to_string_lossy().starts_with('.')));
    fine.then(|| name.to_string()).ok_or_else(|| format!("\"{given}\" cannot be a pin's name — use a plain name like my-project, or a folder in the vault like projects/my-project"))
}

/// The name when `--as` is left out: the project's folder for its README,
/// else the folder and the file (`omanote-agents`). The project folder is the
/// top of its git repository, when there is one.
fn default_name(source: &Path) -> String {
    let folder = source.parent().unwrap_or(Path::new("/"));
    let project = folder.ancestors().find(|d| d.join(".git").exists()).unwrap_or(folder);
    let project = project.file_name().map_or_else(|| "project".to_string(), |n| n.to_string_lossy().into_owned());
    let stem = source.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
    let project = project.trim_start_matches('.').to_string();
    match stem.as_str() {
        "readme" | "index" => project,
        _ => format!("{project}-{}", stem.trim_start_matches('.')),
    }
}

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(path),
    }
}

/// A link's target, as an absolute path: relative targets are from the link's folder.
fn resolve(link: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        return target.to_path_buf();
    }
    link.parent().unwrap_or(Path::new("/")).join(target)
}

/// `docs/projects/omanote.md`: the vault's name and the link inside it.
fn shown(vault: &Vault, link: &Path) -> String {
    Path::new(&vault.name()).join(link.strip_prefix(&vault.path).unwrap_or(link)).display().to_string()
}

/// Ask which vault, on the terminal. One vault needs no asking. The answer
/// can also be piped in (`echo 2 | omanote --pin …`), for scripts and agents.
fn choose(vaults: &[Vault], question: &str) -> Result<Vault, String> {
    match vaults {
        [] => return Err("there are no vaults — see omanote --vls".to_string()),
        [only] => return Ok(only.clone()),
        _ => {}
    }
    let mut err = std::io::stderr();
    let _ = writeln!(err, "{question}");
    for (i, vault) in vaults.iter().enumerate() {
        let kind = match (&vault.github, i) {
            (Some(repo), _) => format!("github: {repo}"),
            (None, 0) => "default".to_string(),
            (None, _) => String::new(),
        };
        let _ = writeln!(err, "  {}  {:<24} {kind}", i + 1, tilde(&vault.path));
    }
    let _ = write!(err, "Vault [1]: ");
    let _ = err.flush();
    let mut answer = String::new();
    let read = std::io::stdin().lock().read_line(&mut answer).map_err(|e| format!("cannot read the answer: {e}"))?;
    if read == 0 {
        return Err("no vault chosen".to_string());
    }
    pick(vaults, answer.trim()).ok_or_else(|| format!("\"{}\" is not one of the vaults listed", answer.trim()))
}

/// The answer to `choose`: nothing for the first, a number, or a vault's name or folder.
fn pick(vaults: &[Vault], answer: &str) -> Option<Vault> {
    if answer.is_empty() {
        return vaults.first().cloned();
    }
    if let Ok(n) = answer.parse::<usize>() {
        return vaults.get(n.checked_sub(1)?).cloned();
    }
    let slug = crate::vaults::github_slug(answer);
    vaults.iter().find(|v| v.name() == answer || tilde(&v.path) == answer || v.path == Path::new(answer) || (slug.is_some() && v.github == slug)).cloned()
}

// ---- .gitignore and index.md ---------------------------------------------

/// Read a text file as lines (a missing file is empty), let `change` edit
/// them, and write it back if anything changed. Returns what `change` said
/// about the state before it ran.
fn edit_lines(file: &Path, change: impl FnOnce(&mut Vec<String>) -> bool) -> Result<bool, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", tilde(file))),
    };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let before = lines.clone();
    let said = change(&mut lines);
    if lines != before {
        let mut out = lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        std::fs::write(file, out).map_err(|e| format!("cannot write {}: {e}", tilde(file)))?;
    }
    Ok(said)
}

/// `/my-project.md`: anchored to the vault, with gitignore's wildcards taken literally.
fn ignore_pattern(rel: &Path) -> String {
    let mut out = String::from("/");
    let text = rel.to_string_lossy();
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '?' | '[') {
            out.push('\\');
        }
        out.push(c);
    }
    if out.ends_with(' ') {
        out.insert(out.len() - 1, '\\');
    }
    out
}

/// Put the pattern under omanote's header, making the header if needed.
/// Returns whether it was there already.
fn add_ignore(lines: &mut Vec<String>, pattern: &str) -> bool {
    if lines.iter().any(|l| l == pattern) {
        return true;
    }
    match lines.iter().position(|l| l == IGNORE_HEADER) {
        Some(header) => {
            let end = (header + 1..lines.len()).find(|&i| !lines[i].starts_with('/')).unwrap_or(lines.len());
            lines.insert(end, pattern.to_string());
        }
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(IGNORE_HEADER.to_string());
            lines.push(pattern.to_string());
        }
    }
    false
}

/// Take the pattern out, and the header with it when it was the last pin.
fn remove_ignore(lines: &mut Vec<String>, pattern: &str) -> bool {
    let had = lines.iter().any(|l| l == pattern);
    lines.retain(|l| l != pattern);
    if let Some(header) = lines.iter().position(|l| l == IGNORE_HEADER)
        && !lines.get(header + 1).is_some_and(|l| l.starts_with('/'))
    {
        lines.remove(header);
        if header > 0 && lines.get(header - 1).is_some_and(|l| l.trim().is_empty()) && lines.get(header).is_none_or(|l| l.trim().is_empty()) {
            lines.remove(header - 1);
        }
    }
    had
}

/// A list item linking to `target`, however it is worded.
fn links_to(line: &str, target: &str) -> bool {
    let item = line.trim_start();
    (item.starts_with("- ") || item.starts_with("* ")) && item.contains(&format!("]({target})"))
}

/// Add the entry at the end of the index, unless the note is listed there
/// already. A new index gets a title.
fn add_to_index(lines: &mut Vec<String>, entry: &str, target: &str) -> bool {
    if lines.iter().any(|l| links_to(l, target)) {
        return true;
    }
    if lines.is_empty() {
        lines.push("# Index".to_string());
    }
    // Straight after another list item it joins that list; after anything
    // else it starts a list of its own, which needs a blank line in between.
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if !lines.last().is_some_and(|l| l.trim_start().starts_with("- ") || l.trim_start().starts_with("* ")) {
        lines.push(String::new());
    }
    lines.push(entry.to_string());
    false
}

fn remove_from_index(lines: &mut Vec<String>, target: &str) -> bool {
    let had = lines.iter().any(|l| links_to(l, target));
    lines.retain(|l| !links_to(l, target));
    while had && lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    had
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omanote-pins-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn setup(name: &str) -> (Vault, PathBuf) {
        let root = scratch(name);
        let vault = root.join("vault");
        let project = root.join("my-project");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("README.md"), "# My project\n").unwrap();
        (Vault { path: vault, github: None }, project)
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn pins_writes_through_and_unpins() {
        let (vault, project) = setup("round");
        let readme = project.join("README.md");
        std::fs::write(vault.path.join(".gitignore"), "*.tmp\n").unwrap();

        let msg = pin_into(&vault, &readme, "my-project").unwrap();
        assert!(msg.starts_with("Pinned vault/my-project.md"), "{msg}");
        let link = vault.path.join("my-project.md");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());

        // An edit made from the vault lands in the project.
        std::fs::write(&link, "# Changed from the vault\n").unwrap();
        assert_eq!(read(&readme), "# Changed from the vault\n");

        assert_eq!(read(&vault.path.join(".gitignore")), format!("*.tmp\n\n{IGNORE_HEADER}\n/my-project.md\n"));
        assert_eq!(read(&vault.path.join("index.md")), "# Index\n\n- [my-project](my-project.md)\n");

        // Again: nothing doubles up.
        pin_into(&vault, &readme, "my-project").unwrap();
        assert_eq!(read(&vault.path.join("index.md")).matches("my-project.md").count(), 1);
        assert_eq!(read(&vault.path.join(".gitignore")).matches("/my-project.md").count(), 1);

        assert_eq!(pins_in(&vault), vec![Pin { link: link.clone(), target: readme.clone(), broken: false }]);

        let msg = unpin_from(&vault, "my-project").unwrap();
        assert!(msg.starts_with("Unpinned vault/my-project.md"), "{msg}");
        assert!(link.symlink_metadata().is_err());
        assert_eq!(read(&readme), "# Changed from the vault\n", "the project's file stays");
        assert_eq!(read(&vault.path.join(".gitignore")), "*.tmp\n", "the header goes with the last pin");
        assert_eq!(read(&vault.path.join("index.md")), "# Index\n");
    }

    #[test]
    fn a_taken_name_is_refused_or_repaired() {
        let (vault, project) = setup("taken");
        let readme = project.join("README.md");
        let other = project.join("AGENTS.md");
        std::fs::write(&other, "agents\n").unwrap();

        std::fs::write(vault.path.join("notes.md"), "mine\n").unwrap();
        assert!(pin_into(&vault, &readme, "notes").unwrap_err().contains("already has a note"));
        assert!(unpin_from(&vault, "notes").unwrap_err().contains("not a pin"));
        assert_eq!(read(&vault.path.join("notes.md")), "mine\n");

        pin_into(&vault, &readme, "my-project").unwrap();
        assert!(pin_into(&vault, &other, "my-project").unwrap_err().contains("already pinned to"));

        // The project moves: the link breaks, `--pins` says so, pinning again repairs it.
        let moved = project.with_file_name("moved");
        std::fs::rename(&project, &moved).unwrap();
        assert!(pins_in(&vault)[0].broken);
        assert!(list(std::slice::from_ref(&vault)).contains("1 broken"));
        let msg = pin_into(&vault, &moved.join("README.md"), "my-project").unwrap();
        assert!(msg.starts_with("Re-pinned"), "{msg}");
        assert!(!pins_in(&vault)[0].broken);
    }

    #[test]
    fn pins_into_folders_of_the_vault() {
        let (vault, project) = setup("folders");
        pin_into(&vault, &project.join("README.md"), "projects/my project").unwrap();
        assert!(vault.path.join("projects/my project.md").exists());
        assert!(read(&vault.path.join(".gitignore")).contains("\n/projects/my project.md\n"));
        assert!(read(&vault.path.join("index.md")).contains("- [my project](projects/my%20project.md)"));
        unpin_from(&vault, "projects/my project").unwrap();
        assert!(!vault.path.join("projects").exists(), "the emptied folder goes too");
    }

    #[test]
    fn refuses_what_is_not_a_project_file() {
        let (vault, _) = setup("refuse");
        std::fs::write(vault.path.join("inside.md"), "x").unwrap();
        assert!(pin_into(&vault, &vault.path.join("inside.md"), "inside2").unwrap_err().contains("already in the vault"));
        for bad in ["", "../escape", "/abs", ".hidden", "a/.b", "."] {
            assert!(clean_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(clean_name("my-project.md").unwrap(), "my-project");
        assert_eq!(clean_name("projects/omanote").unwrap(), "projects/omanote");
    }

    #[test]
    fn names_itself_after_the_project() {
        let root = scratch("names");
        let repo = root.join("omanote");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        assert_eq!(default_name(&repo.join("README.md")), "omanote");
        assert_eq!(default_name(&repo.join("AGENTS.md")), "omanote-agents");
        assert_eq!(default_name(&repo.join("docs/Plan.md")), "omanote-plan", "the repo, not the docs folder");
        assert_eq!(default_name(&root.join("loose/readme.md")), "loose");
    }

    #[test]
    fn picks_a_vault_from_the_answer() {
        let vaults = vec![
            Vault { path: "/v/docs".into(), github: None },
            Vault { path: "/v/vaults/me/notes".into(), github: Some("me/notes".into()) },
        ];
        assert_eq!(pick(&vaults, "").unwrap(), vaults[0]);
        assert_eq!(pick(&vaults, "2").unwrap(), vaults[1]);
        assert_eq!(pick(&vaults, "notes").unwrap(), vaults[1]);
        assert_eq!(pick(&vaults, "me/notes").unwrap(), vaults[1]);
        assert!(pick(&vaults, "3").is_none());
        assert!(pick(&vaults, "0").is_none());
        assert!(pick(&vaults, "nope").is_none());
    }

    #[test]
    fn gitignore_patterns_are_literal() {
        assert_eq!(ignore_pattern(Path::new("a*b?[c].md")), "/a\\*b\\?\\[c].md");
        assert_eq!(ignore_pattern(Path::new("#x.md")), "/#x.md");
    }
}
