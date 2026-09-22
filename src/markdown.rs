//! Line-oriented markdown styling.
//!
//! Every source character gets a `CharCell`: its style, whether it is syntax
//! that should be concealed when the line is not being edited, and an optional
//! replacement glyph (bullets, checkboxes, quote bars). The layout step decides
//! whether to honour `hidden`/`repl` based on whether the line is revealed.

use ratatui::style::{Modifier, Style};

use crate::look::{self, El};
use crate::table;

pub const CHECK_OPEN: &str = "\u{f0131}";
pub const CHECK_DONE: &str = "\u{f0135}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Normal,
    /// A pipe-table row; `start..end` are the table's lines (header, separator, body).
    Table { start: usize, end: usize },
    FenceOpen,
    FenceClose,
    Code,
    /// A line of a file that is not markdown: shown exactly as it is.
    Plain,
    /// The same, for a `# comment` line of a config file: shown quieter.
    Comment,
}

#[derive(Clone, Debug, Default)]
pub struct CharCell {
    pub style: Style,
    pub hidden: bool,
    pub repl: Option<&'static str>,
}

pub struct StyledLine {
    pub cells: Vec<CharCell>,
    /// Virtual text drawn before every visual row of the line (code block gutter).
    pub prefix: Option<(&'static str, Style)>,
    /// Wrapped rows hang-indent to the display width of everything before this column.
    pub hang_col: usize,
    pub quote: bool,
    pub rule: bool,
}

/// Markdown's own characters, where they show (the line being edited).
pub fn marker() -> Style {
    look::of(El::Syntax)
}

fn heading(level: usize) -> Style {
    look::heading(level)
}

fn link_style() -> Style {
    look::of(El::Link)
}

pub fn indent(chars: &[char]) -> usize {
    chars.iter().take_while(|c| **c == ' ' || **c == '\t').count()
}

fn run_len(chars: &[char], at: usize, end: usize, c: char) -> usize {
    chars[at..end].iter().take_while(|x| **x == c).count()
}

fn fence_char(line: &[char]) -> Option<char> {
    let i = indent(line);
    let c = *line.get(i)?;
    ((c == '`' || c == '~') && run_len(line, i, line.len(), c) >= 3).then_some(c)
}

/// Classify each line as normal text or part of a fenced code block.
pub fn classify(lines: &[Vec<char>]) -> Vec<Block> {
    let mut out = Vec::with_capacity(lines.len());
    let mut open: Option<char> = None;
    for line in lines {
        let fence = fence_char(line);
        let block = match (open, fence) {
            (None, Some(c)) => {
                open = Some(c);
                Block::FenceOpen
            }
            (Some(o), Some(c)) if o == c && line.iter().all(|x| *x == c || x.is_whitespace()) => {
                open = None;
                Block::FenceClose
            }
            (Some(_), _) => Block::Code,
            (None, None) => Block::Normal,
        };
        out.push(block);
    }

    let mut i = 0;
    while i + 1 < lines.len() {
        let header = out[i] == Block::Normal
            && out[i + 1] == Block::Normal
            && table::is_row(&lines[i])
            && !table::cells(&lines[i]).is_empty()
            && !table::is_separator(&lines[i])
            && table::is_separator(&lines[i + 1]);
        if !header {
            i += 1;
            continue;
        }
        let mut end = i + 2;
        while end < lines.len() && out[end] == Block::Normal && table::is_row(&lines[end]) {
            end += 1;
        }
        out[i..end].fill(Block::Table { start: i, end });
        i = end;
    }
    out
}

/// Column of the `x`/space inside a `- [ ] ` task marker.
pub fn task_mark(chars: &[char]) -> Option<usize> {
    let i = indent(chars);
    let ok = chars.len() >= i + 5
        && matches!(chars[i], '-' | '*' | '+')
        && chars[i + 1] == ' '
        && chars[i + 2] == '['
        && matches!(chars[i + 3], ' ' | 'x' | 'X')
        && chars[i + 4] == ']'
        && chars.get(i + 5).is_none_or(|c| *c == ' ');
    ok.then_some(i + 3)
}

fn is_bullet(chars: &[char], i: usize) -> bool {
    i + 1 < chars.len() && matches!(chars[i], '-' | '*' | '+') && chars[i + 1] == ' '
}

/// `12. ` / `3) ` at `i`: returns (index of the trailing space, number).
fn ordered(chars: &[char], i: usize) -> Option<(usize, u64)> {
    let digits = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let end = i + digits;
    if matches!(chars.get(end), Some('.' | ')')) && chars.get(end + 1) == Some(&' ') {
        let n = chars[i..end].iter().collect::<String>().parse().ok()?;
        Some((end + 1, n))
    } else {
        None
    }
}

fn is_rule(rest: &[char]) -> bool {
    let Some(&c) = rest.first() else { return false };
    matches!(c, '-' | '*' | '_')
        && rest.iter().filter(|x| **x == c).count() >= 3
        && rest.iter().all(|x| *x == c || *x == ' ')
}

/// Prefix of the target `image_line` returns for a reference-style image, `![alt][label]`.
pub const REF: &str = "#ref:";

/// `[label]: data:image/png;base64,…` — an image embedded in the note itself.
/// Returns the (lowercased) label and the data URI.
pub fn data_definition(line: &str) -> Option<(String, &str)> {
    let (label, uri) = line.strip_prefix('[')?.split_once("]:")?;
    let uri = uri.trim();
    let embedded = uri.starts_with("data:image/") && uri.contains(";base64,") && !label.contains(['[', ']']);
    embedded.then(|| (label.trim().to_lowercase(), uri))
}

/// An ordinary reference definition, `[label]: target`.
pub fn definition(chars: &[char]) -> Option<(String, String)> {
    if chars.first() != Some(&'[') || chars.len() > 2048 {
        return None;
    }
    let text: String = chars.iter().collect();
    let (label, target) = text[1..].split_once("]:")?;
    let target = target.trim().trim_start_matches('<').trim_end_matches('>');
    (!label.contains(['[', ']']) && !target.is_empty() && !target.contains(' ')).then(|| (label.trim().to_lowercase(), target.to_string()))
}

/// The file a line points at, if the whole line is one image: `![alt](pic.png)`,
/// optionally with a "title", Obsidian's `![[pic.png]]` / `![[pic.png|alt]]`, or
/// the reference form `![alt][label]` (returned as `#ref:label`).
pub fn image_line(chars: &[char]) -> Option<String> {
    let i = indent(chars);
    if chars.get(i) != Some(&'!') || chars.get(i + 1) != Some(&'[') {
        return None;
    }
    let text: String = chars[i..].iter().collect();
    let text = text.trim_end();
    let target = if let Some(inner) = text.strip_prefix("![[").and_then(|t| t.strip_suffix("]]")) {
        inner.split('|').next()?
    } else if let Some(inner) = text.strip_suffix(']') {
        let (alt, label) = inner[2..].split_once("][")?;
        let plain = !label.is_empty() && !label.contains(['[', ']']) && !alt.contains("](");
        return plain.then(|| format!("{REF}{}", label.trim().to_lowercase()));
    } else {
        let inner = text.strip_suffix(')')?;
        let target = &inner[inner.find("](")? + 2..];
        if target.contains(')') || target.contains("](") {
            return None;
        }
        target.find(" \"").map_or(target, |q| &target[..q])
    };
    let target = target.trim().trim_start_matches('<').trim_end_matches('>');
    crate::images::is_image_path(target).then(|| target.to_string())
}

/// What pressing Enter on this line should start the next line with:
/// (marker, column where the item's own text starts).
pub fn continuation(chars: &[char]) -> Option<(Vec<char>, usize)> {
    let i = indent(chars);
    let lead: String = chars[..i].iter().collect();
    if let Some(mark) = task_mark(chars) {
        let start = (mark + 3).min(chars.len());
        return Some((format!("{lead}{} [ ] ", chars[i]).chars().collect(), start));
    }
    if is_bullet(chars, i) {
        return Some((format!("{lead}{} ", chars[i]).chars().collect(), i + 2));
    }
    if let Some((space, n)) = ordered(chars, i) {
        let sep = chars[space - 1];
        return Some((format!("{lead}{}{sep} ", n + 1).chars().collect(), space + 1));
    }
    if chars.get(i) == Some(&'>') {
        let start = if chars.get(i + 1) == Some(&' ') { i + 2 } else { i + 1 };
        return Some((format!("{lead}> ").chars().collect(), start));
    }
    None
}

pub fn style_line(chars: &[char], block: Block) -> StyledLine {
    let n = chars.len();
    let mut sl = StyledLine {
        cells: vec![CharCell::default(); n],
        prefix: None,
        hang_col: 0,
        quote: false,
        rule: false,
    };

    match block {
        Block::Plain => {
            for cell in &mut sl.cells {
                cell.style = look::of(El::Text);
            }
            return sl;
        }
        Block::Comment => {
            for cell in &mut sl.cells {
                cell.style = crate::theme::get().muted();
            }
            return sl;
        }
        Block::Code => {
            sl.prefix = Some(("│ ", marker()));
            for cell in &mut sl.cells {
                cell.style = look::of(El::CodeBlock);
            }
            return sl;
        }
        Block::FenceOpen | Block::FenceClose => {
            sl.prefix = Some((if block == Block::FenceOpen { "╭ " } else { "╰ " }, marker()));
            let i = indent(chars);
            let run = run_len(chars, i, n, chars[i]);
            for k in 0..n {
                sl.cells[k].style = marker().add_modifier(Modifier::ITALIC);
                sl.cells[k].hidden = k >= i && k < i + run;
            }
            return sl;
        }
        Block::Normal | Block::Table { .. } => {}
    }

    let mut i = indent(chars);
    if is_rule(&chars[i..]) {
        sl.rule = true;
        for cell in &mut sl.cells {
            cell.style = marker();
        }
        return sl;
    }

    let hashes = run_len(chars, i, n, '#');
    if (1..=6).contains(&hashes) && chars.get(i + hashes).is_none_or(|c| *c == ' ') {
        let start = (i + hashes + 1).min(n);
        for k in i..start {
            hide(&mut sl.cells, k);
        }
        inline(chars, start, n, heading(hashes), &mut sl.cells);
        return sl;
    }

    // Ordinary text has a style too, and everything else is laid over it.
    let mut base = look::of(El::Text);
    while i < n && chars[i] == '>' {
        sl.cells[i].repl = Some("▎");
        sl.cells[i].style = look::of(El::QuoteBar);
        i += 1;
        if i < n && chars[i] == ' ' {
            i += 1;
        }
        sl.quote = true;
        sl.hang_col = i;
        base = base.patch(look::of(El::Quote));
    }

    if let Some(mark) = task_mark(chars) {
        let done = chars[mark] != ' ';
        for k in [mark - 3, mark - 2, mark - 1, mark + 1] {
            hide(&mut sl.cells, k);
        }
        sl.cells[mark].repl = Some(if done { CHECK_DONE } else { CHECK_OPEN });
        sl.cells[mark].style = look::of(if done { El::TaskDone } else { El::Task });
        if done {
            base = look::of(El::TaskDoneText);
        }
        i = (mark + 3).min(n);
        sl.hang_col = i;
    } else if is_bullet(chars, i) {
        sl.cells[i].repl = Some("•");
        sl.cells[i].style = look::of(El::List);
        i += 2;
        sl.hang_col = i;
    } else if let Some((space, _)) = ordered(chars, i) {
        for k in i..space {
            sl.cells[k].style = look::of(El::List);
        }
        i = space + 1;
        sl.hang_col = i;
    }

    inline(chars, i, n, base, &mut sl.cells);

    if image_line(chars).is_some() {
        // The alt text reads as a caption for the picture drawn below, not as a link.
        for cell in sl.cells.iter_mut().filter(|c| !c.hidden) {
            cell.style = marker().add_modifier(Modifier::ITALIC);
        }
    }

    if chars.get(i) == Some(&'|') {
        let separator = chars[i..].iter().all(|c| matches!(c, '|' | ':' | '-' | ' '));
        for k in i..n {
            if separator || chars[k] == '|' {
                sl.cells[k].style = marker();
            }
        }
    }
    sl
}

fn hide(cells: &mut [CharCell], k: usize) {
    cells[k].hidden = true;
    cells[k].style = marker();
}

fn delim_style(c: char, len: usize) -> Style {
    match (c, len) {
        ('~', _) => look::of(El::Strike),
        ('=', _) => look::of(El::Highlight),
        (_, 1) => look::of(El::Italic),
        (_, 2) => look::of(El::Bold),
        _ => look::of(El::Bold).patch(look::of(El::Italic)),
    }
}

/// Find the closing run for an emphasis-style delimiter opening at `i`.
fn find_close(ch: &[char], i: usize, end: usize, c: char, len: usize) -> Option<usize> {
    if run_len(ch, i, end, c) != len {
        return None;
    }
    let s = i + len;
    if s >= end || ch[s].is_whitespace() {
        return None;
    }
    if c == '_' && i > 0 && ch[i - 1].is_alphanumeric() {
        return None;
    }
    let mut j = s + 1;
    while j < end {
        if ch[j] != c {
            j += 1;
            continue;
        }
        let r = run_len(ch, j, end, c);
        let intraword = c == '_' && j + r < end && ch[j + r].is_alphanumeric();
        if r == len && !ch[j - 1].is_whitespace() && !intraword {
            return Some(j);
        }
        j += r;
    }
    None
}

/// `[text](url)` or `[[target|alias]]` starting at `i`. Returns the index after it.
/// Where a link that opens at `ch[i] == '['` closes: `(wiki, j, k)`. For
/// `[[target]]`, `j` is the first `]` and `k` the second. For `[text](url)`
/// and `[text][label]`, `j` closes the text and `k` closes the target.
fn link_span(ch: &[char], i: usize, end: usize) -> Option<(bool, usize, usize)> {
    if i + 1 < end && ch[i + 1] == '[' {
        let j = (i + 2..end.saturating_sub(1)).find(|&k| ch[k] == ']' && ch[k + 1] == ']')?;
        return (j != i + 2).then_some((true, j, j + 1));
    }
    let mut depth = 0usize;
    let mut close = None;
    for k in i..end {
        match ch[k] {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(k);
                    break;
                }
            }
            _ => {}
        }
    }
    let j = close?;
    if j == i + 1 || j + 1 >= end || !matches!(ch[j + 1], '(' | '[') {
        return None;
    }
    // `[text](url)`, or the reference form `[text][label]`.
    let closer = if ch[j + 1] == '(' { ')' } else { ']' };
    let k = (j + 2..end).find(|&k| ch[k] == closer)?;
    (k != j + 2).then_some((false, j, k))
}

