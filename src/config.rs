//! `~/.omanote/config.toml`: how the page is laid out. Everything is optional;
//! `omanote --config` writes a commented file to start from.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Widest the text column gets, in characters. 0 = as wide as the window.
    pub width: u16,
    pub align: Align,
    /// Blank columns kept free on both sides of the window.
    pub margin: u16,
}

impl Default for Config {
    fn default() -> Self {
        Config { width: 84, align: Align::Center, margin: 2 }
    }
}

const TEMPLATE: &str = r#"# omanote settings. Delete a line to get its default back.

# Widest the text column gets, in characters. Long lines are easier to read
# when they are not too long. 0 = use the whole window.
width = 84

# Where the text column sits when the window is wider than it:
# "left", "center" or "right".
align = "center"

# Blank columns always kept free at the window's edges.
margin = 2
"#;

pub fn path(home: &Path) -> PathBuf {
    home.join("config.toml")
}

/// Write the commented template unless a config is already there.
pub fn ensure(home: &Path) -> std::io::Result<PathBuf> {
    let file = path(home);
    if !file.exists() {
        std::fs::create_dir_all(home)?;
        std::fs::write(&file, TEMPLATE)?;
    }
    Ok(file)
}

/// The settings, and a complaint for every line that could not be used.
pub fn load(home: &Path) -> (Config, Vec<String>) {
    parse(&std::fs::read_to_string(path(home)).unwrap_or_default())
}

fn parse(text: &str) -> (Config, Vec<String>) {
    let mut config = Config::default();
    let mut problems = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
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
            "align" => match value.to_lowercase().as_str() {
                "left" => config.align = Align::Left,
                "center" | "centre" => config.align = Align::Center,
                "right" => config.align = Align::Right,
                _ => problems.push(format!("line {}: align is \"left\", \"center\" or \"right\"", n + 1)),
            },
            other => problems.push(format!("line {}: unknown setting `{other}`", n + 1)),
        }
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
        assert_eq!((config, problems.len()), (Config { width: 0, align: Align::Left, margin: 4 }, 0));

        let (config, problems) = parse("width = wide\nalign = middle\ncolour = red\njunk\nmargin = 1");
        assert_eq!(config, Config { margin: 1, ..Config::default() }, "bad lines fall back to defaults");
        assert_eq!(problems.len(), 4);
        assert!(problems[0].contains("line 1") && problems[2].contains("colour"));
    }
}
