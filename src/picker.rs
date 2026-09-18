//! Ctrl+P: fuzzy note picker over every vault.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::vaults::{Vault, tilde};

const MAX_NOTES: usize = 10_000;
const MAX_DEPTH: usize = 8;

pub struct Note {
    pub path: PathBuf,
    /// Path inside the vault without the extension, with the vault's name in
    /// front for every vault but the first; this is what gets matched and shown.
    pub name: Vec<char>,
    lower: Vec<char>,
    pub modified: SystemTime,
}

pub struct Hit {
    pub note: usize,
    /// Indices into `Note::name` that matched the query.
    pub positions: Vec<usize>,
}

pub enum Row<'a> {
    Note(&'a Note, &'a [usize]),
    Create(String),
}

pub struct Picker {
    /// Vault folders; new notes are created in the first.
    roots: Vec<PathBuf>,
    pub query: String,
    pub selected: usize,
    notes: Vec<Note>,
    hits: Vec<Hit>,
}

fn walk(dir: &Path, root: &Path, prefix: &str, depth: usize, out: &mut Vec<Note>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if out.len() >= MAX_NOTES {
            return;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if kind.is_dir() {
            if depth < MAX_DEPTH {
                walk(&path, root, prefix, depth + 1, out);
            }
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown")) {
            let rel = path.strip_prefix(root).unwrap_or(&path).with_extension("");
            let name: Vec<char> = format!("{prefix}{}", rel.to_string_lossy()).chars().collect();
            let lower = name.iter().flat_map(|c| c.to_lowercase()).collect();
            let modified = entry.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            out.push(Note { path, name, lower, modified });
        }
    }
}

impl Picker {
    pub fn open(vaults: &[Vault]) -> Self {
        let mut notes = Vec::new();
        for (i, vault) in vaults.iter().enumerate() {
            let prefix = if i == 0 { String::new() } else { format!("{}/", vault.name()) };
            walk(&vault.path, &vault.path, &prefix, 0, &mut notes);
        }
        notes.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.name.cmp(&b.name)));
        let roots = vaults.iter().map(|v| v.path.clone()).collect();
        let mut picker = Picker { roots, query: String::new(), selected: 0, notes, hits: Vec::new() };
        picker.refresh();
        picker
    }

    /// Re-rank after the query changed. Every whitespace-separated term has to match.
    fn refresh(&mut self) {
        let terms: Vec<Vec<char>> =
            self.query.split_whitespace().map(|t| t.chars().flat_map(|c| c.to_lowercase()).collect()).collect();
        let mut scored: Vec<(i32, Hit)> = Vec::new();
        for (i, note) in self.notes.iter().enumerate() {
            let mut total = 0;
            let mut positions = Vec::new();
            let matched = terms.iter().all(|term| match fuzzy(term, &note.lower) {
                Some((score, at)) => {
                    total += score;
                    positions.extend(at);
                    true
                }
                None => false,
            });
            if matched {
                positions.sort_unstable();
                positions.dedup();
                scored.push((total, Hit { note: i, positions }));
            }
        }
        // Notes are already newest-first, and the sort is stable, so ties stay in that order.
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        self.hits = scored.into_iter().map(|(_, hit)| hit).collect();
        self.selected = 0;
    }

    /// Offer to create the note when nothing is named exactly what was typed.
    fn create_name(&self) -> Option<String> {
        let name = self.query.trim().trim_end_matches(".md").trim();
        let exists = self.notes.iter().any(|n| n.lower.iter().copied().eq(name.chars().flat_map(|c| c.to_lowercase())));
        (!name.is_empty() && !exists).then(|| name.to_string())
    }

    /// What is being searched, for the panel title.
    pub fn title(&self) -> String {
        match &self.roots[..] {
            [only] => tilde(only),
            roots => format!("{} vaults", roots.len()),
        }
    }

    pub fn len(&self) -> usize {
        self.hits.len() + self.create_name().is_some() as usize
    }

    pub fn total(&self) -> usize {
        self.notes.len()
    }

    pub fn row(&self, i: usize) -> Option<Row<'_>> {
        match self.hits.get(i) {
            Some(hit) => Some(Row::Note(&self.notes[hit.note], &hit.positions)),
            None if i == self.hits.len() => self.create_name().map(Row::Create),
            None => None,
        }
    }

    /// The file Enter would open (it may not exist yet).
    pub fn chosen(&self) -> Option<PathBuf> {
        match self.row(self.selected)? {
            Row::Note(note, _) => Some(note.path.clone()),
            Row::Create(name) => Some(self.roots.first()?.join(format!("{name}.md"))),
        }
    }

    pub fn step(&mut self, delta: isize) {
        let n = self.len();
        if n > 0 {
            self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        }
    }

    pub fn push(&mut self, text: &str) {
        self.query.extend(text.chars().filter(|c| !c.is_control()));
        self.refresh();
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.refresh();
    }

    pub fn delete_word(&mut self) {
        let trimmed = self.query.trim_end();
        let cut = trimmed.rfind(char::is_whitespace).map_or(0, |i| i + 1);
        self.query.truncate(cut);
        self.refresh();
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.refresh();
    }
}

