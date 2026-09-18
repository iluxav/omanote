//! The "where do I save this?" prompt for a note that has no file yet:
//! a file name, prefilled from the first line, and a choice of vault.

use std::path::{Component, Path, PathBuf};

use crate::vaults::Vault;

const MAX_SLUG: usize = 60;

/// What was being attempted when the prompt came up, to carry on with afterwards.
pub enum After {
    Stay,
    Quit,
    Open(PathBuf),
}

pub struct SaveAs {
    pub name: String,
    pub vaults: Vec<Vault>,
    pub selected: usize,
    /// Which entry of `vaults` is really the folder omanote was started in.
    pub here: Option<usize>,
    pub after: After,
    /// Set after a first Enter on a name that already exists; a second Enter replaces it.
    pub confirm_replace: bool,
}

/// "# My First Note!" → "my-first-note".
pub fn slug(line: &str) -> String {
    let mut out = String::new();
    for c in line.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.chars().count() >= MAX_SLUG {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

fn timestamp() -> String {
    let out = std::process::Command::new("date").arg("+%Y-%m-%d-%H%M").output();
    out.ok().and_then(|o| String::from_utf8(o.stdout).ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "untitled".into())
}

impl SaveAs {
    /// `here`: the current folder, offered after the vaults unless it is one.
    /// `wanted`: what was asked for on the command line, used when the note has no words yet.
    pub fn new(lines: &[Vec<char>], mut vaults: Vec<Vault>, here: Option<PathBuf>, wanted: Option<&str>, after: After) -> Self {
        // First line that has any words in it; markdown markers fall away in the slug.
        let title = lines.iter().map(|l| slug(&l.iter().collect::<String>())).find(|s| !s.is_empty());
        let name = title.or_else(|| wanted.map(slug).filter(|s| !s.is_empty())).unwrap_or_else(|| format!("note-{}", timestamp()));
        let mut here_at = None;
        if let Some(here) = here.filter(|h| !vaults.iter().any(|v| &v.path == h)) {
            here_at = Some(vaults.len());
            vaults.push(Vault { path: here, github: None });
        }
        SaveAs { name, vaults, selected: 0, here: here_at, after, confirm_replace: false }
    }

    pub fn edit(&mut self, change: impl FnOnce(&mut String)) {
        change(&mut self.name);
        self.confirm_replace = false;
    }

    pub fn step(&mut self, delta: isize) {
        let n = self.vaults.len().max(1) as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        self.confirm_replace = false;
    }

    /// The file this would write. The name may contain folders (`work/ideas`)
    /// but can never leave the vault.
    pub fn target(&self) -> Result<PathBuf, String> {
        let name = self.name.trim();
        let name = name.strip_suffix(".md").unwrap_or(name).trim().trim_matches('/');
        if name.is_empty() {
            return Err("Type a name for the note".into());
        }
        let inside = Path::new(name).components().all(|c| matches!(c, Component::Normal(_)));
        if !inside {
            return Err("The name cannot contain ..".into());
        }
        let vault = self.vaults.get(self.selected).ok_or("No vault to save in")?;
        Ok(vault.path.join(format!("{name}.md")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(src: &str) -> Vec<Vec<char>> {
        src.lines().map(|l| l.chars().collect()).collect()
    }

    fn vaults() -> Vec<Vault> {
        vec![Vault { path: "/v/docs".into(), github: None }, Vault { path: "/v/work".into(), github: None }]
    }

    #[test]
    fn slugs_the_first_meaningful_line() {
        assert_eq!(slug("# My First Note!"), "my-first-note");
        assert_eq!(slug("- [ ] Buy milk & eggs"), "buy-milk-eggs");
        assert_eq!(slug("  **Ünïcödé** title — 2026  "), "ünïcödé-title-2026");
        assert_eq!(slug("---"), "");
        assert_eq!(slug(&"word ".repeat(40)).chars().count(), 59, "capped, no trailing dash");

        assert_eq!(SaveAs::new(&lines("\n---\n## Trip plan\nbody"), vaults(), None, None, After::Stay).name, "trip-plan");
        assert!(SaveAs::new(&lines("\n\n"), vaults(), None, None, After::Stay).name.starts_with("note-"));
    }

    #[test]
    fn builds_a_path_inside_the_chosen_vault() {
        let mut p = SaveAs::new(&lines("# Idea"), vaults(), None, None, After::Stay);
        assert_eq!(p.target(), Ok("/v/docs/idea.md".into()));
        p.step(1);
        assert_eq!(p.target(), Ok("/v/work/idea.md".into()));
        p.step(1);
        assert_eq!(p.selected, 0, "wraps");

        p.edit(|n| *n = " projects/omanote/ideas.md ".into());
        assert_eq!(p.target(), Ok("/v/docs/projects/omanote/ideas.md".into()));
        for bad in ["", "  ", ".md", "../escape", "a/../../b", "/etc/passwd"] {
            p.edit(|n| *n = bad.into());
            assert!(p.target().is_err() || p.target() == Ok("/v/docs/etc/passwd.md".into()), "{bad}");
        }
        p.edit(|n| *n = "../escape".into());
        assert!(p.target().is_err());
    }

    #[test]
    fn offers_the_current_folder_and_the_name_that_was_asked_for() {
        let p = SaveAs::new(&lines(""), vaults(), Some("/home/me/project".into()), Some("Trip Plan"), After::Stay);
        assert_eq!(p.name, "trip-plan", "nothing written yet, so the command-line name is the best guess");
        assert_eq!((p.vaults.len(), p.here, p.selected), (3, Some(2), 0), "listed last; the default vault stays preselected");
        let mut p = SaveAs::new(&lines("# Real title"), vaults(), Some("/home/me/project".into()), Some("trip"), After::Stay);
        assert_eq!(p.name, "real-title", "what was written wins");
        p.step(-1);
        assert_eq!(p.target(), Ok("/home/me/project/real-title.md".into()));
        // Started inside a vault: not offered twice.
        assert_eq!(SaveAs::new(&lines("x"), vaults(), Some("/v/work".into()), None, After::Stay).here, None);
    }

    #[test]
    fn editing_cancels_a_pending_replace() {
        let mut p = SaveAs::new(&lines("x"), vaults(), None, None, After::Stay);
        p.confirm_replace = true;
        p.edit(|n| n.push('y'));
        assert!(!p.confirm_replace);
        p.confirm_replace = true;
        p.step(1);
        assert!(!p.confirm_replace);
    }
}