fn link(ch: &[char], i: usize, end: usize, base: Style, cells: &mut [CharCell]) -> Option<usize> {
    let (wiki, j, k) = link_span(ch, i, end)?;
    if wiki {
        for m in [i, i + 1, j, k] {
            hide(cells, m);
        }
        for cell in &mut cells[i + 2..j] {
            cell.style = base.patch(link_style());
        }
        if let Some(p) = (i + 2..j).find(|&m| ch[m] == '|') {
            for m in i + 2..=p {
                hide(cells, m);
            }
        }
        return Some(k + 1);
    }
    hide(cells, i);
    for m in j..=k {
        hide(cells, m);
    }
    inline(ch, i + 1, j, base.patch(link_style()), cells);
    Some(k + 1)
}

/// What a link points at, as written in the note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTo {
    /// `[text](target)`: a web address or a file.
    Target(String),
    /// `[[note]]`: a note, by name.
    Wiki(String),
    /// `[text][label]`: defined on a `[label]: target` line somewhere else.
    Label(String),
}

/// The link the cursor is in (or touching the end of), or a bare web address.
pub fn link_at(ch: &[char], col: usize) -> Option<LinkTo> {
    let text = |a: usize, b: usize| ch[a..b].iter().collect::<String>();
    let mut i = 0;
    while i < ch.len() {
        let Some((wiki, j, k)) = (ch[i] == '[').then(|| link_span(ch, i, ch.len())).flatten() else {
            i += 1;
            continue;
        };
        if (i..=k + 1).contains(&col) {
            return Some(match (wiki, ch[j + 1]) {
                (true, _) => LinkTo::Wiki(text(i + 2, j)),
                (false, '(') => LinkTo::Target(text(j + 2, k)),
                (false, _) => LinkTo::Label(text(j + 2, k)),
            });
        }
        i = k + 1;
    }
    // No markup: the word under the cursor, if it is an address.
    let from = (0..col.min(ch.len())).rev().find(|&m| ch[m].is_whitespace()).map_or(0, |m| m + 1);
    let to = (col.min(ch.len())..ch.len()).find(|&m| ch[m].is_whitespace()).unwrap_or(ch.len());
    let word = text(from, to);
    let word = word.trim_matches(|c: char| "<>()[]\"'".contains(c)).trim_end_matches(['.', ',', ';', ':', '!', '?']);
    (word.starts_with("http://") || word.starts_with("https://")).then(|| LinkTo::Target(word.to_string()))
}

