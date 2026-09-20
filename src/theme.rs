//! Colours for the app's own chrome: footer, picker, prompts.
//!
//! The document is styled with the terminal's 16 named colours, so it follows
//! whatever theme the terminal has. Chrome needs two things those cannot give:
//! a surface a little apart from the background, and secondary text that is
//! quiet but still readable ("bright black" is nearly invisible in many
//! themes). Both are derived from the terminal's real background and
//! foreground, which we ask it for at startup. No answer: plain fallback.

use std::io::Write;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier, Style};

type Rgb = (u8, u8, u8);

pub struct Theme {
    /// Derived colours are in use (the terminal told us its colours).
    pub rich: bool,
    surface: Option<Color>,
    raised: Option<Color>,
    muted: Color,
    faint: Color,
    shadow: Option<Color>,
}

static THEME: OnceLock<Theme> = OnceLock::new();

pub fn get() -> &'static Theme {
    THEME.get_or_init(|| Theme::new(None))
}

pub fn init(colors: Option<(Rgb, Rgb)>) {
    let _ = THEME.set(Theme::new(colors));
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Color {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::Rgb(ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

impl Theme {
    fn new(colors: Option<(Rgb, Rgb)>) -> Self {
        match colors {
            Some((fg, bg)) => Theme {
                rich: true,
                surface: Some(mix(bg, fg, 0.07)),
                raised: Some(mix(bg, fg, 0.20)),
                muted: mix(fg, bg, 0.38),
                faint: mix(fg, bg, 0.66),
                shadow: Some(mix(bg, (0, 0, 0), 0.45)),
            },
            None => Theme { rich: false, surface: None, raised: None, muted: Color::DarkGray, faint: Color::DarkGray, shadow: None },
        }
    }

    /// Background of the footer and of panels.
    pub fn surface(&self) -> Style {
        self.surface.map_or_else(Style::default, |c| Style::new().bg(c))
    }

    /// The selected row of a list.
    pub fn raised(&self) -> Style {
        match self.raised {
            Some(c) => Style::new().bg(c).add_modifier(Modifier::BOLD),
            None => Style::new().add_modifier(Modifier::REVERSED),
        }
    }

    /// Secondary text: labels, counts, paths.
    pub fn muted(&self) -> Style {
        Style::new().fg(self.muted)
    }

    /// What a popup casts on the page behind it: darker than the background.
    pub fn shadow(&self) -> Option<Color> {
        self.shadow
    }

    /// Rules and separators.
    pub fn faint(&self) -> Style {
        Style::new().fg(self.faint)
    }
}

fn hex(text: &str) -> Option<Rgb> {
    let t = text.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(t.get(i..i + 2)?, 16).ok();
    (t.len() == 6).then(|| Some((byte(0)?, byte(2)?, byte(4)?)))?
}

/// `rgb:c0c0/caca/f5f5` (also 2-digit components) inside an OSC reply.
fn osc_color(reply: &str, code: &str) -> Option<Rgb> {
    let rest = &reply[reply.find(&format!("]{code};rgb:"))? + code.len() + 6..];
    let mut parts = rest.split(|c: char| !c.is_ascii_hexdigit()).filter(|p| !p.is_empty());
    let mut next = || u8::from_str_radix(parts.next()?.get(..2)?, 16).ok();
    Some((next()?, next()?, next()?))
}

/// Ask the terminal for its foreground and background. Must run in raw mode
/// and before anything else reads the keyboard, because the answer arrives on
/// stdin. A device-attributes query rides along: every terminal answers that
/// one, so we know when to stop waiting instead of sitting out the timeout.
///
/// `OMANOTE_COLORS="#c0caf5,#1a1b26"` (foreground, background) skips the
/// question; `OMANOTE_COLORS=plain` turns derived colours off.
pub fn detect() -> Option<(Rgb, Rgb)> {
    if let Ok(given) = std::env::var("OMANOTE_COLORS") {
        let (fg, bg) = given.split_once(',')?;
        return Some((hex(fg)?, hex(bg)?));
    }
    if unsafe { libc::isatty(0) } != 1 {
        return None;
    }
    let mut out = std::io::stdout();
    out.write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c").ok()?;
    out.flush().ok()?;

    let deadline = Instant::now() + Duration::from_millis(200);
    let mut reply = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        let mut fd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        if left == 0 || unsafe { libc::poll(&mut fd, 1, left) } <= 0 {
            break;
        }
        let mut chunk = [0u8; 256];
        let n = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            break;
        }
        reply.extend_from_slice(&chunk[..n as usize]);
        // The device-attributes answer, `ESC [ ? … c`, comes last.
        if let Some(at) = reply.windows(3).position(|w| w == b"\x1b[?") {
            if reply[at..].contains(&b'c') {
                break;
            }
        }
    }
    let reply = String::from_utf8_lossy(&reply);
    Some((osc_color(&reply, "10")?, osc_color(&reply, "11")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_terminals_answer() {
        let reply = "\x1b]10;rgb:c0c0/caca/f5f5\x1b\\\x1b]11;rgb:1a1a/1b1b/2626\x07\x1b[?62;22c";
        assert_eq!(osc_color(reply, "10"), Some((0xc0, 0xca, 0xf5)));
        assert_eq!(osc_color(reply, "11"), Some((0x1a, 0x1b, 0x26)));
        assert_eq!(osc_color("\x1b]11;rgb:ff/80/00\x07", "11"), Some((255, 128, 0)));
        assert_eq!(osc_color("\x1b[?62c", "11"), None);
        assert_eq!(hex("#1a1b26"), Some((0x1a, 0x1b, 0x26)));
        assert_eq!(hex("nope"), None);
    }

    #[test]
    fn derives_a_surface_between_background_and_foreground() {
        let dark = Theme::new(Some(((192, 202, 245), (26, 27, 38))));
        let Some(Color::Rgb(r, g, b)) = dark.surface else { panic!() };
        assert!(r > 26 && r < 60 && g > 27 && b > 38, "a little lighter than a dark background: {r},{g},{b}");
        let light = Theme::new(Some(((30, 30, 30), (250, 250, 250))));
        let Some(Color::Rgb(r, ..)) = light.surface else { panic!() };
        assert!(r < 250 && r > 220, "a little darker than a light one: {r}");
        assert!(!Theme::new(None).rich);
        assert_eq!(Theme::new(None).raised(), Style::new().add_modifier(Modifier::REVERSED));
    }
}
