//! How the text of a note looks: a style per element, each with a default and
//! whatever the settings change about it.
//!
//!     editor.style.h1.color = "#ff9e64"
//!     editor.style.h1.underline = false
//!     editor.style.code.background = "#1f2335"
//!
//! Colour and emphasis are all there is to change. The size and family of the
//! font belong to the terminal, not to a program running in it.

use std::sync::RwLock;

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum El {
    Text,
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
    Bold,
    Italic,
    Strike,
    Highlight,
    Code,
    CodeBlock,
    Link,
    Tag,
    Quote,
    QuoteBar,
    List,
    Task,
    TaskDone,
    TaskDoneText,
    Syntax,
    TableBorder,
    TableHeader,
    Rule,
    Spelling,
}

/// Every element, the name it has in the settings, and what it is.
pub const ELEMENTS: [(El, &str, &str); 26] = [
    (El::Text, "text", "ordinary text"),
    (El::H1, "h1", "# heading"),
    (El::H2, "h2", "## heading"),
    (El::H3, "h3", "### heading"),
    (El::H4, "h4", "#### heading"),
    (El::H5, "h5", "##### heading"),
    (El::H6, "h6", "###### heading"),
    (El::Bold, "bold", "**bold**"),
    (El::Italic, "italic", "*italic*"),
    (El::Strike, "strike", "~~struck out~~"),
    (El::Highlight, "highlight", "==highlighted=="),
    (El::Code, "code", "`inline code`"),
    (El::CodeBlock, "codeblock", "fenced code"),
    (El::Link, "link", "[links](…) and [[wiki links]]"),
    (El::Tag, "tag", "#tags"),
    (El::Quote, "quote", "> quoted text"),
    (El::QuoteBar, "quote.bar", "the bar beside a quote"),
    (El::List, "list", "bullets and numbers"),
    (El::Task, "task", "an open checkbox"),
    (El::TaskDone, "task.done", "a ticked checkbox"),
    (El::TaskDoneText, "task.done.text", "the text of a ticked task"),
    (El::Syntax, "syntax", "markdown's own characters, where they show"),
    (El::TableBorder, "table.border", "the lines of a table"),
    (El::TableHeader, "table.header", "a table's header row"),
    (El::Rule, "rule", "--- a horizontal rule"),
    (El::Spelling, "spelling", "a misspelt word, or a slip of grammar"),
];

fn default(el: El) -> Style {
    let bold = |color: Color| Style::new().fg(color).add_modifier(Modifier::BOLD);
    match el {
        El::Text => Style::default(),
        El::H1 => bold(Color::Magenta).add_modifier(Modifier::UNDERLINED),
        El::H2 => bold(Color::Blue),
        El::H3 => bold(Color::Cyan),
        El::H4 => bold(Color::Green),
        El::H5 | El::H6 => bold(Color::Yellow),
        El::Bold | El::TableHeader => Style::new().add_modifier(Modifier::BOLD),
        El::Italic | El::Quote => Style::new().add_modifier(Modifier::ITALIC),
        El::Strike => Style::new().add_modifier(Modifier::CROSSED_OUT),
        El::Highlight => Style::new().fg(Color::Black).bg(Color::Yellow),
        El::Code => Style::new().fg(Color::Yellow),
        El::CodeBlock | El::TaskDone => Style::new().fg(Color::Green),
        El::Link => Style::new().fg(Color::Blue).add_modifier(Modifier::UNDERLINED),
        El::Tag => Style::new().fg(Color::Cyan),
        El::QuoteBar | El::List | El::Task => Style::new().fg(Color::Blue),
        El::TaskDoneText => Style::new().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT),
        El::Syntax | El::TableBorder | El::Rule => Style::new().fg(Color::DarkGray),
        El::Spelling => Style::new().add_modifier(Modifier::UNDERLINED).underline_color(Color::Red),
    }
}

/// What the settings change about one element. Anything not mentioned keeps its default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Change {
    fg: Option<Color>,
    bg: Option<Color>,
    on: Modifier,
    off: Modifier,
}

/// The settings' changes, by element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Look(Vec<(El, Change)>);

static LOOK: RwLock<Vec<(El, Change)>> = RwLock::new(Vec::new());

