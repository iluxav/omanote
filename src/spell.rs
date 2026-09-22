//! Spelling and grammar, checked as you write, with Harper: an English
//! checker that lives inside omanote. Nothing leaves the machine and nothing
//! has to be installed.
//!
//! The check runs on its own thread, a moment after the last keystroke, and
//! its findings come back as `Problem`s: where, what is wrong, and what would
//! fix it. Words of your own go in `~/.omanote/dictionary.txt`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use harper_core::linting::{LintGroup, LintKind, Linter, Suggestion};
use harper_core::spell::{FstDictionary, MergedDictionary, MutableDictionary};
use harper_core::{Dialect, Document};

use crate::editor::Pos;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fix {
    Replace(String),
    InsertAfter(String),
    Remove,
}

impl Fix {
    /// How it reads in the list.
    pub fn label(&self) -> String {
        match self {
            Fix::Replace(with) => with.clone(),
            Fix::InsertAfter(with) => format!("Insert “{with}” after"),
            Fix::Remove => "Remove it".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub row: usize,
    /// Columns, in characters, on that line. A problem never spans lines.
    pub from: usize,
    pub to: usize,
    /// The text it is about, to tell whether it is still there.
    pub word: String,
    pub message: String,
    pub fixes: Vec<Fix>,
    /// An unknown word, which can go in the dictionary.
    pub spelling: bool,
}

impl Problem {
    pub fn has(&self, pos: Pos) -> bool {
        pos.row == self.row && (self.from..=self.to).contains(&pos.col)
    }

    /// Whether the text this is about is still where it was found.
    pub fn still_there(&self, lines: &[Vec<char>]) -> bool {
        lines.get(self.row).is_some_and(|l| self.to <= l.len() && l[self.from..self.to].iter().copied().eq(self.word.chars()))
    }
}

/// F7: the list under a problem. Its fixes, then the two ways of saying "it is fine".
pub struct Fixer {
    pub problem: Problem,
    pub selected: usize,
}

pub enum Choice<'a> {
    Fix(&'a Fix),
    Learn,
    Ignore,
}

impl Fixer {
    pub fn choices(&self) -> Vec<Choice<'_>> {
        let mut out: Vec<Choice> = self.problem.fixes.iter().map(Choice::Fix).collect();
        if self.problem.spelling {
            out.push(Choice::Learn);
        }
        out.push(Choice::Ignore);
        out
    }

    pub fn labels(&self) -> Vec<String> {
        self.choices()
            .iter()
            .map(|c| match c {
                // What the words become, not how: "Let's", not "insert 's after".
                Choice::Fix(Fix::InsertAfter(with)) => format!("{}{with}", self.problem.word),
                Choice::Fix(Fix::Remove) => format!("Remove “{}”", self.problem.word.trim()),
                Choice::Fix(fix) => fix.label(),
                Choice::Learn => format!("Add “{}” to my dictionary", self.problem.word),
                Choice::Ignore => "Ignore it".into(),
            })
            .collect()
    }

    pub fn step(&mut self, delta: isize) {
        let n = self.choices().len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n.max(1)) as usize;
    }
}

/// What `omanote` calls the dialects, and what Harper does.
pub fn dialect(name: &str) -> Option<Dialect> {
    Some(match name.trim().to_lowercase().replace(['-', '_', ' '], "").as_str() {
        "american" | "us" | "usa" | "enus" => Dialect::American,
        "british" | "uk" | "gb" | "engb" | "english" => Dialect::British,
        "canadian" | "ca" | "enca" => Dialect::Canadian,
        "australian" | "au" | "enau" => Dialect::Australian,
        "indian" | "in" | "enin" => Dialect::Indian,
        _ => return None,
    })
}

pub fn dictionary_file(home: &Path) -> PathBuf {
    home.join("dictionary.txt")
}

enum Job {
    Check(u64, String),
    Learn(String),
}

/// The checker, on its thread. Send it the note; findings come back with the
/// same number, so an answer to an old question is known for what it is.
pub struct Checker {
    jobs: Sender<Job>,
    found: Receiver<(u64, Vec<Problem>)>,
    home: PathBuf,
}

/// Rules that are more taste than error, and get in the way of notes.
const QUIET: [&str; 3] = ["UseTitleCase", "SentenceCapitalization", "LongSentences"];