const MATCH: i32 = 1;
const CONSECUTIVE: i32 = 6;
const BOUNDARY: i32 = 8;
const NAME_START: i32 = 14;
const GAP: i32 = 3;
const NONE: i32 = i32::MIN / 2;

/// Subsequence match of `term` in `cand` (both lowercase). Higher is better;
/// rewards runs, word starts and the start of the file name, and returns the
/// matched positions for highlighting.
pub fn fuzzy(term: &[char], cand: &[char]) -> Option<(i32, Vec<usize>)> {
    let (m, n) = (term.len(), cand.len());
    if m == 0 || m > n {
        return (m == 0).then(|| (0, Vec::new()));
    }
    let name_start = cand.iter().rposition(|c| *c == '/').map_or(0, |i| i + 1);
    let bonus = |j: usize| {
        if j == name_start {
            NAME_START
        } else if j == 0 || matches!(cand[j - 1], '/' | '-' | '_' | ' ' | '.') {
            BOUNDARY
        } else {
            0
        }
    };

    // score[i][j]: best score with term[i] placed at cand[j]; from[i][j]: where term[i-1] went.
    let mut score = vec![vec![NONE; n]; m];
    let mut from = vec![vec![0usize; n]; m];
    for i in 0..m {
        let mut best_before = (NONE, 0usize); // best of score[i-1][..j-1], a gap away
        for j in i..n {
            if i > 0 && j >= 2 && score[i - 1][j - 2] > best_before.0 {
                best_before = (score[i - 1][j - 2], j - 2);
            }
            if term[i] != cand[j] {
                continue;
            }
            let prior = if i == 0 {
                Some((-(j.min(4) as i32), 0))
            } else {
                let run = (score[i - 1][j - 1] > NONE).then(|| (score[i - 1][j - 1] + CONSECUTIVE, j - 1));
                let gap = (best_before.0 > NONE).then(|| (best_before.0 - GAP, best_before.1));
                run.into_iter().chain(gap).max_by_key(|(s, _)| *s)
            };
            if let Some((s, k)) = prior {
                score[i][j] = s + MATCH + bonus(j);
                from[i][j] = k;
            }
        }
    }

    let (mut j, &best) = score[m - 1].iter().enumerate().max_by_key(|(j, s)| (**s, std::cmp::Reverse(*j)))?;
    if best <= NONE {
        return None;
    }
    let mut positions = vec![0; m];
    for i in (0..m).rev() {
        positions[i] = j;
        j = from[i][j];
    }
    // Among equal matches prefer the shorter name.
    Some((best * 4 - n as i32, positions))
}