/// Make these the styles in use, from the next frame on.
pub fn apply(look: &Look) {
    if let Ok(mut current) = LOOK.write() {
        *current = look.0.clone();
    }
}

/// The style of an element: its default, with the settings' changes on top.
pub fn of(el: El) -> Style {
    let mut style = default(el);
    let changes = LOOK.read();
    let Some(change) = changes.as_ref().ok().and_then(|all| all.iter().find(|(e, _)| *e == el)).map(|(_, c)| *c) else { return style };
    // A misspelling is marked by its underline, so its colour is the underline's.
    if el == El::Spelling {
        style.underline_color = change.fg.or(style.underline_color);
    } else {
        style.fg = change.fg.or(style.fg);
    }
    style.bg = change.bg.or(style.bg);
    // A default that is switched off has to go from the style itself: a
    // "remove" left in it would also strip that emphasis from whatever this
    // style is later laid over (bold text inside a heading).
    style.add_modifier = (style.add_modifier | change.on) - change.off;
    style
}

pub fn heading(level: usize) -> Style {
    of([El::H1, El::H2, El::H3, El::H4, El::H5, El::H6][level.clamp(1, 6) - 1])
}

fn color(value: &str) -> Option<Color> {
    let name = value.to_lowercase().replace(['-', '_', ' '], "");
    let hex = |s: &str| u8::from_str_radix(s, 16).ok();
    if let Some(digits) = name.strip_prefix('#') {
        return match digits.len() {
            6 => Some(Color::Rgb(hex(&digits[0..2])?, hex(&digits[2..4])?, hex(&digits[4..6])?)),
            // #f80 is #ff8800
            3 => Some(Color::Rgb(hex(&digits[0..1])? * 17, hex(&digits[1..2])? * 17, hex(&digits[2..3])? * 17)),
            _ => None,
        };
    }
    if let Ok(index) = name.parse::<u8>() {
        return Some(Color::Indexed(index));
    }
    Some(match name.as_str() {
        "default" | "none" | "reset" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" | "purple" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::Gray,
        "gray" | "grey" | "brightblack" | "darkgray" | "darkgrey" => Color::DarkGray,
        "brightred" | "lightred" => Color::LightRed,
        "brightgreen" | "lightgreen" => Color::LightGreen,
        "brightyellow" | "lightyellow" => Color::LightYellow,
        "brightblue" | "lightblue" => Color::LightBlue,
        "brightmagenta" | "lightmagenta" => Color::LightMagenta,
        "brightcyan" | "lightcyan" => Color::LightCyan,
        "brightwhite" => Color::White,
        _ => return None,
    })
}