fn starts_with(ch: &[char], i: usize, end: usize, s: &str) -> bool {
    let len = s.chars().count();
    i + len <= end && ch[i..i + len].iter().copied().eq(s.chars())
}

pub fn inline(ch: &[char], start: usize, end: usize, base: Style, cells: &mut [CharCell]) {
    for cell in &mut cells[start..end] {
        cell.style = base;
    }
    let mut i = start;
    while i < end {
        let c = ch[i];
        match c {
            '\\' if i + 1 < end && ch[i + 1].is_ascii_punctuation() => {
                hide(cells, i);
                i += 2;
            }
            '`' => {
                let run = run_len(ch, i, end, '`');
                let mut j = i + run;
                let mut close = None;
                while j < end {
                    if ch[j] == '`' {
                        let r = run_len(ch, j, end, '`');
                        if r == run {
                            close = Some(j);
                            break;
                        }
                        j += r;
                    } else {
                        j += 1;
                    }
                }
                match close {
                    Some(j) if j > i + run => {
                        for k in (i..i + run).chain(j..j + run) {
                            hide(cells, k);
                        }
                        for cell in &mut cells[i + run..j] {
                            cell.style = look::of(El::Code);
                        }
                        i = j + run;
                    }
                    _ => i += run,
                }
            }
            '*' | '_' | '~' | '=' => {
                let lens: &[usize] = match c {
                    '*' => &[3, 2, 1],
                    '_' => &[2, 1],
                    _ => &[2],
                };
                let hit = lens.iter().find_map(|&len| find_close(ch, i, end, c, len).map(|j| (len, j)));
                match hit {
                    Some((len, j)) => {
                        for k in (i..i + len).chain(j..j + len) {
                            hide(cells, k);
                        }
                        inline(ch, i + len, j, base.patch(delim_style(c, len)), cells);
                        i = j + len;
                    }
                    None => i += run_len(ch, i, end, c),
                }
            }
            '!' if i + 1 < end && ch[i + 1] == '[' => match link(ch, i + 1, end, base, cells) {
                Some(next) => {
                    hide(cells, i);
                    i = next;
                }
                None => i += 1,
            },
            '[' => match link(ch, i, end, base, cells) {
                Some(next) => i = next,
                None => i += 1,
            },
            'h' if (starts_with(ch, i, end, "http://") || starts_with(ch, i, end, "https://"))
                && (i == 0 || !ch[i - 1].is_alphanumeric()) =>
            {
                let j = (i..end).find(|&k| ch[k].is_whitespace() || matches!(ch[k], ')' | '>' | ']')).unwrap_or(end);
                for cell in &mut cells[i..j] {
                    cell.style = base.patch(link_style());
                }
                i = j;
            }
            '#' if (i == 0 || ch[i - 1].is_whitespace()) && i + 1 < end && ch[i + 1].is_alphabetic() => {
                let j = (i + 1..end)
                    .find(|&k| !(ch[k].is_alphanumeric() || matches!(ch[k], '-' | '_' | '/')))
                    .unwrap_or(end);
                for cell in &mut cells[i..j] {
                    cell.style = base.patch(look::of(El::Tag));
                }
                i = j;
            }
            _ => i += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible(src: &str) -> String {
        let chars: Vec<char> = src.chars().collect();
        let sl = style_line(&chars, Block::Normal);
        chars
            .iter()
            .zip(&sl.cells)
            .filter(|(_, cell)| !cell.hidden)
            .map(|(c, cell)| cell.repl.map(str::to_string).unwrap_or(c.to_string()))
            .collect()
    }

    #[test]
    fn conceals_inline_syntax() {
        assert_eq!(visible("some **bold** and *it* and `code`"), "some bold and it and code");
        assert_eq!(visible("a [link](https://x.y) b"), "a link b");
        assert_eq!(visible("[[target|alias]] and [[plain]]"), "alias and plain");
        assert_eq!(visible("***both*** ~~gone~~ ==hl=="), "both gone hl");
        assert_eq!(visible("## Title"), "Title");
    }

    #[test]
    fn leaves_non_syntax_alone() {
        assert_eq!(visible("snake_case_name stays"), "snake_case_name stays");
        assert_eq!(visible("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(visible("a lone ** and [brackets]"), "a lone ** and [brackets]");
        assert_eq!(visible("#notaheading"), "#notaheading");
    }

    #[test]
    fn bold_keeps_nested_italic() {
        let chars: Vec<char> = "**a *b* c**".chars().collect();
        let sl = style_line(&chars, Block::Normal);
        let b = &sl.cells[5];
        assert!(b.style.add_modifier.contains(Modifier::BOLD | Modifier::ITALIC));
    }

    #[test]
    fn tasks_and_lists() {
        assert_eq!(visible("- [ ] todo"), format!("{CHECK_OPEN} todo"));
        assert_eq!(visible("  - [x] done"), format!("  {CHECK_DONE} done"));
        assert_eq!(visible("- item"), "• item");
        assert_eq!(task_mark(&"- [x] a".chars().collect::<Vec<_>>()), Some(3));
        assert_eq!(task_mark(&"- [y] a".chars().collect::<Vec<_>>()), None);
    }

    #[test]
    fn continues_lists() {
        let cont = |s: &str| continuation(&s.chars().collect::<Vec<_>>()).map(|(m, c)| (m.iter().collect::<String>(), c));
        assert_eq!(cont("- [x] a"), Some(("- [ ] ".into(), 6)));
        assert_eq!(cont("  * a"), Some(("  * ".into(), 4)));
        assert_eq!(cont("9. a"), Some(("10. ".into(), 3)));
        assert_eq!(cont("> q"), Some(("> ".into(), 2)));
        assert_eq!(cont("plain"), None);
    }

    #[test]
    fn spots_image_lines() {
        let img = |s: &str| image_line(&s.chars().collect::<Vec<_>>());
        assert_eq!(img("![a cat](pics/cat.png)"), Some("pics/cat.png".into()));
        assert_eq!(img("  ![](<my cat.JPG> \"title\")  "), Some("my cat.JPG".into()));
        assert_eq!(img("![[shot.webp|300]]"), Some("shot.webp".into()));
        assert_eq!(img("![[some note]]"), None);
        assert_eq!(img("![a](https://x.y/cat?w=300)"), Some("https://x.y/cat?w=300".into()));
        assert_eq!(img("see ![a](cat.png)"), None);
        assert_eq!(img("![a](cat.png) and ![b](dog.png)"), None);
        assert_eq!(img("[a](cat.png)"), None);
        assert_eq!(img("![pasted image][Img1]"), Some("#ref:img1".into()));
        assert_eq!(img("![a][]"), None);
        assert_eq!(visible("![pasted image][img1]"), "pasted image");
        assert_eq!(visible("see [the docs][d] here"), "see the docs here");
    }

    #[test]
    fn reads_definitions() {
        let data = "[Img1]: data:image/png;base64,AAAA";
        assert_eq!(data_definition(data), Some(("img1".into(), "data:image/png;base64,AAAA")));
        assert_eq!(data_definition("[logo]: ./logo.png"), None);
        assert_eq!(data_definition("not [a]: data:image/png;base64,AA"), None);
        let def = |s: &str| definition(&s.chars().collect::<Vec<_>>());
        assert_eq!(def("[Logo]: <pics/logo.png>"), Some(("logo".into(), "pics/logo.png".into())));
        assert_eq!(def("[x]: two words"), None);
        assert_eq!(def("plain"), None);
    }

    #[test]
    fn fences() {
        let lines: Vec<Vec<char>> = ["a", "```rs", "**x**", "```", "b"].iter().map(|s| s.chars().collect()).collect();
        assert_eq!(
            classify(&lines),
            [Block::Normal, Block::FenceOpen, Block::Code, Block::FenceClose, Block::Normal]
        );
    }
    #[test]
    fn finds_the_link_under_the_cursor() {
        let line: Vec<char> = "see [the plan](trips/plan.md), [[Ideas|mine]] and [docs][d] or https://omarchy.org.".chars().collect();
        let at = |needle: &str| line.iter().collect::<String>().find(needle).unwrap();
        assert_eq!(link_at(&line, 1), None);
        assert_eq!(link_at(&line, at("[the")), Some(LinkTo::Target("trips/plan.md".into())));
        assert_eq!(link_at(&line, at("plan]")), Some(LinkTo::Target("trips/plan.md".into())), "on the text, as when the syntax is hidden");
        assert_eq!(link_at(&line, at(", [[")), Some(LinkTo::Target("trips/plan.md".into())), "right after it, where a mention leaves the cursor");
        assert_eq!(link_at(&line, at("Ideas")), Some(LinkTo::Wiki("Ideas|mine".into())));
        assert_eq!(link_at(&line, at("docs]")), Some(LinkTo::Label("d".into())));
        assert_eq!(link_at(&line, at("omarchy")), Some(LinkTo::Target("https://omarchy.org".into())), "a bare address, without the full stop");
        assert_eq!(link_at(&line, at(" and ") + 2), None);
    }

}