/// "now", "5m", "3h", "12d", "4mo", "2y".
pub fn age(modified: SystemTime) -> String {
    let secs = SystemTime::now().duration_since(modified).map_or(0, |d| d.as_secs());
    match secs {
        0..60 => "now".to_string(),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        86_400..2_592_000 => format!("{}d", secs / 86_400),
        2_592_000..31_536_000 => format!("{}mo", secs / 2_592_000),
        _ => format!("{}y", secs / 31_536_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(term: &str, cand: &str) -> Option<i32> {
        fuzzy(&term.chars().collect::<Vec<_>>(), &cand.chars().collect::<Vec<_>>()).map(|(s, _)| s)
    }

    #[test]
    fn matches_subsequences_only() {
        assert!(score("ids", "my ideas").is_some());
        assert!(score("xyz", "my ideas").is_none());
        assert!(score("ideasx", "ideas").is_none());
        assert_eq!(fuzzy(&['i', 'd'], &"my ideas".chars().collect::<Vec<_>>()).unwrap().1, [3, 4]);
    }

    #[test]
    fn ranks_the_obvious_note_first() {
        assert!(score("idea", "ideas") > score("idea", "inside-deals"));
        assert!(score("idea", "ideas") > score("idea", "archive/old ideas"));
        assert!(score("todo", "work/todo") > score("todo", "todo-list/work notes"));
        assert!(score("mi", "my ideas") > score("mi", "swimming"));
        assert!(score("plan", "plan") > score("plan", "planning for next year"));
    }

    fn temp_vault(names: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("omanote-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&root);
        for name in names {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        root
    }

    #[test]
    fn lists_filters_and_creates() {
        let root = temp_vault(&["my ideas.md", "work/todo.md", "work/meeting notes.md", ".hidden/secret.md", "image.png"]);
        let mut p = Picker::open(&[Vault { path: root.clone(), github: None }]);
        assert_eq!(p.total(), 3, "markdown only, hidden folders skipped");
        assert_eq!(p.len(), 3);

        p.push("ideas my");
        assert_eq!(p.chosen(), Some(root.join("my ideas.md")), "terms match in any order");
        assert_eq!(p.len(), 2, "one hit + the create row");

        p.clear();
        p.push("My Ideas");
        assert_eq!(p.len(), 1, "exact name (any case): nothing to create");

        p.clear();
        p.push("wrk td");
        assert_eq!(p.chosen(), Some(root.join("work/todo.md")));

        p.clear();
        p.push("brand new");
        assert_eq!(p.len(), 1);
        assert_eq!(p.chosen(), Some(root.join("brand new.md")));
        p.step(1);
        assert_eq!(p.selected, 0, "wraps around");

        p.delete_word();
        assert_eq!(p.query, "brand ");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn searches_every_vault_and_creates_in_the_first() {
        let first = temp_vault(&["inbox.md"]);
        let second = first.join("team-notes");
        std::fs::create_dir_all(second.join("plans")).unwrap();
        std::fs::write(second.join("plans/roadmap.md"), "x").unwrap();
        // `first` contains `second`, so give the default vault its own folder.
        let first = first.join("docs");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::write(first.join("inbox.md"), "x").unwrap();
        let vaults = [Vault { path: first.clone(), github: None }, Vault { path: second.clone(), github: Some("me/team-notes".into()) }];

        let mut p = Picker::open(&vaults);
        assert_eq!((p.total(), p.title()), (2, "2 vaults".to_string()));
        p.push("roadmap");
        assert_eq!(p.chosen(), Some(second.join("plans/roadmap.md")));
        let Some(Row::Note(note, _)) = p.row(0) else { panic!() };
        assert_eq!(note.name.iter().collect::<String>(), "team-notes/plans/roadmap");

        p.clear();
        p.push("team");
        assert_eq!(p.chosen(), Some(second.join("plans/roadmap.md")), "the vault name is searchable too");

        p.clear();
        p.push("fresh idea");
        assert_eq!(p.chosen(), Some(first.join("fresh idea.md")));
        let _ = std::fs::remove_dir_all(first.parent().unwrap());
    }

    #[test]
    fn missing_vault_is_just_empty() {
        let p = Picker::open(&[Vault { path: "/nonexistent/omanote/vault".into(), github: None }]);
        assert_eq!((p.total(), p.len()), (0, 0));
        assert_eq!(p.chosen(), None);
    }
}
