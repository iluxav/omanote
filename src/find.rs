//! Ctrl+F: find text in the note.
//!
//! The search runs as the query is typed, from where the cursor was when the
//! bar opened, so adding a letter narrows the match in front of you rather
//! than jumping on from the last one. Case is ignored until the query has a
//! capital in it.

use crate::editor::{Editor, Pos};

pub struct Find {
    pub query: String,
    /// (line, first column, one past the last), in reading order.
    pub matches: Vec<(usize, usize, usize)>,
    pub current: Option<usize>,
    /// Where the cursor was when the bar opened.
    origin: Pos,
}

fn fold(c: char, exact: bool) -> char {
    if exact { c } else { c.to_lowercase().next().unwrap_or(c) }
}

/// Every place `query` occurs in `lines`. Matches do not overlap.
pub fn search(lines: &[Vec<char>], query: &str) -> Vec<(usize, usize, usize)> {
    let exact = query.chars().any(char::is_uppercase);
    let needle: Vec<char> = query.chars().map(|c| fold(c, exact)).collect();
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    for (row, line) in lines.iter().enumerate() {
        let mut col = 0;
        while col + needle.len() <= line.len() {
            if needle.iter().zip(&line[col..]).all(|(n, c)| *n == fold(*c, exact)) {
                out.push((row, col, col + needle.len()));
                col += needle.len();
            } else {
                col += 1;
            }
        }
    }
    out
}

impl Find {
    /// Opens on the selected words, if a few are selected on one line, or on
    /// what was searched for last.
    pub fn open(ed: &Editor, last: &str) -> Self {
        let selected = ed.selected_text().filter(|t| !t.contains('\n') && !t.trim().is_empty() && t.chars().count() <= 80);
        let origin = ed.selection().map_or(ed.cursor, |(start, _)| start);
        Find { query: selected.unwrap_or_else(|| last.to_string()), matches: Vec::new(), current: None, origin }
    }

    /// Search again (the query changed, or the note did) and settle on the
    /// first match at or after where the search started, wrapping round.
    pub fn refresh(&mut self, ed: &mut Editor) {
        self.matches = search(&ed.lines, &self.query);
        let from = (self.origin.row, self.origin.col);
        self.current = match self.matches.len() {
            0 => None,
            _ => Some(self.matches.iter().position(|&(row, col, _)| (row, col) >= from).unwrap_or(0)),
        };
        self.show(ed);
    }

    /// Next (`1`) or previous (`-1`) match, round and round.
    pub fn step(&mut self, delta: isize, ed: &mut Editor) {
        // The note may have changed under the bar: the agent, a sync.
        self.matches = search(&ed.lines, &self.query);
        let n = self.matches.len() as isize;
        self.current = match (n, self.current) {
            (0, _) => None,
            (_, Some(i)) => Some((i as isize + delta).rem_euclid(n) as usize),
            (_, None) => Some(0),
        };
        self.show(ed);
    }

    /// The current match becomes the selection, which also brings it into view.
    fn show(&self, ed: &mut Editor) {
        match self.current.and_then(|i| self.matches.get(i)) {
            Some(&(row, from, to)) => {
                ed.move_to(Pos { row, col: from }, false);
                ed.move_to(Pos { row, col: to }, true);
            }
            None => ed.move_to(self.origin, false),
        }
    }

    /// `3 of 12`, for the bar.
    pub fn count(&self) -> String {
        match (self.query.is_empty(), self.current) {
            (true, _) => String::new(),
            (false, None) => "no matches".into(),
            (false, Some(i)) => format!("{} of {}", i + 1, self.matches.len()),
        }
    }

    /// The matches on one line, for highlighting.
    pub fn on_row(&self, row: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let start = self.matches.partition_point(|m| m.0 < row);
        self.matches[start..].iter().take_while(move |m| m.0 == row).map(|m| (m.1, m.2))
    }

