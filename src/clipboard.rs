//! System clipboard without a GUI dependency: wl-clipboard on a Wayland
//! desktop, OSC 52 everywhere else (which also works over SSH).

use std::io::Write;
use std::process::{Command, Stdio};

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
    fn base64_matches_reference() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }
}