fn dictionary(home: &Path) -> Arc<MergedDictionary> {
    let mut merged = MergedDictionary::new();
    merged.add_dictionary(FstDictionary::curated());
    let mut own = MutableDictionary::new();
    for word in std::fs::read_to_string(dictionary_file(home)).unwrap_or_default().lines().map(str::trim).filter(|w| !w.is_empty()) {
        own.append_word_str(word, Default::default());
    }
    merged.add_dictionary(Arc::new(own));
    Arc::new(merged)
}

fn linter(dict: Arc<MergedDictionary>, dialect: Dialect) -> LintGroup {
    let mut group = LintGroup::new_curated(dict, dialect);
    for rule in QUIET {
        group.config.set_rule_enabled(rule, false);
    }
    group
}

/// Advice about style is not a mistake: only mistakes get underlined.
fn counts(kind: LintKind) -> bool {
    !matches!(kind, LintKind::Enhancement | LintKind::Readability | LintKind::Style | LintKind::Regionalism | LintKind::Redundancy)
}

/// Run the checker over `text` and say where the problems are.
pub fn check(text: &str, dict: &Arc<MergedDictionary>, linter: &mut LintGroup) -> Vec<Problem> {
    let chars: Vec<char> = text.chars().collect();
    // Where each line starts, to turn a character offset into (row, col).
    let mut starts = vec![0usize];
    starts.extend(chars.iter().enumerate().filter(|(_, c)| **c == '\n').map(|(i, _)| i + 1));
    let doc = Document::new_markdown_default(text, dict.as_ref());
    let mut out: Vec<Problem> = Vec::new();
    for lint in linter.lint(&doc) {
        if !counts(lint.lint_kind) || lint.span.start >= lint.span.end || lint.span.end > chars.len() {
            continue;
        }
        let row = starts.partition_point(|&s| s <= lint.span.start) - 1;
        let (from, to) = (lint.span.start - starts[row], lint.span.end - starts[row]);
        if chars[lint.span.start..lint.span.end].contains(&'\n') {
            continue;
        }
        let word: String = chars[lint.span.start..lint.span.end].iter().collect();
        let fixes = lint
            .suggestions
            .iter()
            .map(|s| match s {
                Suggestion::ReplaceWith(with) => Fix::Replace(with.iter().collect()),
                Suggestion::InsertAfter(with) => Fix::InsertAfter(with.iter().collect()),
                Suggestion::Remove => Fix::Remove,
            })
            .collect();
        // Two rules on one word (a typo that is also lowercase): one mark, the more useful message.
        if let Some(same) = out.iter_mut().find(|p| p.row == row && p.from == from && p.to == to) {
            if lint.lint_kind == LintKind::Spelling && !same.spelling {
                *same = Problem { row, from, to, word, message: lint.message.clone(), fixes, spelling: true };
            }
            continue;
        }
        out.push(Problem { row, from, to, word, message: lint.message.clone(), fixes, spelling: lint.lint_kind == LintKind::Spelling });
    }
    out.sort_by_key(|p| (p.row, p.from));
    out
}