    /// The match the search is on, if it is on this line.
    pub fn here(&self, row: usize) -> Option<(usize, usize)> {
        self.current.and_then(|i| self.matches.get(i)).filter(|m| m.0 == row).map(|m| (m.1, m.2))
    }

    pub fn delete_word(&mut self) {
        let trimmed = self.query.trim_end();
        let cut = trimmed.rfind(char::is_whitespace).map_or(0, |i| i + 1);
        self.query.truncate(cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note() -> Editor {
        Editor::new("# Trip to Japan\n\nBook the ryokan in Kyoto.\nJAPAN rail pass, japan guide\n\n| city | nights |\n| --- | --- |\n| Kyoto | 3 |", None)
    }

    #[test]
    fn finds_every_match_ignoring_case_until_a_capital_is_typed() {
        let ed = note();
        assert_eq!(search(&ed.lines, "japan"), [(0, 10, 15), (3, 0, 5), (3, 17, 22)]);
        assert_eq!(search(&ed.lines, "Japan"), [(0, 10, 15)], "a capital means: exactly this");
        assert_eq!(search(&ed.lines, "kyoto").len(), 2, "in a table too");
        assert_eq!(search(&ed.lines, "aa"), []);
        assert_eq!(search(&[vec!['a'; 5]], "aa"), [(0, 0, 2), (0, 2, 4)], "matches do not overlap");
        assert_eq!(search(&ed.lines, ""), []);
        assert_eq!(search(&[ "Ünïcode ÜNÏ".chars().collect()], "ünï"), [(0, 0, 3), (0, 8, 11)]);
    }

    #[test]
    fn starts_where_the_cursor_was_and_goes_round() {
        let mut ed = note();
        ed.move_to(Pos { row: 2, col: 0 }, false);
        let mut find = Find::open(&ed, "");
        assert_eq!(find.count(), "");
        find.query = "japan".into();
        find.refresh(&mut ed);
        assert_eq!((find.count().as_str(), ed.selection()), ("2 of 3", Some((Pos { row: 3, col: 0 }, Pos { row: 3, col: 5 }))), "the first one after the cursor, selected");
        // One more letter narrows the search from the same starting point.
        find.query = "japan g".into();
        find.refresh(&mut ed);
        assert_eq!((find.count().as_str(), ed.cursor), ("1 of 1", Pos { row: 3, col: 24 }));
        find.query = "japan".into();
        find.refresh(&mut ed);
        find.step(1, &mut ed);
        assert_eq!(find.count(), "3 of 3");
        find.step(1, &mut ed);
        assert_eq!((find.count().as_str(), ed.cursor.row), ("1 of 3", 0), "round to the top");
        find.step(-1, &mut ed);
        assert_eq!(find.count(), "3 of 3");
        assert_eq!(find.on_row(3).collect::<Vec<_>>(), [(0, 5), (17, 22)]);
        assert_eq!(find.on_row(1).count(), 0);
        assert_eq!((find.here(3), find.here(0)), (Some((17, 22)), None), "only the one the search is on");

        find.query = "nowhere".into();
        find.refresh(&mut ed);
        assert_eq!((find.count().as_str(), ed.cursor, ed.selection()), ("no matches", Pos { row: 2, col: 0 }, None), "back where it started");
    }

    #[test]
    fn opens_on_the_selected_words_or_the_last_search() {
        let mut ed = note();
        ed.move_to(Pos { row: 2, col: 9 }, false);
        ed.move_to(Pos { row: 2, col: 15 }, true);
        let mut find = Find::open(&ed, "older");
        assert_eq!(find.query, "ryokan");
        find.refresh(&mut ed);
        assert_eq!(find.count(), "1 of 1", "the selection itself is the first match");
        ed.clear_selection();
        assert_eq!(Find::open(&ed, "older").query, "older");
        ed.select_all();
        assert_eq!(Find::open(&ed, "").query, "", "a whole note is not a search");
        let mut find = Find::open(&ed, "");
        find.query = "rail pass, jap".into();
        find.delete_word();
        assert_eq!(find.query, "rail pass, ");
    }
}