impl Look {
    /// One `editor.style.<element>.<property> = value` line; `key` is what
    /// follows `editor.style.`. `Err` says what is wrong with it.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        // `h1.font.color` and `h1.color` are the same thing.
        let parts: Vec<&str> = key.split('.').map(str::trim).filter(|p| *p != "font").collect();
        let Some((property, element)) = parts.split_last() else { return Err("which element? e.g. editor.style.h1.color".into()) };
        let element = element.join(".").to_lowercase();
        let property = property.to_lowercase();
        if ["size", "family", "face", "name", "height", "weight", "lineheight", "line_height"].contains(&property.as_str()) {
            return Err(format!("font {property} is the terminal's to set, not omanote's (Ghostty: font-size, font-family). Styles have color, background, bold, italic, underline, dim, strike"));
        }
        let Some(&(el, ..)) = ELEMENTS.iter().find(|(_, name, _)| *name == element) else {
            let names: Vec<&str> = ELEMENTS.iter().map(|(_, n, _)| *n).collect();
            return Err(format!("there is no element `{element}`; there are: {}", names.join(", ")));
        };
        // Worked out on a copy: a line that turns out to be wrong leaves nothing behind.
        let at = self.0.iter().position(|(e, _)| *e == el);
        let mut changed = at.map_or_else(Change::default, |at| self.0[at].1);
        let change = &mut changed;
        let mut keep = |changed: Change| match at {
            Some(at) => self.0[at].1 = changed,
            None => self.0.push((el, changed)),
        };
        let bad_color = || format!("`{value}` is not a colour: use a name (red, bright-blue, gray, default), \"#rrggbb\", or a number from 0 to 255");
        let emphasis = match property.as_str() {
            "color" | "colour" | "base_color" | "basecolor" | "fg" | "foreground" => {
                change.fg = Some(color(value).ok_or_else(bad_color)?);
                keep(changed);
                return Ok(());
            }
            "background" | "bg" | "background_color" => {
                change.bg = Some(color(value).ok_or_else(bad_color)?);
                keep(changed);
                return Ok(());
            }
            "bold" => Modifier::BOLD,
            "italic" => Modifier::ITALIC,
            "underline" | "underlined" => Modifier::UNDERLINED,
            "dim" => Modifier::DIM,
            "strike" | "strikethrough" => Modifier::CROSSED_OUT,
            other => return Err(format!("`{other}` is not something a style has; there are: color, background, bold, italic, underline, dim, strike")),
        };
        let on = match value.to_lowercase().as_str() {
            "true" | "yes" | "on" => true,
            "false" | "no" | "off" => false,
            _ => return Err(format!("{property} is true or false, not `{value}`")),
        };
        let (add, remove) = if on { (&mut change.on, &mut change.off) } else { (&mut change.off, &mut change.on) };
        add.insert(emphasis);
        remove.remove(emphasis);
        keep(changed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(lines: &[(&str, &str)]) -> Look {
        let mut look = Look::default();
        for (key, value) in lines {
            look.set(key, value).unwrap_or_else(|e| panic!("{key} = {value}: {e}"));
        }
        look
    }

    /// `of` reads a global, so everything that changes it lives in this one test.
    #[test]
    fn the_settings_change_what_they_name_and_nothing_else() {
        assert_eq!(of(El::H1), Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
        assert_eq!(heading(9), of(El::H6));

        apply(&look(&[
            ("h1.color", "#ff9e64"),
            ("h1.font.underline", "false"),
            ("h1.font.italic", "yes"),
            ("code.background", "#123"),
            ("text.font.base_color", "bright-white"),
            ("task.done.text.strike", "off"),
            ("Table.Border.colour", "240"),
        ]));
        assert_eq!(of(El::H1), Style::new().fg(Color::Rgb(255, 158, 100)).add_modifier(Modifier::BOLD | Modifier::ITALIC), "bold stays: nobody mentioned it");
        assert_eq!(of(El::Code), Style::new().fg(Color::Yellow).bg(Color::Rgb(17, 34, 51)));
        assert_eq!(of(El::Text), Style::new().fg(Color::White));
        assert_eq!(of(El::TaskDoneText), Style::new().fg(Color::DarkGray));
        assert_eq!(of(El::TableBorder), Style::new().fg(Color::Indexed(240)));
        assert_eq!(of(El::H2), Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD), "untouched");

        // Said twice: the last word counts.
        apply(&look(&[("bold.bold", "false"), ("bold.bold", "true"), ("link.color", "red"), ("link.color", "default")]));
        assert_eq!(of(El::Bold), Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(of(El::Link).fg, Some(Color::Reset));
        apply(&Look::default());
        assert_eq!(of(El::H1), default(El::H1), "and gone again when the settings no longer say so");
    }

    #[test]
    fn says_what_is_wrong() {
        let mut look = Look::default();
        let said = |look: &mut Look, key: &str, value: &str| look.set(key, value).unwrap_err();
        assert!(said(&mut look, "text.font.size", "14").contains("the terminal's to set"));
        assert!(said(&mut look, "h1.font.family", "JetBrains Mono").contains("font-family"));
        assert!(said(&mut look, "h7.color", "red").contains("no element `h7`"));
        assert!(said(&mut look, "h1.colour", "reddish").contains("not a colour"));
        assert!(said(&mut look, "h1.color", "#12345").contains("not a colour"));
        assert!(said(&mut look, "h1.bold", "very").contains("true or false"));
        assert!(said(&mut look, "h1.blink", "true").contains("not something a style has"));
        assert!(said(&mut look, "color", "red").contains("no element"));
        assert_eq!(look, Look::default(), "a line that is wrong changes nothing");
        assert!(ELEMENTS.iter().all(|(el, ..)| ELEMENTS.iter().filter(|(e, ..)| e == el).count() == 1));
    }
}
