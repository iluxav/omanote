//! System clipboard without a GUI dependency: wl-clipboard on a Wayland
//! desktop, OSC 52 everywhere else (which also works over SSH).

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

pub fn copy(text: &str) {
    if wayland() && wl_copy(text).is_some() {
        return;
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = out.flush();
}

fn wl_copy(text: &str) -> Option<()> {
    let mut child = Command::new("wl-copy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok()?;
    child.stdin.take()?.write_all(text.as_bytes()).ok()?;
    child.wait().ok()?.success().then_some(())
}

/// How often a selection that keeps changing (Shift+Down held) is published.
const PRIMARY_EVERY: Duration = Duration::from_millis(100);

/// What hands a selection (or `None`, none) to the desktop.
type SetPrimary = Box<dyn FnMut(Option<&str>)>;

/// The desktop's primary selection, kept to what is selected in the note, as
/// GUI apps do. Our selection is drawn by us, so the terminal never sees it:
/// without this, a middle click or a tool that works on "the selected text"
/// (omaestro's `om.selection()`) gets whatever was selected before, in
/// another app. Wayland only (`wl-copy --primary`): OSC 52 would make some
/// terminals ask for permission on every change.
pub struct Primary {
    /// What the desktop has from us: `None` for nothing (or cleared).
    published: Option<String>,
    at: Option<Instant>,
    set: SetPrimary,
}

impl Primary {
    pub fn new() -> Self {
        if wayland() { Self::with(Box::new(set_primary)) } else { Self::with(Box::new(|_| {})) }
    }

    fn with(set: SetPrimary) -> Self {
        Self { published: None, at: None, set }
    }

    /// Hands `selected` to the desktop when it changed: at once, unless the
    /// last change went out less than PRIMARY_EVERY ago. Returns whether a
    /// change is waiting, so the caller looks again soon.
    pub fn update(&mut self, selected: Option<String>) -> bool {
        if selected == self.published {
            return false;
        }
        if self.at.is_some_and(|at| at.elapsed() < PRIMARY_EVERY) {
            return true;
        }
        (self.set)(selected.as_deref());
        self.published = selected;
        self.at = Some(Instant::now());
        false
    }
}

/// The text as the primary selection, or (`None`) no primary selection. A
/// failure leaves the old one; there is nobody to tell.
fn set_primary(text: Option<&str>) {
    let mut wl_copy = Command::new("wl-copy");
    wl_copy.arg("--primary").stdout(Stdio::null()).stderr(Stdio::null());
    let Some(text) = text else {
        let _ = wl_copy.arg("--clear").status();
        return;
    };
    let Ok(mut child) = wl_copy.stdin(Stdio::piped()).spawn() else { return };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    let _ = child.wait();
}

/// System clipboard contents, if we can read them. Terminals that paste by
/// themselves (Ctrl+Shift+V) arrive as a bracketed-paste event instead.
pub fn paste() -> Option<String> {
    if !wayland() {
        return None;
    }
    let out = Command::new("wl-paste").arg("--no-newline").stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A picture on the clipboard (a screenshot, "copy image" in a browser), as
/// (mime type, bytes). Text wins when both are offered, since that is what a
/// paste into a text editor normally means.
pub fn image() -> Option<(String, Vec<u8>)> {
    if !wayland() {
        return None;
    }
    let listed = Command::new("wl-paste").arg("--list-types").stderr(Stdio::null()).output().ok()?;
    let mime = image_type(&String::from_utf8_lossy(&listed.stdout))?;
    let out = Command::new("wl-paste").args(["--type", &mime]).stderr(Stdio::null()).output().ok()?;
    (out.status.success() && !out.stdout.is_empty()).then_some((mime, out.stdout))
}

fn image_type(types: &str) -> Option<String> {
    let types: Vec<&str> = types.lines().map(str::trim).collect();
    if types.iter().any(|t| t.starts_with("text/plain") || *t == "UTF8_STRING" || *t == "text/uri-list") {
        return None;
    }
    ["image/png", "image/jpeg", "image/webp", "image/gif"].iter().find(|m| types.contains(*m)).map(|m| m.to_string())
}

pub fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u8);
    for b in text.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_round_trips() {
        for len in 0..40usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(super::unbase64(&super::base64(&bytes)), Some(bytes));
        }
        assert_eq!(super::unbase64("Zm9v\nYmFy"), Some(b"foobar".to_vec()));
        assert_eq!(super::unbase64("not base64!"), None);
    }

    #[test]
    fn picks_an_image_only_when_no_text_is_offered() {
        assert_eq!(super::image_type("image/png\n"), Some("image/png".into()));
        assert_eq!(super::image_type("text/html\nimage/jpeg\nimage/png\n"), Some("image/png".into()));
        assert_eq!(super::image_type("text/plain;charset=utf-8\ntext/plain\ntext/html\n"), None);
        assert_eq!(super::image_type("text/plain\nimage/png\n"), None);
        assert_eq!(super::image_type("application/pdf\n"), None);
    }

    #[test]
    fn the_primary_selection_follows_the_note_without_flooding() {
        use std::sync::{Arc, Mutex};
        let sent: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let log = sent.clone();
        let mut primary = super::Primary::with(Box::new(move |t| log.lock().unwrap().push(t.map(str::to_string))));
        let sent = || sent.lock().unwrap().clone();

        // Nothing selected from the start: nothing to say.
        assert!(!primary.update(None));
        assert!(sent().is_empty());
        // A selection goes out at once, and only once.
        assert!(!primary.update(Some("one".into())));
        assert!(!primary.update(Some("one".into())));
        assert_eq!(sent(), [Some("one".to_string())]);
        // Changing faster than PRIMARY_EVERY waits, and says so...
        assert!(primary.update(Some("one two".into())));
        assert!(primary.update(Some("one two three".into())));
        assert_eq!(sent().len(), 1);
        // ...then the latest goes out.
        std::thread::sleep(super::PRIMARY_EVERY);
        assert!(!primary.update(Some("one two three".into())));
        assert_eq!(sent().last(), Some(&Some("one two three".to_string())));
        // Deselecting clears it.
        std::thread::sleep(super::PRIMARY_EVERY);
        assert!(!primary.update(None));
        assert_eq!(sent().last(), Some(&None));
        assert_eq!(sent().len(), 3);
    }

    #[test]
    fn base64_matches_reference() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }
}