impl Checker {
    /// The dictionary is loaded on the thread, when first needed: a quarter
    /// of a second that would otherwise go into starting up.
    pub fn start(home: &Path, dialect: Dialect) -> Self {
        let (jobs, inbox) = channel::<Job>();
        let (report, found) = channel();
        let home_for_thread = home.to_path_buf();
        std::thread::spawn(move || {
            let mut dict = None;
            let mut linter_ = None;
            while let Ok(mut job) = inbox.recv() {
                // Only the latest text matters; the ones typed over in the meantime do not.
                while let Ok(newer) = inbox.try_recv() {
                    match (&job, &newer) {
                        (Job::Check(..), Job::Check(..)) => job = newer,
                        (Job::Learn(word), _) => {
                            learn(&home_for_thread, word);
                            dict = None;
                            job = newer;
                        }
                        (_, Job::Learn(word)) => {
                            learn(&home_for_thread, word);
                            dict = None;
                        }
                    }
                }
                match job {
                    Job::Learn(word) => {
                        learn(&home_for_thread, &word);
                        dict = None;
                    }
                    Job::Check(n, text) => {
                        if dict.is_none() {
                            let d = dictionary(&home_for_thread);
                            linter_ = Some(linter(d.clone(), dialect));
                            dict = Some(d);
                        }
                        let (Some(d), Some(l)) = (&dict, &mut linter_) else { continue };
                        if report.send((n, check(&text, d, l))).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Checker { jobs, found, home: home.to_path_buf() }
    }

    pub fn check(&self, n: u64, text: String) {
        let _ = self.jobs.send(Job::Check(n, text));
    }

    /// A word of yours: into the dictionary file, and out of the checks.
    pub fn learn(&self, word: &str) {
        let _ = self.jobs.send(Job::Learn(word.to_string()));
    }

    pub fn results(&self) -> Option<(u64, Vec<Problem>)> {
        self.found.try_recv().ok()
    }

    pub fn dictionary_file(&self) -> PathBuf {
        dictionary_file(&self.home)
    }
}

fn learn(home: &Path, word: &str) {
    let file = dictionary_file(home);
    let known: HashSet<String> = std::fs::read_to_string(&file).unwrap_or_default().lines().map(|l| l.trim().to_string()).collect();
    if known.contains(word) {
        return;
    }
    let _ = std::fs::create_dir_all(home);
    let mut text = std::fs::read_to_string(&file).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(word);
    text.push('\n');
    let _ = std::fs::write(&file, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        let home = std::env::temp_dir().join(format!("omanote-spell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn finds_slips_and_says_how_to_fix_them() {
        let home = home();
        let dict = dictionary(&home);
        let mut linter = linter(dict.clone(), Dialect::American);
        let text = "# Trip notes\n\nteh hotel is booked untill friday, see [the plan](trips/japan.md) and `teh code`.\n\n- [ ] milk tomorow, of of course\n";
        let found = check(text, &dict, &mut linter);
        let at = |word: &str| found.iter().find(|p| p.word == word).unwrap_or_else(|| panic!("{word} not found in {found:?}"));
        assert_eq!((at("teh").row, at("teh").from, at("teh").to, at("teh").spelling), (2, 0, 3, true));
        assert_eq!(at("teh").fixes[0], Fix::Replace("the".into()));
        assert_eq!(at("untill").fixes[0], Fix::Replace("until".into()));
        assert_eq!((at("tomorow").row, at("tomorow").from), (4, 11));
        assert_eq!(at("of of").fixes[0], Fix::Replace("of".into()), "a doubled word");
        assert!(found.iter().filter(|p| p.word == "teh").count() == 1, "the one in the code span is left alone, and the typo has one mark, not two");
        assert!(!found.iter().any(|p| p.word.contains("japan")), "a link's address is not prose");
        assert!(!found.iter().any(|p| p.word == "# Trip notes"), "no lecture about title case");
        assert!(found.windows(2).all(|w| (w[0].row, w[0].from) <= (w[1].row, w[1].from)), "in reading order");

        let lines: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
        assert!(at("teh").still_there(&lines) && at("teh").has(Pos { row: 2, col: 3 }) && !at("teh").has(Pos { row: 2, col: 4 }));
        let mut moved = lines.clone();
        moved[2].insert(0, 'x');
        assert!(!at("teh").still_there(&moved));

        // A word of your own stops being a problem.
        learn(&home, "untill");
        learn(&home, "untill");
        assert_eq!(std::fs::read_to_string(dictionary_file(&home)).unwrap(), "untill\n", "once");
        let dict = dictionary(&home);
        let mut linter = super::linter(dict.clone(), Dialect::American);
        assert!(!check(text, &dict, &mut linter).iter().any(|p| p.word == "untill"));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn the_thread_answers_with_the_number_it_was_asked_with() {
        let home = home();
        let checker = Checker::start(&home, dialect("british").unwrap());
        checker.check(1, "colour is fine here, teh".into());
        checker.check(2, "all good".into());
        let mut got = Vec::new();
        for _ in 0..600 {
            if let Some(found) = checker.results() {
                got.push(found);
            }
            if got.iter().any(|(n, _)| *n == 2) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let last = got.iter().find(|(n, _)| *n == 2).expect("the latest check answered");
        assert!(last.1.is_empty());
        if let Some((_, first)) = got.iter().find(|(n, _)| *n == 1) {
            assert!(first.iter().any(|p| p.word == "teh") && !first.iter().any(|p| p.word == "colour"), "{first:?}");
        }
        assert_eq!(dialect("nope"), None);
        let _ = std::fs::remove_dir_all(home);
    }
}
